// IoVectoredBufMut::iter_uninit_slice exposes initialized bytes as MaybeUninit.
use std::mem::MaybeUninit;

use compio_buf::IoVectoredBufMut;

fn main() {
    let mut bufs = [vec![1u8, 2, 3], vec![4u8, 5, 6]];
    for slice in bufs.iter_uninit_slice() {
        slice[0] = MaybeUninit::uninit(); // safe
    }
    let x = bufs[0][0]; // UB: reads uninitialized memory
    std::hint::black_box(x);
}
