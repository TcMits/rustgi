use crate::core::Rustgi;
use crate::error::Error;
use crate::response::{WSGIResponseBody, WSGIStartResponse};
use crate::stream::Stream;
use bytes::{Buf, BytesMut};
use http::Uri;
use lazy_static::lazy_static;
use log::debug;
use pyo3::ffi::{PyDict_SetItemString, PySys_GetObject};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::{intern, AsPyPointer};
use std::io::{self, IoSlice};
use std::mem::MaybeUninit;
use std::str::from_utf8;

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

#[derive(Debug)]
enum RequestState {
    Parse(Option<Py<PyDict>>),
    ErrorResponse(&'static [u8], Option<Py<PyDict>>),
    Response(BytesMut, WSGIResponseBody),
    Finished,
}

pub(crate) struct Request {
    rustgi: Rustgi,
    stream: Stream,
    parser: llhttp_rs::Parser,
    state: RequestState,
}

impl Request {
    pub(crate) fn new(rustgi: Rustgi, stream: Stream) -> Self {
        Self {
            rustgi,
            stream,
            parser: llhttp_rs::Parser::request(),
            state: RequestState::Parse(None),
        }
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.parser = llhttp_rs::Parser::request();
        self.state = RequestState::Parse(None);

        Ok(())
    }

    fn get_environ<'p>(&self, py: Python<'p>) -> Result<&'p PyDict, Error> {
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

