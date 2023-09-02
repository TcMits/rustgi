use crate::core::Rustgi;
use crate::error::Error;
use crate::response::{WSGIResponseBody, WSGIResponseConfig, WSGIStartResponse};
use crate::stream::Stream;
use bytes::{Buf, BytesMut};
use http::Uri;
use lazy_static::lazy_static;
use log::debug;
use mio::event::Event;
use mio::net::TcpStream;
use mio::{Interest, Registry, Token};
use pyo3::ffi::{PyDict_SetItemString, PySys_GetObject};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::{intern, AsPyPointer};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::str::from_utf8;
use std::sync::Arc;

const RESPONSE_CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";
const RESPONSE_BAD_REQUEST: &[u8] = b"HTTP/1.1 400 Bad Request\r\n\r\n";
const RESPONSE_VERSION_NOT_SUPPORTED: &[u8] = b"HTTP/1.1 505 HTTP Version Not Supported\r\n\r\n";
const RESPONSE_CONTENT_TOO_LARGE: &[u8] = b"HTTP/1.1 413 Content Too Large\r\n\r\n";
const READ_BUFFER_SIZE: usize = 64 * 1024;
const REPONSE_HEADER_SIZE: usize = 2 * 1024;

lazy_static! {
    static ref PY_BYTES_IO: PyObject = Python::with_gil(|py| PyModule::import(py, "io")
        .unwrap()
        .getattr("BytesIO")
        .unwrap()
        .into());
}

struct RequestParserContext<'a> {
    rustgi: Rustgi,
    environ: &'a PyDict,
    header_field: String,
    expect_continue: bool,
    complete: bool,
    error: Option<Error>,
}

impl RequestParserContext<'_> {
    fn parser_result<T, E: Into<Error>>(
        &mut self,
        result: Result<T, E>,
    ) -> Result<T, llhttp_rs::Error> {
        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                self.error = Some(e.into());
                Err(llhttp_rs::Error::new_unkown())
            }
        }
    }
}

