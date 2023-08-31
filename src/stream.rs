use std::io::{Read, Result, Write};

use mio::{event::Source, net::TcpStream};

pub(crate) enum Stream {
    Tcp(TcpStream),
    Ssl(rustls::StreamOwned<rustls::ServerConnection, TcpStream>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Ssl(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Ssl(stream) => stream.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write_vectored(bufs),
            Self::Ssl(stream) => stream.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Ssl(stream) => stream.flush(),
        }
    }
}

impl Source for Stream {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.register(registry, token, interests),
            Self::Ssl(stream) => stream.get_mut().register(registry, token, interests),
        }
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.reregister(registry, token, interests),
            Self::Ssl(stream) => stream.get_mut().reregister(registry, token, interests),
        }
    }

    fn deregister(&mut self, registry: &mio::Registry) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.deregister(registry),
            Self::Ssl(stream) => stream.get_mut().deregister(registry),
        }
    }
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}

impl From<rustls::StreamOwned<rustls::ServerConnection, TcpStream>> for Stream {
    fn from(stream: rustls::StreamOwned<rustls::ServerConnection, TcpStream>) -> Self {
        Self::Ssl(stream)
    }
}
