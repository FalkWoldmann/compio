//! Windows-specific types for signal handling.

#[cfg(feature = "once_cell_try")]
use std::sync::OnceLock;
use std::{
    collections::HashMap,
    io,
    sync::{Arc, LazyLock, Mutex},
};

use asyncband::event::ManualResetEvent;
#[cfg(not(feature = "once_cell_try"))]
use once_cell::sync::OnceCell as OnceLock;
use slab::Slab;
use windows_sys::{
    Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    },
    core::BOOL,
};

static HANDLER: LazyLock<Mutex<HashMap<u32, Slab<Arc<ManualResetEvent>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

unsafe extern "system" fn ctrl_event_handler(ctrltype: u32) -> BOOL {
    let mut handler = HANDLER.lock().unwrap();
    if let Some(handlers) = handler.get_mut(&ctrltype)
        && !handlers.is_empty()
    {
        let handlers = std::mem::replace(handlers, Slab::new());
        for (_, handler) in handlers {
            handler.set();
        }
        return 1;
    }
    0
}

static INIT: OnceLock<()> = OnceLock::new();

fn init() -> io::Result<()> {
    if unsafe { SetConsoleCtrlHandler(Some(ctrl_event_handler), 1) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn register(ctrltype: u32, e: &Arc<ManualResetEvent>) -> usize {
    let mut handler = HANDLER.lock().unwrap();
    let handle = e.clone();
    handler.entry(ctrltype).or_default().insert(handle)
}

fn unregister(ctrltype: u32, key: usize) {
    let mut handler = HANDLER.lock().unwrap();
    if let Some(handlers) = handler.get_mut(&ctrltype)
        && handlers.contains(key)
    {
        let _ = handlers.remove(key);
    }
}

/// Represents a listener to console CTRL event.
#[derive(Debug)]
struct CtrlEvent {
    ctrltype: u32,
    event: Arc<ManualResetEvent>,
    handler_key: usize,
}

impl CtrlEvent {
    pub(crate) fn new(ctrltype: u32) -> io::Result<Self> {
        INIT.get_or_try_init(init)?;

        let event = Arc::new(ManualResetEvent::new());
        let handler_key = register(ctrltype, &event);
        Ok(Self {
            ctrltype,
            event,
            handler_key,
        })
    }

    pub async fn wait(self) {
        self.event.wait().await
    }
}

impl Drop for CtrlEvent {
    fn drop(&mut self) {
        unregister(self.ctrltype, self.handler_key);
    }
}

async fn ctrl_event(ctrltype: u32) -> io::Result<()> {
    let event = CtrlEvent::new(ctrltype)?;
    event.wait().await;
    Ok(())
}

/// Creates a new listener which receives "ctrl-break" notifications sent to the
/// process.
pub async fn ctrl_break() -> io::Result<()> {
    ctrl_event(CTRL_BREAK_EVENT).await
}

/// Creates a new listener which receives "ctrl-close" notifications sent to the
/// process.
pub async fn ctrl_close() -> io::Result<()> {
    ctrl_event(CTRL_CLOSE_EVENT).await
}

/// Creates a new listener which receives "ctrl-c" notifications sent to the
/// process.
pub async fn ctrl_c() -> io::Result<()> {
    ctrl_event(CTRL_C_EVENT).await
}

/// Creates a new listener which receives "ctrl-logoff" notifications sent to
/// the process.
pub async fn ctrl_logoff() -> io::Result<()> {
    ctrl_event(CTRL_LOGOFF_EVENT).await
}

/// Creates a new listener which receives "ctrl-shutdown" notifications sent to
/// the process.
pub async fn ctrl_shutdown() -> io::Result<()> {
    ctrl_event(CTRL_SHUTDOWN_EVENT).await
}