impl llhttp_rs::Callbacks for RequestParserContext<'_> {
    fn on_version(
        &mut self,
        parser: &mut llhttp_rs::Parser,
        _: &[u8],
    ) -> llhttp_rs::ParserResult<()> {
        let py = self.environ.py();
        self.parser_result(self.environ.set_item(
            intern!(py, "SERVER_PROTOCOL"),
            match parser.get_version().unwrap_or(http::Version::HTTP_11) {
                http::Version::HTTP_10 => intern!(py, "HTTP/1.0"),
                http::Version::HTTP_11 => intern!(py, "HTTP/1.1"),
                _ => {
                    self.error = Some(RESPONSE_VERSION_NOT_SUPPORTED.into());
                    return Err(llhttp_rs::Error::new_unkown());
                }
            },
        ))?;

        Ok(())
    }

    fn on_method(
        &mut self,
        parser: &mut llhttp_rs::Parser,
        _: &[u8],
    ) -> llhttp_rs::ParserResult<()> {
        let py = self.environ.py();
        self.parser_result(self.environ.set_item(
            intern!(py, "REQUEST_METHOD"),
            parser.get_method().unwrap_or(http::Method::GET).as_str(),
        ))?;

        Ok(())
    }

    fn on_url(&mut self, _: &mut llhttp_rs::Parser, url: &[u8]) -> llhttp_rs::ParserResult<()> {
        let py = self.environ.py();
        let uri = self.parser_result(Uri::try_from(url))?;

        self.parser_result(self.environ.set_item(intern!(py, "PATH_INFO"), uri.path()))?;
        self.parser_result(
            self.environ
                .set_item(intern!(py, "QUERY_STRING"), uri.query().unwrap_or("")),
        )?;
        self.parser_result(self.environ.set_item(
            intern!(py, "wsgi.url_scheme"),
            uri.scheme_str().unwrap_or("http"),
        ))?;

        Ok(())
    }

    fn on_header_field(
        &mut self,
        _: &mut llhttp_rs::Parser,
        header_field: &[u8],
    ) -> llhttp_rs::ParserResult<()> {
        self.header_field = "HTTP_".to_owned()
            + &self
                .parser_result(from_utf8(header_field))?
                .chars()
                .map(|c| {
                    if c == '-' {
                        '_'
                    } else {
                        c.to_ascii_uppercase()
                    }
                })
                .collect::<String>();

        Ok(())
    }

    fn on_header_value(
        &mut self,
        _: &mut llhttp_rs::Parser,
        header_value: &[u8],
    ) -> llhttp_rs::ParserResult<()> {
        let py = self.environ.py();
        let value = self.parser_result(from_utf8(header_value))?;

        if self
            .header_field
            .eq_ignore_ascii_case("HTTP_CONTENT_LENGTH")
        {
            return self.parser_result(self.environ.set_item(intern!(py, "CONTENT_LENGTH"), value));
        } else if self.header_field.eq_ignore_ascii_case("HTTP_CONTENT_TYPE") {
            return self.parser_result(self.environ.set_item(intern!(py, "CONTENT_TYPE"), value));
        }

        self.parser_result(self.environ.set_item(&self.header_field, value))
    }

    fn on_headers_complete(&mut self, _: &mut llhttp_rs::Parser) -> llhttp_rs::ParserResult<()> {
        let py = self.environ.py();

        if let Some(expect) = self.environ.get_item(intern!(py, "HTTP_EXPECT")) {
            self.expect_continue = expect.to_string().eq_ignore_ascii_case("100-continue")
        };

        Ok(())
    }

    fn on_body(&mut self, _: &mut llhttp_rs::Parser, body: &[u8]) -> llhttp_rs::ParserResult<()> {
        self.expect_continue = false;

        let py = self.environ.py();
        let input = self.environ.get_item(intern!(py, "wsgi.input")).unwrap();

        let remaining = {
            let input_size = self.parser_result(input.call_method0(intern!(py, "tell")))?;
            let input_size: usize = self.parser_result(input_size.extract())?;
            body.len()
                .min(input_size - self.rustgi.get_max_body_size() + 1)
        };

        self.parser_result(input.call_method1(intern!(py, "write"), (&body[0..remaining],)))?;
        Ok(())
    }

    fn on_message_complete(&mut self, _: &mut llhttp_rs::Parser) -> llhttp_rs::ParserResult<()> {
        self.complete = true;
        let py = self.environ.py();
        let input = self.environ.get_item(intern!(py, "wsgi.input")).unwrap();

        {
            let input_size = self.parser_result(input.call_method0(intern!(py, "tell")))?;
            let input_size: usize = self.parser_result(input_size.extract())?;
            if input_size > self.rustgi.get_max_body_size() {
                self.error = Some(RESPONSE_CONTENT_TOO_LARGE.into());
                return Err(llhttp_rs::Error::new_unkown());
            }
        }

        self.parser_result(input.call_method1(intern!(py, "seek"), (0,)))?;
        Ok(())
    }
}

enum RequestState {
    Handshake,
    Parse,
    ErrorResponse(&'static [u8], bool),
    Response(BytesMut, WSGIResponseBody, bool),
}

pub(crate) struct Request {
    rustgi: Rustgi,
    stream: Stream,
    parser: llhttp_rs::Parser,
    environ: Option<Py<PyDict>>,
    state: RequestState,
}

impl Request {
    pub(crate) fn new(
        rustgi: Rustgi,
        stream: TcpStream,
        registry: &Registry,
        token: Token,
    ) -> Result<Self, Error> {
        let interests = Interest::READABLE;
        let mut state = RequestState::Parse;

        let stream = if rustgi.is_tls_enabled() {
            interests.add(Interest::WRITABLE);
            state = RequestState::Handshake;
            rustls::StreamOwned::new(
                rustls::ServerConnection::new(rustgi.get_tls_config().unwrap())?,
                stream,
            )
            .into()
        } else {
            stream.into()
        };

        let mut this = Self {
            rustgi,
            stream,
            parser: llhttp_rs::Parser::request(),
            environ: None,
            state,
        };

        registry.register(&mut this.stream, token, interests)?;
        Ok(this)
    }

    fn reset(&mut self, registry: &Registry, event: &Event) -> Result<(), Error> {
        self.parser = llhttp_rs::Parser::request();
        self.environ = None;
        self.state = RequestState::Parse;

        registry.reregister(&mut self.stream, event.token(), Interest::READABLE)?;
        Ok(())
    }

    pub(crate) fn clean(&mut self, registry: &Registry) -> Result<(), Error> {
        registry.deregister(&mut self.stream)?;
        Ok(())
    }

