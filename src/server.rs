use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::{
    error::Error,
    proto::{self, Addr, Command, Decoder, Encoder, Version},
    proxy::Proxy,
    to_read_buf, Connector, Result,
};
use bytes::{Buf, BytesMut};
use futures::{
    future::{BoxFuture},
    ready, Future,
};
use proto::Reply;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
};
use tracing::{debug, info, trace};

pub struct Server<C> {
    listener: TcpListener,
    connector: C,
}

impl<C> Server<C> {
    pub async fn new(port: Option<u16>, connector: C) -> Result<Self> {
        let port = port.unwrap_or(1080);
        let socket_addr = ("0.0.0.0", port);
        let listener = TcpListener::bind(&socket_addr).await?;
        info!("Socks server bind to {:?}", &socket_addr);
        Ok(Self {
            listener,
            connector,
        })
    }
}
impl<IO: 'static, C: 'static> Server<C>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = IO>,
{
    pub async fn run(&mut self) -> Result<()> {
        while let Ok((stream, addr)) = self.listener.accept().await {
            trace!("Accept addr: {:?}", addr);
            let connector = self.connector.clone();
            let conn = Conn::new(stream, connector);
            let connection = Connection::new(conn);
            tokio::spawn(async move {
                if let Err(err) = connection.await {
                    debug!("Connection error: {:?}", err);
                }
            });
        }
        Ok(())
    }
}

#[pin_project::pin_project]
pub struct Conn<IO, R, C> {
    #[pin]
    io: IO, /* FIX io is pending */
    remote: Option<R>,
    #[pin]
    remote_fut: Option<BoxFuture<'static, std::io::Result<R>>>,
    connector: C,
    state: State,
    read_buf: BytesMut,
    write_buf: BytesMut,
    ver: Option<Version>,
    addr: Option<Addr>,
}

impl<IO, R, C> Conn<IO, R, C> {
    fn new(io: IO, connector: C) -> Self {
        let state = State::Negotiation;
        Self {
            io,
            remote: None,
            remote_fut: None,
            connector,
            state,
            read_buf: BytesMut::with_capacity(8192),
            write_buf: BytesMut::with_capacity(8192),
            ver: None,
            addr: None,
        }
    }
}

