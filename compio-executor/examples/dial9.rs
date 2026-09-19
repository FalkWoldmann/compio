//! Record a compio executor workload into a [`dial9`] trace.
//!
//! ```sh
//! cargo run -p compio-executor --features dial9 --example dial9 -- /tmp/compio-trace.bin
//! ```
//!
//! The trace it writes lands at `<path stem>.0.bin`, and is read by the dial9
//! viewer:
//!
//! ```sh
//! cargo install dial9
//! dial9 view /tmp/compio-trace.0.bin
//! ```
//!
//! [`dial9`]: https://github.com/dial9-rs/dial9

use std::{
    future::Future,
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};

use compio_executor::Executor;
use dial9_core::{buffer::DiskBuffer, recorder::recorder};

/// A future that re-wakes itself `n` times before completing, so that the trace
/// has a task with a recognisable poll/wake pattern in it.
struct SelfWake {
    remaining: usize,
}

impl Future for SelfWake {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.remaining == 0 {
            Poll::Ready(())
        } else {
            self.remaining -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/compio-trace.bin".to_owned());

    // The executor resolves the handle once per thread, so publish it before
    // anything is spawned.
    let rec = recorder(DiskBuffer::single_file(&path)?).build();
    rec.install_global_handle()?;
    println!("recording to {path} (segments are written as <stem>.N.bin)");

    let exe = Executor::new();

    for _ in 0..64 {
        exe.spawn(async { std::hint::black_box(()) }).detach();
    }

    let handle = exe.spawn(SelfWake { remaining: 1_000 });
    let mut handle = pin!(handle);
    let cx = &mut Context::from_waker(Waker::noop());
    while handle.as_mut().poll(cx).is_pending() {
        exe.tick();
    }
    while exe.tick() {}

    drop(exe);
    rec.graceful_shutdown(std::time::Duration::from_secs(5));
    println!("done");

    Ok(())
}
