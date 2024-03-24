use crate::error::Error;
use crate::service::get_service;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use log::{debug, info};
use pyo3::ffi::{PyDict_SetItemString, PySys_GetObject};
use pyo3::types::PyDict;
use pyo3::{intern, prelude::*, AsPyPointer};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tower_http::body::Limited;

#[derive(Clone)]
pub struct Rustgi {
    app: PyObject,
    address: SocketAddr,
    max_body_size: usize,
}

impl Rustgi {
    pub fn new(address: SocketAddr, app: PyObject) -> Self {
        Self {
            app,
            address,
            max_body_size: 1024 * 1024,
        }
    }

    pub fn set_max_body_size(&mut self, max_body_size: usize) {
        self.max_body_size = max_body_size;
    }

    pub fn get_wsgi_app(&self) -> PyObject {
        self.app.clone()
    }

    pub fn get_host(&self) -> String {
        self.address.ip().to_string()
    }

    pub fn get_port(&self) -> u16 {
        self.address.port()
    }

    pub fn get_max_body_size(&self) -> usize {
        self.max_body_size
    }

    pub fn serve(&self) -> Result<(), Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        let local = tokio::task::LocalSet::new();

        info!("You can connect to the server using `nc`:");
        info!("$ nc {}", self.address.to_string());

        local.block_on(&rt, async {
            let listener = TcpListener::bind(self.address).await?;

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl-C received, shutting down");
                }
                _ = async {
                    loop {
                        let (stream, remote_addr) = match listener.accept().await {
                            Ok(result) => result,
                            Err(e) => {
                                debug!("Error accepting connection: {}", e);
                                continue;
                            }
                        };
                        let rustgi = self.clone();

                        tokio::task::spawn_local(async move {
                            let service = tower::ServiceBuilder::new()
                                .layer(tower_http::limit::RequestBodyLimitLayer::new(
                                    rustgi.get_max_body_size(),
                                ))
                                .service(get_service::<Limited<Incoming>>(rustgi, remote_addr));


                            if let Err(err) = http1::Builder::new().serve_connection(
                                hyper_util::rt::TokioIo::new(stream),
                                hyper_util::service::TowerToHyperService::new(service)
                            ).await {
                                debug!("Error serving connection: {}", err);
                            }
                        });
                    }
                } => {}
            };

            Ok(())
        })
    }

    pub(crate) fn get_default_environ<'p>(&self, py: Python<'p>) -> Result<&'p PyDict, Error> {
        let environ = PyDict::new(py);
        environ.set_item(intern!(py, "SCRIPT_NAME"), intern!(py, ""))?;
        environ.set_item(intern!(environ.py(), "SERVER_NAME"), self.get_host())?;
        environ.set_item(intern!(environ.py(), "SERVER_PORT"), self.get_port())?;
        environ.set_item(intern!(environ.py(), "wsgi.version"), (1, 0))?;
        unsafe {
            PyDict_SetItemString(
                environ.as_ptr(),
                "wsgi.errors\0".as_ptr() as *const _,
                PySys_GetObject("stderr\0".as_ptr() as *const _),
            )
        };
        // tell Flask/other WSGI apps that the input has been terminated
        environ.set_item(intern!(environ.py(), "wsgi.input_terminated"), true)?;
        environ.set_item(intern!(environ.py(), "wsgi.multithread"), false)?;
        environ.set_item(intern!(environ.py(), "wsgi.multiprocess"), true)?;
        environ.set_item(intern!(environ.py(), "wsgi.run_once"), false)?;

        Ok(environ)
    }
}
