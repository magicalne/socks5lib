use std::{future::Future, io, mem::MaybeUninit, pin::Pin};

use bytes::BufMut;
use futures::{FutureExt, future::BoxFuture};
use proto::Addr;
use tokio::{io::{AsyncRead, AsyncWrite, ReadBuf}, net::TcpStream};

pub mod error;
pub mod proto;
pub mod server;
pub mod proxy;

pub type Result<T> = std::result::Result<T, crate::error::Error>;

pub trait Connector: Clone {
    type Connection: AsyncRead + AsyncWrite + Unpin;
    // type Future: Future<Output = io::Result<Self::Connection>> + Send + Unpin;

    fn connect(&mut self, a: Addr) -> BoxFuture<'static, io::Result<Self::Connection>>;
}

#[derive(Clone)]
pub struct LocalConnector;

impl Connector for LocalConnector {
    type Connection = TcpStream;

    // type Future = Pin<Box<dyn Future<Output = io::Result<Self::Connection>> + Send>>;

    fn connect(&mut self, a: Addr) -> BoxFuture<'static, io::Result<Self::Connection>> {
        match a {
            Addr::SocketAddr(addr) => Box::pin(TcpStream::connect(addr)),
            Addr::DomainName(host, port) => {
                let addr = (String::from_utf8(host).unwrap(), port);
                Box::pin(TcpStream::connect(addr))
            }
        }
    }
}

pub fn to_read_buf<'a>(buf: &mut impl BufMut) -> ReadBuf<'a> {
    let dst = buf.chunk_mut();
    let dst = unsafe { &mut *(dst as *mut _ as *mut [MaybeUninit<u8>]) };
    ReadBuf::uninit(dst)
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
