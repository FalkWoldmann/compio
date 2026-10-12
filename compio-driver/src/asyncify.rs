use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use asyncband::{
    blocking::FutureExt,
    mpmc::{UnboundedReceiver, UnboundedSender, unbounded},
};

/// An error that may be emitted when all worker threads are busy.
///
/// It simply contains the dispatchable value with a convenient [`fmt::Debug`]
/// and [`fmt::Display`] implementation.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct DispatchError<T>(pub T);

impl<T> DispatchError<T> {
    /// Consume the error, yielding the dispatchable that failed to be sent.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for DispatchError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "DispatchError(..)".fmt(f)
    }
}

impl<T> fmt::Display for DispatchError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "all threads are busy".fmt(f)
    }
}

impl<T> std::error::Error for DispatchError<T> {}

type BoxedDispatchable = Box<dyn Dispatchable + Send>;

/// A trait for dispatching a closure.
///
/// Used by [`AsyncifyPool`] to run closures in other threads.
///
/// It's implemented for all `FnOnce() + Send + 'static` but may also be
/// implemented for any other types that are `Send` and `'static`.
pub trait Dispatchable: Send + 'static {
    /// Run the dispatchable
    fn run(self: Box<Self>);
}

impl<F> Dispatchable for F
where
    F: FnOnce() + Send + 'static,
{
    fn run(self: Box<Self>) {
        (*self)()
    }
}

struct CounterGuard(Arc<AtomicUsize>);

impl Drop for CounterGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Takes one unclaimed idle worker out of `idle`, returning whether there was
/// one.
fn claim_idle(idle: &AtomicUsize) -> bool {
    let mut n = idle.load(Ordering::Acquire);
    while n > 0 {
        match idle.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) => n = actual,
        }
    }
    false
}

/// A worker starts claimed: it was spawned for a dispatchable that is sent
/// right after. After running a dispatchable, it becomes idle and claimable
/// again. Every claim is followed by exactly one send, so a claimed worker
/// always has a dispatchable to receive.
fn worker(
    receiver: UnboundedReceiver<BoxedDispatchable>,
    counter: Arc<AtomicUsize>,
    idle: Arc<AtomicUsize>,
    timeout: Duration,
) -> impl FnOnce() {
    move || {
        counter.fetch_add(1, Ordering::AcqRel);
        let _guard = CounterGuard(counter);
        loop {
            match receiver.recv().wait_timeout(timeout) {
                Some(Ok(f)) => {
                    f.run();
                    idle.fetch_add(1, Ordering::AcqRel);
                }
                Some(Err(_)) => break,
                // Exit only if no dispatcher has claimed this worker meanwhile;
                // otherwise a dispatchable is on its way.
                None if claim_idle(&idle) => break,
                None => {}
            }
        }
    }
}

/// A thread pool to perform blocking operations in other threads.
#[derive(Debug, Clone)]
pub struct AsyncifyPool {
    sender: UnboundedSender<BoxedDispatchable>,
    receiver: UnboundedReceiver<BoxedDispatchable>,
    counter: Arc<AtomicUsize>,
    idle: Arc<AtomicUsize>,
    thread_limit: usize,
    recv_timeout: Duration,
}

impl AsyncifyPool {
    /// Create [`AsyncifyPool`] with thread number limit and channel receive
    /// timeout.
    pub fn new(thread_limit: usize, recv_timeout: Duration) -> Self {
        let (sender, receiver) = unbounded();
        Self {
            sender,
            receiver,
            counter: Arc::new(AtomicUsize::new(0)),
            idle: Arc::new(AtomicUsize::new(0)),
            thread_limit,
            recv_timeout,
        }
    }

    /// Send a dispatchable, usually a closure, to another thread. Usually the
    /// user should not use it. When all threads are busy and thread number
    /// limit has been reached, it will return an error with the original
    /// dispatchable.
    pub fn dispatch<D: Dispatchable>(&self, f: D) -> Result<(), DispatchError<D>> {
        if !claim_idle(&self.idle) {
            if self.thread_limit == 0 {
                panic!("the thread pool is needed but no worker thread is running");
            } else if self.counter.load(Ordering::Acquire) >= self.thread_limit {
                return Err(DispatchError(f));
            }
            std::thread::spawn(worker(
                self.receiver.clone(),
                self.counter.clone(),
                self.idle.clone(),
                self.recv_timeout,
            ));
        }
        if self.sender.send(Box::new(f) as BoxedDispatchable).is_err() {
            unreachable!("receiver should not all disconnected")
        }
        Ok(())
    }
}
