// BytesMut::as_uninit builds a capacity-long slice from a pointer that only
// covers len() bytes. Plain use, no user impl.
use bytes::BytesMut;
use compio_buf::IoBufMut;

fn main() {
    let mut b = BytesMut::with_capacity(16);
    b.extend_from_slice(b"abc");
    let slice = b.as_uninit(); // UB under Stacked Borrows (retag), len 3 < cap 16
    println!("{}", slice.len());
}
