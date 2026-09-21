use std::{iter, mem::MaybeUninit};

use crate::{IntoInner, IoBuf, IoBufMut, IoBufMutExt, SetLen, VectoredSlice, t_alloc};

/// A trait for vectored buffers.
///
/// # Safety
///
/// The slices this yields become an `iovec` array passed to the kernel, so the
/// idempotency below is a memory-safety obligation rather than a convention.
/// Implementors must ensure:
///
/// 1. **Idempotency.** The iterator always yields the same slices in the exact
///    same order, i.e. [`Iterator::enumerate`] marks the same buffer with the
///    same index, until the buffer is mutated through `&mut self`. Unsafe code
///    builds an `iovec` array from one traversal and resolves completions
///    against another.
/// 2. **Validity.** Every yielded slice satisfies [`IoBuf`]'s obligations.
pub unsafe trait IoVectoredBuf: 'static {
    /// An iterator of initialized slice of the buffers.
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]>;

    /// The total length of all buffers.
    fn total_len(&self) -> usize {
        self.iter_slice().map(|buf| buf.len()).sum()
    }

    /// Wrap self into an owned iterator.
    fn owned_iter(self) -> Result<VectoredBufIter<Self>, Self>
    where
        Self: Sized,
    {
        VectoredBufIter::new(self)
    }

    /// Get an owned view of the vectored buffer that skips the first
    /// `begin`-many **initialized** bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use compio_buf::{IoBuf, IoVectoredBuf, VectoredSlice};
    ///
    /// # fn main() {
    /// /// Create a buffer with given content and capacity.
    /// fn new_buf(slice: &[u8], cap: usize) -> Vec<u8> {
    ///     let mut buf = Vec::new();
    ///     buf.reserve_exact(cap);
    ///     buf.extend_from_slice(slice);
    ///     buf
    /// }
    ///
    /// let bufs = [new_buf(b"hello", 10), new_buf(b"world", 10)];
    /// let vectored_buf = bufs.slice(3);
    /// let mut iter = vectored_buf.iter_slice();
    /// let buf1 = iter.next().unwrap();
    /// let buf2 = iter.next().unwrap();
    /// assert_eq!(&buf1.as_init()[..], b"lo");
    /// assert_eq!(&buf2.as_init()[..], b"world");
    ///
    /// let bufs = [new_buf(b"hello", 10), new_buf(b"world", 10)];
    /// let vectored_buf = bufs.slice(6);
    /// let mut iter = vectored_buf.iter_slice();
    /// let buf1 = iter.next().unwrap();
    /// assert!(iter.next().is_none());
    /// assert_eq!(&buf1.as_init()[..], b"orld");
    /// # }
    /// ```
    fn slice(self, begin: usize) -> VectoredSlice<Self>
    where
        Self: Sized,
    {
        let mut offset = begin;
        let mut idx = 0;

        for b in self.iter_slice() {
            let len = b.len();
            if len > offset {
                break;
            }
            offset -= len;
            idx += 1;
        }

        VectoredSlice::new(self, begin, idx, offset)
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf> IoVectoredBuf for &'static [T] {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf> IoVectoredBuf for &'static mut [T] {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf, const N: usize> IoVectoredBuf for [T; N] {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf, #[cfg(feature = "allocator_api")] A: std::alloc::Allocator + 'static>
    IoVectoredBuf for t_alloc!(Vec, T, A)
{
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

#[cfg(feature = "arrayvec")]
// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf, const N: usize> IoVectoredBuf for arrayvec::ArrayVec<T, N> {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

#[cfg(feature = "smallvec")]
// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBuf, const N: usize> IoVectoredBuf for smallvec::SmallVec<[T; N]>
where
    [T; N]: smallvec::Array<Item = T>,
{
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        self.iter().map(|buf| buf.as_init())
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBuf, Rest: IoVectoredBuf> IoVectoredBuf for (T, Rest) {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        std::iter::once(self.0.as_init()).chain(self.1.iter_slice())
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBuf> IoVectoredBuf for (T,) {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        std::iter::once(self.0.as_init())
    }
}

// SAFETY: no buffers at all -- the iterator is empty and there is no length
// to move.
unsafe impl IoVectoredBuf for () {
    fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
        std::iter::empty()
    }
}

/// A static assertion that [`IoVectoredBuf`] and [`IoVectoredBufMut`] are
/// still `unsafe trait`s. See the matching assertion in `io_buf.rs`.
const _: () = {
    struct Empty;

    // SAFETY: yields no slices at all, so idempotency and validity hold
    // vacuously.
    unsafe impl IoVectoredBuf for Empty {
        fn iter_slice(&self) -> impl Iterator<Item = &[u8]> {
            iter::empty()
        }
    }

    // SAFETY: as above -- there is no length to move.
    unsafe impl SetLen for Empty {
        unsafe fn set_len(&mut self, _len: usize) {}
    }

    // SAFETY: as above -- yields no slices.
    unsafe impl IoVectoredBufMut for Empty {
        unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
            iter::empty()
        }
    }
};

/// A trait for mutable vectored buffers.
///
/// # Safety
///
/// In addition to [`IoVectoredBuf`]'s obligations, implementors must ensure
/// that `iter_uninit_slice` is idempotent in the same sense, and that each
/// yielded slice satisfies [`IoBufMut`]'s obligations against the
/// corresponding slice from [`IoVectoredBuf::iter_slice`].
pub unsafe trait IoVectoredBufMut: IoVectoredBuf + SetLen {
    /// An iterator of maybe uninitialized slice of the buffers.
    ///
    /// Each yielded slice spans one buffer's whole extent, initialized prefix
    /// included, exactly as [`IoBufMut::as_uninit`] does — and is `unsafe` for
    /// the same reason.
    ///
    /// # Safety
    ///
    /// For each yielded slice, the caller must not de-initialize any byte
    /// below that buffer's own `buf_len()`. Writing initialized values, and
    /// writing anything at or above `buf_len()`, is allowed.
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]>;

    /// The total capacity of all buffers.
    fn total_capacity(&mut self) -> usize {
        // SAFETY:
        // Operation: `IoVectoredBufMut::iter_uninit_slice`.
        // Contract: no byte below any buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - LOCAL FACT: each slice is only asked for its length and dropped.
        //   Nothing is written through any of them.
        unsafe { self.iter_uninit_slice() }
            .map(|buf| buf.len())
            .sum()
    }

    /// Get an owned view of the vectored buffer.
    ///
    /// Unlike [`IoVectoredBuf::slice`], the iterator returned by this function
    /// will skip both initialized and uninitialized bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use compio_buf::{IoBuf, IoVectoredBuf, IoVectoredBufMut, VectoredSlice};
    ///
    /// # fn main() {
    /// /// Create a buffer with given content and capacity.
    /// fn new_buf(slice: &[u8], cap: usize) -> Vec<u8> {
    ///     let mut buf = Vec::new();
    ///     buf.reserve_exact(cap);
    ///     buf.extend_from_slice(slice);
    ///     buf
    /// }
    ///
    /// let bufs = [new_buf(b"hello", 10), new_buf(b"world", 10)];
    /// let vectored_buf = bufs.slice_mut(13);
    /// let mut iter = vectored_buf.iter_slice();
    /// let buf1 = iter.next().unwrap();
    /// assert!(iter.next().is_none());
    /// assert_eq!(buf1.as_init(), b"ld");
    /// # }
    /// ```
    fn slice_mut(mut self, begin: usize) -> VectoredSlice<Self>
    where
        Self: Sized,
    {
        let mut offset = begin;
        let mut idx = 0;

        // SAFETY:
        // Operation: `IoVectoredBufMut::iter_uninit_slice`.
        // Contract: no byte below any buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - LOCAL FACT: the loop reads each slice's length to locate `begin`
        //   and writes through none of them.
        for b in unsafe { self.iter_uninit_slice() } {
            let len = b.len();
            if len > offset {
                break;
            }
            offset -= len;
            idx += 1;
        }

        VectoredSlice::new(self, begin, idx, offset)
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBufMut> IoVectoredBufMut for &'static mut [T] {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on each element.
        // Contract: the caller must not de-initialize any byte below that
        // element's own `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, stated
        //   per yielded slice, and each yielded slice is one element's own at
        //   that element's own indices. The promise transfers verbatim.
        self.iter_mut().map(|buf| unsafe { buf.as_uninit() })
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, const N: usize> IoVectoredBufMut for [T; N] {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on each element.
        // Contract: the caller must not de-initialize any byte below that
        // element's own `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, stated
        //   per yielded slice, and each yielded slice is one element's own at
        //   that element's own indices. The promise transfers verbatim.
        self.iter_mut().map(|buf| unsafe { buf.as_uninit() })
    }
}

// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, #[cfg(feature = "allocator_api")] A: std::alloc::Allocator + 'static>
    IoVectoredBufMut for t_alloc!(Vec, T, A)
{
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on each element.
        // Contract: the caller must not de-initialize any byte below that
        // element's own `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, stated
        //   per yielded slice, and each yielded slice is one element's own at
        //   that element's own indices. The promise transfers verbatim.
        self.iter_mut().map(|buf| unsafe { buf.as_uninit() })
    }
}

#[cfg(feature = "arrayvec")]
// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, const N: usize> IoVectoredBufMut for arrayvec::ArrayVec<T, N> {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on each element.
        // Contract: the caller must not de-initialize any byte below that
        // element's own `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, stated
        //   per yielded slice, and each yielded slice is one element's own at
        //   that element's own indices. The promise transfers verbatim.
        self.iter_mut().map(|buf| unsafe { buf.as_uninit() })
    }
}

