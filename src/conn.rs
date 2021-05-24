use std::{io, marker::PhantomData, pin::Pin, task::{Context, Poll}, u16};

use bytes::{Buf, BufMut, BytesMut};
use futures::{future::BoxFuture, ready, Future};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::io::{poll_read_buf, poll_write_buf};
use tracing::trace;

use crate::{
    error::Error,
    proto::{Addr, Decoder, Encoder, Version},
    Connector, Result,
};

enum ConnectingState {
    Negotiation,
    SubNegotiation,
    OpenRemote,
}
#[pin_project::pin_project]
struct Connecting<IO, C> {
    #[pin]
    io: IO,
    buf: BytesMut,
    state: ConnectingState,
    _phantom: PhantomData<C>
}

impl<IO, C> Connecting<IO, C> {
    fn new(io: IO, connector: C) -> Self {
        let buf = BytesMut::with_capacity(1024);
        Self {
            io,
            buf,
            state: ConnectingState::Negotiation,
            _phantom: PhantomData
        }
    }
}

impl<IO, C, O> Future for Connecting<IO, C>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut me = self.project();
        loop {
            match me.state {
                ConnectingState::Negotiation => {
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
                    *me.state = ConnectingState::SubNegotiation;
                }
                ConnectingState::SubNegotiation => {
                    trace!("sub negotiation...");
                    me.buf.clear();
                    let n = ready!(poll_read_buf(me.io.as_mut(), cx, me.buf))?;
                    if n == 0 {
                        return Poll::Ready(Err(Error::ConnectionClose));
                    }
                    let buf = me.buf.chunk();
                    let (ver, cmd, addr) = Decoder::parse_nego_req(buf)?;
                }
                ConnectingState::OpenRemote => {}
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
}

impl<C, O> Future for CommandConnect<C, O>
where
    C: Connector<Connection = O>,
    O: AsyncRead + AsyncWrite + Unpin,
{
    type Output = io::Result<O>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut me = self.project();
        loop {
            if me.remote_fut.is_none() {
                *me.remote_fut = Some(me.connector.connect(me.addr.clone()));
            }
            //TODO
            // let remote_io = ready!(me.remote_fut.as_pin_mut().unwrap().poll(cx))?;
        }
    }
}
