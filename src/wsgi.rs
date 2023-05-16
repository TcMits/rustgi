use crate::{error::Error, types::PyBytesBuf};
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{
    body::{Body, Buf, Incoming, SizeHint},
    service::Service,
    Request, StatusCode,
};
use lazy_static::lazy_static;
use log::debug;
use pyo3::{
    exceptions::PyValueError,
    ffi::{PyBytes_Check, PyDict_SetItemString, PySys_GetObject},
    intern,
    prelude::*,
    types::{PyDict, PyIterator, PyTuple},
    AsPyPointer,
};
use std::task::ready;
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

lazy_static! {
    static ref PY_BYTES_IO: PyObject = Python::with_gil(|py| PyModule::import(py, "io")
        .unwrap()
        .getattr("BytesIO")
        .unwrap()
        .into());
}

pub struct WSGICaller {
    rustgi: crate::core::Rustgi,
}

impl WSGICaller {
    pub(crate) fn new(rustgi: crate::core::Rustgi) -> Self {
        Self { rustgi }
    }

    /// Create a new `WSGIFuture` from the given `Request`.
    pub fn get_future(&self, request: Request<Incoming>) -> WSGIFuture {
        WSGIFuture::new(self.rustgi.clone(), request)
    }
}

impl Service<Request<Incoming>> for WSGICaller {
    type Response = hyper::Response<WSGIResponseBody>;
    type Error = Infallible;
    type Future = WSGIFuture;

    /// Call the WSGI application.
    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        self.get_future(req)
    }
}

pub struct WSGIFuture {
    rustgi: crate::core::Rustgi,
    request: Request<Incoming>,
    wsgi_request_body: Option<WSGIRequestBody>,
}

impl WSGIFuture {
    /// Create a new `WSGIFuture` from the given `Rustgi` and `Request`.
    pub(crate) fn new(rustgi: crate::core::Rustgi, request: Request<Incoming>) -> Self {
        Self {
            rustgi,
            request,
            wsgi_request_body: None,
        }
    }
}

impl Future for WSGIFuture {
    type Output = Result<hyper::Response<WSGIResponseBody>, Infallible>;

    /// Poll the WSGI application.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match ready!(Python::with_gil(
            |py| -> Poll<Result<hyper::Response<WSGIResponseBody>, Error>> {
                let pool = unsafe { py.new_pool() };
                let body = {
                    let py = pool.python();

                    if this.wsgi_request_body.is_none() {
                        this.wsgi_request_body.replace(WSGIRequestBody::new(py)?);
                    }

                    let mut body = this.wsgi_request_body.take().unwrap();

                    match body.poll_from_request(cx, py, &mut this.request) {
                        Poll::Ready(Ok(())) => (),
                        Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                        Poll::Pending => {
                            this.wsgi_request_body.replace(body);
                            return Poll::Pending;
                        }
                    };

                    body
                };

                let wsgi_response_builder = Py::new(py, WSGIResponseBuilder::default())?;
                let wsgi_iter = {
                    let py = pool.python();

                    let environ = PyDict::new(py);
                    WSGIEvironBuilder::new(&this.rustgi, &this.request)
                        .build(environ, body.into_input(py)?)?;

                    this.rustgi
                        .get_wsgi_app()
                        .call1(py, (environ, wsgi_response_builder.clone()))?
                };

                let mut builder = wsgi_response_builder.as_ref(py).borrow_mut();
                Poll::Ready(builder.build(py, wsgi_iter))
            },
        )) {
            Ok(mut response) => {
                // ensure that current chunk is not empty
                if let Err(err) = response.body_mut().poll_from_iter() {
                    debug!("WSGI error: {}", err);
                    return Poll::Ready(Ok(response_500()));
                }

                Poll::Ready(Ok(response))
            }
            Err(err) => {
                debug!("WSGI error: {}", err);
                Poll::Ready(Ok(response_500()))
            }
        }
    }
}

pub struct WSGIRequestBody {
    inner: PyObject,
}

impl WSGIRequestBody {
    /// Create a new `WSGIRequestBody`.
    pub fn new(py: Python<'_>) -> Result<Self, Error> {
        Ok(Self {
            inner: PY_BYTES_IO.call0(py)?,
        })
    }

    /// Create a new `WSGIRequestBody` from given `BytesIO`.
    /// Becareful, this function doesn't check the type of `BytesIO`.
    pub fn from_input(input: PyObject) -> Self {
        Self { inner: input }
    }

