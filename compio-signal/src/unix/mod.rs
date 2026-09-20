//! Unix-specific types for signal handling.

use std::io;

use signal_hook_registry::{SigId, register, unregister};
use synchrony::sync::async_flag::{AsyncFlag as Event, AsyncFlagHandle as EventHandle};

/// A listener to unix signal event.
#[derive(Debug)]
struct SignalListener {
    id: SigId,
    event: Option<Event>,
}

impl SignalListener {
    fn new(sig: i32) -> io::Result<Self> {
        let event = Event::new();
        let handle: EventHandle = event.handle();

        // SAFETY: the action runs inside a signal handler, so it must be
        // async-signal-safe. `AsyncFlagHandle::notify` only performs atomic
        // stores and a lock-free waker hand-off; it allocates nothing and takes
        // no locks. This is the same constraint the previous hand-written
        // handler operated under.
        let id = unsafe { register(sig, move || handle.clone().notify()) }?;

        Ok(Self {
            id,
            event: Some(event),
        })
    }

    async fn wait(mut self) {
        self.event
            .take()
            .expect("event could not be None")
            .wait()
            .await
    }
}

impl Drop for SignalListener {
    fn drop(&mut self) {
        unregister(self.id);
    }
}

/// Creates a new listener which will receive notifications when the current
/// process receives the specified signal.
pub async fn signal(sig: i32) -> io::Result<()> {
    let fd = SignalListener::new(sig)?;
    fd.wait().await;
    Ok(())
}
