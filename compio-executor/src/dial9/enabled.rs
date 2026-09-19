//! The actual [`dial9`] instrumentation.
//!
//! [`dial9`]: https://github.com/dial9-rs/dial9

use std::{
    cell::{Cell, RefCell},
    panic::Location,
    sync::atomic::{AtomicU64, Ordering},
};

use dial9_core::{clock::clock_monotonic_ns, handle::Dial9Handle};
use dial9_trace_format::{InternedString, TraceEvent};
use slotmap::Key;

use crate::queue::TaskId;

/// Wire-format event for a task spawn.
///
/// The name and the field names are what the dial9 viewer dispatches on, so
/// they match `dial9_tokio_telemetry::telemetry::TaskSpawnEvent` exactly. So do
/// the field *types*, through their wire encoding: dial9's `TaskId` and
/// `WorkerId` are newtypes that encode as varints, which is what a `u64` does.
#[derive(TraceEvent)]
#[traceevent(wire_slot)]
struct TaskSpawnEvent {
    #[traceevent(timestamp)]
    timestamp_ns: u64,
    task_id: u64,
    spawn_loc: InternedString,
    instrumented: bool,
}

/// Wire-format event for a task being dropped by the executor.
#[derive(TraceEvent)]
#[traceevent(wire_slot)]
struct TaskTerminateEvent {
    #[traceevent(timestamp)]
    timestamp_ns: u64,
    task_id: u64,
}

/// Wire-format event for the start of a poll.
#[derive(TraceEvent)]
#[traceevent(wire_slot)]
struct PollStartEvent {
    #[traceevent(timestamp)]
    timestamp_ns: u64,
    worker_id: u64,
    local_queue: u8,
    task_id: u64,
    spawn_loc: InternedString,
}

/// Wire-format event for the end of a poll.
#[derive(TraceEvent)]
#[traceevent(wire_slot)]
struct PollEndEvent {
    #[traceevent(timestamp)]
    timestamp_ns: u64,
    worker_id: u64,
}

/// Wire-format event for a wake.
#[derive(TraceEvent)]
#[traceevent(wire_slot)]
struct WakeEventEvent {
    #[traceevent(timestamp)]
    timestamp_ns: u64,
    waker_task_id: u64,
    woken_task_id: u64,
    target_worker: u8,
}

/// Id the viewer displays for the executor a task belongs to.
///
/// dial9's model is one runtime with numbered workers, while compio is
/// thread-per-core with an executor per thread, so hand each thread that
/// records anything a number of its own and report it as the worker.
fn next_worker_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    NEXT.fetch_add(1, Ordering::Relaxed)
}

