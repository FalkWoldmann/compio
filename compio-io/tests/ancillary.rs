use std::mem::MaybeUninit;

use compio_buf::{IoBuf, IoBufExt, IoBufMut, SetLen};
use compio_io::ancillary::{
    AncillaryBuf, AncillaryBuilder, AncillaryData, AncillaryIter, CodecError, ancillary_space,
};

fn build_cmsg<B: IoBufMut + ?Sized>(mut builder: AncillaryBuilder<B>) {
    builder.push(0, 0, &()).unwrap(); // 16 / 12
    builder.push(1, 1, &u8::MAX).unwrap(); // 16 + 1 + 7 / 12 + 1 + 3
    builder.push(2, 2, &u32::MAX).unwrap(); // 16 + 4 + 4 / 12 + 4
    builder.push(3, 3, &i64::MIN).unwrap(); // 16 + 8 / 12 + 8
    builder.push(4, 4, &[0; 1]).unwrap(); // 16 + 1 + 7 / 12 + 1 + 3
}

fn check_cmsg(buf: &[u8]) {
    let mut iter = AncillaryIter::new(buf);
    let cmsg = iter.next().unwrap();
    assert_eq!(
        (cmsg.level(), cmsg.ty(), cmsg.data::<()>().unwrap()),
        (0, 0, ())
    );
    let cmsg = iter.next().unwrap();
    assert_eq!(
        (cmsg.level(), cmsg.ty(), cmsg.data::<u8>().unwrap()),
        (1, 1, u8::MAX)
    );
    let cmsg = iter.next().unwrap();
    assert_eq!(
        (cmsg.level(), cmsg.ty(), cmsg.data::<u32>().unwrap()),
        (2, 2, u32::MAX)
    );
    let cmsg = iter.next().unwrap();
    assert_eq!(
        (cmsg.level(), cmsg.ty(), cmsg.data::<i64>().unwrap()),
        (3, 3, i64::MIN)
    );
    let cmsg = iter.next().unwrap();
    assert_eq!(
        (cmsg.level(), cmsg.ty(), cmsg.data().unwrap()),
        (4, 4, [0; 1])
    );
    assert!(iter.next().is_none());
}

#[test]
fn test_cmsg() {
    let mut buf = AncillaryBuf::<128>::new();
    let builder = buf.builder();

    build_cmsg(builder);
    assert!(buf.buf_len() == 112 || buf.buf_len() == 80);

    check_cmsg(&buf)
}

// Test a custom DST buffer. It checks the compatibility for the previous
// `CMsgBuilder`.
#[test]
fn test_custom_buffer_cmsg() {
    struct MaybeUninitBuffer<T: ?Sized> {
        len: usize,
        inner: T,
    }

    impl<T: AsRef<[MaybeUninit<u8>]> + ?Sized + 'static> IoBuf for MaybeUninitBuffer<T> {
        fn as_init(&self) -> &[u8] {
            unsafe { self.inner.as_ref()[..self.len].assume_init_ref() }
        }
    }

    impl<T: ?Sized> SetLen for MaybeUninitBuffer<T> {
        unsafe fn set_len(&mut self, new_len: usize) {
            self.len = new_len;
        }
    }

    impl<T: AsRef<[MaybeUninit<u8>]> + AsMut<[MaybeUninit<u8>]> + ?Sized + 'static> IoBufMut
        for MaybeUninitBuffer<T>
    {
        fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
            self.inner.as_mut()
        }
    }

    let mut buf = MaybeUninitBuffer {
        len: 0,
        inner: [MaybeUninit::zeroed(); 128],
    };
    let builder = AncillaryBuilder::new(&mut buf as &mut MaybeUninitBuffer<[MaybeUninit<u8>]>);

    build_cmsg(builder);
    assert!(buf.buf_len() == 112 || buf.buf_len() == 80);

    check_cmsg(buf.as_init())
}

#[test]
#[should_panic]
fn invalid_buffer_length() {
    AncillaryBuf::<1>::new().builder();
}

/// Records how many payload bytes `decode` was given.
struct PayloadLen(usize);

impl AncillaryData for PayloadLen {
    fn encode(&self, _: &mut [u8]) -> Result<(), CodecError> {
        unreachable!()
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        Ok(PayloadLen(buffer.len()))
    }
}

/// Fails to encode, after writing part of its payload.
struct FailingEncode;

impl AncillaryData for FailingEncode {
    const SIZE: usize = 4;

    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        buffer[0] = 0xff;
        Err(CodecError::other("encode failed"))
    }

    fn decode(_: &[u8]) -> Result<Self, CodecError> {
        unreachable!()
    }
}

/// An aligned byte buffer that isn't an `AncillaryBuf`, built without `unsafe`.
fn aligned(words: &mut [u64]) -> &mut [u8] {
    bytemuck::cast_slice_mut(words)
}

