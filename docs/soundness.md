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
clamp — which is the soundness property — in release. The release half is only
actually reached because CI now runs the release profile too; until it did,
the test for the soundness property was compiled out everywhere and the fix
had no automated coverage at all.

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

### 2c. `reserve` and `extend_from_slice` trusted the same safe methods

Found by reviewing the fix for bug 1, in code the fix had just touched.

`IoBufMut::reserve`'s default implementation asked `self.buf_capacity() - init`,
where `init` is `buf_len()`. Those two come from `as_uninit` and `as_init`
respectively, which — this being a safe trait — need not agree. When
`buf_len()` exceeds `buf_capacity()` the subtraction wraps, `reserve` reports
capacity of about 1.8e19 bytes, and returns `Ok`.

`extend_from_slice` then trusted that `Ok`: it formed
`buf_mut_ptr().wrapping_add(init)` and `copy_nonoverlapping`'d into it. With
`buf_len()` at 1000 over an 8-byte allocation, that writes 992 bytes past the
end. Both methods are safe, so this was reachable with no `unsafe` written
anywhere by the caller. Miri: *"attempting to access 4 bytes, but got
alloc291+0x400 which is at or beyond the end of the allocation of size 32"*.

The SAFETY comment on that `copy_nonoverlapping` asserted the postcondition
"`reserve(len)` returned `Ok`, so the buffer has at least `init + len` bytes of
capacity" — which is exactly the safe-method trust this document declares
untrustworthy, written three functions below the clamp added for bug 2a. A
proof-shaped comment is not a proof; this one named its premise and the premise
was false.

**Fixed** twice over: `reserve` saturates instead of wrapping, so a
disagreement denies the reservation; and `extend_from_slice` no longer trusts
`reserve` at all, taking its destination from a single `as_uninit()` call and
bounding the write against that slice's real length. Regression tests for both
run under Miri in release.

### 2d. `AncillaryBuilder` re-derived a base pointer per push

The same defect as 2b, in a file the 2b fix did not touch.
`AncillaryBuilder::new` records a pointer and length from one `ensure_init()`
call into `CMsgIter`, and `push` then re-derived the base with
`buffer.buf_mut_ptr()` — a fresh `as_uninit()` call — on every message, while
offsetting by a cursor computed against the *first* call's pointer. A buffer
whose `as_uninit()` returns a different or shorter allocation on a later call
writes a `cmsghdr` at an offset from the wrong base. `AncillaryBuf::builder`
and `AncillaryBuilder::new` are public and safe, for any user `B: IoBufMut`.

**Fixed** by capturing the base once in `new` and using that stored pointer in
`push`, the same shape as the 2b fix.

### 2e. `BytesMut::as_uninit` built a slice from a pointer that did not cover it

Found by the contract test added with the `unsafe trait` change, which is the
first thing in the tree to call `BytesMut::as_uninit` under Miri.

```rust
let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
let cap = self.capacity();
unsafe { std::slice::from_raw_parts_mut(ptr, cap) }
```

`BytesMut` has no inherent `as_mut_ptr`; the call resolves through `DerefMut`,
which yields a `&mut [u8]` of length `len()`. The pointer therefore carries
provenance for `len()` bytes while the slice claims `capacity()`. Miri rejects
it whenever `cap > len` -- which, for a read buffer, is always:

> trying to retag from `<150290>` for Unique permission at `alloc50707[0x0]`,
> but that tag does not exist in the borrow stack for this location

The existing SAFETY comment asserted *"DEPENDENCY LEMMA: `BytesMut::as_mut_ptr`
is valid for `capacity()` bytes in one allocation"*. That lemma is false, and
naming it is what made the comment look discharged. This is the third
proof-shaped comment on this branch whose stated premise did not hold.

**Fixed** by deriving the pointer from `spare_capacity_mut`, which `bytes`
implements from its own owning pointer rather than through `Deref`. Shrinking
the length to zero first makes the spare region cover the whole allocation; the
original length is restored before returning.

