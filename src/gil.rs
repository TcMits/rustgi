use event_listener::{Event, EventListener};
use lazy_static::lazy_static;
use pyo3::Python;
use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    sync::Mutex,
    task::{ready, Context, Poll},
};

lazy_static! {
    static ref GIL_LOCK: Mutex<()> = Mutex::new(());
    static ref GIL_UNLOCK_EVENT: Event = Event::new();
}

thread_local! {
    // to detect if the GIL is held in this thread.
    static THREAD_GIL_COUNT: Cell<usize> = Cell::new(0);
}

fn is_gil_acquired_in_this_thread() -> bool {
    THREAD_GIL_COUNT.with(|count| count.get() > 0)
}

struct GILLocalCounter;

impl GILLocalCounter {
    fn new() -> Self {
        THREAD_GIL_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        Self
    }
}

impl Drop for GILLocalCounter {
    fn drop(&mut self) {
        THREAD_GIL_COUNT.with(|count| count.set(count.get().saturating_sub(1)));
    }
}

/// Call the given closure with a Python instance.
pub(crate) fn with_gil_unchecked<F, R>(f: F) -> R
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    let _guard = GILLocalCounter::new();
    Python::with_gil(|py| f(py))
}

// A future that resolves when the GIL is acquired.
pub(crate) struct GILFuture {
    listener: Option<EventListener>,
}

impl GILFuture {
    pub(crate) fn new() -> Self {
        Self { listener: None }
    }

    pub(crate) fn poll_gil<F, R>(&mut self, f: F, cx: &mut Context<'_>) -> Poll<R>
    where
        F: for<'py> FnOnce(Python<'py>) -> R,
    {
        if is_gil_acquired_in_this_thread() {
            return Poll::Ready(with_gil_unchecked(f));
        }

        loop {
            if let Ok(guard) = GIL_LOCK.try_lock() {
                let result = Poll::Ready(with_gil_unchecked(f));
                drop(guard);
                GIL_UNLOCK_EVENT.notify(1);
                return result;
            }

            if self.listener.is_none() {
                self.listener.replace(GIL_UNLOCK_EVENT.listen());
            }

            ready!(Pin::new(self.listener.as_mut().unwrap()).poll(cx));
            self.listener.take();
        }
    }
}
