use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fmt::Debug,
    mem,
    ops::DerefMut,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

use asyncband::event::ManualResetEvent;
use compio_driver::{Cancel, Key, OpCode, Proactor};
use futures_util::{FutureExt, ready};

use crate::{ContextExt, Runtime};

struct Inner {
    tokens: RefCell<HashSet<Cancel>>,
    is_cancelled: Cell<bool>,
    driver: Rc<RefCell<Proactor>>,
    notify: Arc<ManualResetEvent>,
}

impl Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("tokens", &self.tokens)
            .field("is_cancelled", &self.is_cancelled)
            .field("driver", &"...")
            .field("notify", &self.notify)
            .finish()
    }
}

/// A token that can be used to cancel multiple operations at once.
///
/// When [`CancelToken::cancel`] is called, all operations that have been
/// registered with this token will be cancelled.
///
/// It is also possible to use [`CancelToken::wait`] to wait until the token is
/// cancelled, which can be useful for implementing timeouts or other
/// cancellation-based logic.
///
/// To associate a future with this cancel token, use the [`with_cancel`]
/// combinator from the [`FutureExt`] trait.
///
/// [`with_cancel`]: crate::future::FutureExt::with_cancel
/// [`FutureExt`]: crate::future::FutureExt
#[derive(Clone, Debug)]
pub struct CancelToken(Rc<Inner>);

impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancelToken {}

impl CancelToken {
    /// Create a new cancel token.
    ///
    /// # Panics
    ///
    /// [`CancelToken`] can only be created within compio runtime environment.
    /// This will panic without a runtime.
    pub fn new() -> Self {
        Self(Rc::new(Inner {
            tokens: RefCell::new(HashSet::new()),
            is_cancelled: Cell::new(false),
            driver: Runtime::with_current(|r| r.driver.clone()),
            notify: Arc::default(),
        }))
    }

    pub(crate) fn listen(&self) -> CancelListener {
        CancelListener {
            notify: self.0.notify.clone(),
            wait: None,
        }
    }

    /// Cancel all operations registered with this token.
    pub fn cancel(self) {
        self.0.notify.set();
        if self.0.is_cancelled.replace(true) {
            return;
        }
        let tokens = mem::take(self.0.tokens.borrow_mut().deref_mut());
        for t in tokens {
            self.0.driver.borrow_mut().cancel_token(t);
        }
    }

    /// Check if this token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled.get()
    }

    /// Register an operation with this token.
    ///
    /// If the token has already been cancelled, the operation will be cancelled
    /// immediately. Usually this method should not be used directly, but rather
    /// through the [`with_cancel`] combinator.
    ///
    /// Multiple registrations of the same key does nothing, and the key will
    /// only be cancelled once.
    ///
    /// [`with_cancel`]: crate::FutureExt::with_cancel
    pub fn register<T: OpCode>(&self, key: &Key<T>) {
        if self.0.is_cancelled.get() {
            self.0.driver.borrow_mut().cancel(key.clone());
        } else {
            let token = self.0.driver.borrow_mut().register_cancel(key);
            self.0.tokens.borrow_mut().insert(token);
        }
    }

    /// Wait until this token is cancelled.
    pub fn wait(self) -> WaitFuture {
        WaitFuture::new(self)
    }

    /// Try to get the current cancel token associated with the future.
    ///
    /// This is done by checking if the current context has a cancel token
    /// associated with it.
    pub async fn current() -> Option<Self> {
        std::future::poll_fn(|cx| Poll::Ready(cx.get_cancel().cloned())).await
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Future returned by [`CancelToken::wait`].
pub struct WaitFuture {
    listen: CancelListener,
    token: CancelToken,
}

impl WaitFuture {
    fn new(token: CancelToken) -> WaitFuture {
        WaitFuture {
            listen: token.listen(),
            token,
        }
    }
}

/// Resolves once the token is cancelled, and stays resolved when polled again.
pub(crate) struct CancelListener {
    notify: Arc<ManualResetEvent>,
    /// Only allocated once the listener has to wait.
    wait: Option<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Future for CancelListener {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        let this = &mut *self;
        if this.notify.is_set() {
            return Poll::Ready(());
        }
        let wait = this
            .wait
            .get_or_insert_with(|| Box::pin(this.notify.clone().wait_owned()));
        ready!(wait.as_mut().poll(cx));
        this.wait = None;
        Poll::Ready(())
    }
}

impl Future for WaitFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        loop {
            if self.token.is_cancelled() {
                return Poll::Ready(());
            } else {
                ready!(self.listen.poll_unpin(cx))
            }
        }
    }
}
