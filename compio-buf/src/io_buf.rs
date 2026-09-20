#[cfg(feature = "allocator_api")]
use std::alloc::Allocator;
use std::{error::Error, fmt::Display, mem::MaybeUninit, ops::RangeBounds, rc::Rc, sync::Arc};

use crate::*;

/// A trait for immutable buffers.
///
/// The `IoBuf` trait is implemented by buffer types that can be passed to
/// immutable completion-based IO operations, like writing its content to a
/// file. This trait will only take initialized bytes of a buffer into account.
pub trait IoBuf: 'static {
    /// Get the slice of initialized bytes.
    fn as_init(&self) -> &[u8];
}

/// A static assertion that [`IoBuf`] is dyn-compatible (object-safe).
const _: [&dyn IoBuf; 0] = [];

/// Extension trait for immutable buffers.
pub trait IoBufExt: IoBuf {
    /// Length of initialized bytes in the buffer.
    fn buf_len(&self) -> usize {
        self.as_init().len()
    }

    /// Raw pointer to the buffer.
    fn buf_ptr(&self) -> *const u8 {
        self.as_init().as_ptr()
    }

    /// Check if the buffer is empty.
    fn is_empty(&self) -> bool {
        self.buf_len() == 0
    }

    /// Returns a view of the buffer with the specified range.
    ///
    /// This method is similar to Rust's slicing (`&buf[..]`), but takes
    /// ownership of the buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use compio_buf::{IoBuf, IoBufExt};
    ///
    /// let buf = b"hello world";
    /// assert_eq!(buf.slice(6..).as_init(), b"world");
    /// ```
    ///
    /// # Panics
    /// Panics if:
    /// * begin > buf_len()
    /// * end < begin
    fn slice(self, range: impl std::ops::RangeBounds<usize>) -> Slice<Self>
    where
        Self: Sized,
    {
        use std::ops::Bound;

        let begin = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n + 1,
            Bound::Unbounded => 0,
        };

        let end = match range.end_bound() {
            Bound::Included(&n) => Some(n.checked_add(1).expect("out of range")),
            Bound::Excluded(&n) => Some(n),
            Bound::Unbounded => None,
        };

        assert!(begin <= self.buf_len());

        if let Some(end) = end {
            assert!(begin <= end);
        }

        // SAFETY:
        // Operation: `Slice::new(self, begin, end)`.
        // Contract: `begin` must be less than or equal to the length of the
        // underlying buffer.
        // Evidence:
        // - LOCAL FACT: the `assert!(begin <= self.buf_len())` three lines up
        //   panics otherwise, and nothing between it and this call mutates
        //   `self` or `begin`.
        // - DEPENDENCY LEMMA: `IoBuf::buf_len` is defined as `as_init().len()`.
        //   `as_init` is a safe method, so this step rests on the
        //   implementation being correct; see `docs/soundness.md`. `Slice` only
        //   records the offset, so an over-large `begin` yields a wrong view
        //   rather than unsoundness on its own.
        unsafe { Slice::new(self, begin, end) }
    }

    /// Create a [`Reader`] from this buffer, which implements
    /// [`std::io::Read`].
    fn into_reader(self) -> Reader<Self>
    where
        Self: Sized,
    {
        Reader::new(self)
    }

    /// Create a [`ReaderRef`] from a reference of the buffer, which
    /// implements [`std::io::Read`].
    fn as_reader(&self) -> ReaderRef<'_, Self> {
        ReaderRef::new(self)
    }
}

impl<B: IoBuf + ?Sized> IoBufExt for B {}

impl<B: IoBuf + ?Sized> IoBuf for &'static B {
    fn as_init(&self) -> &[u8] {
        (**self).as_init()
    }
}

impl<B: IoBuf + ?Sized> IoBuf for &'static mut B {
    fn as_init(&self) -> &[u8] {
        (**self).as_init()
    }
}

impl<B: IoBuf + ?Sized, #[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBuf
    for t_alloc!(Box, B, A)
{
    fn as_init(&self) -> &[u8] {
        (**self).as_init()
    }
}

impl<B: IoBuf + ?Sized, #[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBuf
    for t_alloc!(Rc, B, A)
{
    fn as_init(&self) -> &[u8] {
        (**self).as_init()
    }
}

impl IoBuf for [u8] {
    fn as_init(&self) -> &[u8] {
        self
    }
}

impl<const N: usize> IoBuf for [u8; N] {
    fn as_init(&self) -> &[u8] {
        self
    }
}

impl<#[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBuf for t_alloc!(Vec, u8, A) {
    fn as_init(&self) -> &[u8] {
        self
    }
}

impl IoBuf for str {
    fn as_init(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl IoBuf for String {
    fn as_init(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<B: IoBuf + ?Sized, #[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBuf
    for t_alloc!(Arc, B, A)
{
    fn as_init(&self) -> &[u8] {
        (**self).as_init()
    }
}

#[cfg(feature = "bytes")]
impl IoBuf for bytes::Bytes {
    fn as_init(&self) -> &[u8] {
        self
    }
}

#[cfg(feature = "bytes")]
impl IoBuf for bytes::BytesMut {
    fn as_init(&self) -> &[u8] {
        self
    }
}

#[cfg(feature = "read_buf")]
impl IoBuf for std::io::BorrowedBuf<'static, u8> {
    fn as_init(&self) -> &[u8] {
        self.filled()
    }
}

#[cfg(feature = "arrayvec")]
impl<const N: usize> IoBuf for arrayvec::ArrayVec<u8, N> {
    fn as_init(&self) -> &[u8] {
        self
    }
}

#[cfg(feature = "smallvec")]
impl<const N: usize> IoBuf for smallvec::SmallVec<[u8; N]>
where
    [u8; N]: smallvec::Array<Item = u8>,
{
    fn as_init(&self) -> &[u8] {
        self
    }
}

#[cfg(feature = "memmap2")]
impl IoBuf for memmap2::Mmap {
    fn as_init(&self) -> &[u8] {
        self
    }
}

#[cfg(feature = "memmap2")]
impl IoBuf for memmap2::MmapMut {
    fn as_init(&self) -> &[u8] {
        self
    }
}

/// An error indicating that reserving capacity for a buffer failed.
#[must_use]
#[derive(Debug)]
pub enum ReserveError {
    /// Reservation is not supported.
    NotSupported,

    /// Reservation failed.
    ///
    /// This is usually caused by out-of-memory.
    ReserveFailed(Box<dyn Error + Send + Sync>),
}

impl ReserveError {
    /// Check if the error is [`NotSupported`].
    ///
    /// [`NotSupported`]: ReserveError::NotSupported
    pub fn is_not_supported(&self) -> bool {
        matches!(self, ReserveError::NotSupported)
    }
}

impl Display for ReserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReserveError::NotSupported => write!(f, "reservation is not supported"),
            ReserveError::ReserveFailed(src) => write!(f, "reservation failed: {src}"),
        }
    }
}

