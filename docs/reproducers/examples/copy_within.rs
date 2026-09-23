// IoBufMutExt::copy_within can copy spare capacity over initialized bytes.
use compio_buf::IoBufMutExt;

fn main() {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&[1u8, 2, 3, 4]);
    v.copy_within(4..8, 0); // safe: copies uninitialized capacity over v[0..4]
    let x = v[0]; // UB: reads uninitialized memory
    std::hint::black_box(x);
}
