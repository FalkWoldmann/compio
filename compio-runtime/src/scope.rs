//! Structured concurrency, see [`scope`].

use std::{
    any::Any,
    cell::{Cell, RefCell},
    convert::Infallible,
    fmt,
    future::Future,
    marker::PhantomData,
    mem,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread,
};

use futures_util::{FutureExt as _, task::AtomicWaker};
use slab::Slab;
use synchrony::unsync::event::EventListener;

use crate::{
    CancelToken, ContextExt,
    future::Ext,
    waker::{ExtWaker, get_ext, with_ext},
};

/// Run `f` in a new [`Scope`], and wait for it and every task it spawned.
///
/// [`spawn`](crate::spawn) hands a future to the executor, which may keep it
/// around for longer than the caller's stack frame, so the future has to be
/// `'static`. A scope instead owns its tasks and polls them itself, on the
/// task that awaits the scope. That lets the tasks borrow anything that
/// outlives the scope, and guarantees that none of them outlive it:
///
/// ```
/// # compio_runtime::Runtime::new().unwrap().block_on(async {
/// use std::cell::Cell;
///
/// let names = vec!["a".to_string(), "b".to_string()];
/// let total = Cell::new(0);
///
/// let first = compio_runtime::scope(async |s| {
///     for name in &names {
///         // Borrows `names` and `total`, neither of which is `'static`.
///         s.spawn(async { total.set(total.get() + name.len()) });
///     }
///     s.spawn(async { names[0].clone() }).await
/// })
/// .await;
///
/// // The scope only finishes once every task has.
/// assert_eq!(first, "a");
/// assert_eq!(total.get(), 2);
/// # });
/// ```
///
/// # Cancellation
///
/// Every scope has its own [`CancelToken`], which the body and every task are
/// polled with. A [`with_cancel`] inside the scope adds its token instead of
/// replacing the scope's. [`Scope::cancel`] cancels the IO operations they
/// have in flight: those complete with a cancellation error (see
/// [`ErrorExt::is_cancelled`]) and hand their buffers back. An operation
/// started after that completes with the same error right away, without being
/// submitted, so cleanup after a cancellation can't do IO. The tasks keep
/// running, and the scope still waits for them.
///
/// When the scope is polled with a cancel token, or inside another scope,
/// cancelling that token or scope cancels this scope too.
///
/// The token reaches operations through the waker they're polled with, so
/// some of them don't observe it:
///
/// - Timers and other futures that aren't compio operations. Race them against
///   [`Scope::cancel_token`]'s [`wait`](CancelToken::wait), or use
///   [`WithCancel::fail_fast`].
/// - Operations polled by a sub-executor with wakers of its own, like
///   `FuturesUnordered`, `join_all` of many futures, or the `compat` streams of
///   `compio-io`. Wrap the futures in `.with_cancel(s.cancel_token())` before
///   handing them over.
/// - Operations that run on the blocking thread pool, like those of
///   [`spawn_blocking`](crate::spawn_blocking): the scope waits for them.
/// - An operation that an IO object keeps between polls stays registered with
///   the tokens it was first submitted with.
///
/// Multishot streams, like `read_multi`, end without an error once the scope is
/// cancelled, unless they have an operation in flight.
///
/// Dropping the scope future drops the body and every task right away. That's
/// safe: an operation whose future is dropped is cancelled, and the driver
/// keeps its buffer until the kernel is done with it.
///
/// # Errors
///
/// [`try_scope`] is the fallible version. Tasks spawned with
/// [`Scope::try_spawn`] report their errors to the scope. The first error,
/// from a task or from the body, cancels the scope; once every task has
/// finished, the scope drops the body if it is still running, and returns that
/// error.
///
/// Awaiting the handle of a task that failed never completes. The scope drops
/// the body or the task that awaits it instead, as if it had used `?`, and a
/// handle of that task fails in turn.
///
/// Cancelling a `try_scope` makes the IO of its tasks fail with the
/// cancellation error, which fails the scope if a task returns it. To stop
/// gracefully, have the tasks treat [`ErrorExt::is_cancelled`] errors as
/// success.
///
/// # Panics
///
/// Panics if it isn't polled inside a compio [`Runtime`](crate::Runtime).
///
/// A panic in the body or in a task drops everything still running in the
/// scope, then resumes unwinding from the task that awaits the scope.
///
/// # Limitations
///
/// - All tasks run on the task that awaits the scope: a task that does not
///   yield stalls the rest of the scope, and the whole scope shows up as a
///   single task in [`console`](crate::console).
/// - Borrowing works for state, not for IO buffers. Operations still need owned
///   buffers, because an operation the future of which is dropped is only asked
///   to stop, and the kernel may keep writing to its buffer for a while.
///
/// # Soundness
///
/// Neither tasks nor their handles can outlive the scope:
///
/// ```compile_fail
/// # compio_runtime::Runtime::new().unwrap().block_on(async {
/// let handle = compio_runtime::scope(async |s| s.spawn(async {})).await;
/// # });
/// ```
///
/// Tasks can only borrow what outlives the scope, which the body's locals
/// don't:
///
/// ```compile_fail,E0373
/// # compio_runtime::Runtime::new().unwrap().block_on(async {
/// compio_runtime::scope(async |s| {
///     let local = 1;
///     s.spawn(async { local + 1 });
/// })
/// .await;
/// # });
/// ```
///
/// And the scope can't outlive what its tasks borrow:
///
/// ```compile_fail,E0597
/// # compio_runtime::Runtime::new().unwrap().block_on(async {
/// let scope;
/// {
///     let data = vec![1];
///     scope = compio_runtime::scope(async |s| {
///         s.spawn(async { data.len() });
///     });
/// }
/// scope.await;
/// # });
/// ```
///
/// Leaking the scope with [`mem::forget`](std::mem::forget) is harmless: its
/// tasks live inside its future, so they are never polled again.
///
/// [`with_cancel`]: crate::FutureExt::with_cancel
/// [`WithCancel::fail_fast`]: crate::WithCancel::fail_fast
/// [`ErrorExt::is_cancelled`]: crate::ErrorExt::is_cancelled
pub async fn scope<'env, F, R>(f: F) -> R
where
    F: for<'scope> AsyncFnOnce(&'scope Scope<'scope, 'env>) -> R,
{
    let scope = Scope::new();
    let _clear = ClearOnDrop(&scope.tasks);
    match Run::new(&scope, f(&scope).map(Ok::<R, Infallible>)).await {
        Ok(value) => value,
    }
}

