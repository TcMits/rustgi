use bytes::BytesMut;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http_body_util::Full;
use hyper::{
    body::{Body, Bytes, Incoming, SizeHint},
    service::Service,
    Request, StatusCode,
};
use pyo3::{
    ffi::{PyDict_SetItemString, PySys_GetObject},
    prelude::*,
    types::{PyBytes, PyDict, PyList, PyTuple},
    AsPyPointer,
};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};
use tracing::debug;

const LINE_SPLIT: u8 = u8::from_be_bytes(*b"\n");

pub struct WSGICaller {
    rustgi: crate::core::Rustgi,
}

impl WSGICaller {
    pub(crate) fn new(rustgi: crate::core::Rustgi) -> Self {
        Self { rustgi }
    }
}

impl Service<Request<Incoming>> for WSGICaller {
    type Response = hyper::Response<WSGIResponseBody>;
    type Error = Infallible;
    type Future = WSGIFuture;

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        WSGIFuture::new(self.rustgi.clone(), req)
    }
}

pub struct WSGIFuture {
    rustgi: crate::core::Rustgi,
    request: Request<Incoming>,
    wsgi_request_body: Option<Py<WSGIRequestBody>>,
}

impl WSGIFuture {
    pub fn new(rustgi: crate::core::Rustgi, request: Request<Incoming>) -> Self {
        Self {
            rustgi,
            request,
            wsgi_request_body: None,
        }
    }
}

impl Future for WSGIFuture {
    type Output = Result<hyper::Response<WSGIResponseBody>, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match Python::with_gil(|py| -> PyResult<Poll<Result<(), hyper::Error>>> {
            if let None = this.wsgi_request_body {
                this.wsgi_request_body
                    .replace(Py::new(py, WSGIRequestBody::new(&this.request))?);
            }

            let mut body = this
                .wsgi_request_body
                .as_mut()
                .unwrap()
                .as_ref(py)
                .borrow_mut();

            Ok(body.poll_from_request(cx, &mut this.request))
        }) {
            Ok(Poll::Ready(Ok(()))) => {}
            Ok(Poll::Pending) => return Poll::Pending,
            Ok(Poll::Ready(Err(err))) => {
                debug!("hyper error: {}", err);
                return Poll::Ready(Ok(response_500()));
            }
            Err(err) => {
                debug!("python error: {}", err);
                return Poll::Ready(Ok(response_500()));
            }
        };

        let eviron_builder = WSGIEvironBuilder::new(&this.rustgi, &this.request);

        match Python::with_gil(
            |py| -> PyResult<http::Result<hyper::Response<WSGIResponseBody>>> {
                let wsgi_response_builder = Py::new(py, WSGIResponseBuilder::default())?;
                let environ = PyDict::new(py);

                eviron_builder.build(&environ, this.wsgi_request_body.as_ref().unwrap().clone())?;

                let wsgi_return = this
                    .rustgi
                    .get_wsgi_app()
                    .call1(py, (environ, wsgi_response_builder.clone()))?;

                let builder = wsgi_response_builder.as_ref(py).borrow_mut();
                Ok(builder.build(wsgi_return))
            },
        ) {
            Ok(Ok(response)) => Poll::Ready(Ok(response)),
            Ok(Err(err)) => {
                debug!("hyper error: {}", err);
                Poll::Ready(Ok(response_500()))
            }
            Err(err) => {
                debug!("python error: {}", err);
                Poll::Ready(Ok(response_500()))
            }
        }
    }
}

#[pyclass]
pub struct WSGIRequestBody {
    inner: BytesMut,
    // save the frame size to estimate allocations in readlines.
    frame_size: usize,
}

impl WSGIRequestBody {
    pub fn new(request: &Request<Incoming>) -> Self {
        Self {
            inner: BytesMut::with_capacity((request.size_hint().lower() as usize).min(1024 * 16)),
            frame_size: 0,
        }
    }

    // Care needs to be taken if the remote is untrusted.
    // The function doesn’t implement any length checks and an malicious peer might make it consume arbitrary amounts of memory.
    // Anyway, wsgi is supposed to be used in behind reverse proxies.
    pub fn poll_from_request(
        &mut self,
        cx: &mut Context<'_>,
        request: &mut Request<Incoming>,
    ) -> Poll<Result<(), hyper::Error>> {
        while !request.is_end_stream() {
            let pin_request = Pin::new(&mut *request);
            match pin_request.poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if frame.is_trailers() {
                        // https://peps.python.org/pep-0444/#request-trailers-and-chunked-transfer-encoding
                        // When using chunked transfer encoding on request content, the RFCs allow there to be request trailers.
                        // These are like request headers but come after the final null data chunk. These trailers are only
                        // available when the chunked data stream is finite length and when it has all been read in. Neither WSGI nor Web3 currently supports them.
                        continue;
                    }

                    let bytes = frame.into_data().unwrap();

                    // extend_from_slice already check the capacity.
                    self.inner.extend_from_slice(&bytes);
                    self.frame_size = self.frame_size.saturating_add(1);
                }
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Err(err)),
                Poll::Ready(None) => continue, // is_end_stream() return value of false does not guarantee that a value will be returned from poll_frame.
                Poll::Pending => return Poll::Pending,
            }
        }

        Poll::Ready(Ok(()))
    }
}

