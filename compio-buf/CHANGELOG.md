# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- *(buf)* `IoBufMut::as_uninit` for `bytes::BytesMut` built a `capacity()`-long
  slice from a pointer obtained through `DerefMut`, which carries provenance
  for only `len()` bytes. Miri reported undefined behaviour whenever the buffer
  had spare capacity. The pointer now comes from `spare_capacity_mut`.

### Changed

- **BREAKING** *(buf)* `IoBuf`, `IoBufMut`, `IoVectoredBuf`, `IoVectoredBufMut`
  and `SetLen` are `unsafe trait`s again. Unsafe code passes the pointers and
  lengths these return straight to the kernel, so an implementation whose
  methods disagree with each other could cause undefined behaviour from
  entirely safe calling code. `IoBuf` and `IoBufMut` carried the marker from
  [#220](https://github.com/compio-rs/compio/issues/220) until
  [#555](https://github.com/compio-rs/compio/pull/555) removed it in favour of
  an `unsafe fn buffer` that was never added; this restores the guarantee and
  extends it to the vectored traits, whose idempotency requirement was
  previously only a note.

  Downstream implementors must write `unsafe impl` and uphold the documented
  obligations (stability across calls, containment of `as_init` within
  `as_uninit`, an initialized prefix, and idempotent vectored iteration). Code
  that only *uses* buffers is unaffected.

- **BREAKING** *(buf)* `IoBufMut::as_uninit`, `IoVectoredBufMut::iter_uninit_slice`
  and `IoBufMutExt::copy_within` are now `unsafe fn`. They expose the buffer's
  initialized prefix as `MaybeUninit`, so safe code could de-initialize bytes
  that were promised to be initialized. Callers must not de-initialize any byte
  below `buf_len()`.


## 0.8.3 - 2026-06-14

### Added

- *(buf,io)* sync with latest read-buf API ([#950](https://github.com/compio-rs/compio/pull/950))

## 0.8.2 - 2026-05-27

## 0.8.2-rc.1 - 2026-04-20

### Added

- *(buf)* add `ensure_init` for convenience ([#884](https://github.com/compio-rs/compio/pull/884))
- *(runtime)* [**breaking**] waker-based future combinator ([#825](https://github.com/compio-rs/compio/pull/825))
- organize features ([#822](https://github.com/compio-rs/compio/pull/822))
- *(buf)* add into_parts for BufResult ([#712](https://github.com/compio-rs/compio/pull/712))
- *(buf)* add support for memmap2 ([#684](https://github.com/compio-rs/compio/pull/684))

### Fixed

- unused_features ([#739](https://github.com/compio-rs/compio/pull/739))

### Other

- remove cross-rs ([#841](https://github.com/compio-rs/compio/pull/841))
- remove "authors" field in metadata ([#711](https://github.com/compio-rs/compio/pull/711))

## [0.8.0](https://github.com/compio-rs/compio/compare/v0.17.0...v0.18.0) - 2026-01-28

### Added

- *(io)* [**breaking**] support generic buffer for `Framed` ([#642](https://github.com/compio-rs/compio/pull/642))
- *(driver,poll)* multi fd ([#623](https://github.com/compio-rs/compio/pull/623))
- *(buf)* add `reserve{,exact}` to `IoBufMut` ([#578](https://github.com/compio-rs/compio/pull/578))
- [**breaking**] fs & net feature ([#564](https://github.com/compio-rs/compio/pull/564))
- *(buf)* make BufResult compatible with more Result types ([#569](https://github.com/compio-rs/compio/pull/569))

### Changed

- *(buf)* rename as_slice to as_init ([#594](https://github.com/compio-rs/compio/pull/594))
- set_buf_init ([#579](https://github.com/compio-rs/compio/pull/579))
- *(buf)* better IoBuf ([#555](https://github.com/compio-rs/compio/pull/555))

### Fixed

- *(buf,driver)* safety around `set_len` ([#585](https://github.com/compio-rs/compio/pull/585))
- *(buf)* `BorrowedCursor::advance` is unsafe ([#558](https://github.com/compio-rs/compio/pull/558))

### Other

- deploy docs ([#641](https://github.com/compio-rs/compio/pull/641))
- deny `rustdoc::broken_intra_doc_links` ([#574](https://github.com/compio-rs/compio/pull/574))
- fix broken builds ([#562](https://github.com/compio-rs/compio/pull/562))
