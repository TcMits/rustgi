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
    HyperError(hyper::Error),
    HyperToStrError(hyper::header::ToStrError),
    BoxError(Box<dyn std::error::Error + Send + Sync>),
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

impl From<hyper::header::ToStrError> for Error {
    fn from(err: hyper::header::ToStrError) -> Self {
        Self::HyperToStrError(err)
    }
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        Self::HyperError(err)
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for Error {
    fn from(err: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::BoxError(err)
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
            Self::HyperError(err) => write!(f, "HyperError: {}", err),
            Self::HyperToStrError(err) => write!(f, "HyperToStrError: {}", err),
            Self::BoxError(err) => write!(f, "BoxError: {}", err),
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
            Self::HyperError(err) => Some(err),
            Self::HyperToStrError(err) => Some(err),
            Self::BoxError(err) => Some(err.as_ref()),
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
            Error::HyperError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::HyperToStrError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err))
            }
            Error::BoxError(err) => {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("{}", err.as_ref()))
            }
        }
    }
}
