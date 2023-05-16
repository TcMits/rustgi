#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{io, mem};

use futures::future::{AbortHandle, Abortable};
use futures::TryFutureExt;
use hyper::server::conn::http1::Builder;

use log::{debug, info};
use pyo3::prelude::*;
use tokio::net::TcpListener;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[pyclass]
struct RustgiConfig {
    address: (String, u16),
    backlog: i32,
    reuse_port: bool,
}

#[pymethods]
impl RustgiConfig {
    #[new]
    fn new() -> Self {
        Default::default()
    }

    fn set_address(
        mut pyself: PyRefMut<'_, Self>,
        address: String,
        port: u16,
    ) -> PyRefMut<'_, Self> {
        pyself.address = (address, port);
        pyself
    }

    fn set_backlog(mut pyself: PyRefMut<'_, Self>, backlog: i32) -> PyRefMut<'_, Self> {
        pyself.backlog = backlog;
        pyself
    }

    fn set_reuse_port(mut pyself: PyRefMut<'_, Self>, reuse_port: bool) -> PyRefMut<'_, Self> {
        pyself.reuse_port = reuse_port;
        pyself
    }
}

impl Default for RustgiConfig {
    fn default() -> Self {
        Self {
            address: ("0.0.0.0".to_string(), 8000),
            backlog: 1024,
            reuse_port: false,
        }
    }
}

#[pyfunction]
fn serve(py: Python<'_>, app: PyObject, config: &RustgiConfig) -> PyResult<()> {
    pyo3_log::init();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let mut service_builder = Builder::new();
    service_builder.max_buf_size(1024 * 64);

    let (abort_handle, abort_registration) = AbortHandle::new_pair();

    let exit = async {
        tokio::signal::ctrl_c().await?;
        info!("Ctrl-C received, shutting down");
        abort_handle.abort();
        Ok(())
    };

    let grateful_server = async {
        // build tcp listener
        let tcp_listener = TcpListener::bind(&config.address).await?;

        #[cfg(unix)]
        if config.reuse_port {
            unsafe {
                let optval: libc::c_int = 1;
                let ret = libc::setsockopt(
                    tcp_listener.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_REUSEPORT,
                    &optval as *const _ as *const libc::c_void,
                    mem::size_of_val(&optval) as libc::socklen_t,
                );
                if ret != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }

        #[cfg(unix)]
        unsafe {
            let ret = libc::listen(tcp_listener.as_raw_fd(), config.backlog as libc::c_int);
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
        }

        // build rustgi
        let mut rustgi_builder = crate::core::RustgiBuilder::new(app);
        rustgi_builder.set_host(tcp_listener.local_addr()?.ip().to_string());
        rustgi_builder.set_port(Some(tcp_listener.local_addr()?.port()));
        let rustgi = rustgi_builder.build();

        // server loop
        let server = async {
            loop {
                let (stream, _) = tcp_listener.accept().await?;

                let service = service_builder.serve_connection(stream, rustgi.wsgi_caller());

                tokio::task::spawn_local(async move {
                    if let Err(e) = service.await {
                        debug!("server connection error: {}", e);
                    }
                });
            }
        };

        info!(
            "Starting server on http://{}:{}",
            rustgi.get_host(),
            rustgi.get_port().unwrap_or(0)
        );
        let serve = Abortable::new(server, abort_registration)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "server aborted"));

        futures::try_join!(exit, serve)?.1
    };

    py.allow_threads(|| {
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, grateful_server)
    })?;
    Ok(())
}

#[pymodule]
fn rustgi(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RustgiConfig>()?;
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    Ok(())
}
