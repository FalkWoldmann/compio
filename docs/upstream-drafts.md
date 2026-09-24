# Upstream drafts

Drafts for compio-rs/compio (and one for rustix). Review and rewrite them in
your own words before posting. Branch names refer to FalkWoldmann/compio.

The reproducers are included in full below, in collapsible blocks (also in
`docs/reproducers`, verified against master 6d40918). Run the Miri ones with
`cargo +nightly miri run --example <name>`.

Commit messages contain no `#123` references, so pushing to the fork doesn't
add links to upstream issues. Put "Fixes #..." in the PR description instead.

Order: post 1 and 2 first (each can link its draft PR). The non-breaking PRs
(4 to 8 and 10 to 12) don't depend on either discussion and can go up right
away. 3 and 9 wait for the answers to 2 and 1. 13 goes to rustix.

---

## 1. Comment on #1053

> Some more findings on the same root cause, from a Miri pass over compio-buf.
>
> **Same bug, other routes.** `IoVectoredBufMut::iter_uninit_slice` is the
> vectored version of `as_uninit` and has the same problem. `IoBufMutExt::copy_within`
> is safe and can copy uninitialized spare capacity over the initialized prefix.
> Besides `[u8; N]`, I reproduced it with `Vec<u8>` and `&mut [u8]`. `BytesMut`,
> `ArrayVec`, `SmallVec` and `MmapMut` expose their full capacity the same way.
>
> ```rust
> let mut bufs = [vec![1u8, 2, 3], vec![4u8, 5, 6]];
> for slice in bufs.iter_uninit_slice() {
>     slice[0] = MaybeUninit::uninit();
> }
> let x = bufs[0][0]; // Miri: reading uninitialized memory
>
> let mut v = Vec::with_capacity(8);
> v.extend_from_slice(&[1u8, 2, 3, 4]);
> v.copy_within(4..8, 0); // copies spare capacity over v[0..4]
> let x = v[0]; // Miri: reading uninitialized memory
> ```
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
> The `recvmsg` one below runs natively: a control buffer whose `as_uninit`
> alternates between an 8-byte and a 256-byte field makes the kernel write a
> 32-byte `IP_PKTINFO` message into the 8-byte one and overwrite the bytes after
> it.
>
> Reproducers (Miri unless the file says otherwise):
>
> <details><summary>Cargo.toml</summary>
>
> ```toml
> [package]
> name = "compio-ub-repro"
> version = "0.0.0"
> edition = "2024"
> publish = false
>
> # compio master at 6d40918
> [dependencies]
> compio = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["net", "macros"] }
> compio-buf = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["bytes"] }
> compio-io = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["ancillary", "bytemuck"] }
> bytemuck = "1"
> bytes = "1"
> futures-executor = "0.3"
> libc = "0.2"
> # compio-io alone does not build under Miri without this (rustix timespec).
> rustix = { version = "1", features = ["fs"] }
>
> [workspace]
> ```
>
> </details>
>
> <details><summary><code>examples/as_uninit_vec.rs</code></summary>
>
> ```rust
> // Same bug as #1053, with Vec<u8> instead of [u8; N]. [u8] behaves the same.
> use std::mem::MaybeUninit;
>
> use compio_buf::IoBufMut;
>
> fn main() {
>     let mut v = vec![1u8, 2, 3, 4];
>     v.as_uninit()[0] = MaybeUninit::uninit();
>     let x = v[0]; // UB
>     std::hint::black_box(x);
> }
> ```
>
> </details>
>
> <details><summary><code>examples/iter_uninit_slice.rs</code></summary>
>
> ```rust
> // IoVectoredBufMut::iter_uninit_slice exposes initialized bytes as MaybeUninit.
> use std::mem::MaybeUninit;
>
> use compio_buf::IoVectoredBufMut;
>
> fn main() {
>     let mut bufs = [vec![1u8, 2, 3], vec![4u8, 5, 6]];
>     for slice in bufs.iter_uninit_slice() {
>         slice[0] = MaybeUninit::uninit(); // safe
>     }
>     let x = bufs[0][0]; // UB: reads uninitialized memory
>     std::hint::black_box(x);
> }
> ```
>
> </details>
>
> <details><summary><code>examples/copy_within.rs</code></summary>
>
> ```rust
> // IoBufMutExt::copy_within can copy spare capacity over initialized bytes.
> use compio_buf::IoBufMutExt;
>
> fn main() {
>     let mut v = Vec::with_capacity(8);
>     v.extend_from_slice(&[1u8, 2, 3, 4]);
>     v.copy_within(4..8, 0); // safe: copies uninitialized capacity over v[0..4]
>     let x = v[0]; // UB: reads uninitialized memory
>     std::hint::black_box(x);
> }
> ```
>
> </details>
>
> <details><summary><code>examples/as_mut_slice_disagree.rs</code></summary>
>
> ```rust
> // as_mut_slice takes its length from as_init and its pointer from as_uninit.
> // A safe IoBuf/IoBufMut impl where they disagree gives a slice past the
> // allocation. The only `unsafe` is the empty set_len the trait requires.
> use std::mem::MaybeUninit;
>
> use compio_buf::{IoBuf, IoBufMut, IoBufMutExt, SetLen};
>
> struct Lying {
>     init: Vec<u8>,
>     spare: [MaybeUninit<u8>; 8],
> }
>
> impl IoBuf for Lying {
>     fn as_init(&self) -> &[u8] {
>         &self.init // 1000 bytes
>     }
> }
>
> impl IoBufMut for Lying {
>     fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
>         &mut self.spare // 8 bytes
>     }
> }
>
> impl SetLen for Lying {
>     unsafe fn set_len(&mut self, _: usize) {}
> }
>
> fn main() {
>     let mut buf = Lying { init: vec![0; 1000], spare: [MaybeUninit::new(0); 8] };
>     let s = buf.as_mut_slice(); // UB: 1000-byte slice over 8 bytes
>     s[999] = 1;
> }
> ```
>
> </details>
>
> <details><summary><code>examples/extend_disagree.rs</code></summary>
>
> ```rust
> // The default reserve computes buf_capacity() - buf_len(), which wraps when
> // they disagree, and extend_from_slice then writes past the allocation.
> // Run in release so the subtraction wraps instead of panicking:
> //   cargo miri run --release --example extend_disagree
> use std::mem::MaybeUninit;
>
> use compio_buf::{IoBuf, IoBufMut, IoBufMutExt, SetLen};
>
> struct Lying {
>     init: Vec<u8>,
>     spare: Box<[MaybeUninit<u8>; 32]>,
> }
>
> impl IoBuf for Lying {
>     fn as_init(&self) -> &[u8] {
>         &self.init // 1024 bytes
>     }
> }
>
> impl IoBufMut for Lying {
>     fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
>         &mut *self.spare // 32 bytes
>     }
> }
>
> impl SetLen for Lying {
>     unsafe fn set_len(&mut self, _: usize) {}
> }
>
> fn main() {
>     let mut buf = Lying { init: vec![0; 1024], spare: Box::new([MaybeUninit::new(0); 32]) };
>     buf.extend_from_slice(&[1, 2, 3, 4]).unwrap(); // UB: writes at offset 1024 of a 32-byte box
> }
> ```
>
> </details>
>
> <details><summary><code>examples/recvmsg_ptr_len.rs</code></summary>
>
> ```rust
> // RecvMsg takes msg_control from one as_uninit() call and msg_controllen
> // from another. A control buffer whose calls return different slices lets
> // the kernel write past an 8-byte buffer. Runs natively (not under Miri):
> //   cargo run --example recvmsg_ptr_len
> // The canary next to the small buffer gets overwritten.
> use std::mem::MaybeUninit;
>
> use compio_buf::{IoBuf, IoBufMut, SetLen};
> use compio::net::UdpSocket;
>
> #[repr(C, align(8))]
> struct Control {
>     empty: [u8; 0],
>     small: [MaybeUninit<u8>; 8],
>     canary: [u8; 64],
>     big: [MaybeUninit<u8>; 256],
>     calls: usize,
> }
>
> impl IoBuf for Control {
>     fn as_init(&self) -> &[u8] {
>         &self.empty
>     }
> }
>
> impl IoBufMut for Control {
>     fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
>         self.calls += 1;
>         if self.calls % 2 == 1 { &mut self.small } else { &mut self.big }
>     }
> }
>
> impl SetLen for Control {
>     unsafe fn set_len(&mut self, _: usize) {}
> }
>
> #[compio::main]
> async fn main() {
>     let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
>     // Ask for IP_PKTINFO so the kernel returns a 32-byte control message.
>     unsafe { rx.set_socket_option(libc::IPPROTO_IP, libc::IP_PKTINFO, &1i32) }.unwrap();
>     let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
>     tx.send_to(b"hi", rx.local_addr().unwrap()).await.unwrap();
>
>     let control = Control {
>         empty: [],
>         small: [MaybeUninit::new(0); 8],
>         canary: [0xAA; 64],
>         big: [MaybeUninit::new(0); 256],
>         calls: 0,
>     };
>     let res = rx.recv_msg(Vec::with_capacity(16), control).await;
>     let (_, control) = res.1;
>     let hit = control.canary.iter().filter(|&&b| b != 0xAA).count();
>     println!("{:?}, canary bytes overwritten: {hit}", res.0.map(|r| r.1));
> }
> ```
>
> </details>
>
> <details><summary><code>examples/bytesmut_as_uninit.rs</code></summary>
>
> ```rust
> // BytesMut::as_uninit builds a capacity-long slice from a pointer that only
> // covers len() bytes. Plain use, no user impl.
> use bytes::BytesMut;
> use compio_buf::IoBufMut;
>
> fn main() {
>     let mut b = BytesMut::with_capacity(16);
>     b.extend_from_slice(b"abc");
>     let slice = b.as_uninit(); // UB under Stacked Borrows (retag), len 3 < cap 16
>     println!("{}", slice.len());
> }
> ```
>
> </details>
>
> <details><summary><code>examples/repeat_advance.rs</code></summary>
>
> ```rust
> // Repeat::read fills the whole buffer from index 0 but advances relative to
> // the current length, so a non-empty buffer ends up with len > capacity.
> use compio_io::AsyncRead;
>
> fn main() {
>     futures_executor::block_on(async {
>         let mut v = Vec::with_capacity(13);
>         v.extend_from_slice(b"abc");
>         let cap = v.capacity();
>         let (n, v) = compio_io::repeat(42).read(v).await.unwrap();
>         println!("read {n}, len {}, capacity {cap}", v.len()); // Vec::set_len precondition violated
>     });
> }
> ```
>
> </details>
>
> #220 fixed this by making the traits `unsafe`, and #555 removed the markers
> (its description planned an `unsafe fn buffer()` instead, which never landed).
>
> **Non-breaking fixes (PRs ready):** clamp in `as_mut_slice`, saturating
> `reserve`, one call for pointer and length, and a `copy_within` that panics
> instead of moving spare capacity into the initialized prefix. Plus two bugs
> Miri found along the way: `BytesMut::as_uninit` provenance (Stacked Borrows
> only) and a `Repeat::read` length bug.
>
> **Proposal (breaking, for the next minor):**
>
> 1. Restore `unsafe trait` on `IoBuf`, `IoBufMut`, `IoVectoredBuf`,
>    `IoVectoredBufMut` and `SetLen`, with the requirements written down.
> 2. Make `as_uninit` and `iter_uninit_slice` `unsafe fn`, and add safe
>    `fill_from_slice` / `fill_bytes` for the common case.
>
> I have this ready as one PR on top of the non-breaking ones. Would you accept
> it?

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
>    tests pass. When the buffer ends right after the message (as below), the
>    slice runs past the allocation, which is UB under any model; for a prefix of
>    a larger buffer, such as `&control[..len]`, Stacked Borrows rejects it and
>    Tree Borrows accepts it.
>
>    ```rust
>    // One u32 message, CMSG_SPACE(4) bytes (Linux glibc, 64-bit).
>    let words: Box<[u64]> = Box::new([20, 1 | 2 << 32, 7]);
>    let bytes: &[u8] = bytemuck::cast_slice(&words);
>    let msg = unsafe { AncillaryIter::new(bytes) }.next().unwrap();
>    msg.data::<u32>().unwrap(); // Miri: dangling reference (beyond the allocation)
>    ```
>
> 2. **Aliasing in `AncillaryBuilder::push`.** The payload is written through a
>    pointer derived from `&mut cmsghdr` (Stacked Borrows rejects this), and
>    `self.buffer.advance(cmsg.encode_data(value)?)` writes through an older
>    pointer after taking a new `&mut` borrow of the buffer (Tree Borrows rejects
>    this). Any `push` shows it:
>
>    ```rust
>    let mut buf = AncillaryBuf::<{ ancillary_space::<u32>() }>::new();
>    buf.builder().push(1, 2, &7u32).unwrap();
>    ```
>
> 3. **`encode` can de-initialize bytes.** `AncillaryData` is a safe trait, and
>    `encode` gets `&mut [MaybeUninit<u8>]` over bytes that `push` then marks as
>    initialized. A safe impl that writes `MaybeUninit::uninit()` makes a later
>    read of the buffer UB (Miri needs `-Zmiri-disable-stacked-borrows` to get
>    past 2 first).
>
> 4. **Hang.** libc's Linux `CMSG_NXTHDR` returns the same header again when
>    `cmsg_len` is within 7 of `usize::MAX`, so the iterator never ends. Not UB,
>    but a corrupt buffer hangs the caller (debug builds abort on an overflow
>    inside libc instead).
>
> 5. **Panic on empty control data.** `recv_msg` returns an empty control buffer
>    for a datagram without control messages, and `AncillaryIter::new` panics on
>    it with "buffer too short". compio-quic parses every datagram this way and
>    only gets control data if its ECN or pktinfo socket options took effect,
>    which it tolerates failing as unsupported. A non-breaking fix is ready as a
>    separate PR.
>
> Full reproducers:
>
> <details><summary>Cargo.toml</summary>
>
> ```toml
> [package]
> name = "compio-ub-repro"
> version = "0.0.0"
> edition = "2024"
> publish = false
>
> # compio master at 6d40918
> [dependencies]
> compio = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["net", "macros"] }
> compio-buf = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["bytes"] }
> compio-io = { git = "https://github.com/compio-rs/compio", rev = "6d40918", features = ["ancillary", "bytemuck"] }
> bytemuck = "1"
> bytes = "1"
> futures-executor = "0.3"
> libc = "0.2"
> # compio-io alone does not build under Miri without this (rustix timespec).
> rustix = { version = "1", features = ["fs"] }
>
> [workspace]
> ```
>
> </details>
>
> <details><summary><code>examples/ancillary_decode_overread.rs</code></summary>
>
> ```rust
> // CMsgRef::decode_data builds a slice of cmsg_len bytes starting after the
> // header. cmsg_len includes the header, so the slice runs CMSG_LEN(0) bytes
> // past the payload. Only visible when the buffer ends right after the message,
> // as it does after recvmsg with a control buffer of CMSG_SPACE(4) bytes.
> // Layout is Linux glibc, 64-bit little endian.
> use compio_io::ancillary::AncillaryIter;
>
> fn main() {
>     // cmsg_len = CMSG_LEN(4) = 20, cmsg_level = 1, cmsg_type = 2, payload 7u32.
>     // 24 bytes = CMSG_SPACE(4), in an allocation of exactly that size.
>     let words: Box<[u64]> = Box::new([20, 1 | 2 << 32, 7]);
>     let bytes: &[u8] = bytemuck::cast_slice(&words);
>
>     let msg = unsafe { AncillaryIter::new(bytes) }.next().unwrap();
>     let value = msg.data::<u32>().unwrap(); // UB: 20-byte slice at offset 16 of 24
>     assert_eq!(value, 7);
> }
> ```
>
> </details>
>
> <details><summary><code>examples/ancillary_push_aliasing.rs</code></summary>
>
> ```rust
> // AncillaryBuilder::push writes through pointers that Miri rejects:
> //   cargo miri run --example ancillary_push_aliasing
> //     Stacked Borrows: payload written through a pointer derived from &mut cmsghdr
> //   MIRIFLAGS=-Zmiri-tree-borrows cargo miri run --example ancillary_push_aliasing
> //     Tree Borrows: write through an older pointer after a new &mut borrow
> use compio_io::ancillary::{AncillaryBuf, ancillary_space};
>
> fn main() {
>     let mut buf = AncillaryBuf::<{ ancillary_space::<u32>() }>::new();
>     buf.builder().push(1, 2, &7u32).unwrap();
> }
> ```
>
> </details>
>
> <details><summary><code>examples/ancillary_encode_deinit.rs</code></summary>
>
> ```rust
> // AncillaryData is a safe trait, but encode gets &mut [MaybeUninit<u8>] over
> // bytes that push then marks as initialized. A safe impl can de-initialize them.
> // push also trips the aliasing bug, so turn aliasing checks off to reach this:
> //   MIRIFLAGS=-Zmiri-disable-stacked-borrows cargo miri run --example ancillary_encode_deinit
> use std::mem::MaybeUninit;
>
> use compio_io::ancillary::{AncillaryBuf, AncillaryData, CodecError, ancillary_space};
>
> struct Deinit;
>
> impl AncillaryData for Deinit {
>     const SIZE: usize = 4;
>
>     fn encode(&self, buffer: &mut [MaybeUninit<u8>]) -> Result<(), CodecError> {
>         buffer[0] = MaybeUninit::uninit(); // safe
>         Ok(())
>     }
>
>     fn decode(_: &[u8]) -> Result<Self, CodecError> {
>         Ok(Deinit)
>     }
> }
>
> fn main() {
>     let mut buf = AncillaryBuf::<{ ancillary_space::<Deinit>() }>::new();
>     buf.builder().push(1, 2, &Deinit).unwrap();
>     let sum: u32 = buf.iter().map(|&b| b as u32).sum(); // UB: reads uninitialized memory
>     println!("{sum}");
> }
> ```
>
> </details>
>
> <details><summary><code>examples/ancillary_walk_hang.rs</code></summary>
>
> ```rust
> // libc's Linux CMSG_NXTHDR returns the same header again when cmsg_len is
> // within 7 of usize::MAX, so AncillaryIter never ends on such a buffer.
> // Not UB. Runs natively:
> //   cargo run --release --example ancillary_walk_hang   (never ends)
> //   cargo run --example ancillary_walk_hang             (aborts: overflow panic
> //                                                        inside extern "C" CMSG_NXTHDR)
> use compio_io::ancillary::AncillaryIter;
>
> fn main() {
>     // One cmsghdr (Linux glibc, 64-bit): cmsg_len, cmsg_level, cmsg_type.
>     let mut words = [0u64; 4];
>     words[0] = u64::MAX - 3;
>     let bytes: &[u8] = bytemuck::cast_slice(&words);
>
>     let iter = unsafe { AncillaryIter::new(bytes) };
>     let n = iter.take(1_000_000).count();
>     println!("{n} messages from a 32-byte buffer"); // 1000000
> }
> ```
>
> </details>
>
> <details><summary><code>examples/ancillary_empty_control.rs</code></summary>
>
> ```rust
> // recv_msg returns an empty control buffer for a datagram without control
> // messages, and AncillaryIter::new panics on it with "buffer too short".
> // Runs natively: cargo run --example ancillary_empty_control
> use compio::net::UdpSocket;
> use compio_io::ancillary::{AncillaryBuf, AncillaryIter};
>
> #[compio::main]
> async fn main() {
>     let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
>     let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
>     tx.send_to(b"hi", rx.local_addr().unwrap()).await.unwrap();
>
>     let res = rx.recv_msg(Vec::with_capacity(16), AncillaryBuf::<64>::new()).await;
>     let ((_, control_len, _, _), (_, control)) = res.unwrap();
>     println!("control_len = {control_len}");
>     let n = unsafe { AncillaryIter::new(&control) }.count(); // panics
>     println!("{n} messages");
> }
> ```
>
> </details>
>
> **Proposed fix** (draft PR: link): parse and build on `&[u8]` / `&mut [u8]`
> with offsets. Headers are copied in and out of a `#[repr(C)]` mirror of
> `cmsghdr` deriving `bytemuck::Pod`, checked against libc's layout by `const`
> asserts on every target. No pointers into the buffer remain, and there is no
> runtime `unsafe` in the parser or builder. Parsing gets about 30% faster.
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
>   looping. The payload slice is clamped to the buffer. Any buffer is accepted,
>   including empty and unaligned ones.
> - The builder keeps an offset, grows the buffer with `extend_from_slice` and
>   rolls back with the new `SetLenExt::truncate` if `encode` fails. It only
>   checks alignment; a short buffer fails the push with `BufferTooSmall`.
> - The pktinfo codecs and the bytemuck blanket impl use `to_ne_bytes` and
>   `bytes_of` instead of raw copies.
>
> Remaining `unsafe` in the module: `CMSG_SPACE` in a `const` (compile time only)
> and two Windows union reads.
>
> **Breaking:** `AncillaryData::encode` takes `&mut [u8]`. `AncillaryIter::new`
> is safe now. The `ancillary` feature enables `bytemuck`.
> `AncillaryBuilder::new` no longer panics on a short buffer.
>
> **Tests:** regression tests for each bug, pktinfo round trips, `ancillary_space`
> against libc's `CMSG_SPACE`, and on Linux a comparison with libc's
> `CMSG_FIRSTHDR` / `CMSG_NXTHDR` over 20,000 random buffers. The test file passes
> under Miri with Stacked and Tree Borrows.
>
> **Performance** (x86-64, release, median of three runs): parsing three
> messages takes 12 ns instead of 17 ns. Building them takes 25 ns instead of
> 18.5 ns, because each push goes through compio-buf's safe buffer methods.