        Ok(environ)
    }

    fn get_response(&self, environ: &PyDict) -> Result<(BytesMut, WSGIResponseBody), Error> {
        let py = environ.py();
        let mut buffer = BytesMut::with_capacity(REPONSE_HEADER_SIZE);
        let mut is_http_11 = false;
        match self.parser.get_version().unwrap_or(http::Version::HTTP_11) {
            http::Version::HTTP_11 => {
                is_http_11 = true;
                buffer.extend_from_slice(b"HTTP/1.1 ")
            }
            _ => buffer.extend_from_slice(b"HTTP/1.0 "),
        };

        let builder = {
            let pool = unsafe { py.new_pool() };
            let py = pool.python();
            let wsgi_response_config = Py::new(py, WSGIStartResponse::new(buffer))?;

            let wsgi_iter = self
                .rustgi
                .get_wsgi_app()
                .call1(py, (environ, &wsgi_response_config))?;

            let mut config = wsgi_response_config.as_ref(py).borrow_mut();
            config.take_body_builder(wsgi_iter)
        };

        let (mut buffer, body) = builder.build(py)?; // trigger start_response

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

    async fn on_parse(&mut self) -> Result<(), Error> {
        let mut data: [MaybeUninit<u8>; READ_BUFFER_SIZE] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let read_buffer = unsafe {
            std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, READ_BUFFER_SIZE)
        };

        while matches!(self.state, RequestState::Parse(..)) {
            match self.stream.read(read_buffer).await {
                Ok(n) if n == 0 => {
                    self.state = RequestState::Finished;
                    return Ok(());
                }
                Ok(n) => {
                    let read_buffer =
                        unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, n) };

                    Python::with_gil(|py| -> Result<(), Error> {
                        let environ = match self.state {
                            RequestState::Parse(Some(ref environ)) => environ.as_ref(py),
                            RequestState::Parse(None) => {
                                let environ = self.get_environ(py)?;
                                self.state = RequestState::Parse(Some(environ.into()));
                                environ
                            }
                            _ => unreachable!(),
                        };

                        let mut context = RequestParserContext {
                            rustgi: self.rustgi.clone(),
                            environ,
                            header_field: "".to_owned(),
                            expect_continue: false,
                            complete: false,
                            error: None,
                        };

                        if let Err(err) = self.parser.parse(&mut context, read_buffer) {
                            if context.error.is_none() {
                                context.error.replace(err.into());
                            }
                        };

                        if let Some(Error::HTTPResponseError(response)) = context.error {
                            self.state = RequestState::ErrorResponse(response, None);
                        } else if context.error.is_some() {
                            debug!("Error parsing request: {:?}", context.error);
                            self.state = RequestState::ErrorResponse(RESPONSE_BAD_REQUEST, None);
                        } else if context.expect_continue {
                            self.state = RequestState::ErrorResponse(
                                RESPONSE_CONTINUE,
                                Some(environ.into()),
                            );
                        } else if context.complete {
                            let (buffer, body) = self.get_response(environ)?;
                            self.state = RequestState::Response(buffer, body);
                        }

                        Ok(())
                    })?;
                }
                Err(err) => match err.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => continue,
                    _ => return Err(err.into()),
                },
            };
        }

        return Ok(());
    }

    async fn on_error_response(&mut self) -> Result<(), Error> {
        if let RequestState::ErrorResponse(ref mut response, ref mut continue_read) = self.state {
            while !response.is_empty() {
                match self.stream.write(response).await {
                    Ok(n) => response.advance(n),
                    Err(err) => match err.kind() {
                        io::ErrorKind::Interrupted => continue,
                        io::ErrorKind::WouldBlock => continue,
                        _ => return Err(err.into()),
                    },
                }
            }

            if continue_read.is_some() {
                self.state = RequestState::Parse(continue_read.take());
                return Ok(());
            }
        }

        self.state = RequestState::Finished;
        Ok(())
    }

    async fn on_response(&mut self) -> Result<(), Error> {
        if let RequestState::Response(ref mut buffer, ref mut body) = self.state {
            let chunked_response = body.size_hint().exact().is_none();
            let keep_alive = self.parser.should_keep_alive()
                && (!chunked_response
                    || matches!(
                        self.parser.get_version().unwrap_or(http::Version::HTTP_11),
                        http::Version::HTTP_11
                    ));

            let mut vectored_body_data: [MaybeUninit<IoSlice<'_>>; 4] =
                unsafe { MaybeUninit::uninit().assume_init() };

            while !buffer.is_empty() {
                match self.stream.write(buffer).await {
                    Ok(n) => _ = buffer.split_to(n),
                    Err(err) => match err.kind() {
                        io::ErrorKind::Interrupted => continue,
                        io::ErrorKind::WouldBlock => continue,
                        _ => return Err(err.into()),
                    },
                };
            }

            while !body.is_end_stream() {
                let vectored_body_buffer = unsafe {
                    std::slice::from_raw_parts_mut(
                        vectored_body_data.as_mut_ptr() as *mut IoSlice<'_>,
                        4,
                    )
                };

                let current_chunk = match body.take_current_chunk() {
                    Some(chunk) => chunk,
                    None => {
                        Python::with_gil(|py| body.poll_from_iter(py))?;
                        body.take_current_chunk().unwrap()
                    }
                };

                let start_chunk_bytes: Vec<u8> = if chunked_response {
                    format!("{:X}\r\n", current_chunk.chunk().remaining()).into_bytes()
                } else {
                    vec![]
                };

                let mut write_buffer = if chunked_response {
                    let mut size = 3;
                    vectored_body_buffer[0] = IoSlice::new(&start_chunk_bytes);
                    vectored_body_buffer[1] = IoSlice::new(current_chunk.chunk());
                    vectored_body_buffer[2] = IoSlice::new(b"\r\n");
                    if body.is_end_stream() {
                        vectored_body_buffer[3] = IoSlice::new(b"0\r\n\r\n");
                        size += 1;
                    }

                    unsafe {
                        std::slice::from_raw_parts(
                            vectored_body_data.as_ptr() as *const IoSlice<'_>,
                            size,
                        )
                    }
                } else {
                    vectored_body_buffer[0] = IoSlice::new(current_chunk.chunk());

                    unsafe {
                        std::slice::from_raw_parts(
                            vectored_body_data.as_ptr() as *const IoSlice<'_>,
                            1,
                        )
                    }
                };

                while !write_buffer.is_empty() {
                    match self.stream.write_vectored(write_buffer).await {
                        Ok(mut n) => {
                            let mut write_buffer_new_size = 0;
                            for slice in write_buffer.iter() {
                                let adv_size = slice.len().min(n);
                                if adv_size == slice.len() {
                                    continue;
                                }

                                write_buffer_new_size += 1;
                                n -= adv_size;
                                vectored_body_buffer[write_buffer_new_size - 1] =
                                    IoSlice::new(&slice[adv_size..]);
                            }

                            write_buffer = unsafe {
                                std::slice::from_raw_parts(
                                    vectored_body_data.as_ptr() as *const IoSlice<'_>,
                                    write_buffer_new_size,
                                )
                            }
                        }
                        Err(err) => match err.kind() {
                            io::ErrorKind::Interrupted => continue,
                            io::ErrorKind::WouldBlock => continue,
                            _ => return Err(err.into()),
                        },
                    };
                }
            }

            if !keep_alive {
                self.state = RequestState::Finished;
                return Ok(());
            }
        }

        self.reset()?;
        Ok(())
    }

    pub(crate) async fn handle(&mut self) -> Result<(), Error> {
        loop {
            match self.state {
                RequestState::Parse(..) => self.on_parse().await?,
                RequestState::ErrorResponse(..) => self.on_error_response().await?,
                RequestState::Response(..) => self.on_response().await?,
                RequestState::Finished => break,
            }
        }

        Ok(())
    }
}
