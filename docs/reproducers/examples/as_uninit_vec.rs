// Same bug as #1053, with Vec<u8> instead of [u8; N]. [u8] behaves the same.
use std::mem::MaybeUninit;

use compio_buf::IoBufMut;

fn main() {
    let mut v = vec![1u8, 2, 3, 4];
    v.as_uninit()[0] = MaybeUninit::uninit();
    let x = v[0]; // UB
    std::hint::black_box(x);
}
