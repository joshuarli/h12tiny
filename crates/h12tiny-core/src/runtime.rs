//! Runtime-neutral Hyper executor and timer adapters.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// The erased future accepted by [`BoxExecutor`].
pub type BoxSendFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// An executor adapter backed by a cloneable submission function.
///
/// Applications with their own runtime can use this to hand Hyper connection
/// work to that runtime without depending on Hyper's concrete executor trait
/// at their application boundary. The closure must arrange for the future to
/// be driven; dropping it would leak connection work.
#[derive(Clone)]
pub struct FnExecutor<F> {
    submit: F,
}

impl<F> FnExecutor<F> {
    /// Creates an executor that delegates every submitted future to `submit`.
    pub fn new(submit: F) -> Self {
        Self { submit }
    }
}

impl<F> hyper::rt::Executor<BoxSendFuture> for FnExecutor<F>
where
    F: Fn(BoxSendFuture) + Send + Sync + Clone + 'static,
{
    fn execute(&self, future: BoxSendFuture) {
        (self.submit)(future);
    }
}

/// A cloneable executor handle for Hyper background tasks.
///
/// This is the small type-erased boundary used by client and server builders:
/// a caller supplies an executor for boxed `Send` futures, while Hyper can use
/// the resulting handle for each concrete connection future it creates.
#[derive(Clone)]
pub struct BoxExecutor(Arc<dyn hyper::rt::Executor<BoxSendFuture> + Send + Sync>);

impl BoxExecutor {
    /// Erase an executor that accepts boxed, sendable unit futures.
    pub fn new<E>(executor: E) -> Self
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + 'static,
    {
        Self(Arc::new(executor))
    }

    /// Submit a concrete unit future to the erased executor.
    pub fn execute<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.execute(Box::pin(future));
    }
}

impl std::fmt::Debug for BoxExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoxExecutor")
            .finish_non_exhaustive()
    }
}

impl<F> hyper::rt::Executor<F> for BoxExecutor
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, future: F) {
        BoxExecutor::execute(self, future);
    }
}

/// A Hyper timer backed by [`async_io::Timer`].
#[derive(Clone, Copy, Debug, Default)]
pub struct AsyncIoTimer;

/// Backwards-compatible descriptive alias for [`AsyncIoTimer`].
pub use AsyncIoTimer as FuturesTimer;

/// The sleep future returned by [`AsyncIoTimer`].
#[derive(Debug)]
pub struct FuturesSleep {
    inner: async_io::Timer,
}

impl FuturesSleep {
    fn reset(self: Pin<&mut Self>, deadline: Instant) {
        // `async_io::Timer` is `Unpin`; accessing it through `get_mut` does not
        // move the pinned sleep object and permits the timer's in-place reset.
        self.get_mut().inner.set_at(deadline);
    }
}

impl Future for FuturesSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().inner).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl hyper::rt::Sleep for FuturesSleep {}

impl hyper::rt::Timer for AsyncIoTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(FuturesSleep {
            inner: async_io::Timer::after(duration),
        })
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(FuturesSleep {
            inner: async_io::Timer::at(deadline),
        })
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn hyper::rt::Sleep>>, new_deadline: Instant) {
        if let Some(sleep) = sleep.as_mut().downcast_mut_pin::<FuturesSleep>() {
            FuturesSleep::reset(sleep, new_deadline);
        } else {
            *sleep = self.sleep_until(new_deadline);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoxExecutor, FuturesSleep, FuturesTimer};
    use hyper::rt::{Executor, Timer};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    use std::time::{Duration, Instant};

    fn noop_waker() -> Waker {
        unsafe fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        // SAFETY: The raw waker's null data pointer is never dereferenced,
        // and every vtable operation is a no-op or creates the same valid
        // vtable/data pair.
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    #[derive(Clone)]
    struct RecordingExecutor(Arc<Mutex<Vec<super::BoxSendFuture>>>);

    impl Executor<super::BoxSendFuture> for RecordingExecutor {
        fn execute(&self, future: super::BoxSendFuture) {
            self.0.lock().unwrap().push(future);
        }
    }

    #[test]
    fn box_executor_erases_concrete_future_types() {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let executor = BoxExecutor::new(RecordingExecutor(queue.clone()));
        Executor::execute(&executor, async {});
        assert_eq!(queue.lock().unwrap().len(), 1);
    }

    #[test]
    fn timer_sleep_and_reset_use_async_io_timer() {
        let timer = FuturesTimer;
        let deadline = Instant::now() + Duration::from_millis(10);
        let mut sleep = timer.sleep_until(deadline);
        assert!(sleep.as_ref().is::<FuturesSleep>());

        let reset_deadline = Instant::now();
        timer.reset(&mut sleep, reset_deadline);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(sleep.as_mut().poll(&mut cx), Poll::Ready(())));
    }

    #[test]
    fn timer_reports_current_time() {
        let timer = FuturesTimer;
        let before = Instant::now();
        let now = timer.now();
        let after = Instant::now();
        assert!(now >= before);
        assert!(now <= after);
    }
}