impl Error for ReserveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ReserveError::ReserveFailed(src) => Some(src.as_ref()),
            _ => None,
        }
    }
}

impl From<ReserveError> for std::io::Error {
    fn from(value: ReserveError) -> Self {
        match value {
            ReserveError::NotSupported => {
                std::io::Error::new(std::io::ErrorKind::Unsupported, "reservation not supported")
            }
            ReserveError::ReserveFailed(src) => {
                std::io::Error::new(std::io::ErrorKind::OutOfMemory, src)
            }
        }
    }
}

/// An error indicating that reserving exact capacity for a buffer failed.
#[must_use]
#[derive(Debug)]
pub enum ReserveExactError {
    /// Reservation is not supported.
    NotSupported,

    /// Reservation failed.
    ///
    /// This is usually caused by out-of-memory.
    ReserveFailed(Box<dyn Error + Send + Sync>),

    /// Reserved size does not match the expected size.
    ExactSizeMismatch {
        /// Expected size to reserve
        expected: usize,

        /// Actual size reserved
        reserved: usize,
    },
}

impl ReserveExactError {
    /// Check if the error is [`NotSupported`]
    ///
    /// [`NotSupported`]: ReserveExactError::NotSupported
    pub fn is_not_supported(&self) -> bool {
        matches!(self, ReserveExactError::NotSupported)
    }
}

impl Display for ReserveExactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReserveExactError::NotSupported => write!(f, "reservation is not supported"),
            ReserveExactError::ReserveFailed(src) => write!(f, "reservation failed: {src}"),
            ReserveExactError::ExactSizeMismatch { reserved, expected } => {
                write!(
                    f,
                    "reserved size mismatch: expected {}, reserved {}",
                    expected, reserved
                )
            }
        }
    }
}

impl From<ReserveError> for ReserveExactError {
    fn from(err: ReserveError) -> Self {
        match err {
            ReserveError::NotSupported => ReserveExactError::NotSupported,
            ReserveError::ReserveFailed(src) => ReserveExactError::ReserveFailed(src),
        }
    }
}

impl Error for ReserveExactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ReserveExactError::ReserveFailed(src) => Some(src.as_ref()),
            _ => None,
        }
    }
}

impl From<ReserveExactError> for std::io::Error {
    fn from(value: ReserveExactError) -> Self {
        match value {
            ReserveExactError::NotSupported => {
                std::io::Error::new(std::io::ErrorKind::Unsupported, "reservation not supported")
            }
            ReserveExactError::ReserveFailed(src) => {
                std::io::Error::new(std::io::ErrorKind::OutOfMemory, src)
            }
            ReserveExactError::ExactSizeMismatch { expected, reserved } => std::io::Error::other(
                format!("reserved size mismatch: expected {expected}, reserved {reserved}",),
            ),
        }
    }
}

#[cfg(feature = "smallvec")]
mod smallvec_err {
    use std::{error::Error, fmt::Display};

    use smallvec::CollectionAllocErr;

    #[derive(Debug)]
    pub(super) struct SmallVecErr(pub CollectionAllocErr);

    impl Display for SmallVecErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "SmallVec allocation error: {}", self.0)
        }
    }

    impl Error for SmallVecErr {}
}

/// A trait for mutable buffers.
///
/// The `IoBufMut` trait is implemented by buffer types that can be passed to
/// mutable completion-based IO operations, like reading content from a file and
/// write to the buffer. This trait will take all space of a buffer into
/// account, including uninitialized bytes.
pub trait IoBufMut: IoBuf + SetLen {
    /// Get the full mutable slice of the buffer, including both initialized
    /// and uninitialized bytes.
    ///
    /// The returned slice spans the buffer's whole extent, so its first
    /// [`buf_len`](IoBufExt::buf_len) elements alias bytes that are already
    /// initialized and are still typed `u8`. That is why this method is
    /// `unsafe`: the type system cannot stop a caller writing
    /// `MaybeUninit::uninit()` over them, and reading those bytes back
    /// afterwards is undefined behaviour.
    ///
    /// For the safe operations, prefer:
    ///
    /// * [`as_mut_slice`](IoBufMutExt::as_mut_slice) to read or write the
    ///   initialized bytes — a `&mut [u8]` cannot de-initialize anything;
    /// * [`buf_capacity`](IoBufMutExt::buf_capacity) and
    ///   [`buf_mut_ptr`](IoBufMutExt::buf_mut_ptr) for the length and address;
    /// * [`uninit`](IoBufMutExt::uninit) for a view of the *spare* capacity
    ///   only, where writing `MaybeUninit::uninit()` is harmless.
    ///
    /// # Safety
    ///
    /// The caller must not de-initialize any byte in `[0, buf_len())` of the
    /// returned slice: every element below that index must still hold an
    /// initialized `u8` when the borrow ends. Writing initialized values, and
    /// writing anything at all at or above `buf_len()`, is allowed.
    ///
    /// # Implementor obligations
    ///
    /// These are not enforced by the signature, but unsafe code in this crate
    /// and in `compio-driver` relies on them. See `docs/soundness.md`.
    ///
    /// 1. Successive calls must return the same pointer and length until the
    ///    buffer is mutated through `&mut self`. `buf_mut_ptr` and
    ///    `buf_capacity` call this separately and are used as a pair.
    /// 2. `as_init().len() <= as_uninit().len()`, and `as_init()` must address
    ///    a prefix of the same allocation.
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>];

    /// Reserve additional capacity for the buffer.
    ///
    /// By default, this checks if the spare capacity is enough to fit in
    /// `len`-bytes. If it does, returns `Ok(())`, and otherwise returns
    /// [`Err(ReserveError::NotSupported)`]. Types that support dynamic
    /// resizing (like `Vec<u8>`) will override this method to actually
    /// reserve capacity. The return value indicates whether the reservation
    /// succeeded. See [`ReserveError`] for details.
    ///
    /// Notice that this may move the memory of the buffer, so it's UB to
    /// call this after the buffer is being pinned.
    ///
    /// [`Err(ReserveError::NotSupported)`]: ReserveError::NotSupported
    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        let init = (*self).buf_len();
        // `buf_capacity()` and `buf_len()` are safe methods on a trait anyone
        // may implement, so they need not agree; a plain subtraction here
        // wraps when they disagree and reports capacity that does not exist.
        // Saturating makes a disagreement deny the reservation instead.
        if len <= self.buf_capacity().saturating_sub(init) {
            return Ok(());
        }
        Err(ReserveError::NotSupported)
    }

    /// Reserve exactly `len` additional capacity for the buffer.
    ///
    /// By default this falls back to [`IoBufMut::reserve`]. Types that support
    /// dynamic resizing (like `Vec<u8>`) will override this method to
    /// actually reserve capacity. The return value indicates whether the
    /// exact reservation succeeded. See [`ReserveExactError`] for details.
    ///
    /// Notice that this may move the memory of the buffer, so it's UB to
    /// call this after the buffer is being pinned.
    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        self.reserve(len)?;
        Ok(())
    }
}