// The payload handed to `decode` doesn't include the header counted in
// `cmsg_len`.
#[test]
fn payload_excludes_header() {
    let mut buf = AncillaryBuf::<{ ancillary_space::<u32>() }>::new();
    buf.builder().push(0, 0, &0u32).unwrap();
    let cmsg = AncillaryIter::new(&buf).next().unwrap();
    assert_eq!(cmsg.data::<PayloadLen>().unwrap().0, 4);
}

// Several pushes, then one that doesn't fit and leaves the buffer untouched.
#[test]
fn builder_fills_up() {
    let mut buf = AncillaryBuf::<{ 3 * ancillary_space::<u64>() }>::new();
    let mut builder = buf.builder();
    builder.push(1, 1, &1u8).unwrap();
    builder.push(2, 2, &2u32).unwrap();
    builder.push(3, 3, &3u64).unwrap();
    assert!(matches!(
        builder.push(4, 4, &4u64),
        Err(CodecError::BufferTooSmall)
    ));
    let got: Vec<_> = AncillaryIter::new(&buf)
        .map(|m| (m.level(), m.ty()))
        .collect();
    assert_eq!(got, [(1, 1), (2, 2), (3, 3)]);
}

// A failed `encode` leaves the buffer as it was before the push.
#[test]
fn builder_rolls_back_failed_encode() {
    let mut buf = AncillaryBuf::<128>::new();
    let mut builder = buf.builder();
    builder.push(1, 1, &1u32).unwrap();
    assert!(builder.push(2, 2, &FailingEncode).is_err());
    builder.push(3, 3, &3u32).unwrap();
    let got: Vec<_> = AncillaryIter::new(&buf)
        .map(|m| (m.level(), m.data::<u32>().unwrap()))
        .collect();
    assert_eq!(got, [(1, 1), (3, 3)]);
}

// A caller-owned `&mut [u8]` works as the builder's buffer.
#[test]
fn builder_on_slice() {
    let mut words = [0u64; 8];
    let bytes = aligned(&mut words);
    let mut builder = AncillaryBuilder::new(&mut *bytes);
    builder.push(7, 8, &9u32).unwrap();
    builder.push(1, 2, &3u8).unwrap();
    let got: Vec<_> = AncillaryIter::new(bytes)
        .take(2)
        .map(|m| (m.level(), m.ty(), m.data::<u8>().unwrap()))
        .collect();
    assert_eq!(got, [(7, 8, 9), (1, 2, 3)]);
}

// A `cmsg_len` that would wrap when aligned, or that is shorter than a header,
// ends the walk instead of looping.
#[test]
fn malformed_lengths_end_the_walk() {
    for len in [0, 1, u64::MAX, u64::MAX - 7] {
        let mut words = [len, 0, 0, 0];
        let bytes = aligned(&mut words);
        assert_eq!(AncillaryIter::new(bytes).take(10).count(), 1);
    }
}

// A `cmsg_len` longer than the buffer yields a payload clamped to the buffer.
#[test]
fn oversized_length_is_clamped() {
    let mut buf = AncillaryBuf::<{ ancillary_space::<u32>() }>::new();
    buf.builder().push(0, 0, &0u32).unwrap();
    let mut bytes = buf.to_vec();
    bytes[..8].copy_from_slice(&u64::MAX.to_ne_bytes()[..8]);
    let mut words = [0u64; 3];
    let aligned = aligned(&mut words);
    aligned.copy_from_slice(&bytes);
    let cmsg = AncillaryIter::new(aligned).next().unwrap();
    assert!(cmsg.data::<PayloadLen>().unwrap().0 <= aligned.len());
}

#[cfg(unix)]
mod unix {
    use super::*;

    #[test]
    fn space_matches_libc() {
        fn check<T: AncillaryData>() {
            let size = T::SIZE as libc::c_uint;
            // SAFETY: `CMSG_SPACE` is integer arithmetic.
            let expected = unsafe { libc::CMSG_SPACE(size) } as usize;
            assert_eq!(ancillary_space::<T>(), expected);
        }
        check::<()>();
        check::<u8>();
        check::<u16>();
        check::<u32>();
        check::<u64>();
        check::<[u8; 3]>();
        check::<[u8; 13]>();
        check::<libc::in_addr>();
        check::<libc::in6_pktinfo>();
    }

