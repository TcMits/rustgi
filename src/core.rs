use std::io;

use pyo3::PyObject;

#[derive(Debug)]
pub struct RustgiBuilder {
    wsgi_app: PyObject,
    host: String,
    port: Option<u16>,
}

impl RustgiBuilder {
    pub fn new(wsgi_app: PyObject) -> Self {
        Self {
            wsgi_app,
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
            wsgi_app: self.wsgi_app,
            host: self.host,
            port: self.port,
        }
    }
}

pub struct Rustgi {
    wsgi_app: PyObject,
    host: String,
    port: Option<u16>,
}

impl Rustgi {
    pub fn new(wsgi_app: PyObject) -> Self {
        RustgiBuilder::new(wsgi_app).build()
    }

    pub fn get_wsgi_app(&self) -> PyObject {
        self.wsgi_app.clone()
    }

    pub fn get_host(&self) -> &str {
        &self.host
    }

    pub fn get_port(&self) -> Option<u16> {
        self.port
    }

    pub fn wsgi_caller(&mut self) -> crate::wsgi::WSGICaller {
        crate::wsgi::WSGICaller::new(self.clone())
    }

    pub async fn serve(&mut self, config: crate::server::ServerConfig) -> io::Result<()> {
        crate::server::Server::new(self.clone(), config)?.await
    }

    pub fn clone(&mut self) -> Self {
        Self {
            wsgi_app: self.wsgi_app.clone(),
            host: unsafe {
                String::from_raw_parts(
                    self.host.as_mut_ptr(),
                    self.host.len(),
                    self.host.capacity(),
                )
            },
            port: self.port,
        }
    }
}
