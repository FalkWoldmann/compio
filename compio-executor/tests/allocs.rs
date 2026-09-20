//! Counts heap allocations per spawn, to see what the 35% of `spawn_ready`
//! spent in malloc is actually made of.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
};

use compio_executor::Executor;

static COUNT: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static ON: AtomicBool = AtomicBool::new(false);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            COUNT.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(l.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

struct Ready;
impl Future for Ready {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}

#[test]
fn allocations_per_spawn() {
    let exe = Executor::new();
    // Warm up so we measure steady state, not queue growth.
    for _ in 0..64 {
        exe.spawn(Ready).detach();
    }
    while exe.tick() {}

    const N: usize = 1000;
    COUNT.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    for _ in 0..N {
        exe.spawn(Ready).detach();
    }
    ON.store(false, Ordering::Relaxed);

    let c = COUNT.load(Ordering::Relaxed);
    let b = BYTES.load(Ordering::Relaxed);
    println!(
        "spawn: {} allocs / {} tasks = {:.2} per spawn",
        c,
        N,
        c as f64 / N as f64
    );
    println!(
        "spawn: {} bytes / {} tasks = {:.1} per spawn",
        b,
        N,
        b as f64 / N as f64
    );

    while exe.tick() {}
}