#[pymethods]
impl WSGIRequestBody {
    pub fn __iter__(pyself: PyRef<'_, Self>) -> PyRef<'_, Self> {
        pyself
    }

    pub fn __next__<'p>(&mut self, py: Python<'p>) -> Option<&'p PyBytes> {
        match self.inner.iter().position(|&c| c == LINE_SPLIT) {
            // next_split is the index of line split.
            // split_to will split the buffer to next_split + 1.
            Some(next_split) => Some(PyBytes::new(
                py,
                &self.inner.split_to(next_split + 1).freeze(),
            )),
            None if !self.inner.is_empty() => Some(PyBytes::new(
                py,
                &self.inner.split_to(self.inner.len()).freeze(),
            )),
            None => None,
        }
    }

    #[pyo3(signature = (size=None))]
    pub fn read<'p>(&mut self, py: Python<'p>, size: Option<usize>) -> &'p PyBytes {
        match size {
            None => PyBytes::new(py, &self.inner.split_to(self.inner.len()).freeze()),
            Some(size) => PyBytes::new(
                py,
                &self.inner.split_to(size.min(self.inner.len())).freeze(),
            ),
        }
    }

    #[pyo3(signature = (size=None))]
    pub fn readline<'p>(&mut self, py: Python<'p>, size: Option<usize>) -> &'p PyBytes {
        let iter_size = match size {
            None => self.inner.len(),
            Some(size) => size.min(self.inner.len()),
        };

        match self
            .inner
            .iter()
            .take(iter_size)
            .position(|&c| c == LINE_SPLIT)
        {
            // next_split is the index of line split.
            // split_to will split the buffer to next_split + 1.
            Some(next_split) => PyBytes::new(py, &self.inner.split_to(next_split + 1).freeze()),
            None if !self.inner.is_empty() => {
                PyBytes::new(py, &self.inner.split_to(iter_size).freeze())
            }
            None => PyBytes::new(py, b""),
        }
    }

    #[pyo3(signature = (hint=None))]
    pub fn readlines<'p>(&mut self, py: Python<'p>, hint: Option<usize>) -> &'p PyList {
        let mut iter_size = match hint {
            None => self.inner.len(),
            // https://docs.python.org/3/library/io.html#io.IOBase.readlines
            // hint values of 0 or less, as well as None, are treated as no hint.
            Some(size) if size == 0 => self.inner.len(),
            Some(size) => size.min(self.inner.len()),
        };

        // use frame_size to estimate the allocation size.
        let mut lines: Vec<&PyBytes> = Vec::with_capacity(self.frame_size);
        while iter_size > 0 {
            match self
                .inner
                .iter()
                .take(iter_size)
                .position(|&c| c == LINE_SPLIT)
            {
                Some(next_split) => {
                    lines.push(PyBytes::new(
                        py,
                        &self.inner.split_to(next_split + 1).freeze(),
                    ));
                    iter_size -= next_split + 1;
                    self.frame_size = self.frame_size.saturating_sub(1);
                }
                None if !self.inner.is_empty() => {
                    lines.push(PyBytes::new(py, &self.inner.split_to(iter_size).freeze()));
                    iter_size = 0;
                    self.frame_size = self.frame_size.saturating_sub(1);
                }
                None => break,
            }
        }

        PyList::new(py, lines)
    }
}

pub struct WSGIEvironBuilder<'a> {
    rustgi: &'a crate::core::Rustgi,
    request: &'a Request<Incoming>,
}

impl<'a> WSGIEvironBuilder<'a> {
    pub fn new(rustgi: &'a crate::core::Rustgi, request: &'a Request<Incoming>) -> Self {
        Self { rustgi, request }
    }

