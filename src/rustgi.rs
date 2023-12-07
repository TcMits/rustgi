use pyo3::prelude::*;

#[pyclass]
#[derive(Clone)]
struct RustgiConfig {
    address: String,
    max_body_size: usize,
}

impl RustgiConfig {
    fn into_rustgi(self, app: PyObject) -> PyResult<crate::core::Rustgi> {
        let mut rustgi = crate::core::Rustgi::new(self.address.parse()?, app);
        rustgi.set_max_body_size(self.max_body_size);

        Ok(rustgi)
    }
}

#[pymethods]
impl RustgiConfig {
    #[new]
    fn new() -> Self {
        Default::default()
    }

    fn set_address<'p, 'a>(mut pyself: PyRefMut<'p, Self>, address: &'a str) -> PyRefMut<'p, Self> {
        pyself.address = address.to_owned();
        pyself
    }

    fn set_max_body_size(
        mut pyself: PyRefMut<'_, Self>,
        max_body_size: usize,
    ) -> PyRefMut<'_, Self> {
        pyself.max_body_size = max_body_size;
        pyself
    }
}

impl Default for RustgiConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8000".to_string(),
            max_body_size: 1024 * 1024 * 10,
        }
    }
}

#[pyfunction]
fn serve(py: Python<'_>, app: PyObject, config: &RustgiConfig) -> PyResult<()> {
    py.allow_threads(|| -> Result<(), crate::error::Error> {
        let rustgi = config.clone().into_rustgi(app)?;
        rustgi.serve()
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