    fn ensure_environ(&mut self, py: Python<'_>) -> Result<(), Error> {
        if self.environ.is_some() {
            return Ok(());
        }

        let environ = PyDict::new(py);
        let remote_addr = self.stream.remote_addr()?;
        environ.set_item(intern!(py, "SCRIPT_NAME"), intern!(py, ""))?;
        environ.set_item(intern!(environ.py(), "SERVER_NAME"), self.rustgi.get_host())?;
        environ.set_item(intern!(environ.py(), "SERVER_PORT"), self.rustgi.get_port())?;
        environ.set_item(intern!(environ.py(), "wsgi.version"), (1, 0))?;
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
        environ.set_item(
            intern!(environ.py(), "REMOTE_ADDR"),
            remote_addr.ip().to_string(),
        )?;
        environ.set_item(intern!(environ.py(), "REMOTE_PORT"), remote_addr.port())?;

        self.environ.replace(environ.into());

        Ok(())
    }

    fn get_response(&mut self, py: Python<'_>) -> Result<(BytesMut, WSGIResponseBody), Error> {
        let mut buffer = BytesMut::with_capacity(REPONSE_HEADER_SIZE);
        let mut is_http_11 = false;
        match self.parser.get_version().unwrap_or(http::Version::HTTP_11) {
            http::Version::HTTP_11 => {
                is_http_11 = true;
                buffer.extend_from_slice(b"HTTP/1.1 ")
            }
            _ => buffer.extend_from_slice(b"HTTP/1.0 "),
        };
        let config = Arc::new(WSGIResponseConfig::new(buffer));

        let builder = {
            let pool = unsafe { py.new_pool() };
            let py = pool.python();
            let wsgi_response_config =
                Py::new(py, WSGIStartResponse::new(Arc::downgrade(&config)))?;

            let wsgi_iter = self
                .rustgi
                .get_wsgi_app()
                .call1(py, (self.environ.take().unwrap(), &wsgi_response_config))?;

            let config = wsgi_response_config.as_ref(py).borrow_mut();
            config.take_body_builder(wsgi_iter)
        };

        let body = builder.build(py)?; // trigger start_response
        let mut buffer = Arc::into_inner(config).unwrap().into_bytes();
        let exact_content_length = body.size_hint().exact();
        let chunked_response = exact_content_length.is_none();
        let keep_alive = self.parser.should_keep_alive() && (!chunked_response || is_http_11);

        if chunked_response {
            buffer.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
        } else {
            let content_length =
                "Content-Length: ".to_owned() + &exact_content_length.unwrap().to_string() + "\r\n";
            buffer.extend_from_slice(content_length.as_bytes());
        }

        if keep_alive {
            buffer.extend_from_slice(b"Connection: keep-alive\r\n");
        } else {
            buffer.extend_from_slice(b"Connection: close\r\n");
        }
        buffer.extend_from_slice(b"\r\n");

        Ok((buffer, body))
    }

    fn on_read_or_handshake(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        Python::with_gil(|py| -> Result<bool, Error> {
            self.ensure_environ(py)?;

            let environ = self.environ.as_ref().unwrap().as_ref(py);
            let mut data: [MaybeUninit<u8>; READ_BUFFER_SIZE] =
                unsafe { MaybeUninit::uninit().assume_init() };
            let mut context = RequestParserContext {
                rustgi: self.rustgi.clone(),
                environ,
                header_field: "".to_owned(),
                expect_continue: false,
                complete: false,
                error: None,
            };
            let read_buffer = unsafe {
                std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, READ_BUFFER_SIZE)
            };
            let handshaking = !matches!(self.state, RequestState::Handshake);
            let mut finished_handshake = !handshaking;

            while !context.complete && context.error.is_none() {
                match self.stream.read(read_buffer) {
                    Ok(n) if n == 0 => {
                        return Ok(false);
                    }
                    Ok(n) => {
                        finished_handshake = true;
                        let read_buffer =
                            unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, n) };

                        match self.parser.parse(&mut context, read_buffer) {
                            Err(err) if context.error.is_none() => {
                                context.error.replace(err.into());
                            }
                            _ => {}
                        }
                    }
                    Err(ref err)
                        if err.kind() == io::ErrorKind::WouldBlock && context.expect_continue =>
                    {
                        break
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                        if handshaking && finished_handshake {
                            self.state = RequestState::Parse;
                            registry.reregister(
                                &mut self.stream,
                                event.token(),
                                Interest::READABLE,
                            )?;
                        }

                        return Ok(true);
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
            }

            registry.reregister(&mut self.stream, event.token(), Interest::WRITABLE)?;
            if let Some(Error::HTTPResponseError(response)) = context.error {
                self.state = RequestState::ErrorResponse(response, false);
            } else if context.error.is_some() {
                debug!("Error parsing request: {:?}", context.error);
                self.state = RequestState::ErrorResponse(RESPONSE_BAD_REQUEST, false);
            } else if context.expect_continue {
                self.state = RequestState::ErrorResponse(RESPONSE_CONTINUE, true);
            } else {
                let (buffer, body) = self.get_response(py)?;
                self.state = RequestState::Response(buffer, body, false);
            }

            Ok(true)
        })
    }

