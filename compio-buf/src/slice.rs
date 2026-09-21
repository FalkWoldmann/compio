use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use crate::*;

/// An owned view into a contiguous sequence of bytes.
///
/// This is similar to Rust slices (`&buf[..]`) but owns the underlying buffer.
/// This type is useful for performing io-uring read and write operations using
/// a subset of a buffer.
///
/// Slices are created using [`IoBufExt::slice`].
///
/// # Examples
///
/// Creating a slice
///
/// ```
/// use compio_buf::IoBufExt;
///
/// let buf = b"hello world";
/// let slice = buf.slice(..5);
///
/// assert_eq!(&slice[..], b"hello");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slice<T> {
    buffer: T,
    begin: usize,
    end: Option<usize>,
}

impl<T> Slice<T> {
    /// # Safety
    ///
    /// User must ensure that `begin` is less than or equal to the length of the
    /// underlying buffer.
    pub(crate) unsafe fn new(buffer: T, begin: usize, end: Option<usize>) -> Self {
        Self { buffer, begin, end }
    }

    /// Offset in the underlying buffer at which this slice starts.
    pub fn begin(&self) -> usize {
        self.begin
    }

    /// Sets the begin offset of the slice.
    ///
    /// # Safety
    ///
    /// User must ensure that `begin` is less than or equal to the length of the
    /// underlying buffer.
    pub unsafe fn set_begin_unchecked(&mut self, begin: usize) {
        self.begin = begin;
    }

    /// Offset in the underlying buffer at which this slice ends.
    pub fn end(&self) -> Option<usize> {
        self.end
    }

    /// Sets the end offset of the slice.
    pub fn set_end(&mut self, end: usize) {
        self.end = Some(end);
    }

    /// Gets a reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner(&self) -> &T {
        &self.buffer
    }

    /// Gets a mutable reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner_mut(&mut self) -> &mut T {
        &mut self.buffer
    }
}

impl<T: IoBuf> Slice<T> {
    /// Sets the begin offset of the slice.
    ///
    /// # Panics
    ///
    /// Panics if `begin` is greater than the length of the underlying buffer.
    pub fn set_begin(&mut self, begin: usize) {
        assert!(begin <= self.buffer.buf_len());
        // SAFETY:
        // Operation: `Slice::set_begin_unchecked(begin)`.
        // Contract: `begin` must be at most the length of the underlying
        // buffer.
        // Evidence:
        // - LOCAL FACT: the `assert!` on the line above panics otherwise, and
        //   nothing between it and this call mutates `self` or `begin`.
        // - DEPENDENCY LEMMA: `IoBuf::buf_len` is defined as `as_init().len()`,
        //   a safe method; see `docs/soundness.md` for what that dependency
        //   does and does not buy. `Slice` stores the offset without forming a
        //   pointer from it, so an over-large `begin` gives a wrong view here
        //   rather than undefined behaviour.
        unsafe { self.set_begin_unchecked(begin) }
    }
}

impl<T: IoBuf> Slice<Slice<T>> {
    /// Flatten nested slices into a single slice.
    pub fn flatten(self) -> Slice<T> {
        let large_begin = self.buffer.begin;
        let large_end = self.buffer.end;

        let new_begin = large_begin + self.begin;
        let new_end = match (self.end, large_end) {
            (Some(small_end), Some(large_end)) => Some((large_begin + small_end).min(large_end)),
            (Some(small_end), None) => Some(large_begin + small_end),
            (None, large_end) => large_end,
        };

        // SAFETY:
        // Operation: `Slice::new(buffer, new_begin, new_end)`.
        // Contract: `new_begin` must be at most the length of `buffer`.
        // Evidence:
        // - INVARIANT: every `Slice<U>` is built with `begin <= U::buf_len()`.
        //   Applied to the outer slice, `self.begin <= self.buffer.buf_len()`,
        //   where `self.buffer` is the inner `Slice<T>`.
        // - LOCAL FACT: `Slice<T>`'s `buf_len` is `end_or_len() - begin`, and
        //   `end_or_len()` is capped at `T::buf_len()`. So `self.begin <=
        //   T::buf_len() - large_begin`, i.e. `large_begin + self.begin <=
        //   T::buf_len()`, which is exactly `new_begin`.
        unsafe { Slice::new(self.buffer.buffer, new_begin, new_end) }
    }
}

#[cfg(feature = "bytes")]
impl Slice<bytes::Bytes> {
    /// A convenient function to slice the underlying [`Bytes`] with
    /// [`Bytes::slice`].
    ///
    /// The returned `Bytes` will deref to the same byte slice as this
    /// [`Slice`].
    ///
    /// [`Bytes`]: bytes::Bytes
    /// [`Bytes::slice`]: bytes::Bytes::slice
    pub fn slice_bytes(&self) -> bytes::Bytes {
        let range = self.initialized_range();
        bytes::Bytes::slice(&self.buffer, range)
    }
}

