use std::mem::MaybeUninit;

use compio_buf::{BufResult, IoVectoredBufMut, SetLenExt};

use crate::{AsyncBufRead, AsyncRead, IoResult};

/// A reader that infinitely repeats one byte constructed via [`repeat`].
///
/// All reads from this reader will succeed by filling the specified buffer with
/// the given byte.
///
/// # Examples
///
/// ```rust
/// # futures_executor::block_on(async {
/// use compio_io::{self, AsyncRead, AsyncReadExt};
///
/// let (len, buffer) = compio_io::repeat(42)
///     .read(Vec::with_capacity(3))
///     .await
///     .unwrap();
///
/// assert_eq!(buffer.as_slice(), [42, 42, 42]);
/// assert_eq!(len, 3);
/// # })
/// ```
pub struct Repeat(u8);

impl AsyncRead for Repeat {
    async fn read<B: compio_buf::IoBufMut>(
        &mut self,
        mut buf: B,
    ) -> compio_buf::BufResult<usize, B> {
        let slice = buf.as_uninit();

        let len = slice.len();
        slice.fill(MaybeUninit::new(self.0));
        // SAFETY: we just initialized exactly `len` bytes in `buf` from index
        // 0, so the buffer's new length is `len`.
        //
        // `advance_to`, not `advance`: `advance` is the relative form and sets
        // the length to `buf_len() + len`. The fill above starts at index 0,
        // so for a buffer that already held bytes and still had spare capacity
        // that ran the length past the allocation. `read_vectored` below
        // already used the absolute form.
        unsafe { buf.advance_to(len) };

        BufResult(Ok(len), buf)
    }

    async fn read_vectored<V: IoVectoredBufMut>(&mut self, mut buf: V) -> BufResult<usize, V> {
        let mut len: usize = 0;
        for slice in buf.iter_uninit_slice() {
            len = len
                .checked_add(slice.len())
                .expect("total vectored buffer length overflow");
            slice.fill(MaybeUninit::new(self.0));
        }
        debug_assert_eq!(len, buf.total_capacity());
        // SAFETY: every byte counted in `len` is initialized in the loop above.
        unsafe { buf.advance_vec_to(len) };

        BufResult(Ok(len), buf)
    }
}

impl AsyncBufRead for Repeat {
    async fn fill_buf(&mut self) -> IoResult<&'_ [u8]> {
        Ok(std::slice::from_ref(&self.0))
    }

    fn consume(&mut self, _: usize) {}
}

/// Creates a reader that infinitely repeats one byte.
///
/// All reads from this reader will succeed by filling the specified buffer with
/// the given byte.
///
/// # Examples
///
/// ```rust
/// # futures_executor::block_on(async {
/// use compio_io::{self, AsyncRead, AsyncReadExt};
///
/// let ((), buffer) = compio_io::repeat(42)
///     .read_exact(Vec::with_capacity(3))
///     .await
///     .unwrap();
///
/// assert_eq!(buffer.as_slice(), [42, 42, 42]);
/// # })
/// ```
pub fn repeat(byte: u8) -> Repeat {
    Repeat(byte)
}

#[cfg(test)]
mod tests {
    use compio_buf::IoBufExt;

    use crate::AsyncRead;

    /// `Repeat::read` used to call `advance(capacity)`, which sets the length
    /// to `buf_len() + capacity`. Given a buffer that already held bytes and
    /// still had spare capacity -- the ordinary shape of a partially filled
    /// read buffer -- that ran the length past the allocation, tripping
    /// `Vec::set_len requires that new_len <= capacity()`. Reachable with no
    /// `unsafe` at the call site.
    #[test]
    fn read_does_not_advance_past_capacity() {
        futures_executor::block_on(async {
            let mut v: Vec<u8> = Vec::with_capacity(13);
            v.extend_from_slice(b"abc");

            let (n, out) = crate::repeat(42).read(v).await.unwrap();

            assert_eq!(n, 13, "should report the whole extent");
            assert_eq!(out.buf_len(), 13, "length must not exceed capacity");
            assert!(out.iter().all(|&b| b == 42), "every byte overwritten");
        })
    }
}
