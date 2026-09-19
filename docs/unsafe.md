# Unsafe code in compio

This document answers a question that comes up periodically: *can compio get rid
of `unsafe`?*

The short answer is **no**, and the reason is structural rather than a matter of
effort. What follows is the evidence for that, an account of the small amount of
`unsafe` that *was* incidental (and has since been removed), and what can be done
about the rest.

## Inventory

Counted by textual occurrence over the whole workspace:

| construct      | count |
| -------------- | ----: |
| `unsafe { … }` |   614 |
| `unsafe fn`    |   344 |
| `unsafe impl`  |   174 |
| `unsafe trait` |     3 |

By crate (`unsafe { … }` blocks):

| crate               | blocks |
| ------------------- | -----: |
| `compio-driver`     |    255 |
| `compio-io`         |     73 |
| `compio-executor`   |     70 |
| `compio-runtime`    |     61 |
| `compio-buf`        |     40 |
| `compio-net`        |     35 |
| `compio-quic`       |     17 |
| `compio-fs`         |     19 |
| `compio-process`    |     13 |
| `compio-term`       |     12 |
| `compio-signal`     |      8 |
| `compio-compat`     |      6 |
| `compio`            |      3 |
| `compio-tls`        |      1 |
| `compio-dispatcher` |      1 |
| `compio-actor`      |      0 |
| `compio-log`        |      0 |
| `compio-macros`     |      0 |
| `compio-ws`         |      0 |

The distribution is the point: 42% of all `unsafe` blocks live in
`compio-driver` alone, another 33% in the three crates directly above it
(`compio-io`, `compio-executor`, `compio-runtime`), and the four crates with
none of it at all are the ones furthest from the kernel.

## The four irreducible categories

### 1. FFI and syscalls

Every `libc::`, `windows_sys::Win32::`, `io_uring::` and `polling::` call is
`unsafe` by definition. This is most of `compio-driver/src/sys/` and all of
`compio-signal`, `compio-term` and `compio-process`'s platform code.

There is no way to remove this, only to move it. Wrapping it in a dependency
relocates the `unsafe` into somebody else's crate without reducing the amount of
code that has to be right.

### 2. The completion-I/O ownership contract — `unsafe trait OpCode`

This is the largest single category and the most interesting one. Of
`compio-driver`'s 219 `unsafe fn`, **143 are the three `OpCode` methods**
(`init`: 65, `set_result`: 43, `operate`: 35), implemented once per operation per
backend (`poll`, `iour`, `iocp`).

`OpCode` is declared `unsafe trait` in all three backends with this contract:

> Caller must guarantee that during the lifetime of `ctrl`, `Self` is unmoved and
> valid.

That requirement is what separates a *completion*-based runtime from a
*readiness*-based one. With epoll, the kernel tells you a fd is ready and you do
the read yourself — your buffer is only borrowed for the duration of a normal
function call, so `&mut [u8]` expresses it fine. With io_uring and IOCP, you hand
the kernel a raw pointer to your buffer and your operation struct and the kernel
keeps it *across the await point* — and, critically, keeps it even if the future
is dropped before the operation completes.

Rust has no type that expresses "the kernel still holds a pointer to this, so you
may not free it yet". `Pin` gets part of the way (it forbids moving) but says
nothing about dropping. This is precisely the difficulty that makes io_uring hard
to wrap safely in Rust, and it is why compio's I/O traits take buffers *by value*
and give them back through `BufResult` instead of borrowing them.

Eliminating this category would mean abandoning completion-based I/O, which is
the reason the project exists.

### 3. The buffer-initialization contract — `SetLen` / `IoBufMut`

`compio-buf` has 25 `unsafe fn set_len` implementations plus the `advance`,
`advance_to` and `map_advanced` family layered on top. `SetLen::set_len` is
`unsafe` for exactly the reason `Vec::set_len` is: the caller asserts that bytes
in `[buf_len(), len)` were actually initialized, usually by the kernel, and the
compiler cannot check that.

`IoBufMut::as_uninit` is the other half. It hands out `&mut [MaybeUninit<u8>]`
covering a buffer's whole capacity. There is no safe stable equivalent —
`Vec::spare_capacity_mut` returns only the uninitialized tail, not the full
allocation, and the `MaybeUninit` slice APIs that would help
(`slice::write_copy_of_slice`, `MaybeUninit::as_bytes_mut` and friends) are still
unstable. A few of these blocks will become removable as those stabilize; the
`set_len` contract itself will not.

### 4. Intrusive data structures in the executor

`compio-executor` builds tasks as a hand-rolled, intrusively reference-counted
`TaskAlloc<F>` driven by a manual `RawWakerVTable` (`src/waker.rs`).

The safe alternative is `std::task::Wake`, which requires the waker to be an
`Arc<T>`. The codebase already uses it wherever the cost is acceptable —
`compio-io`'s `WakerArray` is a `Wake` impl, and only its borrowed sibling
`WakerArrayRef` needs a vtable. Converting the task path would add an allocation
and an indirection per spawned task, which is the wrong trade for the hot path of
a runtime. The same reasoning covers `SharedFd`, `compio-signal`'s `half_lock`
and `SendWrapper`.