/// A static assertion that [`IoBufMut`] is dyn-compatible (object-safe).
const _: [&dyn IoBufMut; 0] = [];

/// Extension trait for mutable buffers.
pub trait IoBufMutExt: IoBufMut {
    /// Initialize all bytes in the buffer and return them.
    ///
    /// Bytes in the already-initialized prefix (`0..buf_len()`) are preserved.
    /// Only the uninitialized tail (`buf_len()..buf_capacity()`) is
    /// zero-initialized.
    fn ensure_init(&mut self) -> &mut [u8] {
        let len = (*self).buf_len();
        // SAFETY:
        // Operation: `IoBufMut::as_uninit`.
        // Contract: the caller must not de-initialize any byte below
        // `buf_len()`.
        // Evidence:
        // - LOCAL FACT: the only write below is `slice[len..].fill(..)`, which
        //   starts at `len == buf_len()` and so touches no byte the contract
        //   protects. The value written is `MaybeUninit::new(0)`, which is
        //   initialized in any case.
        // - TYPE FACT: the slice does not escape as `MaybeUninit`; it leaves
        //   this function as `&mut [u8]`, through which no byte can be
        //   de-initialized.
        let slice = unsafe { self.as_uninit() };
        slice[len..].fill(MaybeUninit::new(0));
        // SAFETY:
        // Operation: `<[MaybeUninit<u8>]>::assume_init_mut` on the whole slice.
        // Contract: every element must be initialized.
        // Evidence:
        // - INVARIANT: `[0, len)` is the buffer's initialized prefix, by the
        //   `IoBufMut` implementor obligations documented on `as_uninit`.
        // - LOCAL FACT: `[len..]` was just overwritten with
        //   `MaybeUninit::new(0)` by the line above, so those elements are
        //   initialized too.
        // - TYPE FACT: `slice` is borrowed from `&mut self` and nothing runs
        //   between the fill and this call, so neither fact can be invalidated.
        // Postcondition: the returned `&mut [u8]` is fully initialized, so the
        // caller may read every byte.
        unsafe { slice.assume_init_mut() }
    }

    /// Total capacity of the buffer, including both initialized and
    /// uninitialized bytes.
    fn buf_capacity(&mut self) -> usize {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit`.
        // Contract: the caller must not de-initialize any byte below
        // `buf_len()`.
        // Evidence:
        // - LOCAL FACT: the slice is only asked for its length and is dropped
        //   on the same expression. Nothing is written through it at all.
        unsafe { self.as_uninit() }.len()
    }

    /// Get the raw mutable pointer to the buffer.
    fn buf_mut_ptr(&mut self) -> *mut MaybeUninit<u8> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit`.
        // Contract: the caller must not de-initialize any byte below
        // `buf_len()`.
        // Evidence:
        // - LOCAL FACT: the slice is only asked for its address. Nothing is
        //   written through it here.
        // - TYPE FACT: what escapes is a raw pointer, and every write through a
        //   raw pointer is itself an `unsafe` operation whose caller carries
        //   this obligation. This method does not hand out the ability to
        //   de-initialize anything from safe code.
        unsafe { self.as_uninit() }.as_mut_ptr()
    }

    /// Get the mutable slice of initialized bytes. The content is the same as
    /// [`IoBuf::as_init`], but mutable.
    fn as_mut_slice(&mut self) -> &mut [u8] {
        let len = (*self).buf_len();
        // SAFETY:
        // Operation: `IoBufMut::as_uninit`.
        // Contract: the caller must not de-initialize any byte below
        // `buf_len()`.
        // Evidence:
        // - LOCAL FACT: nothing is written through `uninit` in this function.
        // - TYPE FACT: the prefix leaves as `&mut [u8]`, which cannot express
        //   an uninitialized byte, so no caller of this safe method can
        //   de-initialize through it either.
        let uninit = unsafe { self.as_uninit() };
        // `IoBuf` and `IoBufMut` are safe traits, so `buf_len()` (which comes
        // from `as_init`) can exceed what `as_uninit` actually exposes.
        // Clamping keeps an inconsistent implementation merely wrong
        // instead of unsound; the assertion makes it loud in debug
        // builds rather than silently handing back a shorter slice than
        // the caller asked for.
        debug_assert!(
            len <= uninit.len(),
            "IoBuf::as_init reports {len} initialized bytes but IoBufMut::as_uninit exposes only \
             {}; the two must describe the same buffer",
            uninit.len(),
        );
        let n = len.min(uninit.len());

        // SAFETY:
        // Operation: `<[MaybeUninit<u8>]>::assume_init_mut` on `uninit[..n]`.
        // Contract: every element of the slice must be initialized.
        // Evidence:
        // - LOCAL FACT: `n <= uninit.len()`, so the index cannot panic and the
        //   slice lies wholly inside the one `as_uninit` returned.
        // - LOCAL FACT: `n <= len`, and `len` is this buffer's initialized
        //   prefix length, so `[0, n)` is within the initialized region.
        // - TYPE FACT: `uninit` is borrowed from `&mut self` and no code runs
        //   between that borrow and this call, so nothing can shorten it.
        // Postcondition: the returned `&mut [u8]` aliases the buffer's
        // initialized prefix for the lifetime of the `&mut self` borrow.
        unsafe { uninit[..n].assume_init_mut() }
    }

