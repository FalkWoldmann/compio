# Upstream drafts

Drafts for compio-rs/compio. Review and rewrite them in your own words before
posting. Branch names refer to FalkWoldmann/compio.

Order: post 1 and 2 first (each can link its draft PR). The PRs in 4 to 8 don't
depend on either discussion and can go up right away.

---

## 1. Comment on #1053

> Some more findings on the same root cause, from a Miri pass over compio-buf.
>
> **Same bug, other routes.** `IoVectoredBufMut::iter_uninit_slice` is the
> vectored version of `as_uninit` and has the same problem. `IoBufMutExt::copy_within`
> is safe and can copy uninitialized bytes over the initialized prefix. Besides
> `[u8; N]`, I reproduced it with `Vec<u8>` and `&mut [u8]`. `BytesMut`,
> `ArrayVec`, `SmallVec` and `MmapMut` expose their full capacity the same way.
>
> **Making `as_uninit` an `unsafe fn` is not enough on its own.** Unsafe code
> also assumes that the safe methods of a user's `IoBuf` / `IoBufMut` agree with
> each other:
>
> - `as_mut_slice` takes its length from `as_init` and its pointer from
>   `as_uninit`. If they disagree, the `&mut [u8]` runs past the allocation
>   (Miri: dangling reference).
> - The default `reserve` computes `buf_capacity() - buf_len()`, which wraps when
>   they disagree, and `extend_from_slice` then writes past the allocation (Miri:
>   out-of-bounds access).
> - `RecvMsg::init_control` takes `msg_control` and `msg_controllen` from two
>   separate `as_uninit()` calls and hands them to `recvmsg`.
>
> #220 fixed this by making the traits `unsafe`, and #555 removed the markers
> (its description planned an `unsafe fn buffer()` instead, which never landed).
>
> **Proposal (breaking, for the next minor):**
>
> 1. Restore `unsafe trait` on `IoBuf`, `IoBufMut`, `IoVectoredBuf`,
>    `IoVectoredBufMut` and `SetLen`, with the obligations written down.
> 2. Make `as_uninit`, `iter_uninit_slice` and `copy_within` `unsafe fn`.
>
> I can send these as two separate PRs. Independently of that, I have
> non-breaking PRs ready for the three cases above (clamp in `as_mut_slice`,
> saturating `reserve`, one call for pointer and length) plus a `BytesMut`
> provenance bug that Miri found along the way.
>
> Would you accept the breaking part?

---

## 2. New issue: ancillary control messages

**Title:** `ancillary: control message parsing and building is unsound`

> `compio_io::ancillary` walks control messages with raw pointers into the
> buffer. Miri finds UB in normal use:
>
> 1. **Reading past the payload.** `CMsgRef::decode_data` builds a slice of
>    `cmsg_len` bytes starting after the header. `cmsg_len` includes the header,
>    so the slice runs `CMSG_LEN(0)` bytes (16 on Linux) past the payload. Decoded
>    values are still right because `decode` only reads its own size, which is why
>    tests pass.
>
>    ```rust
>    const N: usize = ancillary_space::<u32>();
>    let mut buf = AncillaryBuf::<N>::new();
>    buf.builder().push(1, 2, &7u32).unwrap();
>    // copy into an allocation of exactly N bytes, then:
>    let msg = unsafe { AncillaryIter::new(&exact) }.next().unwrap();
>    msg.data::<u32>().unwrap(); // Miri: dangling reference (beyond the allocation)
>    ```
>
> 2. **Aliasing in `AncillaryBuilder::push`.** The payload is written through a
>    pointer derived from `&mut cmsghdr` (Stacked Borrows rejects this), and
>    `self.buffer.advance(cmsg.encode_data(value)?)` writes through an older
>    pointer after taking a new `&mut` borrow of the buffer (Tree Borrows rejects
>    this). Any `push` shows it.
>
> 3. **`encode` can de-initialize bytes.** `AncillaryData` is a safe trait, and
>    `encode` gets `&mut [MaybeUninit<u8>]` over bytes that `push` then marks as
>    initialized. A safe impl that writes `MaybeUninit::uninit()` makes a later
>    read of the buffer UB.
>
> 4. **Hang.** libc's Linux `CMSG_NXTHDR` returns the same header again when
>    `cmsg_len` is within 7 of `usize::MAX`, so the iterator never ends. Not UB,
>    but a corrupt buffer hangs the caller.
>
> **Proposed fix** (draft PR: link): parse and build on `&[u8]` / `&mut [u8]`
> with offsets. Headers are copied in and out of a `#[repr(C)]` mirror of
> `cmsghdr` deriving `bytemuck::Pod`, checked against libc's layout by `const`
> asserts on every target. No pointers into the buffer remain, and there is no
> runtime `unsafe` in the parser or builder.
>
> **Breaking:** fixing 3 needs `encode(&self, buffer: &mut [u8])`. Only
> hand-written `impl AncillaryData` blocks are affected, and the change is
> mechanical. I checked the latest release of all 125 crates.io dependents of
> compio, compio-io, compio-net and compio-quic: one uses the ancillary API
> (comnoq), and none implement `AncillaryData`. `AncillaryIter::new` also stops
> being `unsafe`, since the parser accepts any bytes.
>
> Is that break acceptable for the next minor? If not, 1, 2 and 4 can be fixed
> without it, and 3 stays open.

