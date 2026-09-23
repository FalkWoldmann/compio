// libc's Linux CMSG_NXTHDR returns the same header again when cmsg_len is
// within 7 of usize::MAX, so AncillaryIter never ends on such a buffer.
// Not UB. Runs natively:
//   cargo run --release --example ancillary_walk_hang   (never ends)
//   cargo run --example ancillary_walk_hang             (aborts: overflow panic
//                                                        inside extern "C" CMSG_NXTHDR)
use compio_io::ancillary::AncillaryIter;

fn main() {
    // One cmsghdr (Linux glibc, 64-bit): cmsg_len, cmsg_level, cmsg_type.
    let mut words = [0u64; 4];
    words[0] = u64::MAX - 3;
    let bytes: &[u8] = bytemuck::cast_slice(&words);

    let iter = unsafe { AncillaryIter::new(bytes) };
    let n = iter.take(1_000_000).count();
    println!("{n} messages from a 32-byte buffer"); // 1000000
}