    /// Extend the buffer by copying bytes from `src`.
    ///
    /// The buffer will reserve additional capacity if necessary, and return an
    /// error when reservation failed.
    ///
    /// Notice that this may move the memory of the buffer, so it's UB to
    /// call this after the buffer is being pinned.
    // FIXME: Change to `slice::write_copy_of_slice` when stabilized
    fn extend_from_slice(&mut self, src: &[u8]) -> Result<(), ReserveError> {
        let len = src.len();
        let init = (*self).buf_len();
        self.reserve(len)?;

        // `reserve` returning `Ok` is not evidence of anything: it is a safe
        // method, and its default impl only compares two other safe methods.
        // Take the destination from a single `as_uninit()` call and bound the
        // write against that slice's real length, so a buffer whose
        // `buf_len()`/`buf_capacity()` disagree with it cannot make us write
        // out of bounds.
        //
        // SAFETY: we only write initialized bytes, and only at or above
        // `buf_len()`, so no byte in `[0, buf_len())` is de-initialized.
        let uninit = unsafe { self.as_uninit() };
        let Some(dst) = uninit.get_mut(init..).and_then(|t| t.get_mut(..len)) else {
            return Err(ReserveError::NotSupported);
        };

        unsafe {
            // SAFETY:
            // Operation: `core::ptr::copy_nonoverlapping(src.as_ptr(),
            // dst.as_mut_ptr(), len)`. Contract: both pointers valid for `len`
            // bytes (read and write respectively), both aligned, and the two
            // regions must not overlap. Evidence:
            // - TYPE FACT: `dst` is a live `&mut [MaybeUninit<u8>]` of length
            //   exactly `len`, obtained by a checked slice of the buffer's own
            //   uninit view, so it is valid for `len` writes.
            // - TYPE FACT: `src` is a live `&[u8]` of length `len`, so it is
            //   valid for that many reads.
            // - AXIOM: `u8` has alignment 1, so both pointers are aligned.
            // - TYPE FACT: `src` is borrowed immutably while `self` is borrowed
            //   mutably; the two cannot alias, so the regions are disjoint.
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr() as *mut u8, len);

            // SAFETY:
            // Operation: `SetLenExt::advance_to(init + len)`.
            // Contract: `[buf_len(), init + len)` must be initialized and the
            // new length must not exceed the buffer's uninit length.
            // Evidence:
            // - POSTCONDITION: the `copy_nonoverlapping` above initialized
            //   exactly `[init, init + len)`, and `init` was `buf_len()`.
            // - POSTCONDITION: `dst` was carved out of `uninit` at range
            //   `[init, init + len)`, so `init + len <= uninit.len()`.
            self.advance_to(init + len);
        }

        Ok(())
    }

    /// Like [`slice::copy_within`], copy a range of bytes within the buffer to
    /// another location in the same buffer. This will count in both initialized
    /// and uninitialized bytes.
    ///
    /// # Panics
    ///
    /// This method will panic if the source or destination range is out of
    /// bounds.
    ///
    /// # Safety
    ///
    /// Because `src` may name uninitialized bytes, this can move
    /// uninitialized-ness *into* the initialized prefix. The caller must
    /// ensure that it does not: either every byte of `src` is initialized, or
    /// the destination range `dest..dest + src.len()` lies entirely at or
    /// above [`buf_len`](IoBufExt::buf_len).
    ///
    /// [`slice::copy_within`]: https://doc.rust-lang.org/std/primitive.slice.html#method.copy_within
    unsafe fn copy_within<R>(&mut self, src: R, dest: usize)
    where
        R: RangeBounds<usize>,
    {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit`, then `<[MaybeUninit<u8>]>::
        // copy_within`.
        // Contract: `as_uninit` requires that no byte below `buf_len()` be
        // de-initialized.
        // Evidence:
        // - PRECONDITION: this method's own `# Safety` section requires that
        //   the copy either carries initialized bytes or lands entirely at or
        //   above `buf_len()`. In the first case every byte written is
        //   initialized; in the second no byte below `buf_len()` is written.
        //   Either way the callee's obligation holds.
        unsafe { self.as_uninit() }.copy_within(src, dest);
    }

    /// Returns an [`Uninit`], which is a [`Slice`] that only exposes
    /// uninitialized bytes.
    ///
    /// It will always point to the uninitialized area of a [`IoBufMut`] even
    /// after reading in some bytes, which is done by [`SetLen`]. This
    /// is useful for writing data into buffer without overwriting any
    /// existing bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use compio_buf::{IoBuf, IoBufMut, IoBufMutExt};
    ///
    /// let mut buf = Vec::from(b"hello world");
    /// buf.reserve_exact(10);
    /// let mut slice = buf.uninit();
    ///
    /// assert_eq!(slice.as_init(), b"");
    /// assert_eq!(slice.buf_capacity(), 10);
    /// ```
    fn uninit(self) -> Uninit<Self>
    where
        Self: Sized,
    {
        Uninit::new(self)
    }

    /// Create a [`Writer`] from this buffer, which implements
    /// [`std::io::Write`].
    fn into_writer(self) -> Writer<Self>
    where
        Self: Sized,
    {
        Writer::new(self)
    }

    /// Create a [`Writer`] from a mutable reference of the buffer, which
    /// implements [`std::io::Write`].
    fn as_writer(&mut self) -> WriterRef<'_, Self> {
        WriterRef::new(self)
    }

    /// Indicate whether the buffer has been filled (uninit portion is empty)
    fn is_filled(&mut self) -> bool {
        let len = (*self).as_init().len();
        let cap = (*self).buf_capacity();
        len == cap
    }
}

impl<B: IoBufMut + ?Sized> IoBufMutExt for B {}

impl<B: IoBufMut + ?Sized> IoBufMut for &'static mut B {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on the wrapped buffer.
        // Contract: the caller must not de-initialize any byte below that
        // buffer's `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, and the
        //   wrapper exposes the wrapped buffer's bytes unchanged, at the same
        //   indices. So the caller's promise is exactly the promise this call
        //   needs, with nothing added or relaxed.
        unsafe { (**self).as_uninit() }
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        (**self).reserve(len)
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        (**self).reserve_exact(len)
    }
}

impl<B: IoBufMut + ?Sized, #[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBufMut
    for t_alloc!(Box, B, A)
{
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on the wrapped buffer.
        // Contract: the caller must not de-initialize any byte below that
        // buffer's `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, and the
        //   wrapper exposes the wrapped buffer's bytes unchanged, at the same
        //   indices. So the caller's promise is exactly the promise this call
        //   needs, with nothing added or relaxed.
        unsafe { (**self).as_uninit() }
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        (**self).reserve(len)
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        (**self).reserve_exact(len)
    }
}

impl<#[cfg(feature = "allocator_api")] A: Allocator + 'static> IoBufMut for t_alloc!(Vec, u8, A) {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        let cap = self.capacity();
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // cap)`. Contract: `ptr` must be non-null, aligned, and valid
        // for reads and writes of `cap` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - DEPENDENCY LEMMA: `Vec::as_mut_ptr` is valid for `capacity()`
        //   elements in a single allocation, whose size std bounds by
        //   `isize::MAX`.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, cap) }
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        if let Err(e) = Vec::try_reserve(self, len) {
            return Err(ReserveError::ReserveFailed(Box::new(e)));
        }

        Ok(())
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        if self.capacity() - self.len() >= len {
            return Ok(());
        }

        if let Err(e) = Vec::try_reserve_exact(self, len) {
            return Err(ReserveExactError::ReserveFailed(Box::new(e)));
        }

        if self.capacity() - self.len() != len {
            return Err(ReserveExactError::ExactSizeMismatch {
                reserved: self.capacity() - self.len(),
                expected: len,
            });
        }
        Ok(())
    }
}