    #[test]
    fn pktinfo_round_trips() {
        fn round_trip<T: AncillaryData>(value: &T) -> T {
            let mut buf = AncillaryBuf::<128>::new();
            buf.builder().push(0, 0, value).unwrap();
            AncillaryIter::new(&buf).next().unwrap().data().unwrap()
        }

        let addr = libc::in_addr {
            s_addr: 0x0100_007f,
        };
        assert_eq!(round_trip(&addr).s_addr, addr.s_addr);

        let info6 = libc::in6_pktinfo {
            ipi6_addr: libc::in6_addr {
                s6_addr: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            },
            ipi6_ifindex: 7,
        };
        let got = round_trip(&info6);
        assert_eq!(got.ipi6_addr.s6_addr, info6.ipi6_addr.s6_addr);
        assert_eq!(got.ipi6_ifindex, info6.ipi6_ifindex);

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let info = libc::in_pktinfo {
                ipi_ifindex: 3,
                ipi_spec_dst: libc::in_addr {
                    s_addr: 0x0200_007f,
                },
                ipi_addr: libc::in_addr {
                    s_addr: 0x0100_007f,
                },
            };
            let got = round_trip(&info);
            assert_eq!(got.ipi_ifindex, info.ipi_ifindex);
            assert_eq!(got.ipi_spec_dst.s_addr, info.ipi_spec_dst.s_addr);
            assert_eq!(got.ipi_addr.s_addr, info.ipi_addr.s_addr);
        }
    }

    /// One message as (level, type, `cmsg_len`, payload start, payload end).
    #[cfg(target_os = "linux")]
    type Message = (i32, i32, usize, usize, usize);

    /// Walks `buf` with libc's own macros. `None` if libc would loop forever.
    #[cfg(target_os = "linux")]
    fn libc_walk(buf: &[u8]) -> Option<Vec<Message>> {
        // SAFETY: an all-zero `msghdr` is valid, and only the control fields
        // are set, to a live buffer.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_control = buf.as_ptr().cast_mut().cast();
        msg.msg_controllen = buf.len() as _;
        let base = buf.as_ptr() as usize;
        let mut out = vec![];
        // SAFETY: `msg` describes `buf`; libc only returns headers inside it.
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !cmsg.is_null() {
            // SAFETY: `cmsg` points to a header that fits in `buf`.
            let hdr = unsafe { &*cmsg };
            #[allow(clippy::unnecessary_cast)]
            let len = hdr.cmsg_len as usize;
            if len > usize::MAX - 16 {
                return None;
            }
            let offset = cmsg as usize - base;
            // SAFETY: arithmetic on a pointer to a header inside `buf`.
            let data = unsafe { libc::CMSG_DATA(cmsg) } as usize - base;
            let start = data.min(buf.len());
            let end = offset.saturating_add(len).clamp(start, buf.len());
            out.push((hdr.cmsg_level, hdr.cmsg_type, len, start, end));
            // SAFETY: as for `CMSG_FIRSTHDR`.
            cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
        }
        Some(out)
    }

    /// Records where the payload handed to `decode` starts.
    #[cfg(target_os = "linux")]
    struct PayloadAt(usize);

    #[cfg(target_os = "linux")]
    impl AncillaryData for PayloadAt {
        fn encode(&self, _: &mut [u8]) -> Result<(), CodecError> {
            unreachable!()
        }

        fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
            Ok(PayloadAt(buffer.as_ptr() as usize))
        }
    }

    // The walk matches libc's `CMSG_FIRSTHDR` / `CMSG_NXTHDR` on random
    // buffers, except where libc loops forever.
    #[cfg(target_os = "linux")]
    #[test]
    fn walk_matches_libc() {
        let rounds = if cfg!(miri) { 50 } else { 20_000 };
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..rounds {
            let mut words = vec![0u64; 2 + (rnd() % 30) as usize];
            let bytes = aligned(&mut words);
            let len = bytes.len() - (rnd() % 8) as usize;
            let buf = &mut bytes[..len];
            let mut off = 0;
            while off + 16 <= buf.len() {
                let cmsg_len = match rnd() % 5 {
                    0 => 0,
                    1 => rnd() % 16,
                    2 => 16 + rnd() % 64,
                    3 => rnd() % 4096,
                    _ => 16 + rnd() % 8,
                };
                buf[off..off + 8].copy_from_slice(&cmsg_len.to_ne_bytes());
                buf[off + 8..off + 12].copy_from_slice(&((rnd() % 300) as i32).to_ne_bytes());
                buf[off + 12..off + 16].copy_from_slice(&((rnd() % 300) as i32).to_ne_bytes());
                off += 16 + 8 * (rnd() % 6) as usize;
            }
            let buf: &[u8] = buf;
            // `AncillaryIter::new` rejects buffers shorter than one header.
            if buf.len() < ancillary_space::<()>() {
                continue;
            }
            let Some(expected) = libc_walk(buf) else {
                continue;
            };
            let base = buf.as_ptr() as usize;
            let got: Vec<_> = AncillaryIter::new(buf)
                .map(|m| {
                    let start = m.data::<PayloadAt>().unwrap().0 - base;
                    let end = start + m.data::<PayloadLen>().unwrap().0;
                    (m.level(), m.ty(), m.len(), start, end)
                })
                .collect();
            assert_eq!(got, expected, "buffer {buf:?}");
        }
    }
}
