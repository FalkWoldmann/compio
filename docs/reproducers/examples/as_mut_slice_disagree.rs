// as_mut_slice takes its length from as_init and its pointer from as_uninit.
// A safe IoBuf/IoBufMut impl where they disagree gives a slice past the
// allocation. The only `unsafe` is the empty set_len the trait requires.
use std::mem::MaybeUninit;

use compio_buf::{IoBuf, IoBufMut, IoBufMutExt, SetLen};

struct Lying {
    init: Vec<u8>,
    spare: [MaybeUninit<u8>; 8],
}

impl IoBuf for Lying {
    fn as_init(&self) -> &[u8] {
        &self.init // 1000 bytes
    }
}

impl IoBufMut for Lying {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        &mut self.spare // 8 bytes
    }
}

impl SetLen for Lying {
    unsafe fn set_len(&mut self, _: usize) {}
}

fn main() {
    let mut buf = Lying { init: vec![0; 1000], spare: [MaybeUninit::new(0); 8] };
    let s = buf.as_mut_slice(); // UB: 1000-byte slice over 8 bytes
    s[999] = 1;
}
