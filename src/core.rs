use crate::response::{empty_response, WSGIResponseBody, WSGIStartResponse};
use crate::utils::{new_gil_pool, with_gil};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use encoding::all::ISO_8859_1;
use encoding::{DecoderTrap, Encoding};
use futures::StreamExt;
use http::{header, request::Parts, StatusCode};
use http_body_util::{BodyStream, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{body::Body, Request, Response, Version};
use hyper_util::rt::TokioExecutor;
use hyper_util::server::conn::auto;
use lazy_static::lazy_static;
use log::{debug, info};
use pyo3::ffi::{PyDict_SetItemString, PySys_GetObject};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyModule;
use pyo3::{intern, Py, Python};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use urlencoding::decode_binary;

lazy_static! {
    static ref PY_BYTES_IO: PyObject = Python::with_gil(|py| PyModule::import_bound(py, "io")
        .unwrap()
        .getattr("BytesIO")
        .unwrap()
        .into());
}

#[derive(Clone)]
pub struct Rustgi {
    app: Arc<PyObject>,
    address: SocketAddr,
    max_body_size: usize,
}

impl Rustgi {
    pub fn new(address: SocketAddr, app: PyObject) -> Self {
        Self {
            app: Arc::new(app),
            address,
            max_body_size: 1024 * 1024,
        }
    }

    pub fn set_max_body_size(&mut self, max_body_size: usize) {
        self.max_body_size = max_body_size;
    }

    pub fn get_wsgi_app(&self) -> Arc<PyObject> {
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

    async fn extract_parts_and_body(&self, req: Request<Incoming>) -> Result<(Parts, Bytes)> {
        let mut buf = bytes::BytesMut::with_capacity(req.size_hint().lower() as usize);
        let (parts, body) = req.into_parts();
        let content_length = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
        let body_limit = match content_length {
            Some(len) => self.get_max_body_size().min(len),
            None => self.get_max_body_size(),
        };
        let mut body = BodyStream::new(Limited::new(body, body_limit));

        while let Some(frame) = body.next().await {
            let frame = match frame {
                Ok(frame) => frame,
                Err(err) => return Err(anyhow!(err)),
            };

            if frame.is_trailers() {
                continue;
            }

            let frame = frame.into_data().unwrap();
            buf.extend_from_slice(frame.as_ref());
        }

        Ok((parts, buf.freeze()))
    }

    async fn process_response(
        self,
        pool: Arc<rayon::ThreadPool>,
        remote_addr: SocketAddr,
        parts: Parts,
        body: Bytes,
    ) -> Result<Response<WSGIResponseBody>> {
        with_gil(pool.clone(), move |py| -> Result<_> {
            let environ = self.get_default_environ(py)?;
            environ.set_item(intern!(environ.py(), "wsgi.input"), PY_BYTES_IO.call0(py)?)?;
            environ.set_item(intern!(py, "REMOTE_ADDR"), remote_addr.ip().to_string())?;
            environ.set_item(intern!(py, "REMOTE_PORT"), remote_addr.port())?;
            environ.set_item(
                intern!(py, "SERVER_PROTOCOL"),
                match parts.version {
                    Version::HTTP_09 => intern!(py, "HTTP/0.9"),
                    Version::HTTP_10 => intern!(py, "HTTP/1.0"),
                    Version::HTTP_11 => intern!(py, "HTTP/1.1"),
                    Version::HTTP_2 => intern!(py, "HTTP/2.0"),
                    Version::HTTP_3 => intern!(py, "HTTP/3.0"),
                    _ => {
                        return Ok(empty_response(StatusCode::HTTP_VERSION_NOT_SUPPORTED));
                    }
                },
            )?;
            environ.set_item(intern!(py, "REQUEST_METHOD"), parts.method.as_str())?;

            let uri = parts.uri;
            environ.set_item(
                intern!(py, "PATH_INFO"),
                ISO_8859_1
                    .decode(
                        decode_binary(uri.path().as_bytes()).as_ref(),
                        DecoderTrap::Replace,
                    )
                    .unwrap_or("".into()),
            )?; // i don't understand???? https://peps.python.org/pep-3333/#url-reconstruction
            environ.set_item(intern!(py, "QUERY_STRING"), uri.query().unwrap_or(""))?;
            environ.set_item(
                intern!(py, "wsgi.url_scheme"),
                uri.scheme_str().unwrap_or("http"),
            )?;

            for (key, value) in parts.headers.iter() {
                match *key {
                    header::CONTENT_LENGTH => {
                        environ.set_item(intern!(py, "CONTENT_LENGTH"), value.to_str()?)?;
                        continue;
                    }
                    header::CONTENT_TYPE => {
                        environ.set_item(intern!(py, "CONTENT_TYPE"), value.to_str()?)?;
                        continue;
                    }
                    _ => {}
                }

                let key = "HTTP_".to_owned()
                    + &key
                        .as_str()
                        .chars()
                        .map(|c| {
                            if c == '-' {
                                '_'
                            } else {
                                c.to_ascii_uppercase()
                            }
                        })
                        .collect::<String>();

                environ.set_item(key, value.to_str()?)?;
            }

            let input = environ
                .get_item(intern!(py, "wsgi.input"))
                .unwrap()
                .unwrap();
            input.call_method1(intern!(py, "write"), (body.as_ref(),))?;
            input.call_method1(intern!(py, "seek"), (0,))?;
            let wsgi_response_config: Py<WSGIStartResponse> =
                Py::new(py, WSGIStartResponse::new())?;
            let wsgi_iter = self
                .get_wsgi_app()
                .call1(py, (environ, &wsgi_response_config))?;
            WSGIStartResponse::take_response(wsgi_response_config.bind(py), pool, wsgi_iter)
        })
        .await
    }

    pub fn serve(&self) -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()?;
        let local = tokio::task::LocalSet::new();
        let gil_thread_pool = new_gil_pool()?;

        let service_factory = |remote_addr: std::net::SocketAddr| {
            let rustgi = self.clone();
            let gil_thread_pool = gil_thread_pool.clone();

            move |req: Request<Incoming>| {
                let rustgi = rustgi.clone();
                let gil_thread_pool = gil_thread_pool.clone();
                async move {
                    let (parts, body) = match rustgi.extract_parts_and_body(req).await {
                        Ok(parts_and_body) => parts_and_body,
                        Err(err) => {
                            match err.downcast_ref::<Box<dyn std::error::Error + Send + Sync>>() {
                                Some(inner_err) => {
                                    if inner_err.is::<LengthLimitError>() {
                                        return Ok(empty_response(StatusCode::PAYLOAD_TOO_LARGE));
                                    }
                                }
                                _ => {}
                            }

                            return Err(err);
                        }
                    };

                    rustgi
                        .process_response(gil_thread_pool, remote_addr, parts, body)
                        .await
                }
            }
        };

        local.block_on(&rt, async {
            let address = self.address;
            let listener = TcpListener::bind(address).await?;
            with_gil(gil_thread_pool.clone(), move |_| {
                info!("You can connect to the server using `nc`:");
                info!("$ nc {}", address.to_string());
            }).await;

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    with_gil(gil_thread_pool.clone(), |_| { info!("Ctrl-C received, shutting down"); }).await;
                }
                _ = async {
                    loop {
                        let gil_thread_pool = gil_thread_pool.clone();
                        let (stream, remote_addr) = match listener.accept().await {
                            Ok(result) => result,
                            Err(e) => {
                                with_gil(gil_thread_pool, move |_| { debug!("Error accepting connection: {:?}", e); }).await;
                                continue;
                            }
                        };

                        if let Err(err) = stream.set_nodelay(true) {
                            with_gil(gil_thread_pool, move |_| { debug!("Error setting TCP_NODELAY: {:?}", err); }).await;
                            continue;
                        }

                        let service = service_fn(service_factory(remote_addr));
                        tokio::task::spawn_local(async move {
                            if let Err(err) = auto::Builder::new(TokioExecutor::new()).serve_connection(
                                hyper_util::rt::TokioIo::new(stream),
                                service,
                            ).await {
                                with_gil(gil_thread_pool, move |_| { debug!("Error serving connection: {:?}", err); }).await;
                            }
                        });
                    }
                } => {}
            };

            Ok(())
        })
    }

    pub(crate) fn get_default_environ<'p>(&self, py: Python<'p>) -> PyResult<Bound<'p, PyDict>> {
        let environ = PyDict::new_bound(py);
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
