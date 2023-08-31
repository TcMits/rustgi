use crate::error::Error;
use crate::request::Request;
use log::{debug, info};
use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token};
use mio_signals::{Signal, Signals};
use pyo3::prelude::*;
use slab::Slab;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;

// Setup some tokens to allow us to identify which event is for which socket.
const SERVER: Token = Token(usize::MAX);
const SIGNAL: Token = Token(usize::MAX - 2);

struct RustgiRef {
    app: PyObject,
    address: SocketAddr,
    max_body_size: usize,
    tls_config: Option<Arc<rustls::ServerConfig>>,
}

#[derive(Clone)]
pub struct Rustgi {
    inner: Rc<RustgiRef>,
}

impl Rustgi {
    pub fn new(address: SocketAddr, app: PyObject) -> Self {
        Self {
            inner: Rc::new(RustgiRef {
                app,
                address,
                max_body_size: 1024 * 1024,
                tls_config: None,
            }),
        }
    }

    pub fn set_max_body_size(&mut self, max_body_size: usize) {
        unsafe { &mut *(Rc::as_ptr(&self.inner) as *mut RustgiRef) }.max_body_size = max_body_size;
    }

    pub fn set_tls_config(&mut self, tls_config: Arc<rustls::ServerConfig>) {
        unsafe { &mut *(Rc::as_ptr(&self.inner) as *mut RustgiRef) }.tls_config = Some(tls_config);
    }

    pub fn get_wsgi_app(&self) -> PyObject {
        self.inner.app.clone()
    }

    pub fn get_host(&self) -> String {
        self.inner.address.ip().to_string()
    }

    pub fn get_port(&self) -> u16 {
        self.inner.address.port()
    }

    pub fn get_max_body_size(&self) -> usize {
        self.inner.max_body_size
    }

    pub fn get_tls_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        self.inner.tls_config.clone()
    }

    pub fn is_tls_enabled(&self) -> bool {
        self.inner.tls_config.is_some()
    }

    pub fn serve(&self) -> Result<(), Error> {
        let mut connections = Slab::<Request>::with_capacity(1024);
        let mut events = Events::with_capacity(1024);
        let mut poll = Poll::new()?;

        // Setup the TCP server socket.
        let mut server = TcpListener::bind(self.inner.address)?;
        poll.registry()
            .register(&mut server, SERVER, Interest::READABLE)?;

        // Create a `Signals` instance that will catch signals for us.
        let mut signals = Signals::new(Signal::Interrupt | Signal::Quit)?;
        poll.registry()
            .register(&mut signals, SIGNAL, Interest::READABLE)?;

        info!("You can connect to the server using `nc`:");
        info!("$ nc {}", self.inner.address.to_string());

        loop {
            if let Err(err) = poll.poll(&mut events, None) {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }

                return Err(err.into());
            }

            for event in events.iter() {
                match event.token() {
                    SERVER => loop {
                        let connection = match server.accept() {
                            Ok((connection, _)) => connection,
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                break;
                            }
                            Err(e) => {
                                debug!("Error accepting connection: {}", e);
                                break;
                            }
                        };

                        let entry = connections.vacant_entry();
                        let key = entry.key();
                        match Request::new(self.clone(), connection, poll.registry(), Token(key)) {
                            Ok(r) => {
                                entry.insert(r);
                            }
                            Err(e) => {
                                debug!("Error register connection: {}", e);
                            }
                        };
                    },
                    SIGNAL => {
                        // Received a signal from the OS.
                        match signals.receive()? {
                            Some(Signal::Interrupt) => {
                                // Ctrl-C was pressed.
                                info!("Ctrl-C pressed, shutting down");
                                return Ok(());
                            }
                            Some(Signal::Quit) => {
                                // Ctrl-\ was pressed.
                                info!("Ctrl-\\ pressed, shutting down");
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                    token => {
                        // Maybe received an event for a TCP connection.
                        if let Some(conn) = connections.get_mut(token.0) {
                            let registry = poll.registry();

                            if match conn.handle(registry, event) {
                                Ok(resume) => resume,
                                Err(err) => {
                                    debug!("Error handling connection: {}", err);
                                    false
                                }
                            } {
                                continue;
                            };

                            if let Err(err) = conn.clean(registry) {
                                debug!("Error closing connection: {}", err);
                            };
                            connections.remove(token.0);
                        };
                    }
                }
            }
        }
    }
}
