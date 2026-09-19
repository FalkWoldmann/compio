//! [`dial9`] flight-recorder instrumentation.
//!
//! [`dial9`] records runtime events — task spawns, polls and wakes — into a
//! compact self-describing binary trace that is analysed after the fact, rather
//! than watched live the way [`tokio-console`](crate::console) is. Its
//! instrumentation crate, `dial9-tokio-telemetry`, drives that from tokio's
//! runtime hooks, which compio has none of; but the machinery underneath is not
//! tokio's:
//!
//! * `dial9-core` is the event bus. It owns its own flush thread and its own
//!   worker thread (which builds a current-thread tokio runtime *of its own*
//!   for the segment pipeline), so an application that never touches tokio can
//!   record through it.
//! * `dial9-trace-format` is the wire format, and it is self-describing: a
//!   segment carries a schema naming every event type and field, and the viewer
//!   dispatches on those names. The `wire_slot` an event claims is a
//!   per-process fast-path id handed out by an atomic counter at startup, not a
//!   constant of the format.
//!
//! Together those two mean the events below — named and shaped exactly like the
//! ones `dial9-tokio-telemetry` emits — decode in the dial9 viewer as runtime
//! events rather than as unrecognised custom ones, without compio depending on
//! the tokio integration at all.
//!
//! Enable the `dial9` feature to make this executor emit them:
//!
//! * a `TaskSpawnEvent` when a task is spawned, and a `TaskTerminateEvent` when
//!   the executor drops it;
//! * a `PollStartEvent`/`PollEndEvent` pair around every poll, which is what
//!   the viewer builds poll durations and the per-task timeline from;
//! * a `WakeEventEvent` for every wake, attributed to the task that was being
//!   polled when it happened, which is what makes self-wakes and cross-task
//!   wake chains visible.
//!
//! Without the feature all of this compiles to nothing, exactly like
//! [`console`](crate::console): the guard becomes zero-sized and every hook an
//! empty inlined function.
//!
//! # Usage
//!
//! Build a recorder and publish its handle process-wide before starting the
//! runtime. The executor resolves `Dial9Handle::current` once per thread, so a
//! handle installed after a thread has already recorded its first event is not
//! picked up by that thread.
//!
//! ```ignore
//! use dial9_core::{buffer::DiskBuffer, recorder::recorder};
//!
//! let rec = recorder(DiskBuffer::single_file("/tmp/compio-trace.bin")?).build();
//! rec.install_global_handle()?;
//!
//! compio::runtime::Runtime::new()?.block_on(async {
//!     // ...
//! });
//!
//! rec.graceful_shutdown(std::time::Duration::from_secs(5));
//! ```
//!
//! # Status and limitations
//!
//! This is a proof of concept. What it does not do yet:
//!
//! * **No park/unpark events.** dial9 reports the time a worker spends parked
//!   in the kernel as `WorkerParkEvent`/`WorkerUnparkEvent`, which is where a
//!   completion-based runtime's most interesting time goes. Emitting those
//!   means instrumenting `compio-driver`'s wait, not the executor, so they are
//!   left for the follow-up.
//! * **`local_queue` is always reported as 0.** The hot queue is an intrusive
//!   list with no maintained length, so a true depth would be an O(n) walk per
//!   poll.
//! * **`PollStartEvent::spawn_loc` is the empty string.** The viewer takes the
//!   spawn location from `TaskSpawnEvent` when there is one and only falls back
//!   to the poll event's copy, so nothing is lost for a task whose spawn was
//!   recorded.
//! * **A task id is the executor's slot key**, which is unique per executor but
//!   not across the executors of a thread-per-core application. The viewer's
//!   per-task views can therefore merge two tasks on two threads.
//! * **The event schema is reproduced, not imported.** Depending on
//!   `dial9-tokio-telemetry` (with its `unstable-events` feature, which makes
//!   its event structs constructible from outside) would tie the two together
//!   at compile time instead, at the cost of pulling tokio's multi-threaded
//!   runtime into the build. A rename upstream silently turns these events into
//!   unrecognised custom ones.
//!
//! [`dial9`]: https://github.com/dial9-rs/dial9

cfg_select! {
    feature = "dial9" => {
        mod enabled;
        use enabled as imp;
    }
    _ => {
        mod disabled;
        use disabled as imp;
    }
}

pub(crate) use imp::{poll_start, task_spawn, task_terminate, wake};

/// Assertions that the two variants present the same surface.
///
/// Only one of them is ever compiled, and the one compiled by default is the
/// one nearly every build uses, so a drift between them only shows up for
/// whoever turns the feature on. Coercing each hook to a function pointer pins
/// its whole signature.
#[cfg(test)]
mod parity {
    use std::panic::Location;

    use super::{imp::PollGuard, *};
    use crate::queue::TaskId;

    const _: fn(TaskId, Option<&'static Location<'static>>) = task_spawn;
    const _: fn(TaskId) = task_terminate;
    const _: fn(TaskId) -> PollGuard = poll_start;
    const _: fn(TaskId) = wake;
}