/// Run `f` in a new [`Scope`] that fails as a whole when `f` or a task spawned
/// with [`Scope::try_spawn`] fails.
///
/// The first error cancels the scope, which then waits for its remaining
/// tasks, and returns that error. See [`scope`] for details.
///
/// # Panics
///
/// Panics if it isn't polled inside a compio [`Runtime`](crate::Runtime), and
/// propagates panics from `f` and the tasks it spawned.
pub async fn try_scope<'env, F, T, E>(f: F) -> Result<T, E>
where
    F: for<'scope> AsyncFnOnce(&'scope Scope<'scope, 'env, E>) -> Result<T, E>,
{
    let scope = Scope::new();
    let _clear = ClearOnDrop(&scope.tasks);
    Run::new(&scope, f(&scope)).await
}

/// A scope to spawn tasks that may borrow from their parent.
///
/// Created with [`scope`] or [`try_scope`]. `E` is the error type of a
/// [`try_scope`].
pub struct Scope<'scope, 'env: 'scope, E = Infallible> {
    tasks: Tasks,
    shared: Arc<Shared>,
    cancel: CancelToken,
    error: RefCell<Option<E>>,
    /// Set by the handle of a task that failed, so that whatever awaits it is
    /// dropped.
    doomed: Cell<bool>,
    // Both invariant, like `std::thread::Scope`.
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope, 'env, E> Scope<'scope, 'env, E> {
    fn new() -> Self {
        Self {
            tasks: Tasks::default(),
            shared: Arc::new(Shared {
                ready: Mutex::new(Vec::new()),
                waker: AtomicWaker::new(),
            }),
            cancel: CancelToken::new(),
            error: RefCell::new(None),
            doomed: Cell::new(false),
            scope: PhantomData,
            env: PhantomData,
        }
    }

    /// Spawn a task that runs concurrently with the rest of the scope.
    ///
    /// The scope does not finish until the task does. Unlike
    /// [`JoinHandle`](crate::JoinHandle), dropping the returned handle neither
    /// cancels nor detaches the task. In a [`try_scope`], errors the task
    /// returns are not reported to the scope: spawn fallible tasks with
    /// [`try_spawn`](Self::try_spawn).
    pub fn spawn<F>(&'scope self, future: F) -> ScopedJoinHandle<'scope, F::Output>
    where
        F: Future + 'scope,
        F::Output: 'scope,
    {
        let (output, handle) = self.output();
        self.insert(Box::pin(async move { output.set(future.await) }));
        handle
    }

    /// Spawn a fallible task that runs concurrently with the rest of the scope.
    ///
    /// If the task fails, the scope is cancelled, and the error is what the
    /// [`try_scope`] returns, unless another error came first. Whatever awaits
    /// the returned handle is then dropped, see [`scope`].
    pub fn try_spawn<F, T>(&'scope self, future: F) -> ScopedJoinHandle<'scope, T>
    where
        F: Future<Output = Result<T, E>> + 'scope,
        T: 'scope,
    {
        let (output, handle) = self.output();
        self.insert(Box::pin(async move {
            match future.await {
                Ok(value) => output.set(value),
                // Dropping `output` unset fails the handle.
                Err(error) => self.fail(error),
            }
        }));
        handle
    }

    /// Cancel the IO of the body and every task of this scope, and of the
    /// scopes nested in it, see [`scope`].
    ///
    /// The tasks themselves keep running, and the scope still waits for them.
    pub fn cancel(&self) {
        if !self.cancel.is_cancelled() {
            self.cancel.clone().cancel();
        }
    }

    /// Whether this scope has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The [`CancelToken`] of this scope.
    ///
    /// The body and every task of the scope are already polled with it. Use it
    /// to wait for the scope to be cancelled, e.g. to stop a timer.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    fn fail(&self, error: E) {
        self.error.borrow_mut().get_or_insert(error);
        self.cancel();
    }

    fn output<T>(&'scope self) -> (Output<T>, ScopedJoinHandle<'scope, T>) {
        let slot = Rc::new(RefCell::new(Slot::Pending(None)));
        let handle = ScopedJoinHandle {
            slot: slot.clone(),
            doomed: &self.doomed,
        };
        (Output(slot), handle)
    }

    fn insert(&'scope self, future: Pin<Box<dyn Future<Output = ()> + 'scope>>) {
        // SAFETY: Only the lifetime is erased. Tasks are only polled by `Run`,
        // which borrows the scope for `'scope`. They are dropped by `Run`, or,
        // on the paths that skip that (a panic, or a body that never got to
        // `Run`), by the `ClearOnDrop` that `scope` and `try_scope` declare
        // right after the scope. Both happen before the scope is dropped, while
        // what `'env` borrows is still alive, and both drop the tasks through a
        // shared reference, as they may use the scope while they're dropped.
        // If the future of the scope is leaked instead, the tasks are never
        // polled or dropped again, so nothing they borrow is used after it's
        // gone.
        let future = unsafe {
            mem::transmute::<
                Pin<Box<dyn Future<Output = ()> + 'scope>>,
                Pin<Box<dyn Future<Output = ()>>>,
            >(future)
        };
        let mut tasks = self.tasks.0.borrow_mut();
        let entry = tasks.vacant_entry();
        let flag = Arc::new(TaskWaker {
            shared: self.shared.clone(),
            index: entry.key(),
            queued: AtomicBool::new(false),
        });
        let waker = Waker::from(flag.clone());
        entry.insert(Task {
            future: Some(future),
            waker: waker.clone(),
            flag,
            epoch: 0,
        });
        drop(tasks);
        waker.wake();
    }
}