    // https://peps.python.org/pep-3333/#environ-variables
    pub fn build(self, environ: &PyDict, input: Py<WSGIRequestBody>) -> PyResult<()> {
        environ.set_item(
            "REQUEST_METHOD",
            self.request.method().as_str(), // method as_str always returns a uppercase string
        )?;
        environ.set_item("SCRIPT_NAME", "")?;
        environ.set_item("PATH_INFO", self.request.uri().path())?;
        environ.set_item("QUERY_STRING", self.request.uri().query().unwrap_or(""))?;
        environ.set_item(
            "CONTENT_TYPE",
            self.request
                .headers()
                .get(CONTENT_TYPE)
                .map(|v| v.as_bytes()),
        )?;
        environ.set_item(
            "CONTENT_LENGTH",
            self.request
                .headers()
                .get(CONTENT_LENGTH)
                .map(|v| v.as_bytes()),
        )?;
        environ.set_item("SERVER_NAME", self.rustgi.get_host())?;
        environ.set_item("SERVER_PORT", self.rustgi.get_port())?;
        match self.request.version() {
            hyper::Version::HTTP_09 => environ.set_item("SERVER_PROTOCOL", "HTTP/0.9")?,
            hyper::Version::HTTP_10 => environ.set_item("SERVER_PROTOCOL", "HTTP/1.0")?,
            hyper::Version::HTTP_11 => environ.set_item("SERVER_PROTOCOL", "HTTP/1.1")?,
            hyper::Version::HTTP_2 => environ.set_item("SERVER_PROTOCOL", "HTTP/2")?,
            hyper::Version::HTTP_3 => environ.set_item("SERVER_PROTOCOL", "HTTP/3")?,
            _ => unreachable!(),
        };
        for (name, value) in self.request.headers() {
            environ.set_item(
                &format!("HTTP_{}", name.as_str().to_uppercase().replace("-", "_")),
                value.as_bytes(),
            )?;
        }

        environ.set_item("wsgi.version", (1, 0))?;
        environ.set_item(
            "wsgi.url_scheme",
            self.request.uri().scheme_str().unwrap_or("http"),
        )?;
        unsafe {
            PyDict_SetItemString(
                environ.as_ptr(),
                "wsgi.errors".as_ptr() as *const i8,
                PySys_GetObject("stderr".as_ptr() as *const i8),
            )
        };
        environ.set_item("wsgi.multithread", false)?;
        environ.set_item("wsgi.multiprocess", false)?;
        environ.set_item("wsgi.run_once", false)?;
        environ.set_item("wsgi.input", input)?;

        Ok(())
    }
}

#[derive(Default)]
#[pyclass]
struct WSGIResponseBuilder {
    status: StatusCode,
    headers: Vec<(String, String)>,
}

#[pymethods]
impl WSGIResponseBuilder {
    #[pyo3(signature = (status, headers, exc_info=None))]
    fn __call__(
        &mut self,
        status: &str,
        headers: Vec<(String, String)>,
        exc_info: Option<&PyTuple>,
    ) {
        self.status = StatusCode::from_str(status.split_once(" ").unwrap().0).unwrap();
        self.headers = headers;
        // TODO: exc_info
        let _ = exc_info;
    }
}

impl WSGIResponseBuilder {
    /// Create a new WSGIResponseBuilder
    fn build(&self, wsgi_return: PyObject) -> http::Result<hyper::Response<WSGIResponseBody>> {
        let mut builder = hyper::Response::builder().status(self.status);

        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }

        builder.body(WSGIResponseBody::new(wsgi_return))
    }
}

pub enum WSGIResponseBodyKind {
    FullBytes(Full<Bytes>),
    WSGI {
        wsgi_return: PyObject,
        is_end_stream: bool,
    },
}

pub struct WSGIResponseBody {
    kind: WSGIResponseBodyKind,
}

impl WSGIResponseBody {
    /// Create a new WSGIResponseBody
    pub fn new(wsgi_return: PyObject) -> Self {
        Self {
            kind: WSGIResponseBodyKind::WSGI {
                wsgi_return,
                is_end_stream: false,
            },
        }
    }

    /// Create a new WSGIResponseBody with full bytes
    pub fn bytes(bytes: Bytes) -> Self {
        Self {
            kind: WSGIResponseBodyKind::FullBytes(Full::new(bytes)),
        }
    }
}

impl Body for WSGIResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        match self.kind {
            WSGIResponseBodyKind::FullBytes(ref mut bytes) => Pin::new(bytes).poll_frame(cx),
            WSGIResponseBodyKind::WSGI {
                ref wsgi_return,
                ref mut is_end_stream,
            } => {
                if *is_end_stream {
                    return Poll::Ready(None);
                }

                match Python::with_gil(
                    |py| -> PyResult<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
                        let mut wsgi_return_iter = wsgi_return.as_ref(py).iter()?;

                        loop {
                            match wsgi_return_iter.next() {
                                Some(item) => {
                                    let item = item?;

                                    // skip empty bytes
                                    if item.is_none() || item.len()? == 0 {
                                        continue;
                                    }

                                    return Ok(Some(Ok(hyper::body::Frame::data(
                                        Bytes::copy_from_slice(item.extract()?),
                                    ))));
                                }
                                None => {
                                    *is_end_stream = true;
                                    return Ok(None);
                                }
                            }
                        }
                    },
                ) {
                    Ok(frame) => Poll::Ready(frame),
                    Err(err) => {
                        debug!("python iterator error: {}", err);
                        *is_end_stream = true;
                        Poll::Ready(None)
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self.kind {
            WSGIResponseBodyKind::FullBytes(ref bytes) => bytes.is_end_stream(),
            WSGIResponseBodyKind::WSGI { is_end_stream, .. } => is_end_stream,
        }
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        match self.kind {
            WSGIResponseBodyKind::FullBytes(ref bytes) => bytes.size_hint(),
            WSGIResponseBodyKind::WSGI { .. } => SizeHint::default(),
        }
    }
}

/// Create a 500 response.
pub fn response_500() -> hyper::Response<WSGIResponseBody> {
    hyper::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(WSGIResponseBody::bytes(Bytes::from_static(
            b"Internal Server Error",
        )))
        .unwrap()
}
