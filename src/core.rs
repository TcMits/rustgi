use crate::error::Error;
use crate::types::PyBytesBuf;
use bytes::{BufMut, BytesMut};
use http::{StatusCode, Uri};
use lazy_static::lazy_static;
use log::{debug, info};
use mio::event::Event;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token};
use mio_signals::{Signal, Signals};
use pyo3::exceptions::PyValueError;
use pyo3::ffi::{PyBytes_Check, PyDict_SetItemString, PyList_Check, PyList_Size, PySys_GetObject};
use pyo3::types::{PyDict, PyIterator, PyString, PyTuple};
use pyo3::{intern, Py, PyObject};
use pyo3::{prelude::*, AsPyPointer};
use slab::Slab;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::str::{from_utf8, FromStr};
use std::sync::Arc;

const MAX_HEADERS: usize = 100;
const RESPONSE_CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";

// Setup some tokens to allow us to identify which event is for which socket.
const SERVER: Token = Token(usize::MAX);
const SIGNAL: Token = Token(usize::MAX - 1);

lazy_static! {
    static ref PY_BYTES_IO: PyObject = Python::with_gil(|py| PyModule::import(py, "io")
        .unwrap()
        .getattr("BytesIO")
        .unwrap()
        .into());
}

struct RustgiRef {
    app: PyObject,
    address: SocketAddr,
}

#[derive(Clone)]
pub struct Rustgi {
    inner: Arc<RustgiRef>,
}

impl Rustgi {
    pub fn new(address: SocketAddr, app: PyObject) -> Self {
        Self {
            inner: Arc::new(RustgiRef { app, address }),
        }
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
            if let Err(err) = poll.poll(&mut events, Some(std::time::Duration::from_secs(0))) {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }

                return Err(err.into());
            }

