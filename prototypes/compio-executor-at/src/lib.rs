//! Prototype of `compio-executor`'s public API built on [`async_task`].
//!
//! It exists to measure whether the hand-written task allocation, vtable and
//! waker state machine in `compio-executor` can be replaced by `async-task`
//! without losing performance. It contains no `unsafe` code.

#![forbid(unsafe_code)]

use std::{
    any::Any,
    cell::RefCell,
    collections::VecDeque,
    error::Error,
    fmt::Display,
    io,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use async_task::{Builder, FallibleTask, Runnable};
use compio_send_wrapper::SendWrapper;
use crossbeam_queue::SegQueue;
use slab::Slab;

type Panic = Box<dyn Any + Send + 'static>;

/// Configuration for [`Executor`], mirroring `compio_executor::ExecutorConfig`.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Initial capacity of the local queue.
    pub local_queue_size: usize,
    /// The maximum number of tasks to run in each tick.
    pub max_interval: u32,
    /// A waker to be woken when a task is scheduled from another thread.
    pub waker: Option<Waker>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            local_queue_size: 64,
            max_interval: 61,
            waker: None,
        }
    }
}

/// State reachable from wakers on any thread.
struct Shared {
    /// Queue for wakes on the executor's own thread. `SendWrapper` hands it
    /// out only on that thread.
    local: SendWrapper<RefCell<VecDeque<Runnable>>>,
    /// Queue for wakes from other threads.
    sync: SegQueue<Runnable>,
    pending: AtomicUsize,
    waker: Option<Waker>,
}

impl Shared {
    fn schedule(&self, runnable: Runnable) {
        match self.local.get() {
            Some(local) => {
                self.drain_sync(local);
                local.borrow_mut().push_back(runnable);
            }
            None => {
                self.pending.fetch_add(1, Ordering::Release);
                self.sync.push(runnable);
                if let Some(waker) = &self.waker {
                    waker.wake_by_ref();
                }
            }
        }
    }

    #[inline]
    fn drain_sync(&self, local: &RefCell<VecDeque<Runnable>>) {
        if self.pending.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut drained = 0;
        while let Some(runnable) = self.sync.pop() {
            local.borrow_mut().push_back(runnable);
            drained += 1;
        }
        if drained != 0 {
            self.pending.fetch_sub(drained, Ordering::Release);
        }
    }
}

/// A single-threaded executor with cross-thread wakes, like
/// `compio_executor::Executor`.
pub struct Executor {
    shared: Arc<Shared>,
    /// Wakers of all live tasks, so `clear` can drop every future.
    active: Rc<RefCell<Slab<Waker>>>,
    config: ExecutorConfig,
}

/// Placeholder for `compio_executor::SpawnMeta` (console support is not
/// prototyped).
#[derive(Debug, Clone, Copy, Default)]
pub struct SpawnMeta;

impl SpawnMeta {
    /// Capture the caller's location.
    #[track_caller]
    pub fn capture() -> Self {
        Self
    }
}

struct RemoveOnDrop {
    active: Rc<RefCell<Slab<Waker>>>,
    key: usize,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        self.active.borrow_mut().try_remove(self.key);
    }
}

