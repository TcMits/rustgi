use pyo3::PyObject;
use std::sync::Arc;

#[derive(Debug)]
pub struct RustgiBuilder {
    app: PyObject,
    host: String,
    port: Option<u16>,
}

impl RustgiBuilder {
    pub fn new(app: PyObject) -> Self {
        Self {
            app,
            host: "".to_string(),
            port: None,
        }
    }

    pub fn set_host(&mut self, host: String) -> &Self {
        self.host = host;
        self
    }

    pub fn set_port(&mut self, port: Option<u16>) -> &Self {
        self.port = port;
        self
    }

    pub fn build(self) -> Rustgi {
        Rustgi {
            inner: Arc::new(RustgiRef {
                app: self.app,
                host: self.host,
                port: self.port,
            }),
        }
    }
}

struct RustgiRef {
    app: PyObject,
    host: String,
    port: Option<u16>,
}

#[derive(Clone)]
pub struct Rustgi {
    inner: Arc<RustgiRef>,
}

impl Rustgi {
    pub fn new(app: PyObject) -> Self {
        RustgiBuilder::new(app).build()
    }

    pub fn get_wsgi_app(&self) -> PyObject {
        self.inner.app.clone()
    }

    pub fn get_host(&self) -> &str {
        &self.inner.host
    }

    pub fn get_port(&self) -> Option<u16> {
        self.inner.port
    }

    pub fn wsgi_caller(&self) -> crate::wsgi::WSGICaller {
        crate::wsgi::WSGICaller::new(self.clone())
    }
}