impl IoBufMut for [u8] {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        let len = self.len();
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // len)`. Contract: `ptr` must be non-null, aligned, and valid
        // for reads and writes of `len` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - TYPE FACT: `ptr` and `len` come from the same `&mut [u8]`, so they
        //   describe exactly one live, initialized allocation.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }
}

impl<const N: usize> IoBufMut for [u8; N] {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // N)`. Contract: `ptr` must be non-null, aligned, and valid for
        // reads and writes of `N` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - TYPE FACT: the array is `[u8; N]`, so `ptr` is valid for exactly
        //   `N` initialized elements for as long as the borrow lives.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, N) }
    }
}

#[cfg(feature = "bytes")]
impl IoBufMut for bytes::BytesMut {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        let cap = self.capacity();
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // cap)`. Contract: `ptr` must be non-null, aligned, and valid
        // for reads and writes of `cap` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - DEPENDENCY LEMMA: `BytesMut::as_mut_ptr` is valid for `capacity()`
        //   bytes in one allocation.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, cap) }
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        bytes::BytesMut::reserve(self, len);
        Ok(())
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        if self.capacity() - self.len() >= len {
            return Ok(());
        }

        bytes::BytesMut::reserve(self, len);

        if self.capacity() - self.len() != len {
            Err(ReserveExactError::ExactSizeMismatch {
                reserved: self.capacity() - self.len(),
                expected: len,
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(feature = "read_buf")]
impl IoBufMut for std::io::BorrowedBuf<'static, u8> {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let total_cap = self.capacity();

        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>`.
        // Contract: as for the other `as_uninit` impls above.
        // Evidence:
        // - DEPENDENCY LEMMA: `BorrowedBuf::filled` returns a slice starting at
        //   the beginning of the underlying buffer, so its pointer is the
        //   buffer's base address, and `capacity()` bytes are live from there.
        // - AXIOM: `MaybeUninit<u8>` matches `u8` in size and alignment, and
        //   `u8` is aligned at 1.
        // - TYPE FACT: the returned lifetime is tied to `&mut self`.
        unsafe {
            let filled_ptr = self.filled().as_ptr() as *mut MaybeUninit<u8>;
            std::slice::from_raw_parts_mut(filled_ptr, total_cap)
        }
    }
}

#[cfg(feature = "arrayvec")]
impl<const N: usize> IoBufMut for arrayvec::ArrayVec<u8, N> {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // N)`. Contract: `ptr` must be non-null, aligned, and valid for
        // reads and writes of `N` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - DEPENDENCY LEMMA: `ArrayVec<u8, N>` stores its elements inline, so
        //   `as_mut_ptr` is valid for all `N` of them.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, N) }
    }
}

#[cfg(feature = "smallvec")]
impl<const N: usize> IoBufMut for smallvec::SmallVec<[u8; N]>
where
    [u8; N]: smallvec::Array<Item = u8>,
{
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let ptr = self.as_mut_ptr() as *mut MaybeUninit<u8>;
        let cap = self.capacity();
        // SAFETY:
        // Operation: `core::slice::from_raw_parts_mut::<MaybeUninit<u8>>(ptr,
        // cap)`. Contract: `ptr` must be non-null, aligned, and valid
        // for reads and writes of `cap` elements in one allocation; the
        // referenced memory must not be accessed through any other
        // pointer for the returned lifetime; and the size in bytes must
        // not exceed `isize::MAX`. Evidence:
        // - DEPENDENCY LEMMA: `SmallVec::as_mut_ptr` is valid for `capacity()`
        //   bytes, whether the data is inline or spilled to the heap.
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so the cast preserves both, and `u8`'s alignment of 1 makes
        //   any non-null address aligned.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so the
        //   initialization obligation is discharged for any live bytes.
        // - TYPE FACT: `ptr` is derived from `&mut self`, and the returned
        //   lifetime is tied to that borrow, so no other pointer may access the
        //   region while it lives.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the initialized prefix stays initialized.
        unsafe { std::slice::from_raw_parts_mut(ptr, cap) }
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        if let Err(e) = smallvec::SmallVec::try_reserve(self, len) {
            return Err(ReserveError::ReserveFailed(Box::new(
                smallvec_err::SmallVecErr(e),
            )));
        }
        Ok(())
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        if self.capacity() - self.len() >= len {
            return Ok(());
        }

        if let Err(e) = smallvec::SmallVec::try_reserve_exact(self, len) {
            return Err(ReserveExactError::ReserveFailed(Box::new(
                smallvec_err::SmallVecErr(e),
            )));
        }

        if self.capacity() - self.len() != len {
            return Err(ReserveExactError::ExactSizeMismatch {
                reserved: self.capacity() - self.len(),
                expected: len,
            });
        }
        Ok(())
    }
}

#[cfg(feature = "memmap2")]
impl IoBufMut for memmap2::MmapMut {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        // Operation: `core::mem::transmute::<&mut [u8], &mut
        // [MaybeUninit<u8>]>`. Contract: source and destination must
        // have the same size, and the source value must be valid at the
        // destination type. Evidence:
        // - AXIOM: `MaybeUninit<u8>` has the same size and alignment as `u8`
        //   (std), so `[u8]` and `[MaybeUninit<u8>]` have the same layout and
        //   their references the same size and metadata.
        // - AXIOM: every byte pattern is a valid `MaybeUninit<u8>`, so every
        //   `u8` in the source is valid at the destination type.
        // - CALLER CONTRACT: `as_uninit` is an `unsafe fn` whose `# Safety`
        //   section forbids de-initializing any byte in `[0, buf_len())`, so
        //   the caller may not write `MaybeUninit::uninit()` back over the
        //   mapping's initialized bytes.
        unsafe { std::mem::transmute(self.as_mut()) }
    }
}

/// A helper trait for `set_len` like methods.
pub trait SetLen {
    /// Set the buffer length.
    ///
    /// # Safety
    ///
    /// * `len` must be less or equal than `as_uninit().len()`.
    /// * The bytes in the range `[buf_len(), len)` must be initialized.
    unsafe fn set_len(&mut self, len: usize);
}

/// A static assertion that [`SetLen`] is dyn-compatible (object-safe).
const _: [&dyn SetLen; 0] = [];

