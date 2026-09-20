# Soundness: safe code can cause UB through the buffer traits

Two bugs, both reachable from safe code. Found by reviewing with
[google/rust-skills `unsafe_rust_review`][skill], whose Rule C states that
unsafe code must not rely on a caller-provided *safe* trait implementation
being semantically correct.

[skill]: https://github.com/google/rust-skills/tree/main/unsafe_rust_review

Affected: `compio-buf` (both), `compio-driver` (the first, on the `RecvMsg` /
`SendMsg` control path).

## Root cause

`IoBuf`, `IoBufMut` and `SetLen` are **safe, public traits** that users are
invited to implement, and unsafe code depends on their return values for memory
safety. `SetLen::set_len`'s own `# Safety` section is written in terms of
`as_uninit().len()` and `buf_len()` — so the theorem's premises are supplied by
the very safe methods that cannot be trusted.

## Bug 1 — `as_uninit` hands out initialized memory as `MaybeUninit`

**Status: not fixed.** Structural; needs an API decision.

`IoBufMut::as_uninit` returns `&mut [MaybeUninit<u8>]` covering the buffer's
*whole* extent, including bytes that are already initialized. Safe code can
therefore write `MaybeUninit::uninit()` into memory that is still typed `u8`,
and reading it back is UB. No `unsafe`, and no incorrect implementation — this
is compio's own impl used as documented:

```rust
let mut arr = [1u8, 2, 3, 4];
arr.as_uninit()[0] = MaybeUninit::uninit();   // safe
let v = arr[0];                               // UB
```

```
error: Undefined Behavior: reading memory at alloc147[0x0..0x1], but memory is
       uninitialized at [0x0..0x1], and this operation requires initialized memory
```

Confirmed for `[u8; N]`, `&mut [u8]` and `Vec<u8>` (the initialized prefix). By
inspection the same applies to `BytesMut`, `ArrayVec`, `SmallVec` and `MmapMut`,
whose impls expose full capacity the same way.

### Options

1. **Make `as_uninit` an `unsafe fn`** — the caller promises not to
   de-initialize bytes below `buf_len()`. Smallest change; pushes the obligation
   to where it can be stated.
2. **Split the API** — return the initialized prefix as `&mut [u8]` and only the
   spare capacity as `&mut [MaybeUninit<u8>]`. Sound by construction, but a
   larger reshaping of the trait.
3. **`unsafe trait` with a documented invariant** — pairs with bug 2's fix.

Option 1 or 2 is needed; clamping cannot help here.

## Bug 2 — two safe methods are assumed to agree

**Status: consequences fixed; root cause remains.**

### 2a. `as_init` and `as_uninit` need not describe the same buffer

`IoBufMutExt::as_mut_slice` took its length from `buf_len()` (which comes from
`IoBuf::as_init`) and its pointer from `buf_mut_ptr()` (from
`IoBufMut::as_uninit`). An implementation where those disagree produced a
`&mut [u8]` running past the end of its allocation:

```
error: Undefined Behavior: constructing invalid value of type &mut [u8]:
       encountered a dangling reference (going beyond the bounds of its allocation)
   --> compio-buf/src/io_buf.rs:457
```

The reproducer contains no `unsafe` beyond the empty `set_len` body the trait
signature requires.

**Fixed** by clamping to the slice `as_uninit` actually returned, with a
`debug_assert!` so an inconsistent implementation is loud rather than silently
truncated. Regression tests cover both builds: the assertion in debug, the
clamp — which is the soundness property — in release.

### 2b. Successive `as_uninit` calls need not agree — reaches the kernel

`buf_mut_ptr()` and `buf_capacity()` each call `as_uninit()` *separately*, and
`RecvMsg::init_control` handed both to `recvmsg`:

```rust
ctrl.msg.msg_control    = self.control.buf_mut_ptr() as _;   // one as_uninit()
ctrl.msg.msg_controllen = self.control.buf_capacity() as _;  // another one
```

Nothing obliges two calls to return the same slice. An implementation
alternating between an 8-byte and a 4096-byte buffer yields a pointer into the
8-byte one with a length of 4096 — authorising the kernel to write 4096 bytes
into an 8-byte allocation. Demonstrated by reproducing the two assignments; the
syscall was deliberately not invoked.

**Fixed** for both the `RecvMsg` and `SendMsg` paths by taking the pointer and
length from a single call, which is what `managed/iour.rs` already did.

## Severity

Neither is remotely triggerable; both require the local program to call the API.
Bug 1 needs only ordinary correct usage, which makes it the more serious of the
two. Bug 2 needs a user implementation whose methods disagree — not adversarial,
but an ordinary mistake: a cached length that drifts, an `as_uninit` that
reallocates.

The rule being broken in both cases is that safe code must not be able to cause
undefined behaviour.

## Recommended fix

Make `IoBuf` and `IoBufMut` `unsafe trait`s stating the implementor's
obligations, and resolve bug 1 by option 1 or 2 above:

```rust
/// # Safety
///
/// Implementors must ensure that:
/// 1. `as_uninit` returns the same pointer and length on every call, until the
///    buffer is mutated through `&mut self`.
/// 2. `as_init().len() <= as_uninit().len()`.
/// 3. `as_init()` and `as_uninit()` address the same allocation, with
///    `as_init()` a prefix of it.
```

This is a breaking change for downstream implementors, which is the honest cost
of the guarantee. The fixes already applied remove the reachable consequences of
bug 2 without changing any public signature; bug 1 has no such mitigation.

## Reproducers

Not in this repository: they trigger UB deliberately, and CI runs Miri.
They are archived at
<https://claude.ai/artifact/GnTpXa1LkxHcTmzfCynmrg> (private) and reproduce with
`cargo miri run`.