impl<T: IoBuf> Slice<T> {
    /// Offset in the underlying buffer at which this slice ends. If it does not
    /// exist or exceeds the buffer length, returns the buffer length.
    fn end_or_len(&self) -> usize {
        let len = self.buffer.buf_len();
        self.end.unwrap_or(len).min(len)
    }

    /// Range of initialized bytes in the slice.
    fn initialized_range(&self) -> std::ops::Range<usize> {
        let end = self.end_or_len();
        self.begin..end
    }
}

impl<T: IoBufMut> Slice<T> {
    /// Offset in the underlying buffer at which this slice ends. If it does not
    /// exist or exceeds the buffer capacity, returns the buffer capacity.
    fn end_or_cap(&mut self) -> usize {
        let cap = self.buffer.buf_capacity();
        self.end.unwrap_or(cap).min(cap)
    }

    /// Full range of the slice, include uninitialized bytes.
    fn range(&mut self) -> std::ops::Range<usize> {
        let end = self.end_or_cap();
        self.begin..end
    }
}

impl<T: IoBuf> Deref for Slice<T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        let range = self.initialized_range();
        let bytes = self.buffer.as_init();
        &bytes[range]
    }
}

impl<T: IoBufMut> DerefMut for Slice<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let range = self.initialized_range();
        let bytes = self.buffer.as_mut_slice();
        &mut bytes[range]
    }
}

// SAFETY: views a fixed sub-range of the wrapped buffer. `begin` and `end`
// cannot change without `&mut self`, so the view is stable, and the wrapped
// buffer supplies validity of the bytes inside it.
unsafe impl<T: IoBuf> IoBuf for Slice<T> {
    fn as_init(&self) -> &[u8] {
        self.deref()
    }
}

// SAFETY: as for the `IoBuf` impl -- a fixed sub-range of a buffer that
// already meets these obligations. The range starts at `begin` for both
// `as_init` and `as_uninit`, so containment is preserved.
unsafe impl<T: IoBufMut> IoBufMut for Slice<T> {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let range = self.range();
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on the underlying buffer.
        // Contract: no byte below the underlying buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - LOCAL FACT: only `bytes[range]` escapes, and `range` starts at
        //   `self.begin`, so the bytes the underlying buffer holds below
        //   `begin` are never handed to the caller at all.
        // - PRECONDITION: within the returned view, this method's own contract
        //   forbids de-initializing below `Slice::buf_len()`, which is the
        //   underlying buffer's initialized bytes from `begin` onwards. The two
        //   regions are the same bytes under the shift by `begin`.
        let bytes = unsafe { self.buffer.as_uninit() };
        &mut bytes[range]
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        if self.end.is_some() {
            // Cannot reserve on a fixed-size slice
            Err(ReserveError::NotSupported)
        } else {
            self.buffer.reserve(len)
        }
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        if self.end.is_some() {
            // Cannot reserve on a fixed-size slice
            Err(ReserveExactError::NotSupported)
        } else {
            self.buffer.reserve_exact(len)
        }
    }
}

// SAFETY: shifts the length by the fixed `begin` offset and defers to the
// wrapped buffer's `set_len`, so it moves the same boundary `as_init`
// reports through this view.
unsafe impl<T: SetLen> SetLen for Slice<T> {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `SetLen::set_len(self.begin + len)` on the underlying
        // buffer.
        // Contract: `self.begin + len <= buffer.as_uninit().len()`, and the
        // bytes in `[buffer.buf_len(), self.begin + len)` are initialized.
        // Evidence:
        // - INVARIANT: `self.begin` is this slice's offset into the buffer, so
        //   byte `len` of the slice is byte `self.begin + len` of the buffer;
        //   the two obligations name the same bytes under that shift.
        // - PRECONDITION: `SetLen::set_len` on this slice requires `len <=
        //   self.as_uninit().len()` and `[self.buf_len(), len)` initialized.
        //   `Slice`'s `as_uninit` is the buffer's `as_uninit` from `begin` to
        //   `end_or_cap()`, so translating both by `begin` gives the callee's
        //   obligations.
        // - LOCAL FACT: no overflow check is made here; `begin` and `len` are
        //   both bounded by the buffer's capacity, so their sum is bounded by
        //   twice an allocation size and cannot wrap `usize`.
        unsafe { self.buffer.set_len(self.begin + len) }
    }
}

impl<T> IntoInner for Slice<T> {
    type Inner = T;

    fn into_inner(self) -> Self::Inner {
        self.buffer
    }
}

