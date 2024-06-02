use pyo3::Python;
use std::sync::Arc;
use std::thread;
use tokio_rayon::AsyncThreadPool;

#[inline]
pub(crate) fn new_gil_pool() -> Result<Arc<rayon::ThreadPool>, rayon::ThreadPoolBuildError> {
    Ok(Arc::new(
        rayon::ThreadPoolBuilder::new()
            .spawn_handler(|thread| {
                let mut b = thread::Builder::new();
                if let Some(name) = thread.name() {
                    b = b.name(name.to_owned());
                }
                if let Some(stack_size) = thread.stack_size() {
                    b = b.stack_size(stack_size);
                }
                b.spawn(|| Python::with_gil(|_| thread.run()))?;
                Ok(())
            })
            .num_threads(1)
            .build()?,
    ))
}

#[inline]
pub(crate) async fn with_gil<F, R>(pool: Arc<rayon::ThreadPool>, f: F) -> R
where
    R: std::marker::Send + 'static,
    F: for<'py> FnOnce(Python<'py>) -> R + std::marker::Send + 'static,
{
    pool.spawn_fifo_async(move || f(unsafe { Python::assume_gil_acquired() }))
        .await // spawn fifo to avoid unexpected behaviors
}
