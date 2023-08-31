use pyo3::prelude::*;
use std::fs;
use std::io::{BufReader, Read};
use std::sync::Arc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

fn load_certs(filename: &str) -> PyResult<Vec<rustls::Certificate>> {
    let certfile = fs::File::open(filename)?;
    let mut reader = BufReader::new(certfile);
    Ok(rustls_pemfile::certs(&mut reader)
        .map_err(|_| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid cert"))?
        .iter()
        .map(|v| rustls::Certificate(v.clone()))
        .collect())
}

fn find_suite(name: &str) -> Option<rustls::SupportedCipherSuite> {
    for suite in rustls::ALL_CIPHER_SUITES {
        let sname = format!("{:?}", suite.suite()).to_lowercase();

        if sname == name.to_string().to_lowercase() {
            return Some(*suite);
        }
    }

    None
}

fn lookup_suites(suites: &[&str]) -> PyResult<Vec<rustls::SupportedCipherSuite>> {
    let mut out = Vec::new();

    for csname in suites {
        let scs = find_suite(csname);
        match scs {
            Some(s) => out.push(s),
            None => {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "invalid cipher suite: {}",
                    csname
                )))
            }
        }
    }

    Ok(out)
}

fn load_private_key(filename: &str) -> PyResult<rustls::PrivateKey> {
    let keyfile = fs::File::open(filename)?;
    let mut reader = BufReader::new(keyfile);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|_| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid private key"))?
        {
            Some(rustls_pemfile::Item::RSAKey(key)) => return Ok(rustls::PrivateKey(key)),
            Some(rustls_pemfile::Item::PKCS8Key(key)) => return Ok(rustls::PrivateKey(key)),
            Some(rustls_pemfile::Item::ECKey(key)) => return Ok(rustls::PrivateKey(key)),
            None => break,
            _ => {}
        }
    }

    Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
        "invalid private key",
    ))
}

fn lookup_versions(versions: &[&str]) -> PyResult<Vec<&'static rustls::SupportedProtocolVersion>> {
    let mut out = Vec::new();

    for vname in versions {
        let version = match vname.as_ref() {
            "1.2" => &rustls::version::TLS12,
            "1.3" => &rustls::version::TLS13,
            _ => {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "invalid version ".to_owned() + vname,
                ))
            }
        };

        out.push(version);
    }

    Ok(out)
}

fn load_ocsp(name: &str) -> PyResult<Vec<u8>> {
    let mut ret = Vec::new();
    fs::File::open(name)?.read_to_end(&mut ret)?;
    Ok(ret)
}

#[pyclass]
#[derive(Clone)]
struct TLSConfig {
    certs: Vec<rustls::Certificate>,
    key: rustls::PrivateKey,
    suites: Vec<rustls::SupportedCipherSuite>,
    versions: Vec<&'static rustls::SupportedProtocolVersion>,
    ocsp: Vec<u8>,
}

impl TLSConfig {
    fn into_server_config(self) -> PyResult<rustls::ServerConfig> {
        rustls::ServerConfig::builder()
            .with_cipher_suites(&self.suites)
            .with_safe_default_kx_groups()
            .with_protocol_versions(&self.versions)
            .map_err(|_| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid versions"))?
            .with_no_client_auth()
            .with_single_cert_with_ocsp_and_sct(self.certs, self.key, self.ocsp, vec![])
            .map_err(|_| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid certs"))
    }
}

#[pymethods]
impl TLSConfig {
    #[new]
    fn new() -> Self {
        Default::default()
    }

    fn set_certs<'p, 'a>(
        mut pyself: PyRefMut<'p, Self>,
        cert: &'a str,
    ) -> PyResult<PyRefMut<'p, Self>> {
        pyself.certs = load_certs(&cert)?;
        Ok(pyself)
    }

    fn set_key<'p, 'a>(
        mut pyself: PyRefMut<'p, Self>,
        key: &'a str,
    ) -> PyResult<PyRefMut<'p, Self>> {
        pyself.key = load_private_key(key)?;
        Ok(pyself)
    }

    fn set_suites<'p, 'a>(
        mut pyself: PyRefMut<'p, Self>,
        suites: Vec<&'a str>,
    ) -> PyResult<PyRefMut<'p, Self>> {
        pyself.suites = lookup_suites(&suites)?;
        Ok(pyself)
    }

    fn set_versions<'p, 'a>(
        mut pyself: PyRefMut<'p, Self>,
        versions: Vec<&'a str>,
    ) -> PyResult<PyRefMut<'p, Self>> {
        pyself.versions = lookup_versions(&versions)?;
        Ok(pyself)
    }

    fn set_ocsp<'p, 'a>(
        mut pyself: PyRefMut<'p, Self>,
        oscp: Option<&'a str>,
    ) -> PyResult<PyRefMut<'p, Self>> {
        pyself.ocsp = match oscp {
            Some(name) => load_ocsp(name)?,
            None => vec![],
        };
        Ok(pyself)
    }
}

impl Default for TLSConfig {
    fn default() -> Self {
        Self {
            certs: vec![],
            key: rustls::PrivateKey(vec![]),
            suites: rustls::DEFAULT_CIPHER_SUITES.to_vec(),
            versions: rustls::DEFAULT_VERSIONS.to_vec(),
            ocsp: vec![],
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct RustgiConfig {
    address: String,
    max_body_size: usize,
    tls_config: Option<TLSConfig>,
}

impl RustgiConfig {
    fn into_rustgi(self, app: PyObject) -> PyResult<crate::core::Rustgi> {
        let mut rustgi = crate::core::Rustgi::new(self.address.parse()?, app);
        rustgi.set_max_body_size(self.max_body_size);

        if let Some(tls_config) = self.tls_config {
            let config = Arc::new(tls_config.into_server_config()?);
            rustgi.set_tls_config(config);
        }

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

    fn set_tls_config(
        mut pyself: PyRefMut<'_, Self>,
        tls_config: Option<TLSConfig>,
    ) -> PyRefMut<'_, Self> {
        pyself.tls_config = tls_config;
        pyself
    }
}

impl Default for RustgiConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8000".to_string(),
            max_body_size: 1024 * 1024 * 10,
            tls_config: None,
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
    m.add_class::<TLSConfig>()?;
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    Ok(())
}
