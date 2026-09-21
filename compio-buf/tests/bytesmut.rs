#![cfg(feature = "bytes")]

use compio_buf::*;

/// Under Miri this failed whenever the buffer had spare capacity.
#[test]
fn as_uninit_covers_the_whole_capacity() {
    for (len, cap) in [(0, 8), (3, 16), (16, 16)] {
        let mut b = bytes::BytesMut::with_capacity(cap);
        b.extend_from_slice(&vec![1u8; len]);

        let expected = b.capacity();
        let uninit = b.as_uninit();
        assert_eq!(
            uninit.len(),
            expected,
            "as_uninit must span the whole extent"
        );

        uninit.fill(std::mem::MaybeUninit::new(0xAB));
    }
}
