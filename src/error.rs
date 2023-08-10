use std::fmt::Debug;

use pyo3::prelude::PyErr;

#[derive(Debug)]
pub enum Error {
    PyError(PyErr),
    HTTPError(http::Error),
    InvalidUri(http::uri::InvalidUri),
    IOError(std::io::Error),
    HTTPParseError(httparse::Error),
    InvalidChunkSizeError(httparse::InvalidChunkSize),
    AddrParseError(std::net::AddrParseError),
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

impl From<httparse::Error> for Error {
    fn from(err: httparse::Error) -> Self {
        Self::HTTPParseError(err)
    }
}

impl From<httparse::InvalidChunkSize> for Error {
    fn from(err: httparse::InvalidChunkSize) -> Self {
        Self::InvalidChunkSizeError(err)
    }
}

impl From<std::net::AddrParseError> for Error {
    fn from(err: std::net::AddrParseError) -> Self {
        Self::AddrParseError(err)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PyError(err) => write!(f, "PyError: {}", err),
            Self::HTTPError(err) => write!(f, "HTTPError: {}", err),
            Self::InvalidUri(err) => write!(f, "InvalidUri: {}", err),
            Self::IOError(err) => write!(f, "IOError: {}", err),
            Self::HTTPParseError(err) => write!(f, "HTTPParseError: {}", err),
            Self::InvalidChunkSizeError(err) => write!(f, "InvalidChunkSizeError: {}", err),
            Self::AddrParseError(err) => write!(f, "AddrParseError: {}", err),
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
            Self::HTTPParseError(err) => Some(err),
            Self::InvalidChunkSizeError(_) => None,
            Self::AddrParseError(err) => Some(err),
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
            Error::IOError(err) => PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("{}", err)),
            Error::HTTPParseError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::InvalidChunkSizeError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::AddrParseError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
        }
    }
}
