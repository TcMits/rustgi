use std::{
    cell::Cell,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

use lazy_static::lazy_static;
use pyo3::Python;
use std::mem::MaybeUninit;

static GIL_LOCK: Mutex<()> = Mutex::new(());
const GIL_WAKERS_LENGTH: usize = 1000;

lazy_static! {
    static ref GIL_WAKERS: Mutex<(usize, [MaybeUninit<Waker>; GIL_WAKERS_LENGTH])> =
        Mutex::new((0, unsafe { MaybeUninit::uninit().assume_init() }));
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

/// Call the given closure with a Python instance.
/// If the GIL is held in another thread, the closure will be called later.
pub(crate) fn poll_gil<F, R>(f: F, cx: &mut Context<'_>) -> Poll<R>
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    if is_gil_acquired_in_this_thread() {
        return Poll::Ready(with_gil_unchecked(f));
    }

    let lock = match GIL_LOCK.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let mut wakers = GIL_WAKERS.lock().unwrap();
            let (ref mut index, ref mut wakers) = &mut *wakers;
            if *index >= GIL_WAKERS_LENGTH {
                // max wakers reached, reschedule the task immediately.
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            wakers[*index].write(cx.waker().clone());
            *index += 1;
            return Poll::Pending;
        }
    };

    let result = Poll::Ready(with_gil_unchecked(f));
    drop(lock);

    // drain wakers
    let mut wakers = GIL_WAKERS.lock().unwrap();
    let (ref mut len, ref mut wakers) = &mut *wakers;
    for w in wakers.iter().take(*len) {
        unsafe { w.assume_init_ref() }.wake_by_ref();
    }
    *len = 0;
    result
}
