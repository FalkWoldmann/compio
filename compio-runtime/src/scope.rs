//! Structured concurrency, see [`scope`].

use std::{
    any::Any,
    cell::RefCell,
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
};

use futures_util::{FutureExt as _, task::AtomicWaker};
use slab::Slab;
use synchrony::unsync::event::EventListener;

use crate::{
    CancelToken, ContextExt,
    future::Ext,
    waker::{ExtWaker, with_ext},
};

/// Run `f` in a new [`Scope`], and wait for it and every task it spawned.
///
/// [`spawn`](crate::spawn) hands a future to the executor, which may keep it
/// around for longer than the caller's stack frame, so the future has to be
/// `'static`. A scope instead owns its children and polls them itself, on
/// the task that awaits the scope. That lets children borrow anything that
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
/// // The scope only finishes once every child has.
/// assert_eq!(first, "a");
/// assert_eq!(total.get(), 2);
/// # });
/// ```
///
/// # Cancellation
///
/// Every scope has its own [`CancelToken`], which the body and every child are
/// polled with, as if they were wrapped in [`with_cancel`]. [`Scope::cancel`]
/// cancels the IO operations they have in flight. Those operations complete
/// with a cancellation error and hand their buffers back, so the children get
/// to clean up before they finish, and the scope still waits for them.
///
/// When the scope itself is polled with a cancel token, cancelling that token
/// cancels the scope too, and through it any scope nested inside.
///
/// Timers and other futures that aren't compio operations don't observe the
/// token. To stop those too, wait on [`Scope::cancel_token`] alongside them,
/// for example with [`WithCancel::fail_fast`].
///
/// Dropping the scope future drops the body and every child right away.
/// That's safe: an operation whose future is dropped is cancelled, and the
/// driver keeps its buffer until the kernel is done with it.
///
/// # Errors
///
/// [`try_scope`] is the fallible version. Children spawned with
/// [`Scope::try_spawn`] report their errors to the scope. The first error,
/// from a child or from the body, cancels the scope; once every child has
/// finished, the scope drops the body if it is still running and returns that
/// error. Awaiting the handle of a child that failed never completes, so a
/// body that does so is unwound by the scope, as if it had used `?`.
///
/// # Panics
///
/// Panics if it isn't polled inside a compio [`Runtime`](crate::Runtime).
///
/// A panic in the body or in a child drops everything still running in the
/// scope, then resumes unwinding from the task that awaits the scope.
///
/// # Limitations
///
/// - All children share one task: a child that does not yield stalls the rest
///   of the scope, and the whole scope shows up as a single task in
///   [`console`](crate::console).
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
pub async fn scope<'env, F, R>(f: F) -> R
where
    F: for<'scope> AsyncFnOnce(&'scope Scope<'scope, 'env>) -> R,
{
    let scope = Scope::new();
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
    Run::new(&scope, f(&scope)).await
}

