use std::mem::MaybeUninit;

use crate::*;

/// A [`Slice`] that only exposes uninitialized bytes.
///
/// [`Uninit`] can be created with [`IoBufMutExt::uninit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uninit<T>(Slice<T>);

impl<T: IoBufMut> Uninit<T> {
    pub(crate) fn new(buffer: T) -> Self {
        let len = buffer.buf_len();
        Self(buffer.slice(len..))
    }
}

impl<T> Uninit<T> {
    /// Offset in the underlying buffer at which uninitialized bytes starts.
    pub fn begin(&self) -> usize {
        self.0.begin()
    }

    /// Gets a reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner(&self) -> &T {
        self.0.as_inner()
    }

    /// Gets a mutable reference to the underlying buffer.
    ///
    /// This method escapes the slice's view.
    pub fn as_inner_mut(&mut self) -> &mut T {
        self.0.as_inner_mut()
    }
}

impl<T: IoBuf> IoBuf for Uninit<T> {
    fn as_init(&self) -> &[u8] {
        self.0.as_init() // this is always &[] but we can't return &[] since the pointer will be different
    }
}

impl<T: IoBufMut> IoBufMut for Uninit<T> {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let len = (*self).buf_len();
        // SAFETY:
        // Operation: `IoBufMut::as_uninit` on the wrapped buffer.
        // Contract: no byte below the wrapped buffer's `buf_len()` may be
        // de-initialized.
        // Evidence:
        // - LOCAL FACT: only `[len..]` escapes, where `len` is that same
        //   `buf_len()`. The initialized prefix is sliced off and never reaches
        //   the caller, so no call through this view can reach a byte the
        //   contract protects. `Uninit` discharges the obligation itself rather
        //   than forwarding it.
        let all = unsafe { self.0.as_uninit() };
        &mut all[len..]
    }

    fn reserve(&mut self, len: usize) -> Result<(), ReserveError> {
        IoBufMut::reserve(self.0.as_inner_mut(), len)
    }

    fn reserve_exact(&mut self, len: usize) -> Result<(), ReserveExactError> {
        IoBufMut::reserve_exact(self.0.as_inner_mut(), len)
    }
}

impl<T: SetLen + IoBuf> SetLen for Uninit<T> {
    unsafe fn set_len(&mut self, len: usize) {
        // SAFETY:
        // Operation: `SetLen::set_len(len)` on the inner buffer.
        // Contract: `len <= self.0.as_uninit().len()`, and the bytes in
        // `[self.0.buf_len(), len)` are initialized.
        // Evidence:
        // - PRECONDITION: `SetLen::set_len` on the `Uninit` wrapper carries the
        //   same two facts.
        // - INVARIANT: `Uninit` wraps the buffer without reallocating or
        //   copying it, so `len` names the same byte position in the inner
        //   buffer as it does in the wrapper, and the caller's promise carries
        //   over unchanged.
        // - LOCAL FACT: `Uninit`'s own `as_uninit` returns the tail from
        //   `buf_len()` onward, so it is shorter than the inner buffer's. That
        //   makes the first obligation strictly easier for the callee than for
        //   the caller, never harder.
        unsafe {
            self.0.set_len(len);
        }
    }
}

impl<T> IntoInner for Uninit<T> {
    type Inner = T;

    fn into_inner(self) -> Self::Inner {
        self.0.into_inner()
    }
}