impl<IO, R, C> Conn<IO, R, C>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = R>,
{
    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        loop {
            match &self.state {
                State::Negotiation => {
                    trace!("negotation");
                    let _ = ready!(self.poll_negotiation(cx))?;
                }
                State::SubNegotiation => {
                    trace!("subnegotation");
                    let _ = ready!(self.poll_subnegotiatoin(cx))?;
                }
                State::OpenRemote => {
                    trace!("Open remote");
                    let _ = ready!(self.poll_remote(cx))?;
                }
                State::Connected => {
                    trace!("Connected...");
                    match ready!(self.poll_connected(cx)) {
                        Ok(_) => {}
                        Err(err) => return Poll::Ready(Err(err)),
                    }
                    // let p1 = self.poll_connected(cx);
                    // let p2 = self.poll_connected2(cx);
                    // match (p1, p2) {
                    //     (Poll::Ready(Err(err)), _) | (_, Poll::Ready(Err(err))) => {
                    //         return Poll::Ready(Err(err))
                    //     }
                    //     (Poll::Pending, _) | (_, Poll::Pending) => return Poll::Pending,
                    //     (_, _) => {}
                    // }
                }
            }
        }
    }

    fn poll_negotiation(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut buf = to_read_buf(&mut self.read_buf);
        match ready!(Pin::new(&mut self.io).poll_read(cx, &mut buf)) {
            Ok(_) => {
                let filled = buf.filled();
                trace!("poll negotiation read: {:?}", filled);
                if filled.is_empty() {
                    return Poll::Ready(Err(Error::ConnectionClose));
                }
                match Decoder::parse_connecting(filled) {
                    Ok((ver, methods)) => {
                        let m = methods.first().unwrap();
                        Encoder::encode_method_select_msg(ver, m, &mut self.write_buf);
                        let buf = self.write_buf.chunk();
                        trace!("poll negotiation writing: {:?}", buf);
                        if let Err(err) = ready!(Pin::new(&mut self.io).poll_write(cx, buf)) {
                            return Poll::Ready(Err(Error::IoError(err)));
                        }
                        if let Err(err) = ready!(Pin::new(&mut self.io).poll_flush(cx)) {
                            return Poll::Ready(Err(Error::IoError(err)));
                        }
                        self.state = State::SubNegotiation;
                        trace!("Set to Negotiation state.");
                        Poll::Ready(Ok(()))
                    }
                    Err(err) => Poll::Ready(Err(err)),
                }
            }
            Err(err) => Poll::Ready(Err(Error::IoError(err))),
        }
    }

    fn poll_remote(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let ver = self.ver.as_ref().unwrap();
        let addr = self.addr.as_ref().unwrap();
        trace!("poll remote: {:?}", addr);
        if self.remote_fut.is_none() {
            let addr = self.addr.clone().unwrap();
            let fut = self.connector.connect(addr);
            self.remote_fut = Some(fut);
        }
        let a = self.remote_fut.as_mut().unwrap();
        let remote_io = ready!(a.as_mut().poll(cx));
        match remote_io {
            Ok(io) => {
                trace!("Remote connected!");
                self.remote = Some(io);
                self.state = State::Connected;
                self.write_buf.clear();
                Encoder::encode_server_reply(ver, &Reply::Succeeded, addr, &mut self.write_buf);
                let buf = self.write_buf.chunk();
                trace!("poll sub negotiation writing: {:?}", buf);
                if let Err(err) = ready!(Pin::new(&mut self.io).poll_write(cx, buf)) {
                    return Poll::Ready(Err(Error::IoError(err)));
                }
                if let Err(err) = ready!(Pin::new(&mut self.io).poll_flush(cx)) {
                    return Poll::Ready(Err(Error::IoError(err)));
                }
                Poll::Ready(Ok(()))
            }
            Err(err) => {
                trace!("connect remote with error: {:?}", err);
                Poll::Ready(Err(Error::IoError(err)))
            }
        }
    }

    fn poll_subnegotiatoin(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut buf = to_read_buf(&mut self.read_buf);
        match ready!(Pin::new(&mut self.io).poll_read(cx, &mut buf)) {
            Ok(_) => {
                let filled = buf.filled();
                trace!("poll sub negotiation read: {:?}", filled);
                if filled.is_empty() {
                    return Poll::Ready(Err(Error::ConnectionClose));
                }
                match Decoder::parse_nego_req(filled) {
                    Ok((ver, cmd, addr)) => match cmd {
                        Command::Connect => {
                            trace!("connecting remote: {:?}", &addr);
                            self.state = State::OpenRemote;
                            self.ver = Some(ver);
                            self.addr = Some(addr);
                            Poll::Ready(Ok(()))
                        }
                        Command::Bind => unimplemented!(),
                        Command::UdpAssociate => unimplemented!(),
                    },
                    Err(err) => Poll::Ready(Err(err)),
                }
            }
            Err(err) => Poll::Ready(Err(Error::IoError(err))),
        }
    }

    fn poll_connected(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let me = &mut *self;
        let mut read_buf = to_read_buf(&mut me.read_buf);
        let mut write_buf = to_read_buf(&mut me.write_buf);

        let io_src = &mut me.io;
        let io_dst = me.remote.as_mut().unwrap();

        let mut src_proxy = Proxy::new(io_src, io_dst);
        let p1 = src_proxy.poll_proxy(&mut read_buf, cx);
        let mut dst_proxy = Proxy::new(io_dst, io_src);
        let p2 = dst_proxy.poll_proxy(&mut write_buf, cx);
        trace!("p1: {:?}, p2: {:?}", &p1, &p2);
        match (p1, p2) {
            (Poll::Ready(Err(err)), _) | (_, Poll::Ready(Err(err))) => Poll::Ready(Err(err)),
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            (_, _) => Poll::Ready(Ok(())),
        }
    }
}

unsafe impl<IO, R, C> Send for Conn<IO, R, C> {}

struct Connection<IO, R, C> {
    conn: Conn<IO, R, C>,
}

impl<IO, R, C> Unpin for Connection<IO, R, C> {}

impl<IO, R, C> Connection<IO, R, C> {
    fn new(conn: Conn<IO, R, C>) -> Self {
        Self { conn }
    }
}

impl<IO, R, C> Future for Connection<IO, R, C>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = R>,
{
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        this.conn.poll_inner(cx)
    }
}

unsafe impl<IO, R, C> Send for Connection<IO, R, C> {}

enum State {
    Negotiation,
    SubNegotiation,
    OpenRemote,
    Connected,
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};

    #[test]
    fn test() {
        let mut bytes = BytesMut::with_capacity(100);
        put_u8(1, &mut bytes);
        assert_eq!(1, bytes[0]);
    }

    fn put_u8(b: u8, buf: &mut impl BufMut) {
        buf.put_u8(b);
    }
}
