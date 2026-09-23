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
| **N7** | `AncillaryData::encode` is a safe trait method that receives `&mut [MaybeUninit<u8>]` over bytes the builder already initialized, and `push` then marks them initialized. A safe `encode` that writes `MaybeUninit::uninit()` makes a later safe read UB (confirmed with Miri, Tree Borrows) | `ancillary/sys.rs` `encode_data`, `ancillary/mod.rs` `push` | Yes, with a safe `AncillaryData` impl | open | open |
| **N8** | `AncillaryIter` loops forever if a `cmsg_len` is within 7 of `usize::MAX`: libc's Linux `CMSG_NXTHDR` wraps `CMSG_ALIGN` to 0 and returns the same header again. A hang, not UB | libc 0.2.189 `CMSG_NXTHDR`, used by `ancillary/sys.rs` | No: needs a corrupt buffer, which `AncillaryIter::new`'s contract rules out | open | open |
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

## N1 and N2: pointer fix vs safe slice rewrite

The safe rewrite fixes N1, N2a to N2c, N7 and N8 by construction, and takes the
ancillary core from 22 `unsafe` keywords to 6, and after tuning it is faster
than both master and the pointer fix. The cost is a breaking change to
`AncillaryData::encode` and about 330 changed lines. The pointer fix on
`fix/ancillary-decode-overread` fixes only N1, but it is small and non-breaking.
Recommendation: submit the N1 fix now, and propose the rewrite separately (issue
first, since it breaks an API) as the fix for N2, N7 and N8.

The rewrite is prototyped on branch `prototype/ancillary-safe-slices` (one
commit on top of the N1 branch; not meant to merge as is). It works on `&[u8]` /
`&mut [u8]` with offsets. Header fields are read and written with
`core::mem::offset_of!` and `from_ne_bytes` / `to_ne_bytes`. The walk follows
the `libc` crate's Linux `CMSG_NXTHDR`, but stops instead of looping on a
wrapping length.

| | A: pointer fix (N1 branch) | B: slices, `encode` unchanged | C: slices, `encode(&mut [u8])` (prototype) |
| --- | --- | --- | --- |
| N1 overread | Fixed | Fixed | Fixed |
| N2a to N2c aliasing | Not fixed; needs a separate pointer-based fix | Fixed: no raw pointers are kept | Fixed |
| N7 `encode` can de-initialize | Not fixed | Not fixed: handing `&mut [u8]` to `encode` as `MaybeUninit` needs the same cast B1 condemns | Fixed: `encode` can only write initialized bytes |
| N8 libc hang | Not fixed | Fixed | Fixed (`wrapping_len_terminates` test) |
| `unsafe` in the parse/build core | 22 | About 6 (estimate, not built) | 6: two `CMSG_*` arithmetic calls, the `unsafe fn` marker on `AncillaryIter::new`, three `set_len` |
| Public API | Unchanged | Unchanged | `AncillaryData::encode` takes `&mut [u8]`. Breaks every implementor: 5 in-tree impls, the bytemuck blanket impl, and downstream users |
| Size | +47 / −11 in 2 files | Not built | +187 / −139 in 4 files |
| Miri (SB and TB) | Clean on a hand-built message; the builder still hits N2 | Not run | Clean: N1+N2 test, `tests/ancillary.rs`, 390 random buffers |
| Equivalence with libc | Uses libc's macros | Same as C | Identical level, type, length and payload range on 194,194 random buffers vs libc's `CMSG_FIRSTHDR`/`CMSG_NXTHDR` on Linux (2 skipped where libc hangs) |
| Platform risk | None new | Same as C | The walk uses Linux rules everywhere. Apple's and Windows' macros skip the `cmsg_len < header` check (so they can loop), and musl stops one byte earlier. Windows is compile-checked only |

Other notes on C:

- `AncillaryIter::new` could become a safe `fn`, because the parser is
  bounds-checked for any input. That is non-breaking, but leaves
  `unused_unsafe` warnings at call sites such as compio-quic.
- `CMSG_ALIGN` is derived as `CMSG_SPACE(1) - CMSG_SPACE(0)` and assumes
  power-of-two rounding. That holds for Linux, Apple and Windows; the other
  BSDs are unchecked.
- `set_len` in `push` still relies on `IoBufMut` behaving, which is the 2a to
  2e contract. The unsafe-review branch's `unsafe trait` restore covers it.
- Performance: see the next section.

### Performance and efficiency

After two small tuning changes, the safe rewrite is faster than both master and
the pointer fix: about 2× on building and about 25% on parsing. Building runs
3× fewer instructions. Parsing runs about as many instructions as master and
slightly more on the bare walk, but finishes sooner. Per datagram this is a few
nanoseconds against a `recvmsg` that costs microseconds, so neither version
matters for end-to-end throughput.

