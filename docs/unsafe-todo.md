# Unsafe findings: consolidated list and fix order

This file merges the findings of the zerocopy/bytemuck evaluation with the
unsafe-code findings already recorded on other branches, and puts them in the
order they should be fixed. The last section compares `bytemuck` with `zerocopy`
for compio.

Reviewed with [google/rust-skills `unsafe_rust_review`][skill], the same method
used by the existing review. UB claims were checked with Miri
(`nightly-2026-09-15`) under Stacked Borrows and, where noted, Tree Borrows.

[skill]: https://github.com/google/rust-skills/tree/main/unsafe_rust_review

## Sources

| Source | What it holds |
| --- | --- |
| `claude/compio-unsafe-code-e3j5pw` | The most complete branch. It contains `docs/soundness.md`, `docs/unsafe-review.md` and `docs/dependencies.md`, the fixes, and the `compio-buf` Miri workflow. `docs/soundness` and `unsafe-review` are earlier cuts of the same work. |
| `fix/buffer-trait-soundness`, `fix/buffer-bounds-hardening`, `fix/buffer-pointer-stability`, `fix/bytesmut-as-uninit-provenance`, `fix/repeat-advance-past-capacity` | The same fixes split into reviewable PRs |
| `ci/miri-compio-buf`, `style/document-unsafe-blocks`, `unsafe-tooling` | CI and safety-comment work |
| `archive/unsafe-doc`, `archive/pre-rewrite` | Older `docs/unsafe.md`: inventory, irreducible categories, incidental `unsafe` removed |
| `signal-hook-registry-prototype`, `crate-survey` | Dependency replacement survey |
| `wt/20260905-183421-zerocopy` | Despite the name, this does not use the `zerocopy` crate. It moves `slice_to_uninit` to `write_copy_of_slice`, which `fill_from_slice` on the main branch has since replaced. |
| This branch | Findings **N1 to N6** below, which are new |

IDs `B1` to `B3` and `2a` to `2e` refer to `docs/soundness.md` on
`claude/compio-unsafe-code-e3j5pw`.

## Consolidated findings

Status is given for `master` (M) and for `claude/compio-unsafe-code-e3j5pw` (B).

