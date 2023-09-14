use std::io::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

pub(crate) enum Stream {
    Tcp(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl Stream {
    pub(crate) fn remote_addr(&self) -> Result<std::net::SocketAddr> {
        match self {
            Self::Tcp(stream) => stream.peer_addr(),
            Self::Tls(stream) => stream.get_ref().0.peer_addr(),
        }
    }

    pub(crate) async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf).await,
            Self::Tls(stream) => stream.read(buf).await,
        }
    }

    pub(crate) async fn write(&mut self, buf: &[u8]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf).await,
            Self::Tls(stream) => stream.write(buf).await,
        }
    }

    pub(crate) async fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write_vectored(bufs).await,
            Self::Tls(stream) => stream.write_vectored(bufs).await,
        }
    }
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}

impl From<TlsStream<TcpStream>> for Stream {
    fn from(stream: TlsStream<TcpStream>) -> Self {
        Self::Tls(stream)
    }
}