impl<E> fmt::Debug for Scope<'_, '_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("tasks", &self.tasks.0.borrow().len())
            .field("cancelled", &self.is_cancelled())
            .field("failed", &self.error.borrow().is_some())
            .finish_non_exhaustive()
    }
}

/// A handle to await a task spawned in a [`Scope`].
///
/// Unlike [`JoinHandle`](crate::JoinHandle), dropping it neither cancels nor
/// detaches the task.
pub struct ScopedJoinHandle<'scope, T> {
    slot: Rc<RefCell<Slot<T>>>,
    doomed: &'scope Cell<bool>,
}

impl<T> ScopedJoinHandle<'_, T> {
    /// Whether the task has finished, successfully or not.
    pub fn is_finished(&self) -> bool {
        !matches!(*self.slot.borrow(), Slot::Pending(_))
    }
}

impl<T> Future for ScopedJoinHandle<'_, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut slot = self.slot.borrow_mut();
        match &mut *slot {
            Slot::Pending(waker) => {
                let new = cx.get_waker();
                if !waker.as_ref().is_some_and(|old| old.will_wake(new)) {
                    *waker = Some(new.clone());
                }
                Poll::Pending
            }
            Slot::Ready(_) => match mem::replace(&mut *slot, Slot::Taken) {
                Slot::Ready(value) => Poll::Ready(value),
                _ => unreachable!(),
            },
            Slot::Failed => {
                self.doomed.set(true);
                Poll::Pending
            }
            Slot::Taken => panic!("`ScopedJoinHandle` polled after completion"),
        }
    }
}

