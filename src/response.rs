use crate::error::Error;
use crate::types::PyBytesBuf;
use hyper::body;
use hyper::body::Buf;
use hyper::body::Bytes;
use hyper::Response;
use pyo3::exceptions::PyValueError;
use pyo3::ffi::{PyBytes_Check, PyList_Check, PyList_Size};
use pyo3::types::{PyIterator, PyTuple};
use pyo3::{prelude::*, AsPyPointer};
use pyo3::{Py, PyObject};
use std::str::FromStr;
use std::sync::Arc;

struct WSGIResponseConfig {
    builder: Option<http::response::Builder>,
    content_length: Option<u64>,
}

#[pyclass]
pub(crate) struct WSGIStartResponse {
    config: Arc<WSGIResponseConfig>,
}

impl WSGIStartResponse {
    pub(crate) fn new() -> Self {
        Self {
            config: Arc::new(WSGIResponseConfig {
                builder: Some(http::response::Builder::new()),
                content_length: None,
            }),
        }
    }
}

#[pymethods]
impl WSGIStartResponse {
    #[pyo3(signature = (status, headers, exc_info=None))]
    fn __call__(
        &self, // it will raise borrow checker error if we use &mut self
        status: &str,
        headers: Vec<(&str, &str)>,
        exc_info: Option<&PyTuple>,
    ) -> PyResult<()> {
        let _ = exc_info;
        let this = unsafe { &mut *(Arc::as_ptr(&self.config) as *mut WSGIResponseConfig) };

        let status_pair = status
            .split_once(' ')
            .ok_or(PyValueError::new_err("invalid status"))?;

        if let Some(mut builder) = this.builder.take() {
            builder = builder.status(status_pair.0);
            for (key, value) in headers {
                builder = builder.header(key, value);

                if key.eq_ignore_ascii_case("content-length") {
                    this.content_length = Some(
                        u64::from_str(value)
                            .map_err(|_| PyValueError::new_err("invalid content-length"))?,
                    );
                }
            }

            this.builder.replace(builder);
        }

        Ok(())
    }
}

impl WSGIStartResponse {
    pub(crate) fn take_body_builder(&mut self, wsgi_iter: PyObject) -> WSGIResponseBuilder {
        WSGIResponseBuilder::new(self.config.clone(), wsgi_iter)
    }
}

/// WSGI response builder.
pub(crate) struct WSGIResponseBuilder {
    config: Arc<WSGIResponseConfig>,
    wsgi_iter: PyObject,
}

impl WSGIResponseBuilder {
    fn new(config: Arc<WSGIResponseConfig>, wsgi_iter: PyObject) -> Self {
        Self { config, wsgi_iter }
    }

    pub(crate) fn build(self, py: Python<'_>) -> Result<Response<WSGIResponseBody>, Error> {
        let mut wsgi_iter = self.wsgi_iter;

        // optimize for the common case of a single string
        if unsafe { PyList_Check(wsgi_iter.as_ptr()) } == 1
            && unsafe { PyList_Size(wsgi_iter.as_ptr()) } == 1
        {
            wsgi_iter = wsgi_iter.into_ref(py).get_item(0)?.into();
        }

        // If the wsgi_iter is a bytes object, we can just return it
        // I don't want to iterate char by char
        let mut body = match unsafe { PyBytes_Check(wsgi_iter.as_ptr()) } {
            1 => WSGIResponseBody::new(Some(PyBytesBuf::new(wsgi_iter.extract(py)?)), None),
            _ => {
                let mut iter = wsgi_iter.as_ref(py).iter()?;
                match iter.next() {
                    Some(chunk) => WSGIResponseBody::new(
                        Some(PyBytesBuf::new(chunk?.extract()?)),
                        Some(iter.into()),
                    ),
                    None => WSGIResponseBody::empty(),
                }
            }
        };

        let config = unsafe { &mut *(Arc::as_ptr(&self.config) as *mut WSGIResponseConfig) };
        body.set_content_length(config.content_length);
        Ok(config.builder.take().unwrap().body(body)?)
    }
}

pub(crate) struct WSGIResponseBody {
    current_chunk: Option<PyBytesBuf>,
    next_chunk: Option<PyBytesBuf>,
    wsgi_iter: Option<Py<PyIterator>>,
    content_length: Option<u64>,
}

impl WSGIResponseBody {
    fn new(next_chunk: Option<PyBytesBuf>, wsgi_iter: Option<Py<PyIterator>>) -> Self {
        Self {
            current_chunk: None,
            next_chunk,
            wsgi_iter,
            content_length: None,
        }
    }

    pub(crate) fn empty() -> Self {
        Self {
            current_chunk: None,
            next_chunk: None,
            wsgi_iter: None,
            content_length: Some(0),
        }
    }

    fn set_content_length(&mut self, content_length: Option<u64>) {
        self.content_length = content_length;
    }

    pub(crate) fn take_current_chunk(&mut self) -> Option<PyBytesBuf> {
        self.current_chunk.take()
    }

    pub(crate) fn poll_from_iter(&mut self, py: Python<'_>) -> Result<(), Error> {
        if self.current_chunk.is_some() {
            return Ok(());
        }

        self.current_chunk = self.next_chunk.take();
        if let Some(ref iter) = self.wsgi_iter {
            let mut iter = iter.as_ref(py);
            if let Some(next_chunk) = iter.next() {
                self.next_chunk
                    .replace(PyBytesBuf::new(next_chunk?.extract()?));
            }
        }

        if self.next_chunk.is_none() {
            self.wsgi_iter = None
        }

        Ok(())
    }
}

impl body::Body for WSGIResponseBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<body::Frame<Self::Data>, Self::Error>>> {
        Python::with_gil(|py| self.poll_from_iter(py))?;
        std::task::Poll::Ready(self.take_current_chunk().map(|chunk| {
            Ok(body::Frame::data(hyper::body::Bytes::copy_from_slice(
                chunk.chunk(),
            )))
        }))
    }

    fn is_end_stream(&self) -> bool {
        self.current_chunk.is_none() && self.next_chunk.is_none()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        let mut sh = hyper::body::SizeHint::new();

        // If the content length is set, we can use it as the exact size
        if let Some(content_length) = self.content_length {
            sh.set_exact(content_length);
            return sh;
        }

        if let Some(ref chunk) = self.current_chunk {
            sh.set_lower(chunk.chunk().remaining() as u64);
        }

        if let Some(ref chunk) = self.next_chunk {
            sh.set_lower(sh.lower() + chunk.chunk().remaining() as u64);
        }

        if self.wsgi_iter.is_none() {
            sh.set_upper(sh.lower());
        }

        sh
    }
}
