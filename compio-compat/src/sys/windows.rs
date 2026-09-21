use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    time::Duration,
};

use compio_driver::AsRawFd;
use compio_runtime::Runtime;
use windows_sys::Win32::{
    Foundation::{WAIT_FAILED, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects},
};

use crate::sys::Adapter;

struct WindowsAdapter {
    runtime: Runtime,
}

impl WindowsAdapter {
    fn new(runtime: Runtime) -> io::Result<Self> {
        Ok(Self { runtime })
    }

    async fn wait(&self, timeout: Option<Duration>) -> io::Result<()> {
        let (sender, receiver) = futures_channel::oneshot::channel::<io::Result<()>>();
        // SAFETY: FFI call to `CreateEventW`. Null `lpEventAttributes` and
        // `lpName` are documented as valid (default security, unnamed event);
        // the two `0` arguments are plain `BOOL`s. The call has no safety
        // precondition beyond passing well-formed arguments, and its result is
        // null-checked below.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `OwnedHandle::from_raw_handle` requires a valid handle that
        // this call takes sole ownership of. `event` was just returned by
        // `CreateEventW` and checked non-null, and it is not closed or wrapped
        // anywhere else, so ownership is unique.
        let event_handle = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };

        let timeout = match timeout {
            Some(timeout) => timeout.as_millis() as u32,
            None => INFINITE,
        };

        struct EventGuard(OwnedHandle);

        impl Drop for EventGuard {
            fn drop(&mut self) {
                // SAFETY: FFI call to `SetEvent`. `self.0` is a live
                // `OwnedHandle` to the event created above, so the handle is
                // valid for the duration of this call.
                unsafe { SetEvent(self.0.as_raw_handle()) };
            }
        }

        let _event_handle = EventGuard(event_handle);
        let event = event as usize;
        let driver = self.runtime.as_raw_fd() as usize;
        windows_threading::submit(move || {
            let handles = [event as RawHandle, driver as RawHandle];
            // SAFETY: FFI call to `WaitForMultipleObjects`. `handles` is a
            // live array of exactly 2 handles, matching the count argument,
            // and both outlive the call: the event is kept alive by the
            // `EventGuard` and the driver handle by the `Runtime`.
            let res = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, timeout) };
            let res = match res {
                WAIT_FAILED => Err(io::Error::last_os_error()),
                WAIT_TIMEOUT => Err(io::ErrorKind::TimedOut.into()),
                _ => Ok(()),
            };
            sender.send(res).ok();
        });
        receiver
            .await
            .map_err(|_| io::ErrorKind::Interrupted.into())
            .flatten()
    }
}

macro_rules! impl_adapter {
    ($(#[$($attr:meta)*])? $name:ident) => {
        $(#[$($attr)*])?
        pub struct $name(WindowsAdapter);

        impl Adapter for $name {
            fn new(runtime: Runtime) -> io::Result<Self> {
                WindowsAdapter::new(runtime).map(Self)
            }

            async fn wait(&self, timeout: Option<Duration>) -> io::Result<()> {
                self.0.wait(timeout).await
            }

            fn clear(&self) -> io::Result<()> {
                Ok(())
            }
        }

        impl std::ops::Deref for $name {
            type Target = Runtime;

            fn deref(&self) -> &Self::Target {
                &self.0.runtime
            }
        }
    };
}

#[cfg(feature = "tokio")]
impl_adapter! {
    /// Adapter for `tokio` runtime.
    TokioAdapter
}

#[cfg(feature = "futures")]
impl_adapter! {
    /// Adapter for general runtime.
    FuturesAdapter
}