---

## 3. PR: `fix/ancillary-safe-rewrite`

**Title:** `fix(io)!: parse and build control messages on byte slices`

> Fixes #(issue from 2).
>
> The ancillary module now parses and builds control messages on byte slices
> instead of pointers into the buffer.
>
> - Headers go through a `#[repr(C)]` mirror of `cmsghdr` that derives
>   `bytemuck::Pod`. `const` asserts check its size, alignment and field offsets
>   against `libc::cmsghdr` (or `CMSGHDR`), so a wrong layout on any target is a
>   compile error.
> - The walk follows the `libc` crate's Linux `CMSG_NXTHDR` with checked
>   arithmetic, so a bad `cmsg_len` ends the walk instead of overrunning or
>   looping. The payload slice is clamped to the buffer.
> - The builder keeps an offset, grows the buffer with `extend_from_slice` and
>   rolls back with the new `SetLenExt::truncate` if `encode` fails.
> - The pktinfo codecs and the bytemuck blanket impl use `to_ne_bytes` and
>   `bytes_of` instead of raw copies.
>
> Remaining `unsafe` in the module: `CMSG_SPACE` in a `const` (compile time only)
> and two Windows union reads.
>
> **Breaking:** `AncillaryData::encode` takes `&mut [u8]`. `AncillaryIter::new`
> is safe now. The `ancillary` feature enables `bytemuck`.
>
> **Tests:** regression tests for each bug, pktinfo round trips, `ancillary_space`
> against libc's `CMSG_SPACE`, and on Linux a comparison with libc's
> `CMSG_FIRSTHDR` / `CMSG_NXTHDR` over 20,000 random buffers. The test file passes
> under Miri with Stacked and Tree Borrows. Parsing is as fast as before; building
> three messages takes about 30 ns instead of about 20 ns.

---

## 4. PR: `fix/buffer-bounds-hardening`

**Title:** `fix(buf): stop trusting as_init and as_uninit to agree`

> `as_mut_slice` built its slice from `buf_len()` (from `as_init`) and the
> pointer from `as_uninit`. The default `reserve` computed
> `buf_capacity() - buf_len()`, which wraps when the two disagree, and
> `extend_from_slice` trusted that result. With a buffer whose methods disagree,
> both reach out-of-bounds memory without any `unsafe` at the call site (Miri
> output in the commit).
>
> Now `as_mut_slice` clamps to what `as_uninit` returned (with a
> `debug_assert!`), `reserve` saturates, and `extend_from_slice` bounds its write
> by the slice it actually got. No API change. See #1053.

---

## 5. PR: `fix/buffer-pointer-stability`

Squash both commits before opening, and remove the `AncillaryBuilder` part from
the commit message.

**Title:** `fix(driver): take the control pointer and length from one call`

> `RecvMsg::init_control` set `msg_control` from one `as_uninit()` call and
> `msg_controllen` from another, then passed both to `recvmsg`. `SendMsg` did
> the same with `buf_ptr()` and `buf_len()`. If a buffer's calls disagree, the
> kernel gets a pointer into one region and the length of another. Both now take
> pointer and length from a single call. No API change. See #1053.

---

## 6. PR: `fix/bytesmut-as-uninit-provenance`

**Title:** `fix(buf): derive BytesMut::as_uninit from spare_capacity_mut`

> `BytesMut::as_uninit` built a `capacity()`-long slice from `as_mut_ptr()`,
> which resolves through `DerefMut` and only covers `len()` bytes. Miri rejects
> it whenever the buffer has spare capacity. It now derives the slice from
> `spare_capacity_mut`. Includes a test over empty, partially filled and full
> buffers. No API change.

---

## 7. PR: `fix/repeat-advance-past-capacity`

**Title:** `fix(io): Repeat::read advanced the buffer past its capacity`

> `Repeat::read` fills the buffer from index 0 but called the relative
> `advance(len)`, so a buffer that already held bytes got a length past its
> capacity (`Vec::set_len` precondition violated). It now uses `advance_to`, like
> `read_vectored`. Includes a regression test.

---

## 8. PR: `fix/iour-recvmsg-out-parse`

**Title:** `fix(driver): parse multishot recvmsg output with io_uring's RecvMsgOut`

> `RecvMsgMultiResultImpl` read the `io_uring_recvmsg_out` header with
> `read_unaligned`, and `addr()` copied the kernel-reported `namelen` bytes into
> a `SockAddrStorage` without clamping to the space reserved for the name.
>
> It now uses `io_uring::types::RecvMsgOut::parse` from the `io-uring` crate we
> already depend on, parses once in `new()` and keeps the ranges. Behaviour is
> unchanged for well-formed buffers; an oversized `namelen` is truncated instead
> of overflowing. `test_udp_recv_msg_multi` and
> `test_udp_recv_msg_multi_truncated_datagram` cover this path.
