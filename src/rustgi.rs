use pyo3::prelude::*;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[pyclass]
struct RustgiConfig {
    address: String,
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
}

impl Default for RustgiConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8000".to_string(),
        }
    }
}

#[pyfunction]
fn serve(py: Python<'_>, app: PyObject, config: &RustgiConfig) -> PyResult<()> {
    py.allow_threads(|| -> Result<(), crate::error::Error> {
        let rustgi = crate::core::Rustgi::new(config.address.parse()?, app);
        rustgi.serve(None)
    })?;

    Ok(())
}

#[pymodule]
fn rustgi(_py: Python, m: &PyModule) -> PyResult<()> {
    pyo3_log::init();
    m.add_class::<RustgiConfig>()?;
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    Ok(())
}
