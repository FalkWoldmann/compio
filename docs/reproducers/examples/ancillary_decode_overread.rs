// CMsgRef::decode_data builds a slice of cmsg_len bytes starting after the
// header. cmsg_len includes the header, so the slice runs CMSG_LEN(0) bytes
// past the payload. Only visible when the buffer ends right after the message,
// as it does after recvmsg with a control buffer of CMSG_SPACE(4) bytes.
// Layout is Linux glibc, 64-bit little endian.
use compio_io::ancillary::AncillaryIter;

fn main() {
    // cmsg_len = CMSG_LEN(4) = 20, cmsg_level = 1, cmsg_type = 2, payload 7u32.
    // 24 bytes = CMSG_SPACE(4), in an allocation of exactly that size.
    let words: Box<[u64]> = Box::new([20, 1 | 2 << 32, 7]);
    let bytes: &[u8] = bytemuck::cast_slice(&words);

    let msg = unsafe { AncillaryIter::new(bytes) }.next().unwrap();
    let value = msg.data::<u32>().unwrap(); // UB: 20-byte slice at offset 16 of 24
    assert_eq!(value, 7);
}
