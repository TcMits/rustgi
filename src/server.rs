use hyper::{server::conn::http1::Builder, server::conn::http1::Connection};
use std::cell::Cell;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::{collections::HashMap, future::Future, io, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::debug;

use mio::{
    event::Event,
    net::{TcpListener, TcpStream},
    Events, Interest, Poll, Token,
};

use crate::wsgi::WSGICaller;

const SERVER: mio::Token = mio::Token(0);
const N_EVENTS: usize = 1024;

thread_local! {
    static SERVER_REF: Cell<Option<ServerRef>> = Cell::new(None);
}

fn get_unique_token(identity_token: &mut usize) -> usize {
    *identity_token = identity_token.wrapping_add(1);
    if *identity_token == SERVER.0 {
        *identity_token = identity_token.wrapping_add(1);
    }

    *identity_token
}

pub struct ServerConfig {
    pub tcp_listener: TcpListener,
    pub service_builder: Builder,
}

type ConnectionMap = HashMap<
    mio::Token,
    (
        Connection<ServerTcpStream, WSGICaller>,
        TcpStream,
        Option<Event>,
    ),
>;

struct ServerRef {
    service_builder: Builder,
    server: TcpListener,
    identity_token: usize,
    events: Events,
    mio_poll: Poll,
    connections: ConnectionMap,
}

impl ServerRef {
    fn new(mut config: ServerConfig) -> io::Result<Self> {
        let mio_poll = Poll::new()?;
        mio_poll
            .registry()
            .register(&mut config.tcp_listener, SERVER, Interest::READABLE)?;

        Ok(Self {
            service_builder: config.service_builder,
            server: config.tcp_listener,
            identity_token: SERVER.0,
            events: Events::with_capacity(N_EVENTS),
            mio_poll,
            connections: HashMap::new(),
        })
    }

    fn reset(&mut self, config: ServerConfig) -> io::Result<()> {
        self.mio_poll.registry().deregister(&mut self.server)?;

        self.service_builder = config.service_builder;
        self.server = config.tcp_listener;
        self.identity_token = SERVER.0;

        self.mio_poll
            .registry()
            .register(&mut self.server, SERVER, Interest::READABLE)?;

        for (_, (_, ref mut raw_conn, _)) in self.connections.drain() {
            raw_conn.shutdown(std::net::Shutdown::Both)?;
            self.mio_poll.registry().deregister(raw_conn)?;
        }

        Ok(())
    }
}

pub struct Server {
    rustgi: crate::core::Rustgi,
}

struct ServerTcpStream {
    token: Token,
}

impl AsyncRead for ServerTcpStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context,
        buf: &mut ReadBuf,
    ) -> std::task::Poll<io::Result<()>> {
        let this: &mut ServerRef =
            SERVER_REF.with(|sr| unsafe { &mut *sr.as_ptr() }.as_mut().unwrap());

        let (_, ref mut conn, ref ev) = match this.connections.get_mut(&self.token) {
            Some(v) => v,
            None => {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    "connection not found",
                )));
            }
        };

        match *ev {
            Some(ref e) if !e.is_readable() => {
                return std::task::Poll::Pending;
            }
            None => {
                return std::task::Poll::Pending;
            }
            _ => {}
        };

        let b =
            unsafe { &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>] as *mut [u8]) };

        loop {
            match conn.read(b) {
                Ok(n) => {
                    unsafe { buf.assume_init(n) };
                    buf.advance(n);
                    return std::task::Poll::Ready(Ok(()));
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    return std::task::Poll::Pending;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return std::task::Poll::Ready(Err(e));
                }
            }
        }
    }
}