| ID | Finding | Location | Reachable from safe code? | M | B |
| --- | --- | --- | --- | --- | --- |
| **N1** | `decode_data` builds a slice of `cmsg_len` bytes starting *after* the header, so it reads `CMSG_LEN(0)` bytes (16 on Linux) past the payload | `compio-io/src/ancillary/sys.rs:107` | Yes, after `AncillaryIter::new` on a buffer that obeys the contract | open | open |
| **N2a** | `CMsgMut` holds `&mut cmsghdr` and derives the payload pointer from it, beyond the header's 16 bytes | `sys.rs:110-127` | Yes: `AncillaryBuf::builder().push()` | open (SB) | open |
| **N2b** | `self.buffer.advance(cmsg.encode_data(value)?)` activates a two-phase `&mut` borrow of `buffer`, then writes through an older pointer | `ancillary/mod.rs:154` | Yes: every `push` | open (TB) | open |
| **N2c** | The 2d fix stores a raw `base` pointer from `ensure_init()` next to a live `&mut B`. Moving `buffer` into `Self` retags it, so `base` is invalid on every `push` | `ancillary/mod.rs:138-164` on B | Yes: every `push` | n/a | **introduced** (SB and TB) |
| B1 | `IoBufMut::as_uninit` exposes initialized bytes as `MaybeUninit`, so safe code can de-initialize them. Siblings: `iter_uninit_slice`, `copy_within` | `compio-buf/src/io_buf.rs` | Yes | open | fixed |
| 2a to 2e | Unsafe code trusts safe buffer-trait methods to agree with each other (`as_mut_slice`, `recvmsg` control pointer and length, `reserve`/`extend_from_slice`, `AncillaryBuilder` base, `BytesMut::as_uninit` provenance) | `compio-buf`, `compio-driver`, `compio-io` | Yes | open | fixed (2d: see N2c) |
| B3 | `Repeat::read` advances the buffer past its capacity | `compio-io/src/util/repeat.rs` | Yes | open | fixed |
| **N3** | The `copy_to_bytes`/`copy_from_bytes` unsafe fns have no `# Safety` section. They copy `T::SIZE` bytes, not `size_of::<T>()`, and they are only correct because the blanket impl pins `SIZE` | `ancillary/mod.rs:390-408` | No today | open | open |
| **N4** | The kernel-reported `namelen` is copied into `SockAddrStorage` with no `min(namelen, NLEN)` | `compio-driver/src/sys/op/managed/iour.rs:690` | No: kernel-trusted | open | open |
| **N5** | `copy_addr_from` is a safe fn whose only bound on the copy length is a `debug_assert!` | `compio-driver/src/sys/pal/unix/socket.rs:25` | No: relies on rustix's bound | open | open |
| **N6** | `cargo check -p compio-io --features ancillary` fails to build: rustix 1.1.5 with only `net` hits `cannot find timespec`. Workspace builds hide it through feature unification. It also blocks Miri on `compio-io` | `compio-io/Cargo.toml` | Build only | open | open |
| G1 | The safety comments in `compio-compat`, `compio-process` and `compio-dispatcher` state intent rather than prove the contract | see `unsafe-review.md` | n/a | open | open |
| G2 | `compio-driver`, `compio-executor` and `compio-runtime` have not been reviewed to this standard. Topics to cover: temporal scope of kernel-held pointers, reentrancy, safe-trait trust | see `unsafe-review.md` | unknown | open | open |
| G3 | `AncillaryIter::new`'s `# Safety` section ("should contain valid control messages") is too weak to support N1's fix, or the iterator's own `CMSG_NXTHDR` walk on Windows, which does not reject `cmsg_len < sizeof(CMSGHDR)` | `ancillary/mod.rs:84`, `sys.rs` Windows macros | n/a | open | open |
| D1 | Vendored `half_lock` versus `signal-hook-registry`. Switching changes behaviour: `SIG_DFL` is no longer restored | `compio-signal` | n/a | open | prototype |

### How N1 and N2 were confirmed

Miri test (not checked in because it exercises UB):

```rust
const N: usize = ancillary_space::<u32>();
let mut buf = AncillaryBuf::<N>::new();
buf.builder().push(1, 2, &7u32).unwrap();                   // N2a/N2b (master), N2c (branch)
let mut words = vec![0u64; N / 8];                          // exact-size, aligned allocation
let bytes = unsafe { slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), N) };
bytes.copy_from_slice(&buf);
let m = unsafe { AncillaryIter::new(bytes) }.next().unwrap();
m.data::<u32>().unwrap();                                   // N1
```

- master, Stacked Borrows: `retag … for Unique permission at [0x10]` at `sys.rs:126` (N2a).
- master, Tree Borrows: `reborrow … is forbidden` at `io_buf.rs:771` from `mod.rs:154` (N2b).
- master, Tree Borrows with N2b reordered: `dangling reference (going beyond the bounds of its allocation)` at `sys.rs:107` (N1).
- branch, Stacked Borrows: fails at `mod.rs:138/143`. Branch, Tree Borrows: `write access … forbidden`, where the conflicting tag was *"Frozen due to a reborrow"* when `buffer` was moved into `Self` (N2c).

N1 has not been caught before because `u32::decode` reads only the first 4 bytes,
so decoded values are correct. The overread only matters when a message ends
within 16 bytes of the end of its allocation.

## Upstream issue #1053