thread_local! {
    /// This thread's recording handle, resolved once.
    ///
    /// [`Dial9Handle::current`] reads a thread-local and then an `ArcSwap`, and
    /// clones an `Arc` out of whichever it finds; every hook below runs on the
    /// executor's hot path, so resolve it once and keep it.
    static HANDLE: RefCell<Option<Dial9Handle>> = const { RefCell::new(None) };

    /// This thread's worker number, allocated on first use.
    static WORKER: Cell<Option<u64>> = const { Cell::new(None) };

    /// The task currently being polled on this thread, which a wake happening
    /// now is attributed to. Zero — dial9's `UNKNOWN_TASK_ID` — outside a poll.
    static POLLING: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` with this thread's handle and worker id, unless nothing is
/// recording.
///
/// Every hook goes through this, so the cost of the feature in a build that has
/// it compiled in but no recorder installed is one thread-local read and one
/// `Option` test.
#[inline]
fn with_recorder<R>(f: impl FnOnce(&Dial9Handle, u64) -> R) -> Option<R> {
    HANDLE
        .try_with(|cell| {
            // Resolving borrows the cell, and `Dial9Handle::current` does not
            // reenter us, so the borrow cannot overlap with the one below.
            if cell.borrow().is_none() {
                let resolved = Dial9Handle::current();
                *cell.borrow_mut() = Some(resolved);
            }

            let handle = cell.borrow();
            let handle = handle.as_ref().expect("handle was just resolved");
            if !handle.is_enabled() {
                return None;
            }

            let worker = WORKER.with(|w| match w.get() {
                Some(id) => id,
                None => {
                    let id = next_worker_id();
                    w.set(Some(id));
                    id
                }
            });

            Some(f(handle, worker))
        })
        .ok()
        .flatten()
}

/// The executor's slot key, as the u64 the viewer keys a task on.
///
/// The key carries its slot's version, so a reused slot is a different id
/// rather than the same task coming back to life.
#[inline]
fn task_id(id: TaskId) -> u64 {
    id.data().as_ffi()
}

/// Report a task being spawned.
pub(crate) fn task_spawn(id: TaskId, loc: Option<&'static Location<'static>>) {
    with_recorder(|handle, _worker| {
        handle.with_encoder(|enc| {
            let spawn_loc = match loc {
                Some(loc) => enc.intern_location(loc),
                None => enc.intern_string(""),
            };
            enc.encode(&TaskSpawnEvent {
                timestamp_ns: clock_monotonic_ns(),
                task_id: task_id(id),
                spawn_loc,
                // dial9 uses this for "spawned through a dial9 spawner, so its
                // wakes are recorded too". Every compio task is.
                instrumented: true,
            });
        });
    });
}

/// Report a task being dropped by the executor.
pub(crate) fn task_terminate(id: TaskId) {
    with_recorder(|handle, _worker| {
        handle.with_encoder(|enc| {
            enc.encode(&TaskTerminateEvent {
                timestamp_ns: clock_monotonic_ns(),
                task_id: task_id(id),
            });
        });
    });
}

/// The guard returned by [`poll_start`], which ends the poll when dropped.
#[must_use = "the poll is reported as ended as soon as this is dropped"]
pub(crate) struct PollGuard {
    /// `None` when nothing was recording at the start of the poll, in which
    /// case the end is not recorded either — an unpaired `PollEndEvent` would
    /// close whichever poll the viewer saw last.
    worker: Option<u64>,
    /// What to restore [`POLLING`] to, so that a nested poll — a `block_on`
    /// inside a task — does not leave wakes attributed to the inner task after
    /// it has returned.
    outer: u64,
}

impl Drop for PollGuard {
    fn drop(&mut self) {
        POLLING.with(|c| c.set(self.outer));

        let Some(worker) = self.worker else {
            return;
        };

        with_recorder(|handle, _worker| {
            handle.with_encoder(|enc| {
                enc.encode(&PollEndEvent {
                    timestamp_ns: clock_monotonic_ns(),
                    worker_id: worker,
                });
            });
        });
    }
}

/// Report the start of a poll. The poll ends when the returned guard drops.
pub(crate) fn poll_start(id: TaskId) -> PollGuard {
    let id = task_id(id);
    let outer = POLLING.with(|c| c.replace(id));

    let worker = with_recorder(|handle, worker| {
        handle.with_encoder(|enc| {
            let spawn_loc = enc.intern_string("");
            enc.encode(&PollStartEvent {
                timestamp_ns: clock_monotonic_ns(),
                worker_id: worker,
                // The hot queue keeps no length; see the module docs.
                local_queue: 0,
                task_id: id,
                spawn_loc,
            });
        });
        worker
    });

    PollGuard { worker, outer }
}

/// Report a task being woken, attributed to whatever is being polled here.
pub(crate) fn wake(woken: TaskId) {
    let woken = task_id(woken);

    with_recorder(|handle, worker| {
        let waker = POLLING.with(|c| c.get());
        handle.with_encoder(|enc| {
            enc.encode(&WakeEventEvent {
                timestamp_ns: clock_monotonic_ns(),
                waker_task_id: waker,
                woken_task_id: woken,
                // dial9 caps this at a byte and reserves 255 for "unknown".
                target_worker: u8::try_from(worker).unwrap_or(255),
            });
        });
    });
}
