use pyo3::{
    ffi::{PyBytes_AsString, PyBytes_Size},
    types::PyBytes,
    AsPyPointer, Py,
};

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
            let buffer = PyBytes_AsString(self.0.as_ptr()) as *const u8;
            let length = PyBytes_Size(self.0.as_ptr()) as usize;
            std::slice::from_raw_parts(buffer, length)
        }
    }
}