    /// Care needs to be taken if the remote is untrusted.
    /// The function doesn’t implement any length checks and an malicious peer might make it consume arbitrary amounts of memory.
    /// Anyway, wsgi is supposed to be used in behind reverse proxies.
    pub fn poll_from_request(
        &mut self,
        cx: &mut Context<'_>,
        py: Python<'_>,
        request: &mut Request<Incoming>,
    ) -> Poll<Result<(), Error>> {
        while !request.is_end_stream() {
            let pin_request = Pin::new(&mut *request);
            match ready!(pin_request.poll_frame(cx)) {
                Some(Ok(frame)) => {
                    if frame.is_trailers() {
                        // https://peps.python.org/pep-0444/#request-trailers-and-chunked-transfer-encoding
                        // When using chunked transfer encoding on request content, the RFCs allow there to be request trailers.
                        // These are like request headers but come after the final null data chunk. These trailers are only
                        // available when the chunked data stream is finite length and when it has all been read in. Neither WSGI nor Web3 currently supports them.
                        return Poll::Ready(Ok(()));
                    }

                    let bytes = frame.into_data().unwrap();
                    self.inner
                        .call_method1(py, intern!(py, "write"), (&bytes as &[u8],))?;
                }
                Some(Err(err)) => return Poll::Ready(Err(Error::from(err))),
                None => continue, // is_end_stream() return value of false does not guarantee that a value will be returned from poll_frame.
            }
        }

        Poll::Ready(Ok(()))
    }

    /// Consume the `WSGIRequestBody` and return the underlying `PyObject`.
    /// The returned `PyObject` is a `BytesIO` object.
    /// The `BytesIO` object is positioned at the start of the stream.
    pub fn into_input(self, py: Python<'_>) -> Result<PyObject, Error> {
        self.inner.call_method1(py, intern!(py, "seek"), (0,))?;
        Ok(self.inner)
    }
}

struct WSGIEvironBuilder<'a> {
    rustgi: &'a crate::core::Rustgi,
    request: &'a Request<Incoming>,
}

impl<'a> WSGIEvironBuilder<'a> {
    fn new(rustgi: &'a crate::core::Rustgi, request: &'a Request<Incoming>) -> Self {
        Self { rustgi, request }
    }

    // https://peps.python.org/pep-3333/#environ-variables
    fn build(self, environ: &PyDict, input: PyObject) -> Result<(), Error> {
        environ.set_item(
            intern!(environ.py(), "REQUEST_METHOD"),
            self.request.method().as_str(), // method as_str always returns a uppercase string
        )?;
        environ.set_item(
            intern!(environ.py(), "SCRIPT_NAME"),
            intern!(environ.py(), ""),
        )?;
        environ.set_item(
            intern!(environ.py(), "PATH_INFO"),
            self.request.uri().path(),
        )?;
        environ.set_item(
            intern!(environ.py(), "QUERY_STRING"),
            self.request.uri().query().unwrap_or(""),
        )?;
        environ.set_item(
            intern!(environ.py(), "CONTENT_TYPE"),
            self.request
                .headers()
                .get(CONTENT_TYPE)
                .map(|v| v.to_str().unwrap_or("")),
        )?;
        environ.set_item(
            intern!(environ.py(), "CONTENT_LENGTH"),
            self.request
                .headers()
                .get(CONTENT_LENGTH)
                .map(|v| v.to_str().unwrap_or("")),
        )?;
        environ.set_item(intern!(environ.py(), "SERVER_NAME"), self.rustgi.get_host())?;
        environ.set_item(intern!(environ.py(), "SERVER_PORT"), self.rustgi.get_port())?;
        match self.request.version() {
            hyper::Version::HTTP_09 => environ.set_item(
                intern!(environ.py(), "SERVER_PROTOCOL"),
                intern!(environ.py(), "HTTP/0.9"),
            )?,
            hyper::Version::HTTP_10 => environ.set_item(
                intern!(environ.py(), "SERVER_PROTOCOL"),
                intern!(environ.py(), "HTTP/1.0"),
            )?,
            hyper::Version::HTTP_11 => environ.set_item(
                intern!(environ.py(), "SERVER_PROTOCOL"),
                intern!(environ.py(), "HTTP/1.1"),
            )?,
            hyper::Version::HTTP_2 => environ.set_item(
                intern!(environ.py(), "SERVER_PROTOCOL"),
                intern!(environ.py(), "HTTP/2"),
            )?,
            hyper::Version::HTTP_3 => environ.set_item(
                intern!(environ.py(), "SERVER_PROTOCOL"),
                intern!(environ.py(), "HTTP/3"),
            )?,
            _ => unreachable!(),
        };
        for (name, value) in self.request.headers() {
            environ.set_item(
                &format!("HTTP_{}", name.as_str().to_uppercase().replace('-', "_")),
                value.to_str().unwrap_or(""),
            )?;
        }
        environ.set_item(intern!(environ.py(), "wsgi.version"), (1, 0))?;
        environ.set_item(
            intern!(environ.py(), "wsgi.url_scheme"),
            self.request.uri().scheme_str().unwrap_or("http"),
        )?;
        unsafe {
            PyDict_SetItemString(
                environ.as_ptr(),
                "wsgi.errors\0".as_ptr() as *const i8,
                PySys_GetObject("stderr\0".as_ptr() as *const i8),
            )
        };
        // tell Flask/other WSGI apps that the input has been terminated
        environ.set_item(intern!(environ.py(), "wsgi.input_terminated"), true)?;
        // it can be set to true if the application object is known to only support a single thread
        environ.set_item(intern!(environ.py(), "wsgi.multithread"), true)?;
        // it can be set to true if the application object is known to only support a single process
        environ.set_item(intern!(environ.py(), "wsgi.multiprocess"), true)?;
        environ.set_item(intern!(environ.py(), "wsgi.run_once"), false)?;
        environ.set_item(intern!(environ.py(), "wsgi.input"), input)?;

        Ok(())
    }
}

