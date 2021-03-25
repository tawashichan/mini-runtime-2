use crate::{MiniTokio,ENTRY_MAP,REGISTRY};
use futures::{AsyncRead, AsyncWrite};
use mio;
use std::io::Error;
use std::os::unix::io::AsRawFd;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::io::{self, Read, Write};

#[derive(Debug)]
pub struct TcpStream {
    inner: mio::net::TcpStream,
}

impl TcpStream {
    pub fn from_mio(stream: mio::net::TcpStream) -> TcpStream {
        let fd = stream.as_raw_fd();
        let mut stream = stream;
        {
            MiniTokio::register_source(
                REGISTRY.get().unwrap(),
                &mut stream,
                mio::Token(fd as usize),
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            );
        };
        TcpStream { inner: stream }
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        let waker = ctx.waker();

        match self.inner.read(buf) {
            Ok(len) => Poll::Ready(Ok(len)),
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                let fd = self.inner.as_raw_fd();

                MiniTokio::register_entry(ENTRY_MAP.get().unwrap(), mio::Token(fd as usize), waker.clone());
                Poll::Pending
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}


impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        let waker = ctx.waker();

        match self.inner.write(buf) {
            Ok(len) => Poll::Ready(Ok(len)),
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                println!("would write block");
                let fd = self.inner.as_raw_fd();

                MiniTokio::register_entry(ENTRY_MAP.get().unwrap(), mio::Token(fd as usize), waker.clone());
                Poll::Pending
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _lw: &mut Context) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _lw: &mut Context) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }
}
