use pyo3::{types::PyBytes, Py, Python};

pub struct PyBytesBuf(Py<PyBytes>);

impl PyBytesBuf {
    #[inline]
    pub(crate) fn new(b: Py<PyBytes>) -> Self {
        Self(b)
    }

    #[inline]
    pub(crate) fn chunk(&self) -> &[u8] {
        // safe because Python bytes are immutable, the result may be used for as long as the reference to
        unsafe {
            let py = Python::assume_gil_acquired();
            self.0.as_bytes(py)
        }
    }
}
