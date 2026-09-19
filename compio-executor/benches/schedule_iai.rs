//! Instruction-count benchmarks for the executor scheduling hot paths.
//!
//! These measure the same paths as `schedule.rs`, but count instructions,
//! cache accesses and estimated cycles under Valgrind instead of timing them.
//! That makes them reproducible on a shared or noisy machine — a CI runner —
//! where the criterion numbers move by double-digit percentages between runs of
//! identical code.
//!
//! Running them needs Valgrind and the runner binary, at the exact version of
//! the `iai-callgrind` dependency:
//!
//! ```sh
//! cargo install iai-callgrind-runner --version 0.16.1 --locked
//! cargo bench -p compio-executor --bench schedule_iai
//! ```
//!
//! A regression shows up as a changed instruction count against the previous
//! run, which `iai-callgrind` stores next to the results and prints a diff of.
//! Note that the counts are not comparable to the wall-clock numbers: Valgrind
//! serialises everything, so anything whose cost is contention or a syscall
//! (the cross-thread wake path, most of `compio-driver`) is *understated* here.
//! Use these for the single-threaded paths, and criterion for the rest.

use std::{
    future::Future,
    hint::black_box,
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};

use compio_executor::Executor;
use iai_callgrind::{library_benchmark, library_benchmark_group, main};

/// A future that re-wakes itself `n` times before completing.
///
/// Each self-wake schedules the task again through `Local::schedule`, so
/// driving it to completion performs exactly `n` local schedules.
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

fn drive_local<F: Future + 'static>(exe: &Executor, fut: F) {
    let handle = exe.spawn(fut);
    let mut handle = pin!(handle);
    let cx = &mut Context::from_waker(Waker::noop());
    while handle.as_mut().poll(cx).is_pending() {
        exe.tick();
    }
}

/// Build the executor outside the measured region, so the counts are of the
/// scheduling and not of the one-off allocation of the queue.
///
/// `iai-callgrind` hands the `args` of a case to the `setup` and the `setup`'s
/// return to the benchmark, so the iteration count is passed through.
fn executor(n: usize) -> (Executor, usize) {
    (Executor::new(), n)
}

// Cost of `n` local wakes: `Local::schedule`, the empty drain of the sync
// queue it piggybacks, and the poll each one leads to.
#[library_benchmark]
#[bench::n1(setup = executor, args = (1))]
#[bench::n100(setup = executor, args = (100))]
#[bench::n1000(setup = executor, args = (1_000))]
fn local_wake((exe, n): (Executor, usize)) {
    drive_local(
        &exe,
        SelfWake {
            remaining: black_box(n),
        },
    );
}

// Cost of spawning `n` tasks that finish on their first poll: the task
// allocation, the slot-map insert, the poll and the teardown, with no wake in
// between.
#[library_benchmark]
#[bench::n1(setup = executor, args = (1))]
#[bench::n100(setup = executor, args = (100))]
#[bench::n1000(setup = executor, args = (1_000))]
fn spawn_ready((exe, n): (Executor, usize)) {
    for _ in 0..n {
        exe.spawn(async { black_box(()) }).detach();
    }
    while exe.tick() {}
}

// Cost of a tick that finds nothing to do, which is what the runtime pays
// between every two pieces of real work.
#[library_benchmark]
#[bench::idle(setup = executor, args = (1_000))]
fn idle_tick((exe, n): (Executor, usize)) {
    for _ in 0..black_box(n) {
        black_box(exe.tick());
    }
}

library_benchmark_group!(
    name = schedule;
    benchmarks = local_wake, spawn_ready, idle_tick
);

main!(library_benchmark_groups = schedule);
