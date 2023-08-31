use crate::error::Error;
use crate::types::PyBytesBuf;
use bytes::BytesMut;
use http::StatusCode;
use pyo3::exceptions::PyValueError;
use pyo3::ffi::{PyBytes_Check, PyList_Check, PyList_Size};
use pyo3::types::{PyIterator, PyTuple};
use pyo3::{prelude::*, AsPyPointer};
use pyo3::{Py, PyObject};
use std::str::FromStr;
use std::sync::{Arc, Weak};

pub(crate) struct WSGIResponseConfig {
    head_bytes: BytesMut,
    content_length: Option<u64>,
}

impl WSGIResponseConfig {
    pub(crate) fn new(initial_bytes: BytesMut) -> Self {
        Self {
            head_bytes: initial_bytes,
            content_length: None,
        }
    }

    pub(crate) fn into_bytes(self) -> BytesMut {
        self.head_bytes
    }
}

#[pyclass]
pub(crate) struct WSGIStartResponse {
    config: Weak<WSGIResponseConfig>,
}

impl WSGIStartResponse {
    pub(crate) fn new(config: Weak<WSGIResponseConfig>) -> Self {
        Self { config }
    }
}

#[pymethods]
impl WSGIStartResponse {
    #[pyo3(signature = (status, headers, exc_info=None))]
    fn __call__(
        &self,
        status: &str,
        headers: Vec<(&str, &str)>,
        exc_info: Option<&PyTuple>,
    ) -> PyResult<()> {
        let _ = exc_info;
        let this = unsafe { &mut *(self.config.as_ptr() as *mut WSGIResponseConfig) };

        let status_pair = status
            .split_once(' ')
            .ok_or(PyValueError::new_err("invalid status"))?;

        let status = StatusCode::from_str(status_pair.0).map_err(|_| {
            PyValueError::new_err(format!("invalid status code: {}", status_pair.0))
        })?;

        this.head_bytes
            .extend_from_slice(status.as_str().as_bytes());
        this.head_bytes.extend_from_slice(b" ");
        this.head_bytes
            .extend_from_slice(status.canonical_reason().unwrap_or("Unknown").as_bytes());
        this.head_bytes.extend_from_slice(b"\r\n");

        for (key, value) in headers {
            if key.eq_ignore_ascii_case("Content-Length") {
                this.content_length = Some(value.parse().map_err(|_| {
                    PyValueError::new_err(format!("invalid content-length: {}", value))
                })?);

                continue;
            }

            this.head_bytes.extend_from_slice(key.as_bytes());
            this.head_bytes.extend_from_slice(b": ");
            this.head_bytes.extend_from_slice(value.as_bytes());
            this.head_bytes.extend_from_slice(b"\r\n");
        }

        Ok(())
    }
}

impl WSGIStartResponse {
    pub(crate) fn take_body_builder(&self, wsgi_iter: PyObject) -> WSGIResponseBodyBuilder {
        WSGIResponseBodyBuilder::new(self.config.upgrade(), wsgi_iter)
    }
}

/// WSGI response builder.
pub(crate) struct WSGIResponseBodyBuilder {
    config: Option<Arc<WSGIResponseConfig>>,
    wsgi_iter: PyObject,
}

impl WSGIResponseBodyBuilder {
    fn new(config: Option<Arc<WSGIResponseConfig>>, wsgi_iter: PyObject) -> Self {
        Self { config, wsgi_iter }
    }

    pub(crate) fn build(self, py: Python<'_>) -> Result<WSGIResponseBody, Error> {
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

        body.set_content_length(match self.config {
            Some(config) => config.content_length,
            None => None,
        });
        Ok(body)
    }
}

pub(crate) struct WSGIResponseBody {
    current_chunk: Option<PyBytesBuf>,
    wsgi_iter: Option<Py<PyIterator>>,
    content_length: Option<u64>,
}

impl WSGIResponseBody {
    fn new(current_chunk: Option<PyBytesBuf>, wsgi_iter: Option<Py<PyIterator>>) -> Self {
        Self {
            current_chunk,
            wsgi_iter,
            content_length: None,
        }
    }

    fn empty() -> Self {
        Self {
            current_chunk: None,
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

    pub(crate) fn set_current_chunk(&mut self, chunk: PyBytesBuf) {
        self.current_chunk.replace(chunk);
    }

    pub(crate) fn poll_from_iter(&mut self, py: Python<'_>) -> Result<(), Error> {
        if self.current_chunk.is_none() {
            if let Some(ref iter) = self.wsgi_iter {
                let mut iter = iter.as_ref(py);
                if let Some(next_chunk) = iter.next() {
                    self.current_chunk
                        .replace(PyBytesBuf::new(next_chunk?.extract()?));
                }
            }
        }

        // If the current chunk is still None, there is no more data
        if self.current_chunk.is_none() {
            self.wsgi_iter = None
        }

        Ok(())
    }

    pub(crate) fn is_end_stream(&self) -> bool {
        self.current_chunk.is_none() && self.wsgi_iter.is_none()
    }

    pub(crate) fn size_hint(&self) -> http_body::SizeHint {
        let mut sh = http_body::SizeHint::new();

        // If the content length is set, we can use it as the exact size
        if let Some(content_length) = self.content_length {
            sh.set_exact(content_length);
            return sh;
        }

        // If the current chunk is Some, we can use it's size as the lower bound
        if let Some(ref chunk) = self.current_chunk {
            sh.set_lower(chunk.remaining() as u64);
        }

        // If the iterator is None, we are done
        if self.wsgi_iter.is_none() {
            sh.set_upper(sh.lower());
        }

        sh
    }
}