---

## 4. PR: `fix/buffer-bounds-hardening`

**Title:** `fix(buf): don't trust as_init and as_uninit to agree`

> See #1053.
>
> `as_mut_slice` built its slice from `buf_len()` (from `as_init`) and the
> pointer from `as_uninit`. The default `reserve` computed
> `buf_capacity() - buf_len()`, which wraps when the two disagree, and
> `extend_from_slice` trusted that result. With a buffer whose methods disagree,
> both reach out-of-bounds memory without any `unsafe` at the call site.
>
> Now `as_mut_slice` clamps to what `as_uninit` returned (with a
> `debug_assert!`), `reserve` saturates, and `extend_from_slice` bounds its write
> by the slice it actually got and copies with `write_copy_of_slice`, which
> resolves the FIXME there. No API change.

---

## 5. PR: `fix/buffer-pointer-stability`

**Title:** `fix(driver): take the control pointer and length from one call`

> See #1053.
>
> `RecvMsg::init_control` set `msg_control` from one `as_uninit()` call and
> `msg_controllen` from another, then passed both to `recvmsg`. `SendMsg` did
> the same with `buf_ptr()` and `buf_len()`. If a buffer's calls disagree, the
> kernel gets a pointer into one region and the length of another. Both now take
> pointer and length from a single call. No API change.

