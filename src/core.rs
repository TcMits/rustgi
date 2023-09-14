use crate::error::Error;
use crate::request::Request;
use log::{debug, info};
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

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

    pub fn serve(&self) -> Result<(), Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        let local = tokio::task::LocalSet::new();
        let acceptor = self.get_tls_config().map(TlsAcceptor::from);

        info!("You can connect to the server using `nc`:");
        info!("$ nc {}", self.inner.address.to_string());

        local.block_on(&rt, async {
            let listener = TcpListener::bind(self.inner.address).await?;

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl-C received, shutting down");
                }
                _ = async {
                    loop {
                        let stream = match listener.accept().await {
                            Ok((stream, _)) => stream,
                            Err(e) => {
                                debug!("Error accepting connection: {}", e);
                                continue;
                            }
                        };
                        let rustgi = self.clone();
                        let acceptor = acceptor.clone();

                        tokio::task::spawn_local(async move {
                            let mut request = match acceptor {
                                Some(ref acceptor) => {
                                    let stream = match acceptor.accept(stream).await {
                                        Ok(stream) => stream,
                                        Err(e) => {
                                            debug!("Error accepting TLS connection: {}", e);
                                            return;
                                        }
                                    };
                                    Request::new(rustgi, stream.into())
                                }
                                None => Request::new(rustgi, stream.into()),
                            };


                            if let Err(e) = request.handle().await {
                                debug!("Error serving request: {}", e);
                            }
                        });
                    }
                } => {}
            };

            Ok(())
        })
    }
}