/// A scope to spawn tasks that may borrow from their parent.
///
/// Created with [`scope`] or [`try_scope`]. `E` is the error type of a
/// [`try_scope`].
pub struct Scope<'scope, 'env: 'scope, E = Infallible> {
    // First, so that it's dropped while the other fields are still around, in
    // case a task touches them while it's dropped.
    tasks: Tasks,
    shared: Arc<Shared>,
    cancel: CancelToken,
    error: RefCell<Option<E>>,
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
            scope: PhantomData,
            env: PhantomData,
        }
    }

    /// Spawn a task that runs concurrently with the rest of the scope.
    ///
    /// The scope does not finish until the task does. Dropping the returned
    /// handle does not cancel the task.
    pub fn spawn<F>(&'scope self, future: F) -> ScopedJoinHandle<'scope, F::Output>
    where
        F: Future + 'scope,
        F::Output: 'scope,
    {
        let slot = Rc::new(RefCell::new(Slot::Pending(None)));
        let handle = ScopedJoinHandle::new(slot.clone());
        self.insert(Box::pin(async move { Slot::set(&slot, future.await) }));
        handle
    }

    /// Spawn a fallible task that runs concurrently with the rest of the scope.
    ///
    /// If the task fails, the scope is cancelled, and the error is what the
    /// [`try_scope`] returns, unless another error came first. The returned
    /// handle then never completes.
    pub fn try_spawn<F, T>(&'scope self, future: F) -> ScopedJoinHandle<'scope, T>
    where
        F: Future<Output = Result<T, E>> + 'scope,
        T: 'scope,
    {
        let slot = Rc::new(RefCell::new(Slot::Pending(None)));
        let handle = ScopedJoinHandle::new(slot.clone());
        self.insert(Box::pin(async move {
            match future.await {
                Ok(value) => Slot::set(&slot, value),
                Err(error) => self.fail(error),
            }
        }));
        handle
    }

    /// Cancel the IO operations in flight in this scope, and in the scopes
    /// nested in it.
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

    fn insert(&'scope self, future: Pin<Box<dyn Future<Output = ()> + 'scope>>) {
        // SAFETY: Only the lifetime is erased. Tasks are only polled by `Run`,
        // which borrows the scope for `'scope`, and `Run` drops every task when
        // it's dropped itself, while it still borrows the scope. Only the body
        // and the tasks can spawn, so none are left for the scope to drop. And
        // the future of the scope, which holds `Run`, can't outlive `'env`. If
        // that future is leaked instead, the tasks are never polled or dropped
        // again, so nothing they borrow is used after it's gone.
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
/// Dropping it does not cancel the task.
pub struct ScopedJoinHandle<'scope, T> {
    slot: Rc<RefCell<Slot<T>>>,
    _scope: PhantomData<&'scope ()>,
}

impl<T> ScopedJoinHandle<'_, T> {
    fn new(slot: Rc<RefCell<Slot<T>>>) -> Self {
        Self {
            slot,
            _scope: PhantomData,
        }
    }

    /// Whether the task has finished.
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
    Taken,
}

impl<T> Slot<T> {
    fn set(this: &RefCell<Self>, value: T) {
        if let Slot::Pending(Some(waker)) =
            mem::replace(&mut *this.borrow_mut(), Slot::Ready(value))
        {
            waker.wake();
        }
    }
}

#[derive(Default)]
struct Tasks(RefCell<Slab<Task>>);

impl Tasks {
    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    /// Drop every task, including the ones spawned while they're dropped.
    fn clear(&self) {
        loop {
            let tasks = mem::take(&mut *self.0.borrow_mut());
            if tasks.is_empty() {
                break;
            }
            drop(tasks);
        }
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        self.clear();
    }
}

struct Task {
    /// Taken out while it's polled, so that it can spawn.
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    waker: Waker,
    flag: Arc<TaskWaker>,
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

/// How many tasks to poll before yielding to the executor.
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

/// Polls the body and the tasks of a scope.
struct Run<'scope, 'env, T, E, Fut> {
    scope: &'scope Scope<'scope, 'env, E>,
    body: Option<Pin<Box<Fut>>>,
    body_waker: Waker,
    body_flag: Arc<TaskWaker>,
    output: Option<T>,
    /// The cancel token the scope was last polled with, if any.
    outer: Option<(CancelToken, EventListener)>,
    /// Reused to drain `Shared::ready` into.
    batch: Vec<usize>,
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
            output: None,
            outer: None,
            batch: Vec::new(),
        }
    }

    /// Cancel the scope when the token it's polled with is cancelled.
    fn link_outer(&mut self, cx: &mut Context<'_>) {
        let outer = match cx.get_cancel() {
            Some(outer) if !self.scope.is_cancelled() && *outer != self.scope.cancel => {
                outer.clone()
            }
            _ => {
                self.outer = None;
                return;
            }
        };
        if outer.is_cancelled() {
            self.outer = None;
            self.scope.cancel();
            return;
        }
        if !matches!(&self.outer, Some((token, _)) if *token == outer) {
            self.outer = Some((outer.clone(), outer.listen()));
        }
        if let Some((_, listener)) = &mut self.outer {
            let mut cx = Context::from_waker(cx.get_waker());
            if listener.poll_unpin(&mut cx).is_ready() {
                self.outer = None;
                self.scope.cancel();
            }
        }
    }

    /// Poll the body and the tasks that were woken, until none are left or the
    /// budget is spent.
    fn run_ready(&mut self, ext: &Ext<'_>) -> Result<(), Payload> {
        let mut polled = 0;
        loop {
            let mut batch = mem::take(&mut self.batch);
            mem::swap(&mut *self.scope.shared.ready(), &mut batch);
            if batch.is_empty() {
                self.batch = batch;
                return Ok(());
            }
            for &index in &batch {
                if index == BODY {
                    self.poll_body(ext)?;
                } else {
                    self.poll_task(index, ext)?;
                }
            }
            polled += batch.len();
            batch.clear();
            self.batch = batch;
            if polled >= BUDGET {
                // Let the other tasks of the executor run before the rest.
                if !self.scope.shared.ready().is_empty() {
                    self.scope.shared.waker.wake();
                }
                return Ok(());
            }
        }
    }

    fn poll_body(&mut self, ext: &Ext<'_>) -> Result<(), Payload> {
        let Some(body) = &mut self.body else {
            return Ok(());
        };
        self.body_flag.queued.store(false, Ordering::SeqCst);
        let waker = &self.body_waker;
        let poll = catch_unwind(AssertUnwindSafe(|| {
            ExtWaker::new(waker, ext).poll(body.as_mut())
        }))?;
        if let Poll::Ready(result) = poll {
            self.body = None;
            match result {
                Ok(value) => self.output = Some(value),
                Err(error) => self.scope.fail(error),
            }
        }
        Ok(())
    }

    fn poll_task(&mut self, index: usize, ext: &Ext<'_>) -> Result<(), Payload> {
        let tasks = &self.scope.tasks.0;
        let (mut future, waker) = {
            let mut tasks = tasks.borrow_mut();
            // Spurious wakeups are fine: the task may have finished, and its
            // slot may even hold another task by now.
            let Some(task) = tasks.get_mut(index) else {
                return Ok(());
            };
            let Some(future) = task.future.take() else {
                return Ok(());
            };
            task.flag.queued.store(false, Ordering::SeqCst);
            (future, task.waker.clone())
        };
        let poll = catch_unwind(AssertUnwindSafe(|| {
            ExtWaker::new(&waker, ext).poll(future.as_mut())
        }))?;
        match poll {
            Poll::Pending => {
                if let Some(task) = tasks.borrow_mut().get_mut(index) {
                    task.future = Some(future);
                }
            }
            Poll::Ready(()) => {
                let task = tasks.borrow_mut().try_remove(index);
                // Dropped without the borrow, as they may spawn.
                drop(task);
                drop(future);
            }
        }
        Ok(())
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
        scope.shared.waker.register(cx.get_waker());
        this.link_outer(cx);

        let result = with_ext(cx.waker(), |_, ext| {
            let ext = ext.with_cancel(&scope.cancel);
            this.run_ready(&ext)
        });
        if let Err(payload) = result {
            this.body = None;
            scope.tasks.clear();
            resume_unwind(payload);
        }

        if !scope.tasks.is_empty() {
            return Poll::Pending;
        }
        if let Some(error) = scope.error.borrow_mut().take() {
            // Every task has finished, so a body that's still running is most
            // likely waiting for one that failed.
            this.body = None;
            return Poll::Ready(Err(error));
        }
        match this.output.take() {
            Some(value) => Poll::Ready(Ok(value)),
            None => Poll::Pending,
        }
    }
}

impl<T, E, Fut> Drop for Run<'_, '_, T, E, Fut> {
    fn drop(&mut self) {
        self.body = None;
        self.scope.tasks.clear();
        self.scope.shared.ready().clear();
    }
}
