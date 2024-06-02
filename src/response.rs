use crate::utils::with_gil;
use anyhow::Result;
use bytes::Buf;
use futures::{
    stream::{unfold, BoxStream},
    StreamExt,
};
use http::StatusCode;
use hyper::body::{Body, Frame, SizeHint};
use hyper::Response;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyTuple;
use pyo3::PyObject;
use pyo3::{
    types::{PyBytes, PyIterator},
    Py, PyResult, Python,
};
use std::{sync::Arc, task::ready};

pub(crate) struct PyBytesBuf(Py<PyBytes>, usize);

impl PyBytesBuf {
    #[inline]
    fn new(b: Py<PyBytes>) -> Self {
        Self(b, 0)
    }
}

impl Buf for PyBytesBuf {
    #[inline]
    fn remaining(&self) -> usize {
        self.chunk().len()
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        if cnt > self.remaining() {
            panic!("advancing beyond the end of the buffer");
        }

        self.1 += cnt;
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        // safe because Python bytes are immutable, the result may be used for as long as the reference to
        unsafe { &self.0.as_bytes(Python::assume_gil_acquired())[self.1..] }
    }
}

pub(crate) fn py_stream(
    pool: Arc<rayon::ThreadPool>,
    iter: Py<PyIterator>,
) -> BoxStream<'static, PyResult<Frame<PyBytesBuf>>> {
    struct S {
        pool: Arc<rayon::ThreadPool>,
        iter: Py<PyIterator>,
    }

    let s = S { pool, iter };

    unfold(s, |state| async {
        with_gil(
            state.pool.clone(),
            |py| -> Option<(PyResult<Frame<PyBytesBuf>>, S)> {
                let S { pool, iter } = state;
                let mut iter = iter.into_bound(py);
                match iter.next() {
                    Some(chunk) => match chunk {
                        Ok(chunk) => {
                            let chunk = chunk.extract();
                            if let Err(err) = chunk {
                                return Some((
                                    Err(err),
                                    S {
                                        pool,
                                        iter: iter.into(),
                                    },
                                ));
                            }

                            Some((
                                Ok(Frame::data(PyBytesBuf::new(chunk.unwrap()))),
                                S {
                                    pool,
                                    iter: iter.into(),
                                },
                            ))
                        }
                        Err(err) => Some((
                            Err(err),
                            S {
                                pool,
                                iter: iter.into(),
                            },
                        )),
                    },
                    None => None,
                }
            },
        )
        .await
    })
    .boxed()
}

pub(crate) struct WSGIResponseBody {
    first_chunk: Option<PyBytesBuf>,
    second_chunk: Option<PyBytesBuf>,
    stream: Option<BoxStream<'static, PyResult<Frame<PyBytesBuf>>>>,
}

impl WSGIResponseBody {
    fn new(iter: Option<(Arc<rayon::ThreadPool>, Bound<'_, PyIterator>)>) -> PyResult<Self> {
        let mut result = Self {
            first_chunk: None,
            second_chunk: None,
            stream: None,
        };

        if iter.is_none() {
            return Ok(result);
        }

        let (pool, mut iter) = iter.unwrap();

        match iter.next() {
            Some(chunk) => result.first_chunk = Some(PyBytesBuf::new(chunk?.extract()?)),
            None => return Ok(result),
        }

        match iter.next() {
            Some(chunk) => result.second_chunk = Some(PyBytesBuf::new(chunk?.extract()?)),
            None => return Ok(result),
        }

        result.stream = Some(py_stream(pool, iter.into()));
        Ok(result)
    }
}

impl Body for WSGIResponseBody {
    type Data = PyBytesBuf;
    type Error = PyErr;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::prelude::v1::Result<Frame<Self::Data>, Self::Error>>> {
        if let Some(chunk) = self.first_chunk.take() {
            return std::task::Poll::Ready(Some(Ok(Frame::data(chunk))));
        }

        if let Some(chunk) = self.second_chunk.take() {
            return std::task::Poll::Ready(Some(Ok(Frame::data(chunk))));
        }

        if let Some(ref mut stream) = self.stream {
            let result = ready!(stream.poll_next_unpin(cx));
            // Returns None when the iterator is exhausted. If an exception occurs,
            // returns Some(Err(..)). Further next() calls after an exception occurs
            // are likely to repeatedly result in the same exception.
            if let None = result {
                self.stream.take();
            }

            return std::task::Poll::Ready(result);
        }

        std::task::Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        return self.first_chunk.is_none() && self.second_chunk.is_none() && self.stream.is_none();
    }

    fn size_hint(&self) -> SizeHint {
        let mut min = 0;
        let mut result = SizeHint::default();

        if let Some(chunk) = &self.first_chunk {
            min += chunk.remaining()
        }

        if let Some(chunk) = &self.second_chunk {
            min += chunk.remaining()
        }

        result.set_lower(min.try_into().unwrap_or(0));
        if let None = self.stream {
            result.set_exact(result.lower())
        }

        result
    }
}

#[pyclass]
pub(crate) struct WSGIStartResponse {
    builder: Option<http::response::Builder>,
}

impl WSGIStartResponse {
    pub(crate) fn new() -> Self {
        Self { builder: None }
    }
}

#[pymethods]
impl WSGIStartResponse {
    #[pyo3(signature = (status, headers, exc_info=None))]
    fn __call__(
        slf: &Bound<'_, Self>,
        status: &str,
        headers: Vec<(String, String)>,
        exc_info: Option<&Bound<'_, PyTuple>>,
    ) -> PyResult<()> {
        let _ = exc_info;
        let mut this = slf.borrow_mut();
        assert!(this.builder.is_none());
        let status_pair = status
            .split_once(' ')
            .ok_or(PyValueError::new_err("invalid status"))?;

        let mut builder = Response::builder();
        builder = builder.status(status_pair.0);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        this.builder.replace(builder);

        Ok(())
    }
}

impl WSGIStartResponse {
    pub(crate) fn take_response(
        slf: &Bound<'_, Self>,
        pool: Arc<rayon::ThreadPool>,
        wsgi_iter: PyObject,
    ) -> Result<Response<WSGIResponseBody>> {
        let body = WSGIResponseBody::new(Some((pool, wsgi_iter.bind(slf.py()).iter()?)))?; // have to call this
                                                                                           // first to trigger
                                                                                           // start_response
        Ok(slf.borrow_mut().builder.take().unwrap().body(body)?)
    }
}

pub(crate) fn empty_response(status: StatusCode) -> Response<WSGIResponseBody> {
    Response::builder()
        .status(status)
        .body(WSGIResponseBody::new(None).unwrap())
        .unwrap()
}