impl<T> fmt::Debug for ScopedJoinHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedJoinHandle")
            .field("finished", &self.is_finished())
            .finish()
    }
}

enum Slot<T> {
    Pending(Option<Waker>),
    Ready(T),
    /// The task failed, or was dropped before it finished.
    Failed,
    Taken,
}

/// Where a task puts its output. Dropping it unset fails the handle.
struct Output<T>(Rc<RefCell<Slot<T>>>);

impl<T> Output<T> {
    fn set(&self, value: T) {
        self.resolve(Slot::Ready(value));
    }

    fn resolve(&self, new: Slot<T>) {
        let mut slot = self.0.borrow_mut();
        if let Slot::Pending(waker) = &mut *slot {
            let waker = waker.take();
            *slot = new;
            drop(slot);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl<T> Drop for Output<T> {
    fn drop(&mut self) {
        self.resolve(Slot::Failed);
    }
}

#[derive(Default)]
struct Tasks(RefCell<Slab<Task>>);

impl Tasks {
    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// Drop every task, including the ones spawned while they're dropped, and
    /// even if dropping some of them panics. Returns the first panic.
    fn clear(&self) -> Option<Payload> {
        let mut payload = None;
        loop {
            let tasks = mem::take(&mut *self.0.borrow_mut());
            if tasks.is_empty() {
                return payload;
            }
            for (_, task) in tasks {
                if let Err(p) = catch_unwind(AssertUnwindSafe(|| drop(task))) {
                    payload.get_or_insert(p);
                }
            }
        }
    }
}

/// Drops the tasks that are left when the scope future is dropped, before the
/// scope itself, and through a shared reference, as they may use the scope.
struct ClearOnDrop<'a>(&'a Tasks);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(payload) = self.0.clear()
            && !thread::panicking()
        {
            resume_unwind(payload);
        }
    }
}

struct Task {
    /// Taken out while it's polled, so that it can spawn.
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    waker: Waker,
    flag: Arc<TaskWaker>,
    /// The poll of `Run` that polled it last.
    epoch: u64,
}

/// The part of the scope its wakers need, which may be on other threads.
struct Shared {
    /// Tasks woken since they were last polled, by index, or [`BODY`].
    ready: Mutex<Vec<usize>>,
    /// The waker of the task that awaits the scope.
    waker: AtomicWaker,
}

impl Shared {
    fn ready(&self) -> MutexGuard<'_, Vec<usize>> {
        self.ready.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The index of the body in [`Shared::ready`].
const BODY: usize = usize::MAX;

/// How many tasks to poll at most before yielding to the executor.
const BUDGET: usize = 128;

struct TaskWaker {
    shared: Arc<Shared>,
    index: usize,
    /// Whether `index` is in `ready` already.
    queued: AtomicBool,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.queued.swap(true, Ordering::SeqCst) {
            self.shared.ready().push(self.index);
            self.shared.waker.wake();
        }
    }
}