---

## 6. PR: `fix/bytesmut-as-uninit-provenance`

**Title:** `fix(buf): derive BytesMut::as_uninit from spare_capacity_mut`

> `BytesMut::as_uninit` built a `capacity()`-long slice from `as_mut_ptr()`,
> which resolves through `DerefMut` and only covers `len()` bytes. Miri with
> Stacked Borrows rejects it whenever the buffer has spare capacity (Tree
> Borrows accepts it). It now derives the slice from
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
> unchanged for well-formed buffers. The missing clamp can't be hit with today's
> kernel address sizes, so this is hardening. `test_udp_recv_msg_multi` and
> `test_udp_recv_msg_multi_truncated_datagram` cover this path; I also ran IPv4
> copies of them, since the originals need IPv6.

---

## 9. PR: `fix/buffer-trait-soundness`

Open after 4 to 7 and 10 are merged (it is stacked on them), or open as a draft
and say so. Once they merge, only the last commit remains.

**Title:** `fix(buf)!: make the buffer traits unsafe and as_uninit an unsafe fn`

> Fixes #1053.
>
> - `as_uninit` and `iter_uninit_slice` are `unsafe fn`. The caller must not
>   de-initialize bytes below `buf_len()`.
> - `IoBuf`, `IoBufMut`, `IoVectoredBuf`, `IoVectoredBufMut` and `SetLen` are
>   `unsafe trait` again (#220, removed in #555), with the requirements unsafe
>   code relies on written down: stable pointers and lengths across calls, and
>   the initialized prefix at the start of `as_uninit`.
> - New safe `fill_from_slice` and `fill_bytes`, so filling a buffer needs no
>   `unsafe`. Two call sites moved to them.
> - A static assertion that the traits stay `unsafe`, and a test that checks the
>   in-tree buffer types against the requirements.
>
> **Breaking:** `impl IoBuf for T` becomes `unsafe impl`, and callers of
> `as_uninit` and `iter_uninit_slice` need `unsafe`. Code that only uses buffers
> is unaffected.
>
> Checked on Linux and with `--target x86_64-pc-windows-msvc`, and compio-buf's
> tests under Miri.

---

## 10. PR: `fix/copy-within-init-check`

**Title:** `fix(buf): don't let copy_within move spare capacity into the initialized prefix`

> See #1053.
>
> `IoBufMutExt::copy_within` copies within the whole buffer, including spare
> capacity, and is safe. A copy towards lower indices could move uninitialized
> bytes over `[0, buf_len())`:
>
> ```rust
> let mut v = Vec::with_capacity(8);
> v.extend_from_slice(&[1u8, 2, 3, 4]);
> v.copy_within(4..8, 0);
> let x = v[0]; // Miri: reading uninitialized memory
> ```
>
> It now panics in exactly that case. Copies within the prefix and copies into
> spare capacity work as before, including both in-tree callers. The check is
> conservative: it also refuses spare bytes the caller already wrote through
> `as_uninit` but hasn't counted in the length yet; advancing the length first
> avoids that. No API change.

---

## 11. PR: `fix/ancillary-empty-control`

**Title:** `fix(io): don't panic on empty control data`

> `recv_msg` returns an empty control buffer when a datagram carries no control
> messages, and `AncillaryIter::new` panicked on it with "buffer too short".
> compio-quic parses every received datagram this way, and only gets control
> data if its ECN or pktinfo socket options took effect.
>
> A buffer too short for a header now yields no messages. The builder keeps its
> length check. No API change.

---

## 12. PR: `fix/compio-io-rustix-net`

**Title:** `build(io): make compio-io build without other compio crates`

> rustix 1.1.5 doesn't build with only its `net` feature on Linux: the `net`
> sockopt code uses `crate::timespec`, which is gated on other features. In this
> workspace compio-driver enables more rustix features and hides it, but a crate
> that depends only on `compio-io` with `ancillary` fails to build.
>
> This enables rustix's `time` feature too, which is small and pulls in
> `timespec`. It can go once rustix fixes the gate (reported upstream: link).

---

## 13. rustix: issue or PR

Patch: `docs/rustix-timespec-net.patch` (one line in `src/lib.rs`).

**Title:** `net: build fails with only the net feature since 1.1.5`

> With `default-features = false, features = ["net", "std"]`, rustix 1.1.5
> fails to build on Linux:
>
> ```
> error[E0433]: cannot find `timespec` in the crate root
>    --> src/backend/linux_raw/net/sockopt.rs:293:39
> ```
>
> The `net` sockopt code in both backends uses `crate::timespec`, but the
> `mod timespec` gate in `lib.rs` doesn't include `net`. 1.1.4 builds. Adding
> `feature = "net"` to the gate fixes it for the linux_raw and libc backends,
> no_std and Windows.