    fn on_write_error_response(
        &mut self,
        registry: &Registry,
        event: &Event,
    ) -> Result<bool, Error> {
        if let RequestState::ErrorResponse(ref mut response, continue_read) = self.state {
            while !response.is_empty() {
                match self.stream.write(response) {
                    Ok(n) => response.advance(n),
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }

            if continue_read {
                self.state = RequestState::Parse;
                registry.reregister(&mut self.stream, event.token(), Interest::READABLE)?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn on_write_response(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        Python::with_gil(|py| -> Result<bool, Error> {
            if let RequestState::Response(ref mut buffer, ref mut body, ref mut chunking) =
                self.state
            {
                let chunked_response = body.size_hint().exact().is_none();
                let keep_alive = self.parser.should_keep_alive()
                    && (!chunked_response
                        || matches!(
                            self.parser.get_version().unwrap_or(http::Version::HTTP_11),
                            http::Version::HTTP_11
                        ));

                while !body.is_end_stream() || !buffer.is_empty() {
                    let pool = unsafe { py.new_pool() };
                    let py = pool.python();

                    while !buffer.is_empty() {
                        match self.stream.write(buffer) {
                            Ok(n) => _ = buffer.split_to(n),
                            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                                return Ok(true)
                            }
                            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                            Err(e) => return Err(e.into()),
                        };
                    }

                    body.poll_from_iter(py)?;
                    let mut current_chunk = match body.take_current_chunk() {
                        Some(chunk) => chunk,
                        None => break, // no more data
                    };

                    if current_chunk.is_new() && chunked_response && !*chunking {
                        buffer.extend_from_slice(
                            format!("{:X}\r\n", current_chunk.remaining()).as_bytes(),
                        );
                        body.set_current_chunk(current_chunk);
                        *chunking = true;

                        continue;
                    }

                    match self.stream.write(current_chunk.chunk()) {
                        Ok(n) => current_chunk.advance(n),
                        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                            if current_chunk.remaining() > 0 {
                                body.set_current_chunk(current_chunk);
                            }

                            return Ok(true);
                        }
                        Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {
                            if current_chunk.remaining() > 0 {
                                body.set_current_chunk(current_chunk);
                            }

                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    };

                    body.poll_from_iter(py)?;
                    if *chunking {
                        buffer.extend_from_slice(b"\r\n");
                        *chunking = false;
                    }

                    if chunked_response && body.is_end_stream() {
                        buffer.extend_from_slice(b"0\r\n\r\n");
                    }
                }

                if !keep_alive {
                    return Ok(false);
                }
            }

            self.reset(registry, event)?;
            Ok(true)
        })
    }

    pub(crate) fn handle(&mut self, registry: &Registry, event: &Event) -> Result<bool, Error> {
        if event.is_readable() {
            if matches!(self.state, RequestState::Parse | RequestState::Handshake)
                && !self.on_read_or_handshake(registry, event)?
            {
                return Ok(false);
            }
        }

        if event.is_writable() {
            if matches!(self.state, RequestState::Handshake)
                && !self.on_read_or_handshake(registry, event)?
            {
                return Ok(false);
            }

            if matches!(self.state, RequestState::Response(..))
                && !self.on_write_response(registry, event)?
            {
                return Ok(false);
            }

            if matches!(self.state, RequestState::ErrorResponse(..))
                && !self.on_write_error_response(registry, event)?
            {
                return Ok(false);
            }
        }

        Ok(true)
    }
}