pin_project_lite::pin_project! {
    struct CatchUnwind<F> {
        #[pin]
        fut: F,
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Panic>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = self.project().fut;
        match catch_unwind(AssertUnwindSafe(|| fut.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

impl Executor {
    /// Create a new executor.
    pub fn new() -> Self {
        Self::with_config(ExecutorConfig::default())
    }

    /// Create a new executor with config.
    pub fn with_config(mut config: ExecutorConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                local: SendWrapper::new(RefCell::new(VecDeque::with_capacity(
                    config.local_queue_size,
                ))),
                sync: SegQueue::new(),
                pending: AtomicUsize::new(0),
                waker: config.waker.take(),
            }),
            active: Rc::new(RefCell::new(Slab::with_capacity(config.local_queue_size))),
            config,
        }
    }

    /// Spawn a future onto the executor.
    #[track_caller]
    pub fn spawn<F: Future + 'static>(&self, fut: F) -> JoinHandle<F::Output> {
        self.spawn_at(fut, SpawnMeta::capture())
    }

    /// Spawn a future onto the executor, attributing it to `meta`.
    pub fn spawn_at<F: Future + 'static>(&self, fut: F, _meta: SpawnMeta) -> JoinHandle<F::Output> {
        let mut active = self.active.borrow_mut();
        let entry = active.vacant_entry();
        let guard = RemoveOnDrop {
            active: self.active.clone(),
            key: entry.key(),
        };
        let fut = async move {
            let _guard = guard;
            CatchUnwind { fut }.await
        };
        let shared = self.shared.clone();
        let (runnable, task) =
            Builder::new().spawn_local(move |_| fut, move |r| shared.schedule(r));
        entry.insert(runnable.waker());
        drop(active);
        runnable.schedule();
        JoinHandle {
            task: Some(task.fallible()),
        }
    }

    fn local(&self) -> &RefCell<VecDeque<Runnable>> {
        self.shared
            .local
            .get()
            .expect("Executor used off its thread")
    }

    /// Run at most `max_interval` scheduled tasks. Returns whether tasks are
    /// still scheduled.
    pub fn tick(&self) -> bool {
        let local = self.local();
        self.shared.drain_sync(local);
        // Like compio-executor, a task woken while it runs waits for the
        // next tick instead of running again in this one.
        let n = local.borrow().len().min(self.config.max_interval as usize);
        for _ in 0..n {
            let Some(runnable) = local.borrow_mut().pop_front() else {
                break;
            };
            runnable.run();
        }
        !local.borrow().is_empty()
    }

    /// Check if there's still scheduled task that needs to be ran.
    pub fn has_task(&self) -> bool {
        !self.local().borrow().is_empty()
    }

    /// Drop all tasks.
    pub fn clear(&self) {
        let wakers: Vec<Waker> = self.active.borrow_mut().drain().collect();
        for waker in wakers {
            waker.wake();
        }
        let local = self.local();
        loop {
            self.shared.drain_sync(local);
            let batch: Vec<Runnable> = local.borrow_mut().drain(..).collect();
            if batch.is_empty() {
                break;
            }
            drop(batch);
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.clear();
    }
}

/// A handle that awaits the result of a task. Dropping it cancels the task.
#[must_use = "Drop `JoinHandle` will cancel the task. Use `detach` to run it in background."]
pub struct JoinHandle<T> {
    task: Option<FallibleTask<Result<T, Panic>>>,
}

impl<T> Unpin for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    /// Cancel the task and wait for the result, if any.
    pub async fn cancel(mut self) -> Option<T> {
        self.task.take()?.cancel().await.and_then(Result::ok)
    }

    /// Detach the task to let it run in the background.
    pub fn detach(mut self) {
        if let Some(task) = self.task.take() {
            task.detach();
        }
    }

    /// Returns true if the task has completed or been cancelled.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(FallibleTask::is_finished)
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let task = self.task.as_mut().expect("Cannot poll after completion");
        Pin::new(task).poll(cx).map(|res| {
            self.task = None;
            match res {
                Some(Ok(value)) => Ok(value),
                Some(Err(panic)) => Err(JoinError::Panicked(panic)),
                None => Err(JoinError::Cancelled),
            }
        })
    }
}

/// Task failed to execute to completion.
#[derive(Debug)]
pub enum JoinError {
    /// The task was cancelled.
    Cancelled,
    /// The task panicked.
    Panicked(Panic),
}

impl JoinError {
    /// Resume unwind if the task panicked, otherwise do nothing.
    pub fn resume_unwind(self) {
        if let JoinError::Panicked(e) = self {
            resume_unwind(e)
        }
    }
}

impl Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Cancelled => write!(f, "Task was cancelled"),
            JoinError::Panicked(_) => write!(f, "Task has panicked"),
        }
    }
}

impl Error for JoinError {}

impl From<JoinError> for io::Error {
    fn from(e: JoinError) -> Self {
        io::Error::other(e.to_string())
    }
}
