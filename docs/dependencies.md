# Can well-known crates replace what compio hand-rolls?

A survey of the hand-written machinery in the workspace against the crates that
already solve the same problem. One candidate turned out to be worth replacing;
the rest are hand-rolled for reasons that hold up.

Compio already leans on the obvious ones — `slotmap`, `slab`, `crossbeam-queue`,
`socket2`, `bytemuck`, `io-uring`, `polling`. This is about what is left.

## Replace: the vendored signal registry

`compio-signal/src/unix/half_lock.rs` was a copy of `signal-hook-registry`'s
private internals, pinned at commit `7c8c5199` and since drifted locally, and
`unix/mod.rs` re-implemented the registry on top of it.

`signal-hook-registry` is the crate that code was taken from. Depending on it
removes 316 lines and takes `compio-signal` from 8 `unsafe` blocks to 1 — the
`register` call itself, which is unsafe because the action runs in a signal
handler.

It carries a **behaviour change**, which is why it is a prototype and not a
proposal: the hand-rolled registry restored `SIG_DFL` once the last listener for
a signal went away; `signal-hook-registry` leaves its `sigaction` handler
installed for the life of the process and `unregister` only drops the action.
So after a single `ctrl_c().await`, a later SIGINT is caught and discarded
rather than terminating the process. Whether that trade is acceptable is a
maintainer call.

Prototype and a test pinning the behaviour change: branch
`signal-hook-registry-prototype`.

## Keep: the intrusive task queue

`compio-executor/src/queue.rs` threads a doubly-linked list through a
`SlotMap`, giving O(1) removal of a task from the middle of the run queue by
`TaskId`.

The general-purpose intrusive list crates (`intrusive-collections` and friends)
want to own the node, which here lives inside the slot-map entry the executor
already has to look up. Adopting one costs a second indirection on the hot path
and buys no `unsafe` reduction, because the pointers being linked are still raw.
The list is ~300 lines and entirely internal.

Measurements for this path, including the variants that were tried and
rejected, are in `perf.md` and `queue-design.md` on the `perf` branch.

## Keep: the task allocation and waker

`compio-executor/src/task/` is an intrusively refcounted allocation with a
hand-written `RawWakerVTable`, ~900 lines.

`async-task` is the well-known replacement and would remove most of that
`unsafe`. It does not fit as-is: compio's `Task` carries executor-specific state
(`state.rs`, the hot/cold placement, the console metadata) in the same
allocation, and `async-task`'s `Task`/`Runnable` split does not have a place to
put it without a second allocation per spawn. Allocation count is already
exactly 1.00 per spawn and roughly a third of `spawn_ready`'s cost, so adding
one is the wrong direction.

Worth revisiting if `async-task` ever grows a user-data slot.

## Rejected: `zerocopy`

`zerocopy` cannot be applied to the libc types compio needs it for. Its traits
are **sealed to the derive macro**: a hand-written `unsafe impl` on even a
`#[repr(transparent)]` newtype fails with

```
error[E0046]: not all trait items implemented, missing:
             `only_derive_is_allowed_to_implement_this_trait`
```

and the derive itself fails because the inner foreign type does not implement
the traits either. There is no escape hatch for a foreign type. `bytemuck`
permits exactly this and is already a dependency.

The three cases that establish it are archived at
<https://claude.ai/artifact/GnTpXa1LkxHcTmzfCynmrg> (private) — a crate whose
purpose is to *not* compile has nowhere to live in a workspace that must build.

## Not a dependency question

`compio-driver` holds 633 of the workspace's `unsafe` occurrences. Almost all of
it is syscall surface — `io_uring`, IOCP and `polling` submission — where the
`unsafe` is the FFI boundary itself and no crate can take it away. The buffer
traits are the other concentration, and their problem is a contract one, not a
missing dependency: see [`soundness.md`](./soundness.md).
