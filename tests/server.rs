use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
};

use futures::{Future, future::BoxFuture};
use lib::{proto::Addr, server::Server, Connector, LocalConnector, Result};
use tokio::net::TcpStream;
use tracing::Level;

#[tokio::test]
pub async fn socks_server_test() -> Result<()> {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Cannot set trace.");

    let connector = LocalConnector;
    let mut server = Server::new(None, connector).await?;
    server.run().await?;
    Ok(())
}

#[tokio::test]
pub async fn local_connector_test() -> Result<()> {
    let mut connector = LocalConnector;
    let addr = Addr::SocketAddr(SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(180, 101, 49, 11),
        80,
    )));
    let stream = connector.connect(addr);

    let mut my_fut = MyFut {fut: None};
    let stream = my_fut.await?;
    Ok(())
}

#[pin_project::pin_project]
struct MyFut {
    #[pin]
    fut: Option<BoxFuture<'static, std::io::Result<TcpStream>>>,
}

impl Future for MyFut {
    type Output = std::io::Result<TcpStream>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut connector = LocalConnector;
        let addr_v4 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(180, 101, 49, 11), 80));
        let addr = Addr::SocketAddr(addr_v4);
        let mut me = self.project();
        if me.fut.is_none() {
            let fut = connector.connect(addr);
            *me.fut = Some(fut);
        }
        me.fut.as_pin_mut().unwrap().poll(cx)
    }
}