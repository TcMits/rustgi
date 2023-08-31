use std::fmt::Debug;

use pyo3::prelude::PyErr;

#[derive(Debug)]
pub enum Error {
    PyError(PyErr),
    HTTPError(http::Error),
    InvalidUri(http::uri::InvalidUri),
    IOError(std::io::Error),
    AddrParseError(std::net::AddrParseError),
    UTF8Error(std::str::Utf8Error),
    LLHTTPError(llhttp_rs::Error),
    TLSError(rustls::Error),
    HTTPResponseError(&'static [u8]),
}

impl From<PyErr> for Error {
    fn from(err: PyErr) -> Self {
        Self::PyError(err)
    }
}

impl From<http::Error> for Error {
    fn from(err: http::Error) -> Self {
        Self::HTTPError(err)
    }
}

impl From<http::uri::InvalidUri> for Error {
    fn from(err: http::uri::InvalidUri) -> Self {
        Self::InvalidUri(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::IOError(err)
    }
}

impl From<std::net::AddrParseError> for Error {
    fn from(err: std::net::AddrParseError) -> Self {
        Self::AddrParseError(err)
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(err: std::str::Utf8Error) -> Self {
        Self::UTF8Error(err)
    }
}

impl From<llhttp_rs::Error> for Error {
    fn from(err: llhttp_rs::Error) -> Self {
        Self::LLHTTPError(err)
    }
}

impl From<rustls::Error> for Error {
    fn from(err: rustls::Error) -> Self {
        Self::TLSError(err)
    }
}

impl From<&'static [u8]> for Error {
    fn from(err: &'static [u8]) -> Self {
        Self::HTTPResponseError(err)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PyError(err) => write!(f, "PyError: {}", err),
            Self::HTTPError(err) => write!(f, "HTTPError: {}", err),
            Self::InvalidUri(err) => write!(f, "InvalidUri: {}", err),
            Self::IOError(err) => write!(f, "IOError: {}", err),
            Self::AddrParseError(err) => write!(f, "AddrParseError: {}", err),
            Self::UTF8Error(err) => write!(f, "UTF8Error: {}", err),
            Self::LLHTTPError(err) => write!(f, "LLHTTPError: {}", err),
            Self::TLSError(err) => write!(f, "TLSError: {}", err),
            Self::HTTPResponseError(err) => write!(f, "HTTPResponseError: {:?}", err),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PyError(err) => Some(err),
            Self::HTTPError(err) => Some(err),
            Self::InvalidUri(err) => Some(err),
            Self::IOError(err) => Some(err),
            Self::AddrParseError(err) => Some(err),
            Self::UTF8Error(err) => Some(err),
            Self::LLHTTPError(err) => Some(err),
            Self::TLSError(err) => Some(err),
            Self::HTTPResponseError(_) => None,
        }
    }
}

impl From<Error> for PyErr {
    fn from(err: Error) -> Self {
        match err {
            Error::PyError(err) => err,
            Error::HTTPError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::InvalidUri(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::IOError(err) => err.into(),
            Error::AddrParseError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::UTF8Error(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::LLHTTPError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::TLSError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::HTTPResponseError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{:?}", err))
            }
        }
    }
}
