use event_listener::{Event, EventListener};
use lazy_static::lazy_static;
use pyo3::Python;
use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

lazy_static! {
    static ref GIL_LOCK: AtomicBool = AtomicBool::new(false);
    static ref GIL_UNLOCK_EVENT: Event = Event::new();
}

thread_local! {
    // to detect if the GIL is held in this thread.
    static THREAD_GIL_COUNT: Cell<usize> = Cell::new(0);
}

#[inline]
fn is_gil_acquired_in_this_thread() -> bool {
    THREAD_GIL_COUNT.with(|count| count.get() > 0)
}

struct GILLocalCounter;

impl GILLocalCounter {
    #[inline]
    fn new() -> Self {
        THREAD_GIL_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        Self
    }
}

impl Drop for GILLocalCounter {
    #[inline]
    fn drop(&mut self) {
        THREAD_GIL_COUNT.with(|count| count.set(count.get().saturating_sub(1)));
    }
}

/// Call the given closure with a Python instance.
#[inline]
pub(crate) fn with_gil_unchecked<F, R>(f: F) -> R
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    let _guard = GILLocalCounter::new();
    Python::with_gil(|py| f(py))
}

// A future that resolves when the GIL is acquired.
// GILFuture is designed to be used in one task/future.
pub(crate) struct GILFuture {
    listener: Option<EventListener>,
}

impl GILFuture {
    #[inline]
    pub(crate) fn new() -> Self {
        Self { listener: None }
    }

    #[inline]
    fn try_lock(&mut self) -> bool {
        // previous value is false, so we can acquire the GIL.
        !GIL_LOCK.swap(true, Ordering::Acquire)
    }

    #[inline]
    fn unlock(&mut self) {
        // force release the lock.
        GIL_LOCK.store(false, Ordering::Release);
    }

    pub(crate) fn poll_gil<F, R>(&mut self, cx: &mut Context<'_>, f: F) -> Poll<R>
    where
        F: for<'py> FnOnce(Python<'py>) -> R,
    {
        if is_gil_acquired_in_this_thread() {
            return Poll::Ready(with_gil_unchecked(f));
        }

        loop {
            // listener always None in first loop.
            //
            // if listener is not None, it means this task is waked up by GIL_UNLOCK_EVENT.
            // so if we can acquire the GIL, we can drop this listener to notify
            // another listener.
            let mut listener = self.listener.take();

            if self.try_lock() {
                let result = Poll::Ready(with_gil_unchecked(f));
                self.unlock();

                if listener.is_none() {
                    GIL_UNLOCK_EVENT.notify(1);
                }

                // returning here will drop the listener to notify another listener.
                return result;
            }

            if listener.is_none() {
                listener.replace(GIL_UNLOCK_EVENT.listen());
            }

            if let Poll::Pending = Pin::new(listener.as_mut().unwrap()).poll(cx) {
                self.listener = listener;
                return Poll::Pending;
            }
        }
    }
}