impl AsyncWrite for ServerTcpStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let this: &mut ServerRef =
            SERVER_REF.with(|sr| unsafe { &mut *sr.as_ptr() }.as_mut().unwrap());

        let (_, ref mut conn, ref ev) = match this.connections.get_mut(&self.token) {
            Some(v) => v,
            None => {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    "connection not found",
                )));
            }
        };

        match *ev {
            Some(ref e) if !e.is_writable() => {
                return std::task::Poll::Pending;
            }
            None => {
                return std::task::Poll::Pending;
            }
            _ => {}
        };

        loop {
            match conn.write(buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    return std::task::Poll::Pending;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return std::task::Poll::Ready(Err(e));
                }
                result => {
                    return std::task::Poll::Ready(result);
                }
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context,
    ) -> std::task::Poll<io::Result<()>> {
        // tcp flush is a no-op
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context,
    ) -> std::task::Poll<io::Result<()>> {
        let this: &mut ServerRef =
            SERVER_REF.with(|sr| unsafe { &mut *sr.as_ptr() }.as_mut().unwrap());

        let (_, ref conn, _) = match this.connections.get_mut(&self.token) {
            Some(v) => v,
            None => {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    "connection not found",
                )));
            }
        };

        conn.shutdown(std::net::Shutdown::Write)?;
        std::task::Poll::Ready(Ok(()))
    }
}

impl Server {
    pub(crate) fn new(rustgi: crate::core::Rustgi, config: ServerConfig) -> io::Result<Self> {
        SERVER_REF.with(|sr| {
            let server_ref = unsafe { &mut *sr.as_ptr() };
            match *server_ref {
                Some(ref mut some_sr) => some_sr.reset(config),
                None => {
                    server_ref.replace(ServerRef::new(config)?);
                    Ok(())
                }
            }
        })?;

        Ok(Self { rustgi })
    }
}

impl Future for Server {
    type Output = io::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // assume that SERVER_REF is initialized
        let this: &mut ServerRef =
            SERVER_REF.with(|sr| unsafe { &mut *sr.as_ptr() }.as_mut().unwrap());

        this.connections.reserve(N_EVENTS);

        let mut poll_tokens: [MaybeUninit<Token>; N_EVENTS] =
            unsafe { MaybeUninit::uninit().assume_init() };

        for _ in 0..16 {
            let mut poll_tokens_size: usize = 0;

            match this
                .mio_poll
                .poll(&mut this.events, Some(Duration::from_secs(0)))
            {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                err => {
                    return std::task::Poll::Ready(err);
                }
            }

            for event in this.events.iter() {
                match event.token() {
                    SERVER => loop {
                        // indicates we can accept an connection.
                        let (mut connection, _) = match this.server.accept() {
                            Ok((connection, address)) => (connection, address),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                // If we get a `WouldBlock` error we know our
                                // listener has no more incoming connections queued,
                                // so we can return to polling and wait for some
                                // more.
                                break;
                            }
                            Err(e) => {
                                // If it was any other kind of error, something went
                                // wrong and we terminate with an error.
                                return std::task::Poll::Ready(Err(e));
                            }
                        };

                        let token = Token(get_unique_token(&mut this.identity_token));
                        this.mio_poll.registry().register(
                            &mut connection,
                            token,
                            Interest::READABLE.add(Interest::WRITABLE),
                        )?;

                        let stream = ServerTcpStream { token };

                        this.connections.insert(
                            token,
                            (
                                this.service_builder
                                    .serve_connection(stream, self.rustgi.wsgi_caller()),
                                connection,
                                None,
                            ),
                        );
                    },
                    token => {
                        if let Some((_, _, ref mut ev)) = this.connections.get_mut(&token) {
                            ev.replace(event.clone());
                            poll_tokens[poll_tokens_size].write(token);
                            poll_tokens_size += 1;
                        }
                    }
                }
            }

            for token in &poll_tokens[0..poll_tokens_size] {
                let init_token = unsafe { token.assume_init() };
                let (ref mut conn, ref mut raw_conn, _) =
                    this.connections.get_mut(&init_token).unwrap();

                match Pin::new(conn).poll(cx) {
                    std::task::Poll::Ready(result) => {
                        if let Err(e) = result {
                            debug!("error serving connection: {}", e)
                        }

                        this.mio_poll.registry().deregister(raw_conn)?;
                        this.connections.remove(&init_token);
                    }
                    _ => {}
                }
            }
        }

        this.connections.shrink_to(N_EVENTS);
        std::task::Poll::Pending
    }
}