[compio-rs/compio#1053](https://github.com/compio-rs/compio/issues/1053),
*Safe code can de-initialize a buffer through as_uninit* (opened 2026-09-20,
open, no comments), covers only **B1**. It gives the `[u8; N]` reproducer and
proposes making `as_uninit` an `unsafe fn`, which is the fix
`claude/compio-unsafe-code-e3j5pw` implements. Every other finding here is
unreported upstream.

| Finding | In #1053? | Gap |
| --- | --- | --- |
| B1, `as_uninit` on `[u8; N]` | Yes | None: this is the issue's reproducer |
| B1 on `Vec<u8>`, `&mut [u8]`, `BytesMut`, `ArrayVec`, `SmallVec`, `MmapMut` | Partly | The issue shows only the array. The same UB was confirmed for `Vec<u8>` and `&mut [u8]` |
| B1 siblings `iter_uninit_slice`, `copy_within` | No | Same UB by another route. The fix has to cover them too |
| 2a to 2d | No | Shares the root cause the issue cites (#220's `unsafe trait` removed by #555). Making `as_uninit` an `unsafe fn` alone does not fix these; restoring the `unsafe trait` markers does |
| 2e, `BytesMut::as_uninit` provenance | No | Separate bug, reachable through ordinary use |
| B3, `Repeat::read` | No | Separate bug in `compio-io` |
| N1, N2a, N2b | No | New. Present on master |
| N3 to N6, G1 to G3, D1 | No | Hardening, build fix and review debt; they don't need issues of their own |

Suggested upstream follow-ups:

- Comment on #1053: add the sibling methods, the other affected impls, and the
  point that the `unsafe trait` markers need restoring too (2a to 2d).
- Open a new issue for N1 and N2 (ancillary UB in `compio-io`), using the Miri
  test above.
- Open a new issue for B3 and 2e, or reference them in the PRs that fix them.

## Fix order

The order is: UB reachable from safe code first, then anything that would make
landing the big branch unsafe, then CI that catches regressions, then
hardening, then review debt.

### P0: UB reachable from safe code

1. **N1: `decode_data` overread.** This is small and independent, so send it to
   master as a standalone PR. Use `cmsg_len - CMSG_LEN(0)` as the payload length,
   checked with `checked_sub` so a short `cmsg_len` is rejected, and clamp it to
   the end of the buffer. Keep a slice of the whole buffer in `CMsgRef` so the
   clamp has something to measure against. Add a Miri regression test that uses
   an exact-size allocation.
2. **N2a, N2b, N2c: `AncillaryBuilder` aliasing.** Fix this on
   `claude/compio-unsafe-code-e3j5pw` *before* that branch lands, because its
   2d fix makes it worse (N2c).
   - Make `CMsgMut` wrap `*mut cmsghdr` instead of `&mut cmsghdr`, and write
     the level, type, length and payload through raw pointers.
   - Compute `encode_data` into a local before calling `self.buffer.advance(n)`.
   - Don't cache `base` next to `&mut B`. Re-derive it on each `push` from one
     `as_uninit()` call and `assert_eq!` it against the address and length
     recorded in `new`. That keeps 2d's guarantee without holding a stale
     pointer.
   - Port the same change to master, or land it with the branch.
3. **Land the buffer-trait work (B1, 2a to 2e, B3)** from
   `claude/compio-unsafe-code-e3j5pw`, or through the split `fix/*` PRs. It is
   breaking: `unsafe trait IoBuf`/`IoBufMut`/`SetLen`/…, and `unsafe fn
   as_uninit`. Do step 2 first. This closes upstream #1053. The small `fix/*` branches
   (`buffer-pointer-stability`, `buffer-bounds-hardening`,
   `bytesmut-as-uninit-provenance`, `repeat-advance-past-capacity`) are not
   breaking and can go ahead of `fix/buffer-trait-soundness`.

### P1: stop regressions

4. **N6: make `compio-io` build on its own.** Add the rustix feature that
   compiles its `timespec` module (for example `fs` or `time`) to compio-io's
   `ancillary` feature, or pin rustix at a version that doesn't need it.
5. **Miri CI for `compio-io`.** Extend the `ci_test_buf` workflow from the
   branch to `compio-io --features ancillary,bytemuck`, running both Stacked
   and Tree Borrows. The existing `tests/ancillary.rs` already drives `push`,
   so it hits N2 as soon as it runs under Miri. N1 needs the exact-size
   allocation test above, because the existing test's buffer has slack after
   the last message. Step 4 is needed first.

### P2: hardening and removing unsafe (see the next section)

6. **N3: remove `copy_to_bytes`/`copy_from_bytes`.**
   - For `BitwiseAncillaryData`, use `bytemuck::bytes_of` with
     `write_copy_of_slice` to encode, and `bytemuck::pod_read_unaligned` on
     `buf[..size_of::<T>()]` to decode. This was prototyped: the existing
     `tests/ancillary.rs` passes (3/3) with no `unsafe` left in `bytemuck_ext.rs`,
     and the public API doesn't change.
   - For the libc and windows-sys pktinfo types, encode field by field with
     `to_ne_bytes`/`from_ne_bytes`. That needs no crate and no `unsafe` except
     the Windows union reads.
7. **`io_uring_recvmsg_out` header reads** (`iour.rs:652-681`): replace
   `read_unaligned` with a safe decode, either four `u32::from_ne_bytes` calls
   or a `zerocopy` derive.
8. **N4:** clamp `namelen` to `NLEN`. **N5:** make the bound a real `assert!`,
   or clamp.
9. **IOCP `transmute::<SOCKADDR_STORAGE, SockAddrStorage>(read_unaligned(..))`**
   (`iocp.rs:100`): copy `remote_addr_len` bytes (bounded) into
   `SockAddrStorage::zeroed()` instead of transmuting between two foreign types.
10. **G3:** write a precise `# Safety` section for `AncillaryIter::new`, and
    reject `cmsg_len < sizeof(CMSGHDR)` in the Windows `CMSG_NXTHDR` shim.
    Longer term, consider a safe, bounds-checked cmsg parser over `&[u8]`
    (see the comparison below). That would make `AncillaryIter::new` safe.

### P3: review debt and decisions

11. **G1:** rewrite the safety comments in `compio-compat`, `compio-process` and
    `compio-dispatcher` to the proof standard.
12. **G2:** review `compio-driver` (about 255 blocks), `compio-executor` and
    `compio-runtime`. Start with the temporal scope of kernel-held buffers after
    a future is dropped, and with executor reentrancy.
13. **D1:** a maintainer decision on `signal-hook-registry`: it removes 7 unsafe
    blocks, but `SIG_DFL` is no longer restored.

## Safe alternatives from std and established crates

Six findings have a ready-made safe replacement. The most useful is
`io_uring::types::RecvMsgOut::parse`: it is already a dependency, it fixes N4,
and it removes two `unsafe` blocks. No crate can parse a control-message buffer
that compio fills through io_uring or IOCP, so N1, N2 and G3 need a safe rewrite
using std. Every API below was checked against the version in `Cargo.lock` or
the std source for `nightly-2026-09-15`.

| Finding | Safe alternative | Source | Status | Effect |
| --- | --- | --- | --- | --- |
| N4, plus the `read_unaligned` header reads in `iour.rs:652-704` | `io_uring::types::RecvMsgOut::parse(buffer, &msghdr)` | `io-uring` 0.7.15, already a dependency | Stable | Checks the buffer length and clamps the name, control and payload lengths to the allocation. `name_data()`, `control_data()`, `payload_data()` and `is_name_data_truncated()` replace compio's own offset arithmetic |
| N3, bitwise types | `bytemuck::bytes_of` with `write_copy_of_slice`; `bytemuck::pod_read_unaligned` | bytemuck (already a dependency); std | Stable (`write_copy_of_slice` since 1.93) | Prototyped: `tests/ancillary.rs` passes 3/3 with no `unsafe` |
| N3, libc and Windows pktinfo types | `to_ne_bytes` / `from_ne_bytes` per field | std | Stable | No crate needed. Only the Windows union reads stay `unsafe` |
| B1 | A write-only view modelled on `bytes::buf::UninitSlice` | bytes 1.12 (optional dependency) | Stable | `UninitSlice` allows writes but no reads and no uninit writes, so it can't de-initialize. Building one from `&mut [u8]` or `&mut [MaybeUninit<u8>]` is safe. This is the safe version of #1053's second option |
| 2e | `BytesMut::spare_capacity_mut` | bytes | Stable | Already used on the branch |
| B3 | `<[MaybeUninit<u8>]>::write_filled` | std | Unstable (`maybe_uninit_fill`) | Keep the branch's `fill_bytes` until it stabilizes |
| D1 | `signal-hook-registry` | crate | Stable | Prototyped. Stops restoring `SIG_DFL` |

### Precedent, not a replacement

- **2a to 2e:** no crate removes these, but established designs back the
  branch's fix. `bytes::BufMut` is a `pub unsafe trait` for the same reason.
  std's `BorrowedBuf` tracks the initialized range and makes raw access
  `unsafe`, but it is unstable (`core_io_borrowed_buf`) and already behind
  compio's `read_buf` feature.

### No safe alternative exists

| Finding | Why nothing fits | Recommended safe approach |
| --- | --- | --- |
| N1, N2, G3: control-message parsing and building | `rustix` models only `SCM_RIGHTS`, `SCM_CREDENTIALS` and `TxTime`. `nix` 0.31 covers everything compio-quic uses (`Ipv4Tos`, `Ipv6TClass`, packet info, `UdpGroSegments`, `UdpGsoSegments`), but its `CmsgIterator` only comes from nix's own blocking `recvmsg` and it is Unix only. `quinn-udp`'s cmsg module is private. std's `SocketAncillary` is unstable and limited to Unix domain sockets | Work on `&[u8]` / `&mut [u8]`. Get offsets from `CMSG_LEN`/`CMSG_SPACE` (`const fn` in libc) and `core::mem::offset_of!(cmsghdr, cmsg_len)` (stable since 1.77). Read and write fields with `from_ne_bytes` / `to_ne_bytes` on bounds-checked subslices. That rules out N1 and N2 by construction, needs no raw pointers, and lets `AncillaryIter::new` become safe. Or use zerocopy `Ref::from_prefix` over a local `cmsghdr` mirror for typed headers |
| N5 (`copy_addr_from`) and the IOCP `transmute` | `socket2::SockAddr::new(storage, len)` is itself `unsafe`, and no crate converts safely between rustix, socket2 and windows-sys address types for every family | For IP addresses only, `SocketAddrAny` → `std::net::SocketAddr` → `SockAddr::from` is safe but loses Unix-domain sockets. Otherwise keep the copy and clamp the length |
| `MmapMut::as_uninit` | std deliberately has no safe `&mut [u8]` → `&mut [MaybeUninit<u8>]` conversion | Covered by the B1 fix |
| N6 | A rustix feature-gating bug, not an `unsafe` problem | Add the missing rustix feature |

Suggested next step: switch `iour.rs` to `RecvMsgOut::parse` as a P2 change. It
is small and self-contained, and it closes N4.

## bytemuck vs zerocopy for compio

### Where each could apply

| Site | bytemuck | zerocopy | Neither needed |
| --- | --- | --- | --- |
| `BitwiseAncillaryData` (public, user types) | Used today (`Pod` bound) | Possible (`IntoBytes + FromBytes + Immutable`) | n/a |
| `copy_to_bytes`/`copy_from_bytes` for bitwise types | `bytes_of`/`pod_read_unaligned` (safe) | `as_bytes`/`read_from_prefix` (safe) | n/a |
| libc/windows-sys `in_addr`, `in_pktinfo`, `in6_pktinfo`, `IN_PKTINFO`, `IN6_PKTINFO` | Can't implement `Pod` for a foreign type. Possible on a local `#[repr(transparent)]` newtype with a hand-written `unsafe impl` | Not possible: traits are derive-only, and the derive needs the inner type to implement them | Field-wise `to_ne_bytes` is fully safe |
| `io_uring_recvmsg_out` (local, 4×`u32`) | `#[derive(Pod)]` needs the `derive` feature | `#[derive(FromBytes, KnownLayout, Immutable)]` | 4× `u32::from_ne_bytes` |
| `SOCKADDR_STORAGE` to `SockAddrStorage` | No: both are foreign | No | Bounded byte copy |
| `cmsghdr` walking | Poor fit | Good fit: a per-target local mirror of `cmsghdr` plus `Ref::from_prefix` gives a bounds-checked parser | n/a |
| Everything else (FFI, `OpCode`, executor) | No | No | Irreducible, per `archive/unsafe-doc` |

### Crate properties that matter here

| | bytemuck 1.25 | zerocopy 0.8.57 |
| --- | --- | --- |
| In the tree today | Optional direct dependency of `compio-io`, re-exported publicly (`pub use bytemuck::{Pod, Zeroable}`), no `derive` feature | Not a direct dependency. Only in the lockfile through `criterion → ciborium → half` (dev) |
| Version stability | 1.x. Already a public commitment | Pre-1.0. Exposing its traits in `compio-io`'s public API would tie compio's semver to zerocopy's next 0.x release |
| Implementing for user types | `unsafe impl Pod`, and the doc example does exactly that, or `#[derive(Pod)]` with the `derive` feature. A wrong `unsafe impl` (padding, for example) puts uninitialized bytes into the cmsg buffer, which is then read as `&[u8]` | Derive-only. Layout is checked at compile time, and user code has no `unsafe` |
| Foreign types | Allowed through a local newtype with `unsafe impl` | Not possible, even through a newtype (`only_derive_is_allowed_to_implement_this_trait`, as `docs/dependencies.md` records) |
| Parsing and validation | `Pod`/`AnyBitPattern` casts, `CheckedBitPattern` | Additionally `KnownLayout`, `Ref`/`Unaligned` for prefix parsing, `TryFromBytes` for validated enums and `bool` |
| Build cost for users | Light with no `derive` feature. `derive` adds a proc-macro and `syn` | Users need `zerocopy-derive` (proc-macro, `syn`) to implement anything |
| Unsafe removed from compio | Same as zerocopy for this code: both remove N3's unsafe | Same, plus the option of a safe cmsg parser |

### Recommendation

- **Now:** keep bytemuck. Rewrite `bytemuck_ext.rs` on bytemuck's safe
  functions (P2, step 6) and move the libc impls to field-wise encoding. This
  removes N3's `unsafe` with no API break and no new dependency.
- **For the header and pktinfo decoding:** safe code needs neither crate.
- **Consider zerocopy only if** the cmsg iterator is rewritten as a safe parser
  (P2, step 10), where `KnownLayout`/`Ref` do real work. Even then, keep it an
  internal dependency and don't re-export its traits until zerocopy reaches 1.0.
  Switching `BitwiseAncillaryData` from `Pod` to zerocopy traits is a breaking
  change for every implementor. Its benefit is that users no longer write
  `unsafe impl`. That is worth doing in a planned `compio-io` minor bump, but it
  is not a soundness fix.
- **Correction to `docs/dependencies.md`:** it rejects zerocopy because it
  "cannot be applied to the libc types compio needs it for", which is accurate,
  but compio doesn't use bytemuck for those types either. They go through the
  hand-written `copy_to_bytes`. Bytemuck's newtype escape hatch is available but
  unused. The fair comparison is on `BitwiseAncillaryData` and the local
  `io_uring_recvmsg_out`, and there the two crates are about even.