type Payload = Box<dyn Any + Send>;

/// What became of a task that was woken.
enum Turn {
    Polled,
    /// It was polled during this poll of `Run` already.
    Deferred,
    /// It's gone.
    Skipped,
}

/// Polls the body and the tasks of a scope.
struct Run<'scope, 'env, T, E, Fut> {
    scope: &'scope Scope<'scope, 'env, E>,
    body: Option<Pin<Box<Fut>>>,
    body_waker: Waker,
    body_flag: Arc<TaskWaker>,
    body_epoch: u64,
    output: Option<T>,
    /// Listeners of the tokens the scope is polled with, as in `Ext::tokens`.
    outer: [Option<(CancelToken, EventListener)>; 2],
    /// Counts the polls, so that each task is polled at most once per poll.
    epoch: u64,
    /// Reused to drain `Shared::ready` into.
    batch: Vec<usize>,
    /// Tasks woken again during a poll, left for the next one.
    deferred: Vec<usize>,
}

// `output` is never pinned, and `body` is boxed.
impl<T, E, Fut> Unpin for Run<'_, '_, T, E, Fut> {}

impl<'scope, 'env, T, E, Fut> Run<'scope, 'env, T, E, Fut>
where
    Fut: Future<Output = Result<T, E>>,
{
    fn new(scope: &'scope Scope<'scope, 'env, E>, body: Fut) -> Self {
        let body_flag = Arc::new(TaskWaker {
            shared: scope.shared.clone(),
            index: BODY,
            queued: AtomicBool::new(false),
        });
        let body_waker = Waker::from(body_flag.clone());
        body_waker.wake_by_ref();
        Self {
            scope,
            body: Some(Box::pin(body)),
            body_waker,
            body_flag,
            body_epoch: 0,
            output: None,
            outer: [None, None],
            epoch: 0,
            batch: Vec::new(),
            deferred: Vec::new(),
        }
    }

    /// Cancel the scope when a token it's polled with is cancelled.
    fn link_outer(&mut self, cx: &Context<'_>) {
        let scope = self.scope;
        let tokens = get_ext(cx.waker()).map_or([None, None], Ext::tokens);
        for (outer, token) in self.outer.iter_mut().zip(tokens) {
            let Some(token) = token.filter(|t| **t != scope.cancel && !scope.is_cancelled()) else {
                *outer = None;
                continue;
            };
            if token.is_cancelled() {
                *outer = None;
                scope.cancel();
                continue;
            }
            if !matches!(outer, Some((t, _)) if t == token) {
                *outer = Some((token.clone(), token.listen()));
            }
            if let Some((_, listener)) = outer {
                let mut cx = Context::from_waker(cx.get_waker());
                if listener.poll_unpin(&mut cx).is_ready() {
                    *outer = None;
                    scope.cancel();
                }
            }
        }
    }

    /// Poll the body and the tasks that were woken, each at most once, until
    /// none are left or the budget is spent.
    fn run_ready(&mut self, ext: &Ext<'_>) -> Result<(), Payload> {
        let mut polled = 0;
        while polled < BUDGET {
            let mut batch = mem::take(&mut self.batch);
            mem::swap(&mut *self.scope.shared.ready(), &mut batch);
            let before = polled;
            for &index in &batch {
                let turn = if polled >= BUDGET {
                    Turn::Deferred
                } else if index == BODY {
                    self.poll_body(ext)?
                } else {
                    self.poll_task(index, ext)?
                };
                match turn {
                    Turn::Polled => polled += 1,
                    Turn::Deferred => self.deferred.push(index),
                    Turn::Skipped => {}
                }
            }
            batch.clear();
            self.batch = batch;
            if polled == before {
                break;
            }
        }
        if !self.deferred.is_empty() {
            let mut ready = self.scope.shared.ready();
            ready.splice(0..0, self.deferred.drain(..));
        }
        Ok(())
    }

    fn poll_body(&mut self, ext: &Ext<'_>) -> Result<Turn, Payload> {
        let Some(body) = &mut self.body else {
            return Ok(Turn::Skipped);
        };
        if self.body_epoch == self.epoch {
            return Ok(Turn::Deferred);
        }
        self.body_epoch = self.epoch;
        self.body_flag.queued.store(false, Ordering::SeqCst);
        self.scope.doomed.set(false);
        let waker = &self.body_waker;
        let poll = catch_unwind(AssertUnwindSafe(|| {
            ExtWaker::transient(waker, ext).poll(body.as_mut())
        }))?;
        match poll {
            Poll::Ready(Ok(value)) => {
                self.body = None;
                self.output = Some(value);
            }
            Poll::Ready(Err(error)) => {
                self.body = None;
                self.scope.fail(error);
            }
            // It awaits a task that failed.
            Poll::Pending if self.scope.doomed.get() => self.body = None,
            Poll::Pending => {}
        }
        Ok(Turn::Polled)
    }

    fn poll_task(&mut self, index: usize, ext: &Ext<'_>) -> Result<Turn, Payload> {
        let tasks = &self.scope.tasks.0;
        let (mut future, waker) = {
            let mut tasks = tasks.borrow_mut();
            // Spurious wakeups are fine: the task may have finished, and its
            // slot may even hold another task by now.
            let Some(task) = tasks.get_mut(index) else {
                return Ok(Turn::Skipped);
            };
            if task.epoch == self.epoch {
                return Ok(Turn::Deferred);
            }
            let Some(future) = task.future.take() else {
                return Ok(Turn::Skipped);
            };
            task.epoch = self.epoch;
            task.flag.queued.store(false, Ordering::SeqCst);
            (future, task.waker.clone())
        };
        self.scope.doomed.set(false);
        let poll = catch_unwind(AssertUnwindSafe(|| {
            ExtWaker::transient(&waker, ext).poll(future.as_mut())
        }));
        // Finished, or awaits a task that failed.
        let done = match poll {
            Ok(Poll::Ready(())) => true,
            Ok(Poll::Pending) => self.scope.doomed.get(),
            Err(_) => false,
        };
        if done {
            let task = tasks.borrow_mut().try_remove(index);
            // Dropped without the borrow, as they may spawn.
            drop(task);
            drop(future);
        } else if let Some(task) = tasks.borrow_mut().get_mut(index) {
            // A task that panicked is put back to be dropped with the rest.
            task.future = Some(future);
        }
        poll.map(|_| Turn::Polled)
    }
}