The other in-tree implementations were checked the same way and are fine:
`Vec` and `ArrayVec` have inherent `as_mut_ptr`, and a non-spilled `SmallVec`
keeps its storage inside the struct, so `&mut self` already covers it. A
spilled `SmallVec` is heap-backed and is now covered by the test too.

## Severity

Neither is remotely triggerable; both require the local program to call the API.
Bug 1 needs only ordinary correct usage, which makes it the more serious of the
two. Bug 2 needs a user implementation whose methods disagree — not adversarial,
but an ordinary mistake: a cached length that drifts, an `as_uninit` that
reallocates.

The rule being broken in both cases is that safe code must not be able to cause
undefined behaviour. Both are now fixed: bug 1 by the `unsafe fn` signatures,
bug 2 by the trait obligations below.

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
has no Miri coverage, which the `TestBuf` workflow on this branch adds. That
workflow runs both profiles on purpose: the soundness properties here are the
ones that must hold once `debug_assert!` is compiled out, and the debug
assertions fire before the clamped path is reached, so a dev-profile run alone
never executes them. It is also a separate workflow rather than a step in
`TestExecutor`, whose `paths:` filter matches only `compio-executor` — a step
added there would not have run on a `compio-buf` change at all.

## The fix

`IoBuf`, `IoBufMut`, `IoVectoredBuf`, `IoVectoredBufMut` and `SetLen` are now
`unsafe trait`s carrying the implementor obligations unsafe code already
depended on:

| Trait | Obligations |
| --- | --- |
| `IoBuf` | `as_init` is stable across calls until `&mut self`; the slice is one live, fully initialized allocation |
| `IoBufMut` | `as_uninit` is stable; `as_init` is a prefix of it in the same allocation; every byte below `as_init().len()` is initialized |
| `IoVectoredBuf` | `iter_slice` is idempotent in slices *and order*; each slice meets `IoBuf`'s obligations |
| `IoVectoredBufMut` | same for `iter_uninit_slice`, against the matching `iter_slice` slice |
| `SetLen` | after `set_len(n)`, `as_init().len() == n`, and `IoBufMut`'s obligations still hold |

This closes bug 2 at the root rather than at each call site. `IoVectoredBuf`'s
idempotency was already written down, as a "Note for implementors" — unsafe
code built an `iovec` array from one traversal and resolved completions against
another, so it was always a safety obligation wearing a convention's clothes.

Bug 1 is closed separately, by `as_uninit` and its siblings becoming
`unsafe fn`: that hazard is the *caller* de-initializing bytes the buffer
promised were initialized, which no implementor obligation can prevent.

### What this does not change

The defensive measures added before this — the `as_mut_slice` clamp, the
saturating `reserve`, `extend_from_slice` bounding against the real slice,
capturing base pointers once in `RecvMsg`/`SendMsg` and `AncillaryBuilder` —
all stay. They are no longer load-bearing: a correct implementation cannot
reach them. They remain as belt-and-braces, so an implementation that breaks
its contract is merely wrong rather than memory-unsafe, and the
`debug_assert!` says so out loud. The adversarial test types that drive them
are now marked as deliberately contract-violating.

### Cost

This is a breaking change for downstream implementors: every
`impl IoBuf for MyBuf` becomes `unsafe impl IoBuf for MyBuf`, and the
implementor takes on the obligations above. That is the honest price of the
guarantee, and it is the state the crate was in from #220 until #555 removed
the marker by accident.

Nothing else in the public API changes. Callers who only *use* buffers are
unaffected.

### Guarding the regression

#555 dropped the `unsafe` marker and shipped, and the loss went unnoticed
because nothing failed. `io_buf.rs` and `io_vec_buf.rs` now carry static
assertions — an `unsafe impl` of each trait for a private empty type — that
fail to compile with `error[E0199]: implementing the trait is not unsafe` if
any marker is removed again. A plain `cargo check` catches it; no test run is
needed.

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
