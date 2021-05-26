use crate::{Connector, Result};
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
            let connection = crate::conn::Connection::new(stream, connector);
            // let conn = Conn::new(stream, connector);
            // let connection = Connection::new(conn);
            tokio::spawn(async move {
                if let Err(err) = connection.await {
                    debug!("Connection error: {:?}", err);
                }
            });
        }
        Ok(())
    }
}