impl<T, E, Fut> Future for Run<'_, '_, T, E, Fut>
where
    Fut: Future<Output = Result<T, E>>,
{
    type Output = Result<T, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let scope = this.scope;
        this.epoch += 1;
        // Tasks woken from here on are either polled below, or found in `ready`
        // once the waker is registered again, so their wakes needn't schedule
        // the parent.
        scope.shared.waker.take();
        this.link_outer(cx);

        let result = with_ext(cx.waker(), |_, ext| {
            let ext = ext.in_scope(&scope.cancel);
            this.run_ready(&ext)
        });
        if let Err(payload) = result {
            // Keep the panic that got us here: the hook reported any later one.
            let _ = catch_unwind(AssertUnwindSafe(|| this.body = None));
            let _ = scope.tasks.clear();
            resume_unwind(payload);
        }

        if scope.tasks.is_empty() {
            if scope.error.borrow().is_some() {
                // Every task has finished, so a body that's still running waits
                // for something that won't come. Dropped without the borrow, as
                // it may spawn.
                let body = this.body.take();
                drop(body);
                if scope.tasks.is_empty() {
                    let error = scope.error.take().expect("the error is set");
                    return Poll::Ready(Err(error));
                }
            } else if this.body.is_none() {
                let value = this.output.take();
                return Poll::Ready(Ok(
                    value.expect("the body of a scope is only dropped when the scope fails")
                ));
            }
        }

        scope.shared.waker.register(cx.get_waker());
        if !scope.shared.ready().is_empty() {
            // Left for the next poll, or woken since.
            cx.get_waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl<T, E, Fut> Drop for Run<'_, '_, T, E, Fut> {
    fn drop(&mut self) {
        self.body = None;
        let payload = self.scope.tasks.clear();
        self.scope.shared.ready().clear();
        if let Some(payload) = payload
            && !thread::panicking()
        {
            resume_unwind(payload);
        }
    }
}