/// Extension trait for `set_len` like methods.
pub trait SetLenExt: SetLen {
    /// Advance the buffer length by `len`.
    ///
    /// # Safety
    ///
    /// * The bytes in the range `[buf_len(), buf_len() + len)` must be
    ///   initialized.
    unsafe fn advance(&mut self, len: usize)
    where
        Self: IoBuf,
    {
        let current_len = (*self).buf_len();
        let new_len = current_len.checked_add(len).expect("length overflow");
        // SAFETY:
        // Operation: `SetLen::set_len(new_len)`.
        // Contract: `new_len <= as_uninit().len()` and `[buf_len(), new_len)`
        // must be initialized.
        // Evidence:
        // - PRECONDITION: this function's own `# Safety` section requires both
        //   of exactly those facts for `buf_len() + len`.
        // - LOCAL FACT: `new_len` is that sum, computed with `checked_add`, so
        //   it cannot have wrapped to a smaller value that would silently
        //   satisfy the bound while naming different bytes.
        unsafe { self.set_len(new_len) };
    }

    /// Set the buffer length to `len`. If `len` is less than the current
    /// length, this operation is a no-op.
    ///
    /// # Safety
    ///
    /// * `len` must be less or equal than `as_uninit().len()`.
    /// * The bytes in the range `[buf_len(), len)` must be initialized.
    unsafe fn advance_to(&mut self, len: usize)
    where
        Self: IoBuf,
    {
        let current_len = (*self).buf_len();
        if len > current_len {
            // SAFETY:
            // Operation: `SetLen::set_len(len)`.
            // Contract: `len <= as_uninit().len()` and `[buf_len(), len)`
            // initialized.
            // Evidence:
            // - PRECONDITION: this function's `# Safety` section states both.
            // - LOCAL FACT: the `if` restricts this to `len > current_len`, so
            //   the range is non-empty; the shrinking case never reaches here.
            unsafe { self.set_len(len) };
        }
    }

    /// Set the vector buffer's total length to `len`. If `len` is less than the
    /// current total length, this operation is a no-op.
    ///
    /// # Safety
    ///
    /// * `len` must be less or equal than `total_len()`.
    /// * The bytes in the range `[total_len(), len)` must be initialized.
    unsafe fn advance_vec_to(&mut self, len: usize)
    where
        Self: IoVectoredBuf,
    {
        let current_len = (*self).total_len();
        if len > current_len {
            // SAFETY:
            // Operation: `SetLen::set_len(len)` on a vectored buffer.
            // Contract: `len <= total_len()` and `[total_len(), len)` must be
            // initialized across the constituent buffers.
            // Evidence:
            // - PRECONDITION: this function's `# Safety` section states both.
            // - LOCAL FACT: the `if` restricts this to the growing case.
            unsafe { self.set_len(len) };
        }
    }

    /// Clear the buffer, setting its length to 0 without touching its content
    /// or capacity.
    fn clear(&mut self)
    where
        Self: IoBuf,
    {
        // SAFETY:
        // Operation: `SetLen::set_len(0)`.
        // Contract: `0 <= as_uninit().len()`, and `[buf_len(), 0)` initialized.
        // Evidence:
        // - AXIOM: `0` is `<=` any `usize`.
        // - LOCAL FACT: the range `[buf_len(), 0)` is empty whenever `buf_len()
        //   >= 0`, which every `usize` is, so there is nothing to initialize.
        //   This is why `clear` can be a safe function.
        unsafe { self.set_len(0) };
    }
}

impl<B: SetLen + ?Sized> SetLenExt for B {}

impl<B: SetLen + ?Sized> SetLen for &'static mut B {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `<B as SetLen>::set_len(len)` on the wrapped buffer.
        // Contract: `len <= as_uninit().len()` and `[buf_len(), len)` must be
        // initialized — both stated about `**self`.
        // Evidence:
        // - PRECONDITION: the caller discharged those same two obligations for
        //   this wrapper.
        // - TYPE FACT: this wrapper's `as_uninit` and `buf_len` are themselves
        //   forwarding impls that delegate to `**self`, so the caller's
        //   statement about the wrapper *is* the statement about `**self`; the
        //   obligation is not merely passed on, it is the identical claim.
        unsafe { (**self).set_len(len) }
    }
}

impl<B: SetLen + ?Sized, #[cfg(feature = "allocator_api")] A: Allocator + 'static> SetLen
    for t_alloc!(Box, B, A)
{
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `<B as SetLen>::set_len(len)` on the wrapped buffer.
        // Contract: `len <= as_uninit().len()` and `[buf_len(), len)` must be
        // initialized — both stated about `**self`.
        // Evidence:
        // - PRECONDITION: the caller discharged those same two obligations for
        //   this wrapper.
        // - TYPE FACT: this wrapper's `as_uninit` and `buf_len` are themselves
        //   forwarding impls that delegate to `**self`, so the caller's
        //   statement about the wrapper *is* the statement about `**self`; the
        //   obligation is not merely passed on, it is the identical claim.
        unsafe { (**self).set_len(len) }
    }
}

impl<#[cfg(feature = "allocator_api")] A: Allocator + 'static> SetLen for t_alloc!(Vec, u8, A) {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `Vec::<u8>::set_len`, the inherent method, not the trait
        // one. Contract: `len <= capacity()`, and every element below
        //   `len` must be initialized.
        // Evidence:
        // - PRECONDITION: this trait method's `# Safety` requires `len <=
        //   as_uninit().len()` and `[buf_len(), len)` initialized.
        // - DEPENDENCY LEMMA: this type's `as_uninit` exposes its full capacity
        //   and `buf_len` its current length, so the two contracts state the
        //   same requirement in different words.
        unsafe { self.set_len(len) };
    }
}

impl SetLen for [u8] {
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.len());
    }
}

impl<const N: usize> SetLen for [u8; N] {
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= N);
    }
}

#[cfg(feature = "bytes")]
impl SetLen for bytes::BytesMut {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `BytesMut::set_len`, the inherent method, not the trait
        // one. Contract: `len` must not exceed capacity, and the bytes
        //   below it must be initialized.
        // Evidence:
        // - PRECONDITION: this trait method's `# Safety` requires `len <=
        //   as_uninit().len()` and `[buf_len(), len)` initialized.
        // - DEPENDENCY LEMMA: this type's `as_uninit` exposes its full capacity
        //   and `buf_len` its current length, so the two contracts state the
        //   same requirement in different words.
        unsafe { self.set_len(len) };
    }
}

