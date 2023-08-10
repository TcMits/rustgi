extern crate rustgi;

use pyo3::prelude::*;

fn get_rustgi(host: &str) -> rustgi::core::Rustgi {
    let app: PyObject = Python::with_gil(|py| -> Result<PyObject, rustgi::error::Error> {
        Ok(PyModule::from_code(
            py,
            r#"
RESPONSE = b'x' * 2

def application(environment, start_response):
    start_response(
        '200 OK',  # Status
        [('Content-type', 'text/plain'), ('Content-Length', str(len(RESPONSE)))]  # Headers
    )
    return [RESPONSE]
"#,
            "",
            "",
        )
        .unwrap()
        .getattr("application")
        .unwrap()
        .into())
    })
    .unwrap();

    rustgi::core::Rustgi::new(host.parse().unwrap(), app)
}

fn main() {
    pyo3::prepare_freethreaded_python();
    let rustgi = get_rustgi("0.0.0.0:8000");
    rustgi.serve().unwrap();
}