## What *was* incidental

A sweep of the workspace found seven `unsafe` blocks and two `unsafe impl`s that
carried no weight. All are now gone, with no change in behaviour and no runtime
cost:

| site                                   | was                                                  | now                                                 |
| -------------------------------------- | ---------------------------------------------------- | --------------------------------------------------- |
| `compio-executor/src/lib.rs`            | `NonNull::new_unchecked(Box::into_raw(…))`           | `NonNull::from(Box::leak(…))`                       |
| `compio-executor/src/task/mod.rs`       | `NonNull::new_unchecked(Box::into_raw(…))`           | `NonNull::from(Box::leak(…)).cast()`                |
| `compio-driver/src/buffer_pool.rs`      | `NonNull::new_unchecked(Box::into_raw(…))`           | `NonNull::from(Box::leak(…)).cast()`                |
| `compio-io/src/read/ext.rs` (×2)        | `ArrayVec::into_inner_unchecked`                     | `into_inner().expect(…)` — one length check         |
| `compio-io/src/read/ext.rs`             | `String::from_utf8_unchecked` on an emptied `Vec`    | `String::from_utf8(…).expect(…)` — O(1) on 0 bytes  |
| `compio-net/src/incoming/unix.rs`       | `hint::unreachable_unchecked()` in a cold `let`-else | `Option::expect` on a cold path                     |
| `compio-driver/src/lib.rs` (×2 impls)   | unconditional `unsafe impl Send/Sync`                | `#[cfg(windows)]` + a `Send + Sync` static assertion |

The last one is worth calling out. `ProactorBuilder`'s manual `Send`/`Sync` impls
are only needed on Windows, where `RawFd` is `HANDLE` (a raw pointer). On Unix
`RawFd` is `i32` and the struct derives both automatically, so the manual impls
were suppressing auto-trait inference for no reason — meaning a future field that
genuinely isn't thread safe would have been silently papered over instead of
rejected. Gating them to Windows and adding a `const` assertion keeps the public
guarantee while letting the compiler check it on Unix.

Four crates carry no `unsafe` at all and now say so with `#![forbid(unsafe_code)]`
(`compio-actor`, `compio-log`, `compio-macros`, `compio-ws`), so the property is
enforced rather than merely true today.

That is roughly 1% of the total. The ratio is the finding: the remaining 99% is
load-bearing.

## Making the rest harder to get wrong

Since the volume of `unsafe` is fixed, the useful work is in review surface and
tooling.

**`// SAFETY:` coverage is the biggest gap.** Only 111 of 614 `unsafe` blocks
(18%) have a safety comment within six lines. Per crate:

| crate             | documented |
| ----------------- | ---------: |
| `compio-fs`       |   9/19 47% |
| `compio-buf`      |  16/40 40% |
| `compio-quic`     |   5/17 29% |
| `compio-executor` |  14/70 20% |
| `compio-runtime`  |  12/61 19% |
| `compio-net`      |   6/35 17% |
| `compio-io`       |  11/73 15% |
| `compio-driver`   | 37/255 14% |
| `compio-process`  |   0/13  0% |
| `compio-term`     |   0/12  0% |
| `compio-signal`   |    0/8  0% |
| `compio-compat`   |    0/6  0% |

Enabling `clippy::undocumented_unsafe_blocks` at `warn` and burning it down one
crate at a time costs nothing at runtime and is the thing most likely to catch
contract drift in code like `OpCode::init`. `clippy::multiple_unsafe_ops_per_block`
is a reasonable companion — several blocks in `compio-driver` combine a
`ptr::read`, a `ptr::write` and a syscall under one `unsafe`.

**Miri coverage is narrower than it could be.** CI already runs Miri, but only
against `compio-executor` (`.github/workflows/ci_test_executor.yml`), alongside a
loom profile for the same crate. `compio-buf` is the obvious crate to add next:
its 40 `unsafe` blocks are pure-Rust pointer and `MaybeUninit` work with no
syscalls, which is exactly what Miri checks well, and it sits underneath every
other crate in the workspace. It already passes as-is —
`cargo miri test -p compio-buf --features arrayvec,bytes` is green — so adding it
to the existing Miri job is a one-line change, not a cleanup project. The driver itself can't run under Miri for the
usual reason — it is nothing but syscalls — which is what the ASan profile in
`.config/nextest.toml` is there for.

**Already in good shape:** edition 2024 makes `unsafe_op_in_unsafe_fn` an error,
so every `unsafe fn` body already has explicit inner `unsafe` blocks rather than
the implicit-unsafe-body behaviour of older editions.

## Summary

`unsafe` in compio is not accidental complexity that better discipline would
remove. It is the cost of a completion-based I/O runtime: the kernel holds raw
pointers into user memory across suspension points, and no stable Rust
abstraction expresses that lifetime. The incidental remainder has been removed;
the rest is best addressed by documenting and testing it, not by trying to
delete it.
