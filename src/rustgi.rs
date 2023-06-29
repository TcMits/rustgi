use std::net::TcpListener;

use hyper::server::conn::http1::Builder;

use log::debug;
use pyo3::prelude::*;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[pyclass]
struct RustgiConfig {
    address: String,
    worker_threads: Option<usize>,
}

#[pymethods]
impl RustgiConfig {
    #[new]
    fn new() -> Self {
        Default::default()
    }

    fn set_address(mut pyself: PyRefMut<'_, Self>, address: String) -> PyRefMut<'_, Self> {
        pyself.address = address;
        pyself
    }

    fn set_worker_threads(
        mut pyself: PyRefMut<'_, Self>,
        worker_threads: Option<usize>,
    ) -> PyRefMut<'_, Self> {
        pyself.worker_threads = worker_threads;
        pyself
    }
}

impl Default for RustgiConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8000".to_string(),
            // using current thread by default
            worker_threads: Some(0),
        }
    }
}

#[pyfunction]
fn serve(py: Python<'_>, app: PyObject, config: &RustgiConfig) -> PyResult<()> {
    py.allow_threads(|| -> PyResult<()> {
        let runtime = match config.worker_threads {
            Some(0) => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            Some(threads) => tokio::runtime::Builder::new_multi_thread()
                .worker_threads(threads)
                .enable_all()
                .build()?,
            None => tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
        };

        // build tcp listener
        let tcp_listener = TcpListener::bind(config.address.as_str())?;
        tcp_listener.set_nonblocking(true)?;

        // build rustgi
        let mut rustgi_builder = crate::core::RustgiBuilder::new(app);
        rustgi_builder.set_host(tcp_listener.local_addr()?.ip().to_string());
        rustgi_builder.set_port(Some(tcp_listener.local_addr()?.port()));
        let rustgi = rustgi_builder.build();

        // server loop
        let server = async {
            let tcp_listener = tokio::net::TcpListener::from_std(tcp_listener)?;

            loop {
                let (stream, _) = tcp_listener.accept().await?;
                let wsgi_caller = rustgi.wsgi_caller();

                runtime.spawn(async move {
                    if let Err(e) = Builder::new().serve_connection(stream, wsgi_caller).await {
                        debug!("server connection error: {}", e);
                    }
                });
            }
        };

        runtime.block_on(async {
            tokio::select! {
                server_result = server => server_result,
                exit_result = tokio::signal::ctrl_c() => exit_result,
            }
        })?;

        Ok(())
    })
}

#[pymodule]
fn rustgi(_py: Python, m: &PyModule) -> PyResult<()> {
    pyo3_log::init();
    m.add_class::<RustgiConfig>()?;
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    Ok(())
}
