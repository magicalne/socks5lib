use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::{error::Error, Result};
use futures::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
pub struct Proxy<'a, I, O> {
    src: &'a mut I,
    dst: &'a mut O,
}

impl<'a, I, O> Proxy<'a, I, O>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    pub fn new(src: &'a mut I, dst: &'a mut O) -> Self {
        Self { src, dst }
    }

    pub fn poll_proxy(&mut self, buf: &mut ReadBuf, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let me = &mut *self;
        if buf.filled().is_empty() {
            if let Err(err) = ready!(Pin::new(&mut *me.src).poll_read(cx, buf)) {
                return Poll::Ready(Err(Error::IoError(err)));
            }
        }
        if let Err(err) = ready!(Pin::new(&mut *me.dst).poll_write(cx, buf.filled())) {
            return Poll::Ready(Err(Error::IoError(err)));
        }
        if let Err(err) = ready!(Pin::new(&mut *me.dst).poll_flush(cx)) {
            return Poll::Ready(Err(Error::IoError(err)));
        }
        buf.clear();
        Poll::Ready(Ok(()))
    }
}
