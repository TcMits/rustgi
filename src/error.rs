use pyo3::prelude::PyErr;

#[derive(Debug)]
pub enum Error {
    PyError(PyErr),
    HyperError(hyper::Error),
    HTTPError(http::Error),
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        Self::HyperError(err)
    }
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

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PyError(err) => write!(f, "PyError: {}", err),
            Self::HyperError(err) => write!(f, "HyperError: {}", err),
            Self::HTTPError(err) => write!(f, "HTTPError: {}", err),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PyError(err) => Some(err),
            Self::HyperError(err) => Some(err),
            Self::HTTPError(err) => Some(err),
        }
    }
}
