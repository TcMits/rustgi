use crate::core::Rustgi;
use crate::error::Error;
use crate::response::{WSGIResponseBody, WSGIStartResponse};
use encoding::all::ISO_8859_1;
use encoding::{DecoderTrap, Encoding};
use futures::StreamExt;
use hyper::{body::Body, Request, Response, Version};
use lazy_static::lazy_static;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::{intern, Py, Python};
use urlencoding::decode_binary;

lazy_static! {
    static ref PY_BYTES_IO: PyObject = Python::with_gil(|py| PyModule::import(py, "io")
        .unwrap()
        .getattr("BytesIO")
        .unwrap()
        .into());
}

pub(crate) fn get_service<B>(
    rustgi: Rustgi,
    remote_addr: std::net::SocketAddr,
) -> impl Clone + tower::Service<Request<B>, Response = Response<WSGIResponseBody>, Error = Error>
where
    B: Body + std::marker::Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B::Data: std::fmt::Debug + AsRef<[u8]>,
{
    tower::service_fn(move |req: Request<B>| {
        let rustgi = rustgi.clone();
        async move {
            let (parts, body) = {
                let mut buf = bytes::BytesMut::with_capacity(req.size_hint().lower() as usize);
                let (parts, body) = req.into_parts();
                let mut body = http_body_util::BodyStream::new(body);
                while let Some(frame) = body.next().await {
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(err) => {
                            let err: Box<_> = err.into();
                            if let Some(_) = err.downcast_ref::<http_body_util::LengthLimitError>()
                            {
                                return Ok(Response::builder()
                                    .status(413)
                                    .body(WSGIResponseBody::empty())
                                    .unwrap());
                            }

                            return Err(err.into());
                        }
                    };

                    if frame.is_trailers() {
                        continue;
                    }

                    let frame = frame.into_data().unwrap();
                    buf.extend_from_slice(frame.as_ref());
                }

                (parts, buf.freeze())
            };

            Python::with_gil(move |py| {
                let builder = {
                    let pool = unsafe { py.new_pool() };
                    let py = pool.python();
                    let environ = rustgi.get_default_environ(py)?;
                    environ
                        .set_item(intern!(environ.py(), "wsgi.input"), PY_BYTES_IO.call0(py)?)?;
                    environ.set_item(intern!(py, "REMOTE_ADDR"), remote_addr.ip().to_string())?;
                    environ.set_item(intern!(py, "REMOTE_PORT"), remote_addr.port())?;
                    environ.set_item(
                        intern!(py, "SERVER_PROTOCOL"),
                        match parts.version {
                            Version::HTTP_10 => intern!(py, "HTTP/1.0"),
                            Version::HTTP_11 => intern!(py, "HTTP/1.1"),
                            _ => {
                                return Ok(Response::builder()
                                    .status(505)
                                    .body(WSGIResponseBody::empty())
                                    .unwrap());
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
                            hyper::header::CONTENT_LENGTH => {
                                environ.set_item(intern!(py, "CONTENT_LENGTH"), value.to_str()?)?;
                                continue;
                            }
                            hyper::header::CONTENT_TYPE => {
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
                    let wsgi_response_config = Py::new(py, WSGIStartResponse::new())?;
                    let wsgi_iter = rustgi
                        .get_wsgi_app()
                        .call1(py, (environ, &wsgi_response_config))?;
                    let mut config = wsgi_response_config.as_ref(py).borrow_mut();
                    config.take_body_builder(wsgi_iter)
                };

                builder.build(py) // trigger start_response
            })
        }
    })
}
