use crate::tcp_stream;
use crate::{MiniTokio, ENTRY_MAP, REGISTRY};
use futures::Stream;
use mio;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use std::os::unix::io::AsRawFd;

#[derive(Debug)]
pub struct TcpListener {
    socket: mio::net::TcpListener,
}

impl TcpListener {
    pub fn bind(addr: SocketAddr) -> Result<TcpListener, Box<dyn std::error::Error>> {
        let mut socket = mio::net::TcpListener::bind(addr)?;
        let fd = socket.as_raw_fd();
        // ロック期間をなるべく短くする
        {
            MiniTokio::register_source(
                REGISTRY.get().unwrap(),
                &mut socket,
                mio::Token(fd as usize),
                mio::Interest::READABLE,
            );
        }

        Ok(TcpListener { socket })
    }
}

impl Stream for TcpListener {
    type Item = tcp_stream::TcpStream;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Option<Self::Item>> {
        let waker = ctx.waker();

        match self.socket.accept() {
            Ok((conn, _)) => {
                println!("ready...");
                let stream = tcp_stream::TcpStream::from_mio(conn);
                Poll::Ready(Some(stream))
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                let fd = self.socket.as_raw_fd();

                println!("would listener block");
                MiniTokio::register_entry(
                    ENTRY_MAP.get().unwrap(),
                    mio::Token(fd as usize),
                    waker.clone(),
                );
                Poll::Pending
            }
            Err(err) => panic!("error {:?}", err),
        }
    }
}