#[derive(Default)]
#[pyclass]
struct WSGIResponseBuilder {
    builder: Option<http::response::Builder>,
}

#[pymethods]
impl WSGIResponseBuilder {
    #[pyo3(signature = (status, headers, exc_info=None))]
    fn __call__(
        &mut self,
        status: &str,
        headers: Vec<(&str, &str)>,
        exc_info: Option<&PyTuple>,
    ) -> PyResult<()> {
        let _ = exc_info;
        let status_pair = status
            .split_once(' ')
            .ok_or(PyValueError::new_err("invalid status"))?;

        let mut builder =
            hyper::Response::builder().status(StatusCode::from_str(status_pair.0).map_err(
                |_| PyValueError::new_err(format!("invalid status code: {}", status_pair.0)),
            )?);

        for (name, value) in headers {
            builder = builder.header(name, value);
        }

        self.builder.replace(builder);
        Ok(())
    }
}

impl WSGIResponseBuilder {
    fn build(
        &mut self,
        py: Python<'_>,
        wsgi_iter: PyObject,
    ) -> Result<hyper::Response<WSGIResponseBody>, Error> {
        // If the wsgi_iter is a bytes object, we can just return it
        // I don't want to iterate char by char
        if unsafe { PyBytes_Check(wsgi_iter.as_ptr()) } == 1 {
            return self
                .builder
                .take()
                .unwrap_or_default()
                .body(WSGIResponseBody::new(
                    Some(PyBytesBuf::new(wsgi_iter.extract(py)?)),
                    None,
                ))
                .map_err(Error::from);
        }

        self.builder
            .take()
            .unwrap_or_default()
            .body(WSGIResponseBody::new(
                None,
                Some(wsgi_iter.as_ref(py).iter()?.into()),
            ))
            .map_err(Error::from)
    }
}

pub struct WSGIResponseBody {
    current_chunk: Option<PyBytesBuf>,
    wsgi_iter: Option<Py<PyIterator>>,
}

impl WSGIResponseBody {
    /// Create a new WSGIResponseBody
    pub fn new(current_chunk: Option<PyBytesBuf>, wsgi_iter: Option<Py<PyIterator>>) -> Self {
        Self {
            current_chunk,
            wsgi_iter,
        }
    }

    /// Create an empty WSGIResponseBody
    pub fn empty() -> Self {
        Self {
            current_chunk: None,
            wsgi_iter: None,
        }
    }

    /// Take the current chunk
    pub fn take_current_chunk(&mut self) -> Option<PyBytesBuf> {
        self.current_chunk.take()
    }

    /// Poll the iterator for the next chunk
    /// if the current chunk is None, it will poll the iterator and set the current chunk to the next chunk
    /// if the current chunk is Some, it will do nothing
    pub fn poll_from_iter(&mut self) -> Result<(), Error> {
        if self.current_chunk.is_none() {
            if let Some(ref iter) = self.wsgi_iter {
                Python::with_gil(|py| -> Result<(), Error> {
                    let mut iter = iter.as_ref(py);

                    if let Some(next_chunk) = iter.next() {
                        self.current_chunk
                            .replace(PyBytesBuf::new(next_chunk?.extract()?));
                    }

                    Ok(())
                })?;
            }
        }

        // If the current chunk is still None, there is no more data
        if self.current_chunk.is_none() {
            self.wsgi_iter = None
        }

        Ok(())
    }
}

impl Body for WSGIResponseBody {
    type Data = PyBytesBuf;
    type Error = Infallible;

    /// Poll the iterator for the next chunk
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let chunk = self.take_current_chunk();
        if let Err(err) = self.poll_from_iter() {
            debug!("error polling from iterator: {}", err);
        }

        match chunk {
            Some(chunk) => std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(chunk)))),
            None => std::task::Poll::Ready(None),
        }
    }

    /// Check if the iterator is done
    fn is_end_stream(&self) -> bool {
        self.current_chunk.is_none() && self.wsgi_iter.is_none()
    }

    /// Get the size hint
    fn size_hint(&self) -> hyper::body::SizeHint {
        let mut sh = SizeHint::new();
        if let Some(ref chunk) = self.current_chunk {
            sh.set_lower(chunk.remaining() as u64);
        }

        sh
    }
}

/// Create a 500 response.
fn response_500() -> hyper::Response<WSGIResponseBody> {
    hyper::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(WSGIResponseBody::empty())
        .unwrap()
}