            for event in events.iter() {
                match event.token() {
                    SERVER => loop {
                        // Received an event for the TCP server socket, which
                        // indicates we can accept an connection.
                        let (connection, _) = match server.accept() {
                            Ok((connection, address)) => (connection, address),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                // If we get a `WouldBlock` error we know our
                                // listener has no more incoming connections queued,
                                // so we can return to polling and wait for some
                                // more.
                                break;
                            }
                            Err(e) => {
                                // If it was any other kind of error, something went
                                // wrong and we terminate with an error.
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

enum RequestState {
    ParseHead,
    ParseBody,
    StartResponse,
    Response(bool), // bool to indicate if we are chunking

    // special state
    ResponseContinue,
}

#[derive(Debug)]
struct RequestInfo {
    keep_alive: bool,
    chunked: bool,
    content_length: u64,
    is_http_11: bool,
}

struct Request {
    rustgi: Rustgi,
    stream: TcpStream,
    state: RequestState,
    info: RequestInfo,
    buffer: BytesMut,

    environ: Option<Py<PyDict>>,
    response: Option<http::Response<WSGIResponseBody>>,
}

impl Request {
    fn new(
        rustgi: Rustgi,
        stream: TcpStream,
        registry: &Registry,
        token: Token,
    ) -> Result<Self, Error> {
        let mut this = Self {
            rustgi,
            stream,
            state: RequestState::ParseHead,
            info: RequestInfo {
                keep_alive: false,
                chunked: false,
                content_length: 0,
                is_http_11: false,
            },
            buffer: BytesMut::with_capacity(1024),

            environ: None,
            response: None,
        };

        registry.register(&mut this.stream, token, Interest::READABLE)?;
        Ok(this)
    }

    fn reset(&mut self, registry: &Registry, event: &Event) -> Result<(), Error> {
        self.state = RequestState::ParseHead;
        self.info = RequestInfo {
            keep_alive: false,
            chunked: false,
            content_length: 0,
            is_http_11: false,
        };
        self.buffer.clear();
        self.environ = None;
        self.response = None;

        registry.reregister(&mut self.stream, event.token(), Interest::READABLE)?;
        Ok(())
    }

    fn clean(&mut self, registry: &Registry) -> Result<(), Error> {
        registry.deregister(&mut self.stream)?;
        Ok(())
    }

    fn build_environ_from_head(
        &self,
        py: Python<'_>,
        request: &httparse::Request<'_, '_>,
    ) -> Result<Py<PyDict>, Error> {
        let environ = PyDict::new(py);
        let uri = Uri::from_str(request.path.unwrap())?;

        environ.set_item(
            intern!(py, "REQUEST_METHOD"),
            request.method.unwrap().to_uppercase(),
        )?;
        environ.set_item(intern!(py, "SCRIPT_NAME"), intern!(py, ""))?;
        environ.set_item(intern!(py, "PATH_INFO"), uri.path())?;
        environ.set_item(intern!(py, "QUERY_STRING"), uri.query().unwrap_or(""))?;
        environ.set_item(intern!(environ.py(), "SERVER_NAME"), self.rustgi.get_host())?;
        environ.set_item(intern!(environ.py(), "SERVER_PORT"), self.rustgi.get_port())?;
        environ.set_item(
            intern!(environ.py(), "SERVER_PROTOCOL"),
            request
                .version
                .map(|v| match v {
                    0 => intern!(py, "HTTP/1.0"),
                    1 => intern!(py, "HTTP/1.1"),
                    _ => intern!(py, "HTTP/1.1"),
                })
                .unwrap_or(intern!(py, "HTTP/1.1")),
        )?;

        for header in request.headers.iter() {
            if header.name.eq_ignore_ascii_case("Content-Type") {
                environ.set_item(
                    intern!(environ.py(), "CONTENT_TYPE"),
                    from_utf8(header.value).unwrap_or(""),
                )?;

                continue;
            }

            if header.name.eq_ignore_ascii_case("Content-Length") {
                environ.set_item(
                    intern!(environ.py(), "CONTENT_LENGTH"),
                    from_utf8(header.value).unwrap_or(""),
                )?;

                continue;
            }

            environ.set_item(
                &format!("HTTP_{}", header.name.to_uppercase().replace('-', "_")),
                from_utf8(header.value).unwrap_or(""),
            )?;
        }

        environ.set_item(intern!(environ.py(), "wsgi.version"), (1, 0))?;
        environ.set_item(
            intern!(environ.py(), "wsgi.url_scheme"),
            uri.scheme_str().unwrap_or("http"),
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
        environ.set_item(intern!(environ.py(), "wsgi.multithread"), false)?;
        environ.set_item(intern!(environ.py(), "wsgi.multiprocess"), true)?;
        environ.set_item(intern!(environ.py(), "wsgi.run_once"), false)?;
        environ.set_item(intern!(environ.py(), "wsgi.input"), PY_BYTES_IO.call0(py)?)?;

        Ok(environ.into())
    }

    fn poll_from_stream(&mut self, _: &Registry, _: &Event) -> Result<bool, Error> {
        loop {
            if self.buffer.capacity() == self.buffer.len() {
                self.buffer.reserve(1024);
            }

            let buf = self.buffer.chunk_mut();
            let buf = unsafe { &mut *(buf as *mut _ as *mut [u8]) };

            match self.stream.read(buf) {
                Ok(n) if n == 0 => {
                    return Ok(false);
                }
                Ok(n) => unsafe { self.buffer.advance_mut(n) },
                // Would block "errors" are the OS's way of saying that the
                // connection is not actually ready to perform this I/O operation.
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
        }

        Ok(true)
    }

    fn handle_parse_head(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        Python::with_gil(|py| -> Result<bool, Error> {
            let (head_size, environ) = {
                let mut headers: [MaybeUninit<httparse::Header<'_>>; MAX_HEADERS] =
                    unsafe { MaybeUninit::uninit().assume_init() };
                let mut req = httparse::Request::new(&mut []);
                let bytes = self.buffer.as_ref();

                match req.parse_with_uninit_headers(bytes, &mut headers) {
                    Ok(httparse::Status::Complete(parsed_len)) => {
                        (parsed_len, self.build_environ_from_head(py, &req)?)
                    }
                    Ok(httparse::Status::Partial) => return Ok(true),
                    Err(e) => return Err(e.into()),
                }
            };

            self.environ = Some(environ);
            let _ = self.buffer.split_to(head_size);
            let mut expect_continue = false;
            let environ = self.environ.as_ref().unwrap().as_ref(py);
            let http_version = environ
                .get_item(intern!(py, "SERVER_PROTOCOL"))
                .unwrap()
                .downcast::<PyString>()
                .unwrap()
                .to_str()?;
            let is_http_11 = http_version.eq("HTTP/1.1");
            self.info.is_http_11 = is_http_11;

            if is_http_11 {
                self.info.keep_alive = true;
            } else {
                self.info.keep_alive = false;
            }

            if let Some(te) = environ.get_item(intern!(py, "HTTP_TRANSFER_ENCODING")) {
                let te = te.downcast::<PyString>().unwrap().to_str()?;
                if let Some(encoding) = te.split(',').next() {
                    if encoding.trim().eq_ignore_ascii_case("chunked") {
                        self.info.chunked = true;
                    }
                }
            }

            if let Some(cl) = environ.get_item(intern!(py, "CONTENT_LENGTH")) {
                let cl = cl.downcast::<PyString>().unwrap().to_str()?;
                self.info.content_length = from_digits(cl.as_bytes()).unwrap_or(0);
            } else if !self.info.chunked {
                environ.set_item(intern!(py, "CONTENT_LENGTH"), "0")?;
            }

            if let Some(conn) = environ.get_item(intern!(py, "HTTP_CONNECTION")) {
                let conn = conn.downcast::<PyString>().unwrap().to_str()?;
                if self.info.keep_alive {
                    // HTTP/1.1 defaults to keep-alive
                    for token in conn.split(',') {
                        if token.trim().eq_ignore_ascii_case("close") {
                            self.info.keep_alive = false;
                            break;
                        }
                    }
                } else {
                    // HTTP/1.0 defaults to close
                    for token in conn.split(',') {
                        if token.trim().eq_ignore_ascii_case("keep-alive") {
                            self.info.keep_alive = true;
                            break;
                        }
                    }
                }
            }

            if let Some(exp) = environ.get_item(intern!(py, "HTTP_EXPECT")) {
                let exp = exp.downcast::<PyString>().unwrap().to_str()?;
                if exp.trim().eq_ignore_ascii_case("100-continue") {
                    expect_continue = true;
                }
            }

            if expect_continue && self.buffer.is_empty() {
                // if head_size < size, we have read more data than the head so we
                // don't have to response with 100-continue
                registry.reregister(&mut self.stream, event.token(), Interest::WRITABLE)?;
                self.buffer.clear();
                self.buffer.extend_from_slice(RESPONSE_CONTINUE);
                self.state = RequestState::ResponseContinue;
            } else {
                self.state = RequestState::ParseBody;
            }

            Ok(true)
        })
    }

    fn handle_parse_body(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        Python::with_gil(|py| -> Result<bool, Error> {
            let input = self
                .environ
                .as_ref()
                .unwrap()
                .as_ref(py)
                .get_item(intern!(py, "wsgi.input"))
                .unwrap();

            if self.info.chunked {
                let environ = self.environ.as_ref().unwrap().as_ref(py);
                let mut cl: u64 = match environ.get_item(intern!(py, "CONTENT_LENGTH")) {
                    None => 0,
                    Some(cl) => {
                        from_digits(cl.downcast::<PyString>().unwrap().to_str()?.as_bytes())
                            .unwrap()
                    }
                };

                loop {
                    let pool = unsafe { py.new_pool() };
                    let py = pool.python();

                    match httparse::parse_chunk_size(&self.buffer) {
                        Ok(httparse::Status::Complete((idx, size)))
                            if self.buffer.len() >= idx + size as usize + 2 =>
                        {
                            let bytes = self
                                .buffer
                                .split_to(idx + size as usize + 2)
                                .split_off(idx)
                                .split_to(size as usize);
                            input.call_method1(intern!(py, "write"), (&bytes as &[u8],))?;
                            cl += size;
                            environ.set_item(intern!(py, "CONTENT_LENGTH"), cl.to_string())?;

                            if size == 0 {
                                // https://peps.python.org/pep-0444/#request-trailers-and-chunked-transfer-encoding
                                // When using chunked transfer encoding on request content, the RFCs allow there to be request trailers.
                                // These are like request headers but come after the final null data chunk. These trailers are only
                                // available when the chunked data stream is finite length and when it has all been read in. Neither WSGI nor Web3 currently supports them.
                                break;
                            }
                        }
                        Ok(_) => return Ok(true),
                        Err(e) => return Err(e.into()),
                    };
                }
            } else {
                let write_size = self.buffer.len().min(self.info.content_length as usize);
                let bytes = self.buffer.split_to(write_size);
                input.call_method1(intern!(py, "write"), (&bytes as &[u8],))?;
                self.info.content_length -= write_size as u64;

                if self.info.content_length != 0 {
                    return Ok(true);
                }
            }

            registry.reregister(&mut self.stream, event.token(), Interest::WRITABLE)?;
            input.call_method1(intern!(py, "seek"), (0,))?;
            self.state = RequestState::StartResponse;
            Ok(true)
        })
    }

    fn handle_response_continue(
        &mut self,
        registry: &Registry,
        event: &Event,
    ) -> Result<bool, Error> {
        while !self.buffer.is_empty() {
            match self.stream.write(&self.buffer) {
                Ok(n) => _ = self.buffer.split_to(n),
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }

        registry.reregister(&mut self.stream, event.token(), Interest::READABLE)?;
        self.state = RequestState::ParseBody;
        Ok(true)
    }

    fn handle_start_response(&mut self, _: &Registry, _: &Event) -> Result<bool, Error> {
        self.buffer.clear(); // clear buffer

        Python::with_gil(|py| -> Result<bool, Error> {
            let builder = {
                let pool = unsafe { py.new_pool() };
                let py = pool.python();
                let wsgi_response_config = Py::new(py, WSGIResponseConfig::default())?;

                let wsgi_iter = self
                    .rustgi
                    .get_wsgi_app()
                    .call1(py, (self.environ.take().unwrap(), &wsgi_response_config))?;

                let mut config = wsgi_response_config.as_ref(py).borrow_mut();
                config.take_builder(wsgi_iter)
            };

            let response = builder.build(py)?;
            let is_content_length_set = response.headers().contains_key("Content-Length");

            // write head into buffer
            if self.info.is_http_11 {
                self.buffer.extend_from_slice(b"HTTP/1.1 ");
            } else {
                self.buffer.extend_from_slice(b"HTTP/1.0 ");
            }

            self.buffer
                .extend_from_slice(response.status().as_str().as_bytes());
            self.buffer.extend_from_slice(b" ");
            self.buffer.extend_from_slice(
                response
                    .status()
                    .canonical_reason()
                    .unwrap_or("Unknown")
                    .as_bytes(),
            );
            self.buffer.extend_from_slice(b"\r\n");

            for (key, value) in response.headers() {
                self.buffer.extend_from_slice(key.as_str().as_bytes());
                self.buffer.extend_from_slice(b": ");
                self.buffer.extend_from_slice(value.as_bytes());
                self.buffer.extend_from_slice(b"\r\n");
            }

            let exact_content_length = response.body().size_hint().exact();
            let chunked_response = exact_content_length.is_none();
            let keep_alive = self.info.keep_alive && (!chunked_response || self.info.is_http_11);

            if chunked_response {
                self.buffer
                    .extend_from_slice(b"Transfer-Encoding: chunked\r\n");
            } else if !is_content_length_set {
                self.buffer.extend_from_slice(
                    format!("Content-Length: {}\r\n", exact_content_length.unwrap()).as_bytes(),
                );
            }

            if keep_alive {
                self.buffer.extend_from_slice(b"Connection: keep-alive\r\n");
            } else {
                self.buffer.extend_from_slice(b"Connection: close\r\n");
            }

            self.buffer.extend_from_slice(b"\r\n");
            self.response.replace(response);
            self.state = RequestState::Response(false);
            Ok(true)
        })
    }

    fn handle_response(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        let status = self.response.as_ref().unwrap().status();
        let body = self.response.as_mut().unwrap().body_mut();
        let chunked_response = body.size_hint().exact().is_none();
        let keep_alive = self.info.keep_alive && (!chunked_response || self.info.is_http_11);
        let chunking = match self.state {
            RequestState::Response(ref mut chunking) => chunking,
            _ => unreachable!(),
        };

        while !body.is_end_stream() || !self.buffer.is_empty() {
            while !self.buffer.is_empty() {
                match self.stream.write(&self.buffer) {
                    Ok(n) => _ = self.buffer.split_to(n),
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }

            match status {
                StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED => break,
                _ => {}
            }

            body.poll_from_iter()?;
            let mut current_chunk = match body.take_current_chunk() {
                Some(chunk) => chunk,
                None => break, // no more data
            };

            if current_chunk.is_new() && chunked_response && !*chunking {
                self.buffer
                    .extend_from_slice(format!("{:X}\r\n", current_chunk.remaining()).as_bytes());
                body.set_current_chunk(current_chunk);
                *chunking = true;

                continue;
            }

            match self.stream.write(current_chunk.chunk()) {
                Ok(n) => current_chunk.advance(n),
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            };

            if current_chunk.remaining() > 0 {
                body.set_current_chunk(current_chunk);
                continue;
            }

            body.poll_from_iter()?;
            if *chunking {
                self.buffer.extend_from_slice(b"\r\n");
                *chunking = false;
            }

            if chunked_response && body.is_end_stream() {
                self.buffer.extend_from_slice(b"0\r\n\r\n");
            }
        }

        if keep_alive {
            self.reset(registry, event)?;
            return Ok(true);
        }

        Ok(false)
    }

    fn handle(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        if event.is_writable() {
            if let RequestState::StartResponse = self.state {
                if !self.handle_start_response(registry, event)? {
                    return Ok(false);
                }
            }

            if let RequestState::Response(_) = self.state {
                if !self.handle_response(registry, event)? {
                    return Ok(false);
                }
            }

            if let RequestState::ResponseContinue = self.state {
                if !self.handle_response_continue(registry, event)? {
                    return Ok(false);
                }
            }
        }

        if event.is_readable() {
            if !self.poll_from_stream(registry, event)? {
                return Ok(false);
            }

            if let RequestState::ParseHead = self.state {
                if !self.handle_parse_head(registry, event)? {
                    return Ok(false);
                }
            }

            if let RequestState::ParseBody = self.state {
                if !self.handle_parse_body(registry, event)? {
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }
}

fn from_digits(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }

    let mut result = 0u64;
    const RADIX: u64 = 10;

    for &b in bytes {
        match b {
            b'0'..=b'9' => {
                result = result.checked_mul(RADIX)?;
                result = result.checked_add((b - b'0') as u64)?;
            }
            _ => {
                return None;
            }
        }
    }

    Some(result)
}

#[derive(Default)]
#[pyclass]
struct WSGIResponseConfig {
    builder: Option<http::response::Builder>,
    content_length: Option<u64>,
}

#[pymethods]
impl WSGIResponseConfig {
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
            http::Response::builder().status(StatusCode::from_str(status_pair.0).map_err(
                |_| PyValueError::new_err(format!("invalid status code: {}", status_pair.0)),
            )?);

        for (name, value) in headers {
            builder = builder.header(name, value);
            if name.eq_ignore_ascii_case("Content-Length") {
                self.content_length = Some(value.parse().map_err(|_| {
                    PyValueError::new_err(format!("invalid content-length: {}", value))
                })?);
            }
        }

        self.builder.replace(builder);
        Ok(())
    }
}

impl WSGIResponseConfig {
    fn take_builder(&mut self, wsgi_iter: PyObject) -> WSGIResponseBuilder {
        WSGIResponseBuilder::new(
            self.builder.take().unwrap_or_default(),
            self.content_length.take(),
            wsgi_iter,
        )
    }
}

/// WSGI response builder.
struct WSGIResponseBuilder {
    builder: http::response::Builder,
    content_length: Option<u64>,
    wsgi_iter: PyObject,
}

impl WSGIResponseBuilder {
    fn new(
        builder: http::response::Builder,
        content_length: Option<u64>,
        wsgi_iter: PyObject,
    ) -> Self {
        Self {
            builder,
            content_length,
            wsgi_iter,
        }
    }

    fn build(self, py: Python<'_>) -> Result<http::Response<WSGIResponseBody>, Error> {
        // optimize for the common case of a single string
        let mut wsgi_iter = self.wsgi_iter;

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

        body.set_content_length(self.content_length);
        self.builder.body(body).map_err(Error::from)
    }
}

struct WSGIResponseBody {
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

    fn take_current_chunk(&mut self) -> Option<PyBytesBuf> {
        self.current_chunk.take()
    }

    fn set_current_chunk(&mut self, chunk: PyBytesBuf) {
        self.current_chunk.replace(chunk);
    }

    fn poll_from_iter(&mut self) -> Result<(), Error> {
        if self.current_chunk.is_none() {
            if let Some(ref iter) = self.wsgi_iter {
                Python::with_gil(|py| {
                    let mut iter = iter.as_ref(py);
                    if let Some(next_chunk) = iter.next() {
                        self.current_chunk
                            .replace(PyBytesBuf::new(next_chunk?.extract()?));
                    }

                    Result::<(), Error>::Ok(())
                })?;
            }
        }

        // If the current chunk is still None, there is no more data
        if self.current_chunk.is_none() {
            self.wsgi_iter = None
        }

        Ok(())
    }

    fn is_end_stream(&self) -> bool {
        self.current_chunk.is_none() && self.wsgi_iter.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
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
