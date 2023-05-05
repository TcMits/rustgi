#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(windows)]
use std::os::fd::FromRawSocket;

use futures::executor::block_on;
use hyper::server::conn::http1::Builder;
use mio::net::TcpListener;
use pyo3::{ffi::PyObject_AsFileDescriptor, prelude::*, types::PyTuple, AsPyPointer};

use crate::server::ServerConfig;

#[pyclass]
struct RustgiConfig {
    #[pyo3(get, set)]
    socket: PyObject,
    #[pyo3(get, set)]
    wsgi_app: PyObject,
}

#[pyfunction]
fn serve(py: Python<'_>, config: &RustgiConfig) -> PyResult<()> {
    let mut rustgi_builder = crate::core::RustgiBuilder::new(config.wsgi_app.clone());

    #[cfg(unix)]
    let listener = unsafe {
        let raw_socket = PyObject_AsFileDescriptor(config.socket.as_ptr());
        TcpListener::from_raw_fd(raw_socket)
    };

    // Note: select.select() uses "int PyObject_AsFileDescriptor(PyObject *o)" to get the socket handle of a socket
    #[cfg(windows)]
    let listener = unsafe {
        let raw_socket = PyObject_AsFileDescriptor(config_ref.socket.as_ptr());
        TcpListener::from_raw_socket(raw_socket)
    };

    let socket_ref = config.socket.as_ref(py);
    if socket_ref.hasattr("getsockname")? {
        let sockname: &PyTuple = socket_ref.getattr("getsockname")?.call0()?.extract()?;

        if sockname.len() == 2 {
            let host: String = sockname.get_item(0)?.extract()?;
            let port: u16 = sockname.get_item(1)?.extract()?;
            rustgi_builder.set_host(host);
            rustgi_builder.set_port(Some(port));
        }
    }

    let mut rustgi = rustgi_builder.build();
    py.allow_threads(|| {
        block_on(rustgi.serve(ServerConfig {
            tcp_listener: listener,
            service_builder: Builder::new(),
        }))
    })?;

    Ok(())
}

#[pymodule]
fn rustgi(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RustgiConfig>()?;
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    Ok(())
}