#[cfg(feature = "read_buf")]
impl SetLen for std::io::BorrowedBuf<'static, u8> {
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(self.capacity() >= len);

        // SAFETY:
        // Operation: `BorrowedBuf::clear` then `BorrowedCursor::advance(len)`.
        // Contract: `advance` requires that the first `len` bytes of the
        // cursor's unfilled part are initialized.
        // Evidence:
        // - PRECONDITION: `SetLen::set_len` requires the bytes in `[buf_len(),
        //   len)` to be initialized and `len <= as_uninit().len()`. The `IoBuf`
        //   impl above defines `as_init` as `filled()`, and the `IoBufMut` impl
        //   exposes the whole capacity, so together with the bytes already
        //   filled this makes `[0, len)` initialized.
        // - AXIOM: `BorrowedBuf::clear` is documented to reset the filled
        //   length to zero while leaving the initialized region intact, so the
        //   cursor returned by `unfilled()` starts at offset 0 and its first
        //   `len` bytes are exactly the bytes shown initialized above.
        // - LOCAL FACT: the `debug_assert!` above documents `len <=
        //   capacity()`; the initialization argument is what `advance` needs.
        #[allow(unused_unsafe)]
        unsafe {
            self.clear().unfilled().advance(len)
        };
    }
}

#[cfg(feature = "arrayvec")]
impl<const N: usize> SetLen for arrayvec::ArrayVec<u8, N> {
    unsafe fn set_len(&mut self, len: usize) {
        if (**self).buf_len() < len {
            // SAFETY:
            // Operation: `ArrayVec::<u8, N>::set_len(len)`.
            // Contract: `len <= N`, and the elements below it initialized.
            // Evidence:
            // - PRECONDITION: the trait's `# Safety` gives `len <=
            //   as_uninit().len()`, which this impl reports as `N`.
            // - LOCAL FACT: the enclosing `if` restricts this to the growing
            //   case, so no already-counted element is dropped from the length.
            unsafe { self.set_len(len) };
        }
    }
}

#[cfg(feature = "smallvec")]
impl<const N: usize> SetLen for smallvec::SmallVec<[u8; N]>
where
    [u8; N]: smallvec::Array<Item = u8>,
{
    unsafe fn set_len(&mut self, len: usize) {
        if (**self).buf_len() < len {
            // SAFETY:
            // Operation: `SmallVec::<[u8; N]>::set_len(len)`.
            // Contract: `len <= capacity()`, and the elements below it
            // initialized. Evidence:
            // - PRECONDITION: the trait's `# Safety` gives `len <=
            //   as_uninit().len()`, which this impl reports as `capacity()`.
            // - LOCAL FACT: the enclosing `if` restricts this to the growing
            //   case, so no already-counted element is dropped from the length.
            unsafe { self.set_len(len) };
        }
    }
}

#[cfg(feature = "memmap2")]
impl SetLen for memmap2::MmapMut {
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.len())
    }
}

impl<T: IoBufMut> SetLen for [T] {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `default_set_len(self.iter_mut(), len)`.
        // Contract: `len` is at most the sum of the elements'
        // `buf_capacity()`, and for each element the bytes in
        // `[buf_len(), new_len)` are initialized.
        // Evidence:
        // - PRECONDITION: `SetLen::set_len` states the same two facts. It
        //   phrases the first as `len <= as_uninit().len()`; `[T]` is a
        //   vectored buffer and has no `as_uninit` of its own, so the sum over
        //   its elements is the only available reading. That the trait's
        //   wording does not cover vectored implementors is a documentation
        //   gap, not a second contract.
        // - LOCAL FACT: `iter_mut()` yields every element exactly once and in
        //   order, so the sum the callee walks is the sum the caller promised.
        unsafe { default_set_len(self.iter_mut(), len) }
    }
}

impl<T: IoBufMut, const N: usize> SetLen for [T; N] {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the `[T]` impl above, unchanged: `iter_mut()` yields each
        // element exactly once in order, so the caller's sum-of-capacities
        // obligation is the sum `default_set_len` walks.
        unsafe { default_set_len(self.iter_mut(), len) }
    }
}

impl<T: IoBufMut, #[cfg(feature = "allocator_api")] A: Allocator + 'static> SetLen
    for t_alloc!(Vec, T, A)
{
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the `[T]` impl above, unchanged: `iter_mut()` yields each
        // element exactly once in order, so the caller's sum-of-capacities
        // obligation is the sum `default_set_len` walks.
        unsafe { default_set_len(self.iter_mut(), len) }
    }
}

#[cfg(feature = "arrayvec")]
impl<T: IoBufMut, const N: usize> SetLen for arrayvec::ArrayVec<T, N> {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the `[T]` impl above, unchanged: `iter_mut()` yields each
        // element exactly once in order, so the caller's sum-of-capacities
        // obligation is the sum `default_set_len` walks.
        unsafe { default_set_len(self.iter_mut(), len) }
    }
}

#[cfg(feature = "smallvec")]
impl<T: IoBufMut, const N: usize> SetLen for smallvec::SmallVec<[T; N]>
where
    [T; N]: smallvec::Array<Item = T>,
{
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the `[T]` impl above, unchanged: `iter_mut()` yields each
        // element exactly once in order, so the caller's sum-of-capacities
        // obligation is the sum `default_set_len` walks.
        unsafe { default_set_len(self.iter_mut(), len) }
    }
}

/// # Safety
/// * `len` should be less or equal than the sum of `buf_capacity()` of all
///   buffers.
/// * The bytes in the range `[buf_len(), new_len)` of each buffer must be
///   initialized
unsafe fn default_set_len<'a, B: IoBufMut>(
    iter: impl IntoIterator<Item = &'a mut B>,
    mut len: usize,
) {
    let mut iter = iter.into_iter();
    while len > 0 {
        let Some(curr) = iter.next() else { return };
        let sub = (*curr).buf_capacity().min(len);
        // SAFETY:
        // Operation: `SetLen::set_len(sub)` on `curr`.
        // Contract: `sub <= curr.as_uninit().len()`, and the bytes in
        // `[curr.buf_len(), sub)` are initialized.
        // Evidence:
        // - LOCAL FACT: `sub` is `(*curr).buf_capacity().min(len)`, so `sub <=
        //   curr.buf_capacity()`.
        // - DEPENDENCY LEMMA: `IoBufMut::buf_capacity` is defined as
        //   `as_uninit().len()`, which turns the line above into the first
        //   obligation. `buf_capacity` and `as_uninit` are safe methods and are
        //   called separately here and inside `set_len`, so this step trusts
        //   the implementation to answer consistently; that assumption is the
        //   subject of `docs/soundness.md` and is not discharged here.
        // - PRECONDITION: this function's `# Safety` section requires the bytes
        //   in `[buf_len(), new_len)` of each buffer to be initialized, and
        //   `sub` is the length this loop assigns to `curr`.
        unsafe { curr.set_len(sub) };
        len -= sub;
    }
}

#[cfg(test)]
mod test {
    use crate::{IoBufMut, IoBufMutExt};

