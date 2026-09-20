//! Documents the one behaviour change in the `signal-hook-registry` swap.
//!
//! The previous hand-written registry restored `SIG_DFL` once the last listener
//! for a signal went away, so a signal that arrived afterwards took its default
//! action again. `signal-hook-registry` leaves its `sigaction` handler
//! installed for the lifetime of the process; `unregister` only removes the
//! action from the list it calls.
//!
//! The practical consequence: after awaiting `ctrl_c()` once, a later SIGINT is
//! caught and discarded instead of terminating the process. This test pins that
//! down so the change is a decision rather than a surprise.
#![cfg(unix)]

use std::process::{Command, exit};

/// Child mode: register for SIGUSR1, drop the registration, then raise it.
///
/// SIGUSR1 terminates by default, so the child dies iff `SIG_DFL` was restored.
fn child() -> ! {
    let id = unsafe { signal_hook_registry::register(libc::SIGUSR1, || {}) }
        .expect("register should succeed");
    assert!(signal_hook_registry::unregister(id));

    unsafe { libc::raise(libc::SIGUSR1) };

    // Reached only if the handler is still installed and swallowed the signal.
    exit(42);
}

#[test]
fn unregister_does_not_restore_sig_dfl() {
    if std::env::var_os("COMPIO_SIGNAL_CHILD").is_some() {
        child();
    }

    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new(exe)
        .arg("--exact")
        .arg("unregister_does_not_restore_sig_dfl")
        .env("COMPIO_SIGNAL_CHILD", "1")
        .output()
        .expect("spawn child");

    // 42 means the child survived the signal: the handler was still installed.
    // A kill-by-signal exit would mean `SIG_DFL` had been restored.
    assert_eq!(
        out.status.code(),
        Some(42),
        "expected the child to survive SIGUSR1 after unregister, meaning SIG_DFL is NOT restored; \
         got {:?}",
        out.status
    );
}
