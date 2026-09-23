// The default reserve computes buf_capacity() - buf_len(), which wraps when
// they disagree, and extend_from_slice then writes past the allocation.
// Run in release so the subtraction wraps instead of panicking:
//   cargo miri run --release --example extend_disagree
use std::mem::MaybeUninit;

use compio_buf::{IoBuf, IoBufMut, IoBufMutExt, SetLen};

struct Lying {
    init: Vec<u8>,
    spare: Box<[MaybeUninit<u8>; 32]>,
}

impl IoBuf for Lying {
    fn as_init(&self) -> &[u8] {
        &self.init // 1024 bytes
    }
}

impl IoBufMut for Lying {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        &mut *self.spare // 32 bytes
    }
}

impl SetLen for Lying {
    unsafe fn set_len(&mut self, _: usize) {}
}

fn main() {
    let mut buf = Lying { init: vec![0; 1024], spare: Box::new([MaybeUninit::new(0); 32]) };
    buf.extend_from_slice(&[1, 2, 3, 4]).unwrap(); // UB: writes at offset 1024 of a 32-byte box
}