    #[test]
    fn test_vec_reserve() {
        let mut buf = Vec::new();
        IoBufMut::reserve(&mut buf, 10).unwrap();
        assert!(buf.capacity() >= 10);

        let mut buf = Vec::new();
        IoBufMut::reserve_exact(&mut buf, 10).unwrap();
        assert!(buf.capacity() == 10);

        let mut buf = Box::new(Vec::new());
        IoBufMut::reserve_exact(&mut buf, 10).unwrap();
        assert!(buf.capacity() == 10);
    }

    #[test]
    #[cfg(feature = "bytes")]
    fn test_bytes_reserve() {
        let mut buf = bytes::BytesMut::new();
        IoBufMut::reserve(&mut buf, 10).unwrap();
        assert!(buf.capacity() >= 10);
    }

    #[test]
    #[cfg(feature = "smallvec")]
    fn test_smallvec_reserve() {
        let mut buf = smallvec::SmallVec::<[u8; 8]>::new();
        IoBufMut::reserve(&mut buf, 10).unwrap();
        assert!(buf.capacity() >= 10);
    }

    #[test]
    #[cfg(feature = "memmap2")]
    fn tests_memmap2() {
        use std::{
            fs::{OpenOptions, remove_file},
            io::{Seek, SeekFrom, Write},
        };

        use memmap2::MmapOptions;

        use super::*;

        let path = std::env::temp_dir().join("compio_buf_mmap_mut_test");

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let data = b"hello memmap2";
        file.write_all(data).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        {
            // SAFETY: mapping a file is unsafe because another process may
            // mutate it underneath the mapping. `file` is this test's own
            // freshly created temporary, which nothing else has a handle to.
            let mmap = unsafe { MmapOptions::new().map(&file).unwrap() };

            let init_slice = mmap.as_init();
            assert_eq!(init_slice, data);
        }

        {
            // SAFETY: as above - the file is this test's own temporary and is
            // not mapped or written by anything else.
            let mut mmap = unsafe { MmapOptions::new().map_mut(&file).unwrap() };

            let init_slice = mmap.as_mut_slice();
            assert_eq!(init_slice, data);
        }

        remove_file(path).unwrap();
    }

    #[test]
    fn test_other_reserve() {
        let mut buf = [1, 1, 4, 5, 1, 4];
        let res = IoBufMut::reserve(&mut buf, 10);
        assert!(res.is_err_and(|x| x.is_not_supported()));
        assert!(buf.buf_capacity() == 6);
    }

    #[test]
    fn test_extend() {
        let mut buf = Vec::from(b"hello");
        IoBufMutExt::extend_from_slice(&mut buf, b" world").unwrap();
        assert_eq!(buf.as_slice(), b"hello world");

        let mut buf = [];
        let res = IoBufMutExt::extend_from_slice(&mut buf, b" ");
        assert!(res.is_err_and(|x| x.is_not_supported()));
    }
}

#[cfg(test)]
mod soundness_tests {
    use std::mem::MaybeUninit;

    use crate::*;

    /// `IoBuf`/`IoBufMut` are safe traits, so an implementation can report a
    /// longer initialized prefix than it actually exposes. `as_mut_slice` used
    /// to build a slice from `buf_len()` and `as_uninit()`'s pointer, which put
    /// the resulting `&mut [u8]` past the end of the allocation. It now clamps.
    struct Inconsistent {
        storage: [MaybeUninit<u8>; 8],
        claimed: Vec<u8>,
    }

    impl IoBuf for Inconsistent {
        fn as_init(&self) -> &[u8] {
            &self.claimed
        }
    }

    impl SetLen for Inconsistent {
        unsafe fn set_len(&mut self, _len: usize) {}
    }

    impl IoBufMut for Inconsistent {
        unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
            &mut self.storage
        }
    }

    fn inconsistent() -> Inconsistent {
        Inconsistent {
            storage: [MaybeUninit::new(0); 8],
            claimed: vec![0; 1000],
        }
    }

    /// Debug builds should say so loudly rather than quietly hand back a
    /// shorter slice than `buf_len()` advertised.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "describe the same buffer")]
    fn as_mut_slice_asserts_when_the_impls_disagree() {
        let _ = inconsistent().as_mut_slice();
    }

    /// The soundness property, which must hold with assertions compiled out:
    /// the slice never runs past what `as_uninit` actually exposes. Before the
    /// clamp this produced a 1000-byte slice over 8 bytes of storage, which
    /// Miri reported as a dangling reference.
    #[test]
    #[cfg(not(debug_assertions))]
    fn as_mut_slice_clamps_when_the_impls_disagree() {
        assert_eq!(
            inconsistent().as_mut_slice().len(),
            8,
            "as_mut_slice must not exceed what as_uninit exposes"
        );
    }
}

#[cfg(test)]
mod tests_disagree_extend {
    use std::mem::MaybeUninit;

    use crate::*;

    /// A buffer whose two halves describe different memory: `as_init` reports
    /// a real 1000-byte allocation, `as_uninit` a real 8-byte one. Both
    /// slices are valid, so the type itself is not UB -- the two methods
    /// simply disagree, which safe code is free to do.
    struct Inconsistent {
        init: Vec<u8>,
        uninit: [u8; 8],
    }

    impl IoBuf for Inconsistent {
        fn as_init(&self) -> &[u8] {
            &self.init
        }
    }

    impl SetLen for Inconsistent {
        unsafe fn set_len(&mut self, _len: usize) {}
    }

    impl IoBufMut for Inconsistent {
        unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
            // SAFETY: `self.uninit` is a live `[u8; 8]` borrowed mutably for
            // the returned lifetime, and every `u8` is a valid
            // `MaybeUninit<u8>`, so the 8-element slice is in bounds and
            // correctly typed.
            unsafe {
                std::slice::from_raw_parts_mut(self.uninit.as_mut_ptr() as *mut MaybeUninit<u8>, 8)
            }
        }
    }

    /// `buf_len()` is 1000 and `buf_capacity()` is 8. The old
    /// `buf_capacity() - init` wrapped to ~1.8e19, so `reserve` said Ok and
    /// `extend_from_slice` wrote 992 bytes past an 8-byte allocation. It must
    /// refuse instead. Entirely safe code: no `unsafe` at this call site.
    #[test]
    fn extend_from_slice_refuses_when_the_impls_disagree() {
        let mut buf = Inconsistent {
            init: vec![0; 1000],
            uninit: [0; 8],
        };
        assert!(buf.extend_from_slice(b"abcd").is_err());
    }

    /// The same disagreement must not make `reserve` itself claim capacity.
    #[test]
    fn reserve_refuses_when_the_impls_disagree() {
        let mut buf = Inconsistent {
            init: vec![0; 1000],
            uninit: [0; 8],
        };
        assert!(buf.reserve(4).is_err());
    }
}
