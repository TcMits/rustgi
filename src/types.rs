use pyo3::{
    ffi::{PyBytes_AsString, PyBytes_Size},
    types::PyBytes,
    AsPyPointer, Py,
};

pub struct PyBytesBuf(Py<PyBytes>, usize);

impl PyBytesBuf {
    #[inline]
    pub fn new(b: Py<PyBytes>) -> Self {
        Self(b, 0)
    }
}

impl hyper::body::Buf for PyBytesBuf {
    #[inline]
    fn remaining(&self) -> usize {
        self.chunk().len()
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        // safe because Python bytes are immutable, the result may be used for as long as the reference to
        let chunk: &[u8] = unsafe {
            let buffer = PyBytes_AsString(self.0.as_ptr()) as *const u8;
            let length = PyBytes_Size(self.0.as_ptr()) as usize;
            std::slice::from_raw_parts(buffer, length)
        };

        &chunk[self.1..]
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        if cnt > self.remaining() {
            panic!("attempted to advance past the end of the buffer")
        }

        self.1 += cnt;
    }
}
