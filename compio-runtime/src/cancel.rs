use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fmt::Debug,
    mem,
    ops::DerefMut,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use compio_driver::{Cancel, Key, OpCode, Proactor};
use futures_util::{FutureExt, ready};
use synchrony::unsync::event::{Event, EventListener};

use crate::{ContextExt, Runtime};

/// The size of [`Inner::tokens`] past which it is first pruned.
const MIN_PRUNE_AT: usize = 64;

struct Inner {
    tokens: RefCell<HashSet<Cancel>>,
    /// Prune `tokens` of dropped operations when it grows past this, so that a
    /// long-lived token doesn't keep every operation it has seen.
    prune_at: Cell<usize>,
    is_cancelled: Cell<bool>,
    driver: Rc<RefCell<Proactor>>,
    notify: Event,
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
            prune_at: Cell::new(MIN_PRUNE_AT),
            is_cancelled: Cell::new(false),
            driver: Runtime::with_current(|r| r.driver.clone()),
            notify: Event::new(),
        }))
    }

    pub(crate) fn listen(&self) -> EventListener {
        self.0.notify.listen()
    }

    /// Cancel all operations registered with this token.
    pub fn cancel(self) {
        self.0.notify.notify_all();
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
            let mut tokens = self.0.tokens.borrow_mut();
            if tokens.len() >= self.0.prune_at.get() {
                tokens.retain(|t| !t.is_dropped());
                self.0.prune_at.set((tokens.len() * 2).max(MIN_PRUNE_AT));
            }
            tokens.insert(token);
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
    listen: EventListener,
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

#[cfg(test)]
mod tests {
    use compio_buf::BufResult;
    use compio_driver::op::Asyncify;

    use super::*;
    use crate::FutureExt;

    #[test]
    fn prunes_dropped_operations() {
        Runtime::new().unwrap().block_on(async {
            let token = CancelToken::new();
            for _ in 0..1000 {
                crate::submit(Asyncify::new(|| BufResult(Ok(0), ())))
                    .with_cancel(token.clone())
                    .await
                    .0
                    .unwrap();
            }
            let len = token.0.tokens.borrow().len();
            assert!(len <= MIN_PRUNE_AT, "{len} operations kept");
        })
    }
}