**Workload.** What compio-quic does per datagram: a 128-byte `AncillaryBuf`
holding three messages (`IP_TOS` u8, `IP_PKTINFO` `in_pktinfo`, `UDP_GRO` i32).
*build* creates the buffer and pushes the three messages; *parse* iterates and
decodes each by level and type; *walk* iterates and only reads each `len()`.
Release build, 5M iterations × 7 runs per round, 3 interleaved rounds, on a
4-core 2.8 GHz Xeon container.

| | master | Pointer fix (N1 branch, `#[inline]` added) | Safe rewrite (tuned) |
| --- | --- | --- | --- |
| build, wall-clock | 18.9 ns | 20.8 ns | **9.5 ns** |
| parse, wall-clock | 17.3 ns | 19.0 ns | **13.7 ns** |
| walk, wall-clock | 10.5 ns | 12.3 ns | **9.9 ns** |
| build, instructions/op | 204 | 205 | **70** |
| parse, instructions/op | **174** | 212 | 193 |
| walk, instructions/op | **131** | 137 | 149 |
| Code size of `parse` in the benchmark | 368 B (+ 90 B `next`, not inlined) | 446 B | 577 B |

Wall-clock values are the median of each round's median. Instruction counts are
exact: callgrind, taking the difference between 20k and 40k iterations so setup
cost cancels.

**The first cut was 2× slower.** Before tuning, the safe version took 37 ns to
build and 40 ns to parse. Profiling found two prototype mistakes, neither caused
by the bounds checks:

1. `push` called `ensure_init()` on every message, which zero-fills the whole
   unused tail of the buffer, about 300 bytes of redundant memset per build. Now
   `new()` zeroes it once, and `push` advances the length and writes through
   `as_mut_slice`, shrinking back if `encode` fails. This adds one `unsafe`
   (`set_len`) that relies on the same buffer-honesty contract as before.
2. `CMsgIter::next` wasn't inlined into `AncillaryIter::next`, so every message
   paid a call and a 32-byte return through memory. `#[inline]` on the iterator
   and the header helpers fixes it. For fairness, the pointer fix got the same
   `#[inline]`.

**Why the safe version is faster.** The old code builds a `msghdr` for every
`CMSG_FIRSTHDR`/`CMSG_NXTHDR` call and derives pointers through it. The safe
version does plain offset arithmetic, which LLVM compiles to branch-light code
with conditional moves. Its bounds checks cost a few instructions per message,
but they don't sit on a dependency chain. The pointer fix is slightly slower
than master because computing the clamped payload length adds work that master
skips by being wrong.

**Caveats.** This is a microbenchmark in a shared container: wall-clock noise is
a few percent, and the instruction counts are the more reliable signal. Only
x86-64 Linux was measured. The tuning is committed on
`prototype/ancillary-safe-slices`; the benchmark harness isn't.

### How big the break is

Only hand-written `impl AncillaryData` blocks break, and no published crate has
one. The break comes from fixing N7, not from moving to slices: slices alone
(option B) fix N1, N2 and N8 without changing the API.

| Code that… | Affected? |
| --- | --- |
| iterates with `AncillaryIter` and decodes with `data::<T>()` | No |
| pushes built-in types (integers, arrays, pktinfo) | No |
| implements `BitwiseAncillaryData` | No; the blanket impl is updated inside compio |
| uses `AncillaryData` only as a generic bound | No |
| writes `impl AncillaryData for MyType` by hand | **Yes**: change `encode`'s parameter to `&mut [u8]`; a mechanical edit |

- **Exposure:** the API shipped in compio-io 0.10.0 (27 May 2026). `compio-net`
  always enables the `ancillary` feature, and the umbrella crate exposes it as
  `io-ancillary`, so any compio networking user can reach it.
- **Actual use:** the latest published source of all 125 external crates on
  crates.io that depend on compio, compio-io, compio-net or compio-quic was
  downloaded and searched (23 Sep 2026). One crate, comnoq, uses the API: it
  iterates, decodes, and uses `AncillaryData` as a bound. None implement
  `AncillaryData`. Private code and code pulled from git can't be checked.
- **Semver:** a compio-io 0.11 bump. The unsafe-review branch's `unsafe trait`
  changes need the same bump, so both could ship together.

### Does the rewrite fix N1 and N2?

Yes, all four, checked under Miri with Stacked and Tree Borrows on the tuned
commit:

| Finding | Test | Master | Rewrite |
| --- | --- | --- | --- |
| N1 | Push one message, decode it from an exact-size allocation | UB | Clean |
| N2a | Same test | UB (SB) | Clean |
| N2b | Same test, plus three pushes and an overflowing fourth | UB (TB) | Clean |
| N2c | Builder on a caller-owned `&mut [u8]` | Not on master (unsafe-review branch only) | Clean; the cached pointer no longer exists |

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