#[cfg(feature = "smallvec")]
// SAFETY: iterates the container in index order, yielding each element's
// own slice. The container's length and element identity are fixed while
// borrowed, so every traversal yields the same slices in the same order,
// and each element's implementation supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, const N: usize> IoVectoredBufMut for smallvec::SmallVec<[T; N]>
where
    [T; N]: smallvec::Array<Item = T>,
{
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on each element.
        // Contract: the caller must not de-initialize any byte below that
        // element's own `buf_len()`.
        // Evidence:
        // - PRECONDITION: this method carries the identical contract, stated
        //   per yielded slice, and each yielded slice is one element's own at
        //   that element's own indices. The promise transfers verbatim.
        self.iter_mut().map(|buf| unsafe { buf.as_uninit() })
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, Rest: IoVectoredBufMut> IoVectoredBufMut for (T, Rest) {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        let (h, t) = self;
        // SAFETY: the head's slice and the tail's slices are this tuple's own
        // buffers at their own indices, so this method's per-slice contract is
        // each callee's contract verbatim.
        unsafe { iter::once(h.as_uninit()).chain(t.iter_uninit_slice()) }
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBufMut> IoVectoredBufMut for (T,) {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        // SAFETY: a one-tuple's only buffer is its element, so the contract
        // transfers verbatim.
        unsafe { iter::once(self.0.as_uninit()) }
    }
}

// SAFETY: no buffers at all -- the iterator is empty and there is no length
// to move.
unsafe impl IoVectoredBufMut for () {
    unsafe fn iter_uninit_slice(&mut self) -> impl Iterator<Item = &mut [MaybeUninit<u8>]> {
        iter::empty()
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBufMut, Rest: IoVectoredBufMut> SetLen for (T, Rest) {
    unsafe fn set_len(&mut self, len: usize) {
        let head_len = std::cmp::min(len, self.0.buf_capacity());
        let rest_len = len - head_len;

        // SAFETY:
        // Operation: `SetLen::set_len(head_len)` on the head buffer.
        // Contract: `head_len <= self.0.as_uninit().len()`, and the bytes in
        // `[self.0.buf_len(), head_len)` are initialized.
        // Evidence:
        // - LOCAL FACT: `head_len` is `min(len, self.0.buf_capacity())`, so it
        //   is at most `self.0.buf_capacity()`.
        // - DEPENDENCY LEMMA: `IoBufMut::buf_capacity` is defined as
        //   `as_uninit().len()`, which turns the line above into the first
        //   obligation. It is read here and again inside `set_len`, so this
        //   step trusts a safe implementation to answer both calls
        //   consistently; see `docs/soundness.md`.
        // - PRECONDITION: the caller promised the first `len` bytes of the
        //   tuple, taken in order, are initialized; the head holds the first
        //   `head_len` of them.
        unsafe { self.0.set_len(head_len) };
        // SAFETY:
        // Operation: `SetLen::set_len(rest_len)` on the tail.
        // Contract: `rest_len` is within the tail's total capacity, and the
        // bytes it names are initialized.
        // Evidence:
        // - LOCAL FACT: `rest_len` is `len - head_len`, which is non-zero only
        //   when `head_len` saturated at `self.0.buf_capacity()`; in that case
        //   `rest_len = len - self.0.buf_capacity()`. The subtraction cannot
        //   underflow because `head_len <= len` by construction.
        // - PRECONDITION: the caller promised `len` is at most the sum of the
        //   tuple's capacities, so subtracting the head's leaves at most the
        //   tail's sum, and the bytes named are the remainder of the
        //   initialized prefix.
        unsafe { self.1.set_len(rest_len) };
    }
}

// SAFETY: walks the cons list in a fixed order, so every traversal yields
// the same slices with the same indices; each element's implementation
// supplies the per-slice guarantees.
unsafe impl<T: IoBufMut> SetLen for (T,) {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `SetLen::set_len(len)` on the single element.
        // Contract: `len <= self.0.as_uninit().len()`, and the bytes in
        // `[self.0.buf_len(), len)` are initialized.
        // Evidence:
        // - PRECONDITION: `SetLen::set_len` on the one-tuple carries those
        //   facts for the tuple.
        // - LOCAL FACT: a one-tuple's capacity sum and initialized prefix are
        //   its element's, so there is nothing to redistribute and the
        //   obligations transfer verbatim.
        unsafe { self.0.set_len(len) };
    }
}

// SAFETY: no buffers at all -- the iterator is empty and there is no length
// to move.
unsafe impl SetLen for () {
    unsafe fn set_len(&mut self, len: usize) {
        assert_eq!(len, 0, "set_len called with non-zero len on empty buffer");
    }
}

/// An owned iterator over a vectored buffer.
///
/// Normally one would use [`IoVectoredBuf::owned_iter`] to create this
/// iterator.
pub struct VectoredBufIter<T> {
    buf: T,
    total_filled: usize,
    index: usize,
    len: usize,
    filled: usize,
}

impl<T> VectoredBufIter<T> {
    /// Create a new [`VectoredBufIter`] from an indexable container. If the
    /// container is empty, return the buffer back in `Err(T)`.
    pub fn next(mut self) -> Result<Self, T> {
        self.index += 1;
        if self.index < self.len {
            self.total_filled += self.filled;
            self.filled = 0;
            Ok(self)
        } else {
            Err(self.buf)
        }
    }
}

impl<T: IoVectoredBuf> VectoredBufIter<T> {
    fn new(buf: T) -> Result<Self, T> {
        let len = buf.iter_slice().count();
        if len > 0 {
            Ok(Self {
                buf,
                index: 0,
                len,
                total_filled: 0,
                filled: 0,
            })
        } else {
            Err(buf)
        }
    }
}

impl<T> IntoInner for VectoredBufIter<T> {
    type Inner = T;

    fn into_inner(self) -> Self::Inner {
        self.buf
    }
}

// SAFETY: yields the element at `index`, offset by `filled`. Both are
// plain fields that cannot change without `&mut self`, and
// `IoVectoredBuf`'s idempotency obligation guarantees `nth(index)` names
// the same buffer on every call -- which is what makes this stable.
unsafe impl<T: IoVectoredBuf> IoBuf for VectoredBufIter<T> {
    fn as_init(&self) -> &[u8] {
        let curr = self
            .buf
            .iter_slice()
            .nth(self.index)
            .expect("`index` should not exceed `len`");

        &curr[self.filled..]
    }
}

// SAFETY: translates the view's length into the underlying buffer's
// coordinates by adding `total_filled`, then defers to its `set_len`, so it
// moves the boundary this view reports through `as_init`.
unsafe impl<T: IoVectoredBuf + SetLen> SetLen for VectoredBufIter<T> {
    unsafe fn set_len(&mut self, len: usize) {
        self.filled = len;

        // SAFETY:
        // Operation: `SetLen::set_len(self.total_filled + self.filled)` on the
        // underlying vectored buffer.
        // Contract: that sum is within the buffer's total capacity, and the
        // bytes it adds are initialized.
        // Evidence:
        // - INVARIANT: `total_filled` is the number of bytes in the buffers
        //   this iterator has already walked past, so position `len` in the
        //   current view is position `total_filled + len` in the buffer.
        // - PRECONDITION: `SetLen::set_len` on the iterator carries those facts
        //   for `len` in the view's coordinates; `self.filled` was just
        //   assigned `len` on the line above, and the shift by `total_filled`
        //   restates them in the buffer's coordinates.
        // - LOCAL FACT: both operands are bounded by the buffer's capacity, so
        //   the sum cannot wrap `usize`.
        unsafe { self.buf.set_len(self.total_filled + self.filled) };
    }
}

// SAFETY: as for the `IoBuf` impl -- the element at `index`, named through
// `IoVectoredBufMut`'s idempotency obligation, with that element's own
// implementation supplying containment and the initialized prefix.
unsafe impl<T: IoVectoredBufMut> IoBufMut for VectoredBufIter<T> {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY:
        // Operation: `IoVectoredBufMut::iter_uninit_slice`.
        // Contract: no byte below any buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - PRECONDITION: this method carries the same contract for the one
        //   slice it returns, which is the element at `index`, unmodified. The
        //   other slices are dropped by `nth` without being written to.
        unsafe { self.buf.iter_uninit_slice() }
            .nth(self.index)
            .expect("`index` should not exceed `len`")
    }
}
