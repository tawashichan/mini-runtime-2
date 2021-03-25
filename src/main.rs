use crossbeam::channel;
use futures::stream::StreamExt;
use futures::task::{self, ArcWake};
use mio::{self};
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Waker};
use std::thread;
use uuid;

mod tcp_listener;
mod tcp_stream;

pub static EVENTS: OnceCell<Mutex<mio::Events>> = OnceCell::new();
pub static POLLER: OnceCell<Mutex<mio::Poll>> = OnceCell::new();
pub static REGISTRY: OnceCell<Mutex<mio::Registry>> = OnceCell::new();
pub static ENTRY_MAP: OnceCell<Mutex<EntryMap>> = OnceCell::new();
static SENDER: OnceCell<Sender> = OnceCell::new();
static TASK_LIST: OnceCell<Mutex<TaskMap>> = OnceCell::new();

pub type TaskMap = HashMap<TaskId,()>;

type Sender = channel::Sender<Arc<Task>>;

type TaskId = uuid::Uuid;

pub struct MiniTokio {
    scheduled: channel::Receiver<Arc<Task>>,
}

pub type EntryMap = HashMap<mio::Token, Entry>;

#[derive(Debug)]
pub struct Entry {
    token: mio::Token,
    waker: Waker,
}

struct Task {
    id: TaskId,
    future: Mutex<Pin<Box<dyn Future<Output = ()> + Send>>>,
    executor: channel::Sender<Arc<Task>>,
}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.schedule();
    }
}

impl Task {
    fn schedule(self: &Arc<Self>) {
        self.executor.send(self.clone());
    }

    fn poll(self: Arc<Self>) -> std::task::Poll<()> {
        println!("poll task {:?}", self.id);
        let waker = task::waker(self.clone());

        let mut cx = Context::from_waker(&waker);

        let mut future = self.future.try_lock().unwrap();

        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(_) => {
                println!("task finished {:?}", self.id);
                std::task::Poll::Ready(())
            }
            p => p,
        }
    }

    fn spawn<F>(future: F, sender: &channel::Sender<Arc<Task>>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = Arc::new(Task {
            id: uuid::Uuid::new_v4(),
            future: Mutex::new(Box::pin(future)),
            executor: sender.clone(),
        });

        // mioへのsourceの登録など,spawnして即座にpollして欲しいtaskが存在するので一旦pollする
        if let std::task::Poll::Ready(_) = task.clone().poll() {
            println!("this task is ready");
            return;
        };

        {
            let mut task_list = TASK_LIST.get().unwrap().lock().unwrap();
            task_list.insert(task.id, ());
        }

        let _ = sender.send(task.clone());
    }
}

impl MiniTokio {
    fn run<F: Future<Output = ()> + Send + 'static>(self, f: F) {
        MiniTokio::spawn(f);

        self.spawn_mio_thread();

        while let Ok(task) = self.scheduled.recv() {
            let task_presence = {
                let mut task_list = TASK_LIST.get().unwrap().lock().unwrap();
                task_list.remove(&task.id).is_some()
            };

            if task_presence {
                match task.clone().poll() {
                    std::task::Poll::Ready(_) => {
                        println!("task {:?} completed", task.id);
                        let id = task.id;
                        println!("task {:?} dropped", id);
                    }
                    std::task::Poll::Pending => {
                        {
                            let mut task_list = TASK_LIST.get().unwrap().lock().unwrap();
                            task_list.insert(task.id, ());
                        }
                    }
                };
            }
        }
    }

    // mioによるfdの監視を行うthreadを生やす
    fn spawn_mio_thread(&self) {
        let events = EVENTS.get().unwrap().clone();

        thread::spawn(move || {
            loop {
                if let Ok(mut events) = events.lock() {
                    if let Ok(mut poller) = POLLER.get().unwrap().lock() {
                        // error handlingどうする?
                        poller.poll(&mut events, None);
                    };

                    for event in events.iter() {
                        let token = event.token();

                        println!(
                            "token: {:?},is_writable: {:?},is_readable: {:?}",
                            token,
                            event.is_writable(),
                            event.is_readable()
                        );

                        if let Some(entry) = ENTRY_MAP.get().unwrap().lock().unwrap().get(&token) {
                            entry.waker.wake_by_ref();
                        }
                    }
                };
            }
        });
    }

    fn new() -> MiniTokio {
        let poller = mio::Poll::new().unwrap();
        let registry = poller.registry().try_clone().unwrap();

        POLLER.set(Mutex::new(poller));
        REGISTRY.set(Mutex::new(registry));
        ENTRY_MAP.set(Mutex::new(HashMap::new()));
        EVENTS.set(Mutex::new(mio::Events::with_capacity(4096)));
        TASK_LIST.set(Mutex::new(HashMap::new()));
        let (sender, scheduled) = channel::unbounded();

        SENDER.set(sender);
        MiniTokio { scheduled }
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Task::spawn(future, SENDER.get().unwrap());
    }

    pub fn register_entry(entry_map: &Mutex<EntryMap>, token: mio::Token, waker: Waker) {
        let mut entry_map = entry_map.lock().unwrap();

        println!("{:?}", entry_map.values());

        entry_map.insert(
            token,
            Entry {
                token: token.clone(),
                waker,
            },
        );
    }

    pub fn register_source<S: mio::event::Source + std::fmt::Debug>(
        registry: &'static Mutex<mio::Registry>,
        source: &mut S,
        token: mio::Token,
        interest: mio::Interest,
    ) {
        let registry = registry.lock().unwrap();
        registry.register(source, token, interest);
    }
}

fn main() {
    let mini_tokio = MiniTokio::new();

    let block = async {
        let addr = "127.0.0.1:8888".parse().unwrap();
        let mut server = tcp_listener::TcpListener::bind(addr).unwrap();

        while let Some(stream) = server.next().await {
            MiniTokio::spawn(process(stream));
        }
    };

    mini_tokio.run(block);
}

async fn process(mut stream: tcp_stream::TcpStream) {
    use futures::{AsyncReadExt, AsyncWriteExt};

    let mut buf = [0; 10];

    let _ = stream.read_exact(&mut buf).await;

    println!("request: {}", String::from_utf8_lossy(&buf));
    let _ = stream
        .write_all(b"HTTP/1.0 200 OK\nContent-Length: 8\nContent-Type: text/html\r\n\r\nhogehoge")
        .await;
}
