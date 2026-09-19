//! No-op stand-ins used when the `dial9` feature is disabled.

use std::panic::Location;

use crate::queue::TaskId;

/// The guard returned by [`poll_start`], which ends the poll when dropped.
///
/// The enabled variant reports the poll as lasting exactly as long as its guard
/// does, so dropping it right away is a bug that this makes visible in both
/// configurations.
#[must_use = "the poll is reported as ended as soon as this is dropped"]
pub(crate) struct PollGuard;

#[inline(always)]
pub(crate) fn task_spawn(_id: TaskId, _loc: Option<&'static Location<'static>>) {}

#[inline(always)]
pub(crate) fn task_terminate(_id: TaskId) {}

#[inline(always)]
pub(crate) fn poll_start(_id: TaskId) -> PollGuard {
    PollGuard
}

#[inline(always)]
pub(crate) fn wake(_woken: TaskId) {}
