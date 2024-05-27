use lazy_static::lazy_static;
use pyo3::Python;
use tokio_rayon::AsyncThreadPool;

lazy_static! {
    static ref GIL_THREAD_POOL: rayon::ThreadPool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
}

#[inline]
pub(crate) async fn with_gil<F, R>(f: F) -> R
where
    R: std::marker::Send + 'static,
    F: for<'py> FnOnce(Python<'py>) -> R + std::marker::Send + 'static,
{
    GIL_THREAD_POOL
        .spawn_async(move || Python::with_gil(f))
        .await
}