/// Return type for [`IoVectoredBuf::slice`] and
/// [`IoVectoredBufMut::slice_mut`].
///
/// # Behavior
///
/// When constructing the [`VectoredSlice`], it will first compute how
/// many buffers to skip. Imaging vectored buffers as concatenated slices, there
/// will be uninitialized "slots" in between. This slice type provides two
/// behaviors of how to skip through those slots, controlled by the marker type
/// `B`:
///
/// - [`IoVectoredBuf::slice`]: Ignore uninitialized slots, i.e., skip
///   `begin`-many **initialized** bytes.
/// - [`IoVectoredBufMut::slice_mut`]: Consider uninitialized slots, i.e., skip
///   `begin`-many bytes.
///
/// This will only affect how the slice is being constructed. The resulting
/// slice will always expose all of the remaining bytes, no matter initialized
/// or not (in particular, [`IoVectoredBufMut::iter_uninit_slice`]).
pub struct VectoredSlice<T> {
    buf: T,
    begin: usize,
    idx: usize,
    offset: usize,
}

impl<T> IntoInner for VectoredSlice<T> {
    type Inner = T;

    fn into_inner(self) -> Self::Inner {
        self.buf
    }
}

impl<T> VectoredSlice<T> {
    /// Offset in the underlying buffer at which this slice starts.
    pub fn begin(&self) -> usize {
        self.begin
    }

    /// Gets a reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner(&self) -> &T {
        &self.buf
    }

    /// Gets a mutable reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner_mut(&mut self) -> &mut T {
        &mut self.buf
    }

    pub(crate) fn new(buf: T, begin: usize, idx: usize, offset: usize) -> Self {
        Self {
            buf,
            begin,
            idx,
            offset,
        }
    }
}

// SAFETY: forwards to the wrapped buffer's implementation, which carries
// the same obligations. This wrapper stores no pointer or length of its
// own, so it cannot make successive calls disagree.
unsafe impl<T: IoVectoredBuf> IoVectoredBuf for VectoredSlice<T> {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        let mut offset = self.offset;
        self.buf.iter_slice().skip(self.idx).map(move |buf| {
            let ret = &buf[offset..];
            offset = 0;
            ret
        })
    }
}

// SAFETY: shifts by the fixed `begin` offset and defers to the wrapped
// vectored buffer's `set_len`.
unsafe impl<T: SetLen> SetLen for VectoredSlice<T> {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `SetLen::set_len(self.begin + len)` on the underlying
        // vectored buffer.
        // Contract: `self.begin + len` is within that buffer's total capacity,
        // and the bytes it adds are initialized.
        // Evidence:
        // - INVARIANT: `self.begin` is the number of bytes of the underlying
        //   vectored buffer that this slice skips, so position `len` in the
        //   slice is position `self.begin + len` in the buffer.
        // - PRECONDITION: `SetLen::set_len` on this slice carries exactly those
        //   facts for `len` in the slice's own coordinates; the shift by
        //   `begin` restates them in the buffer's.
        // - LOCAL FACT: both operands are bounded by the buffer's capacity, so
        //   the sum cannot wrap `usize`.
        unsafe { self.buf.set_len(self.begin + len) }
    }
}

// SAFETY: forwards to the wrapped buffer's implementation, which carries
// the same obligations. This wrapper stores no pointer or length of its
// own, so it cannot make successive calls disagree.
unsafe impl<T: IoVectoredBufMut> IoVectoredBufMut for VectoredSlice<T> {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        let mut offset = self.offset;
        // SAFETY:
        // Operation: `IoVectoredBufMut::iter_uninit_slice` on the underlying
        // vectored buffer.
        // Contract: no byte below any buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - LOCAL FACT: the first `idx` slices are dropped by `skip` without
        //   being written to, and each slice that does escape is narrowed to
        //   `buf[offset..]`, so bytes before the slice's start are never
        //   exposed.
        // - PRECONDITION: this method carries the same per-slice contract for
        //   everything it yields.
        unsafe { self.buf.iter_uninit_slice() }
            .skip(self.idx)
            .map(move |buf| {
                let ret = &mut buf[offset..];
                offset = 0;
                ret
            })
    }
}

#[test]
fn test_slice() {
    let buf = b"hello world";
    let slice = buf.slice(6..);
    assert_eq!(slice.as_init(), b"world");

    let slice = buf.slice(..5);
    assert_eq!(slice.as_init(), b"hello");

    let slice = buf.slice(3..8);
    assert_eq!(slice.as_init(), b"lo wo");

    let slice = buf.slice(..);
    assert_eq!(slice.as_init(), b"hello world");

    let slice = buf.slice(11..);
    assert_eq!(slice.as_init(), b"");
}
