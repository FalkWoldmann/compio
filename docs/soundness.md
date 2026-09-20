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

**Status: fixed** by option 1 below — `as_uninit` is now an `unsafe fn`.

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

### The fix

Option 1 of the three considered: `IoBufMut::as_uninit` is an `unsafe fn` whose
contract is that the caller must not de-initialize any byte in `[0, buf_len())`.
Writing initialized values anywhere, and writing anything at or above
`buf_len()`, stays allowed. (The alternatives were splitting the API so the
initialized prefix comes back as `&mut [u8]`, and making the whole trait
`unsafe`; clamping cannot help here.)

Two sibling methods reached the same bytes and had to move with it:

- `IoVectoredBufMut::iter_uninit_slice` is the vectored analogue and is
  literally implemented as `.map(|buf| buf.as_uninit())`. Now `unsafe fn` with
  the same contract, stated per yielded slice.
- `IoBufMutExt::copy_within` was a *safe* method that could copy an
  uninitialized source range over the initialized prefix — a third route to the
  same UB, found while making the change. Now `unsafe fn`: either the source
  range is initialized, or the destination lies at or above `buf_len()`.

The derived accessors stay **safe**, because none of them can de-initialize:
`buf_capacity` only reads a length, `buf_mut_ptr` and `compio-driver`'s
`sys_slice_mut` hand out a raw pointer (writing through which is already
`unsafe`), and `as_mut_slice`, `ensure_init` and `IoBufMutExt::uninit` return
either `&mut [u8]`, which cannot express an uninitialized byte, or a view of the
spare capacity only. So the safe surface is unchanged apart from the three
methods above.

### Cost

Breaking for every implementor of `IoBufMut` and `IoVectoredBufMut`, and for
every caller of the three methods. In-tree that was 25 implementations and
about 30 call sites across `compio-buf`, `compio-driver`, `compio-io`,
`compio-quic` and the tests. Every call site kept its behaviour; three in
`compio-driver`'s tests were switched to the safe `buf_capacity` and
`buf_mut_ptr` instead of taking `unsafe` at all.

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

## Prior art: this was fixed once and regressed

Bug 1's root cause is not a new observation. It was raised as
[#220](https://github.com/compio-rs/compio/issues/220), *"IoBuf should be a
unsafe trait"*, in March 2024 — "the trait user can't guarantee ptr is valid,
but implementer know how to make sure the ptr is valid" — and **accepted and
fixed** two days later in `b7caef95`, *fix(buf): make IoBuf(Mut) unsafe*, which
added:

```rust
/// # Safety
///
/// The implementer should ensure the pointer, len and capacity are valid, so
/// that the returned slice of [`IoBuf::as_slice`] is valid.
pub unsafe trait IoBuf: 'static {
```

[#555](https://github.com/compio-rs/compio/pull/555), *refactor(buf): better
IoBuf* (merged December 2025, `65917da0`), removed it:

```diff
-pub unsafe trait IoBuf: 'static {
+pub trait IoBuf: 'static {
-pub unsafe trait IoBufMut: IoBuf + SetBufInit {
+pub trait IoBufMut: IoBuf + SetBufInit {
```

That PR did not intend to drop the guarantee. Its description says `IoBuf`
would instead require "a single `unsafe fn buffer(&self) -> IoBuffer`" — moving
the obligation from the trait to one unsafe method, which would have been
sound. **That method never landed.** `git log -S "unsafe fn buffer"` over
`compio-buf` on master returns nothing; what merged was a safe
`fn as_slice(&self) -> &[u8]`, later renamed `as_init`. So the refactor removed
the marker and shipped without the replacement.

The fix recommended below is therefore a restoration, not a new design.

### Related, already accepted as bugs

- [#581](https://github.com/compio-rs/compio/issues/581), *`map_advanced` is
  unsound*, closed as completed: "If user (either by accident or on purpose)
  passed in a not well-formed `BufResult` to `map_advanced`, uninitialized bytes
  will be marked as initialized, hence UB." Same rule, different site — the
  project has already treated a safe API trusting caller-supplied lengths as a
  soundness bug.
- [#1007](https://github.com/compio-rs/compio/issues/1007), *Stack overflow in
  `IoBufMut` impl for `memmap2::MmapMut`*, closed as completed: `as_uninit`,
  `as_mut_slice` and `buf_mut_ptr` were mutually recursive. That is the same
  trio bug 2a lives in; their interaction has already caused one bug.

Bug 2b — `msg_control` and `msg_controllen` taken from separate `as_uninit()`
calls — has no prior report.

### Why these survived

Upstream CI runs `cargo miri test` against `compio-executor` only. `compio-buf`
has no Miri coverage, which is what `ci: run miri over compio-buf` on this
branch adds.

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
bug 2 without changing any public signature. Bug 1 had no such mitigation and is
fixed by the signature change described above; what remains here is bug 2's root
cause, which needs the implementor obligations to become a trait-level
guarantee.

## Reproducers

Not in this repository: they trigger UB deliberately, and CI runs Miri.
They are archived at
<https://claude.ai/artifact/GnTpXa1LkxHcTmzfCynmrg> (private) and reproduce with
`cargo miri run`.

Bug 1's three reproducers — through `as_uninit`, through `iter_uninit_slice` and
through `copy_within` — no longer compile against this branch. Each now fails
with `E0133: call to unsafe function ... requires unsafe function or block`,
which is the fix working: the UB is still expressible, but only by a caller who
has written `unsafe` and taken on the contract.
