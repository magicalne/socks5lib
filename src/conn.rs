use std::{
    io,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BytesMut};
use futures::{future::BoxFuture, ready, Future};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::io::{poll_read_buf, poll_write_buf};
use tracing::trace;

use crate::{
    error::Error,
    proto::{Addr, Command, Decoder, Encoder, Reply, Version},
    Connector, Result,
};

#[pin_project::pin_project(project = ConnectingStateProj)]
enum ConnectingState<C, O> {
    Negotiation,
    SubNegotiation,
    OpenRemote(#[pin] CommandConnect<C, O>),
}
#[pin_project::pin_project]
struct Connecting<IO, C, O> {
    #[pin]
    io: IO,
    buf: BytesMut,
    #[pin]
    o: Option<O>,
    o_buf: BytesMut,
    #[pin]
    state: ConnectingState<C, O>,
    connector: C,
    phantom_data: PhantomData<O>,
}

impl<IO, C, O> Connecting<IO, C, O> {
    fn new(io: IO, connector: C) -> Self {
        let buf = BytesMut::with_capacity(1024 * 8);
        let o_buf = BytesMut::with_capacity(1024 * 8);
        Self {
            io,
            buf,
            o: None,
            o_buf,
            state: ConnectingState::Negotiation,
            connector,
            phantom_data: PhantomData,
        }
    }
}

impl<IO, C, O> AsyncRead for Connecting<IO, C, O>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().io.poll_read(cx, buf)
    }
}

impl<IO, C, O> AsyncWrite for Connecting<IO, C, O>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.project().io.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().io.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().io.poll_shutdown(cx)
    }
}

impl<IO, C, O> Future for Connecting<IO, C, O>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    type Output = Result<O>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut me = self.project();
        loop {
            match me.state.as_mut().project() {
                ConnectingStateProj::Negotiation => {
                    trace!("negotiation...");
                    me.buf.clear();
                    let n = ready!(poll_read_buf(me.io.as_mut(), cx, me.buf))?;
                    if n == 0 {
                        return Poll::Ready(Err(Error::ConnectionClose));
                    }
                    let buf = me.buf.chunk();
                    let (ver, methods) = Decoder::parse_connecting(buf)?;
                    let m = methods.first().unwrap();
                    me.buf.clear();
                    Encoder::encode_method_select_msg(ver, m, me.buf);
                    trace!("poll negotiation writing: {:?}", me.buf);
                    let n = ready!(poll_write_buf(me.io.as_mut(), cx, me.buf))?;
                    if n == 0 {
                        return Poll::Ready(Err(Error::ConnectionClose));
                    }
                    ready!(me.io.as_mut().poll_flush(cx))?;
                    me.state.set(ConnectingState::SubNegotiation);
                }
                ConnectingStateProj::SubNegotiation => {
                    trace!("sub negotiation...");
                    me.buf.clear();
                    let n = ready!(poll_read_buf(me.io.as_mut(), cx, me.buf))?;
                    if n == 0 {
                        return Poll::Ready(Err(Error::ConnectionClose));
                    }
                    let buf = me.buf.chunk();
                    let (ver, cmd, addr) = Decoder::parse_nego_req(buf)?;
                    match cmd {
                        Command::Connect => {
                            let cmd_connect = CommandConnect {
                                ver,
                                addr,
                                remote_fut: None,
                                connector: me.connector.clone(),
                                phantom: *me.phantom_data,
                            };
                            me.state.set(ConnectingState::OpenRemote(cmd_connect));
                        }
                        Command::Bind => {
                            unimplemented!()
                        }
                        Command::UdpAssociate => {
                            unimplemented!()
                        }
                    };
                }
                ConnectingStateProj::OpenRemote(connect) => {
                    let ver = connect.ver.clone();
                    let addr = connect.addr.clone();
                    let remote_io = ready!(connect.poll(cx))?;
                    // me.o.set(Some(remote_io));
                    trace!("Remote connected.");
                    me.buf.clear();
                    Encoder::encode_server_reply(&ver, &Reply::Succeeded, &addr, me.buf);
                    let n = ready!(poll_write_buf(me.io.as_mut(), cx, me.buf))?;
                    if n == 0 {
                        return Poll::Ready(Err(Error::ConnectionClose));
                    }
                    let _ = ready!(me.io.as_mut().poll_flush(cx))?;
                    return Poll::Ready(Ok(remote_io));
                    // me.state.set(ConnectingState::Proxy);
                }
            }
        }
    }
}

#[pin_project::pin_project]
struct CommandConnect<C, O> {
    ver: Version,
    addr: Addr,
    #[pin]
    remote_fut: Option<BoxFuture<'static, io::Result<O>>>,
    connector: C,
    phantom: PhantomData<O>,
}

impl<C, O> Future for CommandConnect<C, O>
where
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    type Output = io::Result<O>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut me = self.project();
        if me.remote_fut.is_none() {
            *me.remote_fut = Some(me.connector.connect(me.addr.clone()));
        }
        let remote_io = ready!(me.remote_fut.as_pin_mut().unwrap().poll(cx))?;
        Poll::Ready(Ok(remote_io))
    }
}
