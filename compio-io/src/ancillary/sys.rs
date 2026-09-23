//! Platform layer: the `cmsghdr` layout, `CMSG_*` arithmetic, and parsing and
//! writing control messages in a byte buffer.
//!
//! Everything here works on `&[u8]` / `&mut [u8]` with offsets. Headers are
//! copied in and out of a local mirror of `cmsghdr` with `bytemuck`, so no
//! pointer into the buffer is ever formed.

use std::mem::{align_of, offset_of, size_of};

use bytemuck::{Pod, Zeroable};
#[cfg(unix)]
pub(crate) use libc::cmsghdr;
#[cfg(windows)]
pub(crate) use windows_sys::Win32::Networking::WinSock::CMSGHDR as cmsghdr;
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::{self, IN_PKTINFO, IN6_PKTINFO};

use super::{AncillaryData, CodecError};

/// `CMSG_SPACE(0)` and `CMSG_SPACE(1)`, evaluated at compile time.
///
/// `libc` declares `CMSG_SPACE` as an `unsafe fn` only because it mirrors a C
/// macro: it is integer arithmetic on its argument. The calls sit in `const`
/// items, so they run during compilation and never at run time.
#[cfg(unix)]
const SPACE: [usize; 2] = {
    // SAFETY: `CMSG_SPACE` computes a size from its argument and touches no
    // memory.
    #[allow(clippy::unnecessary_cast)]
    unsafe {
        [libc::CMSG_SPACE(0) as usize, libc::CMSG_SPACE(1) as usize]
    }
};

/// Windows has no `CMSG_*` macros in `windows-sys`; this follows `ws2def.h`,
/// where everything is aligned to `CMSGHDR`.
#[cfg(windows)]
const SPACE: [usize; 2] = {
    let align = align_of::<cmsghdr>();
    let hdr = size_of::<cmsghdr>().next_multiple_of(align);
    [hdr, hdr + align]
};

/// Offset of the payload from its header: `CMSG_LEN(0)`, which equals
/// `CMSG_SPACE(0)` on every platform.
const DATA_OFFSET: usize = SPACE[0];

/// The unit `CMSG_ALIGN` rounds up to.
const ALIGN: usize = SPACE[1] - SPACE[0];

/// `CMSG_SPACE(len)`: header, `len` payload bytes and padding.
pub(crate) const fn cmsg_space(len: usize) -> usize {
    DATA_OFFSET + len.next_multiple_of(ALIGN)
}

/// The integer type of `cmsghdr::cmsg_len`: `size_t` on glibc, uClibc, bionic
/// and Windows, `socklen_t` elsewhere.
#[cfg(any(
    windows,
    target_os = "android",
    target_os = "cygwin",
    all(
        any(target_os = "linux", target_os = "l4re"),
        not(any(target_env = "musl", target_env = "ohos"))
    ),
))]
type LenT = usize;
#[cfg(not(any(
    windows,
    target_os = "android",
    target_os = "cygwin",
    all(
        any(target_os = "linux", target_os = "l4re"),
        not(any(target_env = "musl", target_env = "ohos"))
    ),
)))]
type LenT = u32;

/// A layout-compatible mirror of the platform's `cmsghdr`.
///
/// `libc::cmsghdr` is a foreign type, so it can't implement `Pod`. The
/// assertions below check this mirror against it field by field on every
/// target, so a mismatch is a compile error rather than a misread header.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CmsgHdr {
    #[cfg(all(
        any(target_env = "musl", target_env = "ohos"),
        target_pointer_width = "64",
        target_endian = "big"
    ))]
    _pad: i32,
    len: LenT,
    #[cfg(all(
        any(target_env = "musl", target_env = "ohos"),
        target_pointer_width = "64",
        target_endian = "little"
    ))]
    _pad: i32,
    level: i32,
    ty: i32,
}

const _: () = {
    assert!(size_of::<CmsgHdr>() == size_of::<cmsghdr>());
    assert!(align_of::<CmsgHdr>() == align_of::<cmsghdr>());
    assert!(offset_of!(CmsgHdr, len) == offset_of!(cmsghdr, cmsg_len));
    assert!(offset_of!(CmsgHdr, level) == offset_of!(cmsghdr, cmsg_level));
    assert!(offset_of!(CmsgHdr, ty) == offset_of!(cmsghdr, cmsg_type));
    assert!(DATA_OFFSET >= HDR);
    assert!(ALIGN.is_power_of_two());
};

const HDR: usize = size_of::<CmsgHdr>();

impl CmsgHdr {
    fn new(len: LenT, level: i32, ty: i32) -> Self {
        let mut hdr = Self::zeroed();
        hdr.len = len;
        hdr.level = level;
        hdr.ty = ty;
        hdr
    }

    /// Reads the header at `offset`, or `None` if it doesn't fit.
    #[inline]
    fn read(buf: &[u8], offset: usize) -> Option<Self> {
        let bytes = buf.get(offset..offset.checked_add(HDR)?)?;
        Some(bytemuck::pod_read_unaligned(bytes))
    }

    #[inline]
    #[allow(clippy::unnecessary_cast)]
    fn len(&self) -> usize {
        self.len as usize
    }
}

/// Checks the preconditions the iterator and the builder document.
///
/// Parsing and writing don't need alignment, since headers are copied, but the
/// buffer is handed to the kernel, so the requirement is kept.
pub(crate) fn check_buffer(ptr: *const u8, len: usize) {
    assert!(len >= cmsg_space(0), "buffer too short");
    assert!(ptr.cast::<cmsghdr>().is_aligned(), "misaligned buffer");
}

/// A parsed control message.
pub(crate) struct CMsgRef<'a> {
    level: i32,
    ty: i32,
    len: usize,
    data: &'a [u8],
}

impl CMsgRef<'_> {
    pub(crate) fn level(&self) -> i32 {
        self.level
    }

    pub(crate) fn ty(&self) -> i32 {
        self.ty
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn decode_data<T: AncillaryData>(&self) -> Result<T, CodecError> {
        T::decode(self.data)
    }
}

/// Walks the messages in a buffer, following the `libc` crate's Linux
/// `CMSG_FIRSTHDR` / `CMSG_NXTHDR` on every platform.
pub(crate) struct CMsgIter<'a> {
    buf: &'a [u8],
    offset: Option<usize>,
}

impl<'a> CMsgIter<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        check_buffer(buf.as_ptr(), buf.len());
        Self {
            buf,
            offset: (buf.len() >= HDR).then_some(0),
        }
    }
}

impl<'a> Iterator for CMsgIter<'a> {
    type Item = CMsgRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<CMsgRef<'a>> {
        let buf = self.buf;
        let offset = self.offset?;
        let hdr = CmsgHdr::read(buf, offset)?;
        let len = hdr.len();
        // `cmsg_len` counts the header, so the payload runs from the aligned
        // header end to `offset + cmsg_len`, clamped to the buffer.
        let data_start = (offset + DATA_OFFSET).min(buf.len());
        let data_end = offset.saturating_add(len).clamp(data_start, buf.len());
        // The next header, unless this length is shorter than a header (which
        // would loop) or the next header doesn't fit.
        self.offset = (len >= HDR)
            .then(|| offset.checked_add(len.checked_next_multiple_of(ALIGN)?))
            .flatten()
            .filter(|&next| next.checked_add(HDR).is_some_and(|end| end <= buf.len()));
        Some(CMsgRef {
            level: hdr.level,
            ty: hdr.ty,
            len,
            data: &buf[data_start..data_end],
        })
    }
}

/// Writes one message into `msg`, which must be exactly
/// `cmsg_space(T::SIZE)` bytes of zeroed buffer.
pub(crate) fn write_message<T: AncillaryData>(
    msg: &mut [u8],
    level: i32,
    ty: i32,
    value: &T,
) -> Result<(), CodecError> {
    #[allow(clippy::useless_conversion)]
    let len = LenT::try_from(DATA_OFFSET + T::SIZE).map_err(|_| CodecError::BufferTooSmall)?;
    let hdr = CmsgHdr::new(len, level, ty);
    msg.get_mut(..HDR)
        .ok_or(CodecError::BufferTooSmall)?
        .copy_from_slice(bytemuck::bytes_of(&hdr));
    let payload = msg
        .get_mut(DATA_OFFSET..DATA_OFFSET + T::SIZE)
        .ok_or(CodecError::BufferTooSmall)?;
    value.encode(payload)
}

/// Copies `N` bytes at `offset`, or fails if the buffer is too short.
fn get<const N: usize>(buffer: &[u8], offset: usize) -> Result<[u8; N], CodecError> {
    buffer
        .get(offset..offset + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(CodecError::BufferTooSmall)
}

/// Writes `bytes` at `offset`, or fails if the buffer is too short.
fn put(buffer: &mut [u8], offset: usize, bytes: &[u8]) -> Result<(), CodecError> {
    buffer
        .get_mut(offset..offset + bytes.len())
        .ok_or(CodecError::BufferTooSmall)?
        .copy_from_slice(bytes);
    Ok(())
}

#[cfg(unix)]
impl AncillaryData for libc::in_addr {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        put(buffer, 0, &self.s_addr.to_ne_bytes())
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        Ok(libc::in_addr {
            s_addr: u32::from_ne_bytes(get(buffer, 0)?),
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl AncillaryData for libc::in_pktinfo {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        use libc::in_pktinfo as P;
        put(
            buffer,
            offset_of!(P, ipi_ifindex),
            &self.ipi_ifindex.to_ne_bytes(),
        )?;
        put(
            buffer,
            offset_of!(P, ipi_spec_dst),
            &self.ipi_spec_dst.s_addr.to_ne_bytes(),
        )?;
        put(
            buffer,
            offset_of!(P, ipi_addr),
            &self.ipi_addr.s_addr.to_ne_bytes(),
        )
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        use libc::in_pktinfo as P;
        Ok(libc::in_pktinfo {
            ipi_ifindex: i32::from_ne_bytes(get(buffer, offset_of!(P, ipi_ifindex))?),
            ipi_spec_dst: libc::in_addr {
                s_addr: u32::from_ne_bytes(get(buffer, offset_of!(P, ipi_spec_dst))?),
            },
            ipi_addr: libc::in_addr {
                s_addr: u32::from_ne_bytes(get(buffer, offset_of!(P, ipi_addr))?),
            },
        })
    }
}

#[cfg(unix)]
impl AncillaryData for libc::in6_pktinfo {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        use libc::in6_pktinfo as P;
        put(buffer, offset_of!(P, ipi6_addr), &self.ipi6_addr.s6_addr)?;
        put(
            buffer,
            offset_of!(P, ipi6_ifindex),
            &self.ipi6_ifindex.to_ne_bytes(),
        )
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        use libc::in6_pktinfo as P;
        Ok(libc::in6_pktinfo {
            ipi6_addr: libc::in6_addr {
                s6_addr: get(buffer, offset_of!(P, ipi6_addr))?,
            },
            ipi6_ifindex: u32::from_ne_bytes(get(buffer, offset_of!(P, ipi6_ifindex))?),
        })
    }
}

#[cfg(windows)]
impl AncillaryData for IN_PKTINFO {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        // SAFETY: every field of the `IN_ADDR_0` union is plain integers
        // covering the same 4 bytes, so any of them can be read.
        let addr = unsafe { self.ipi_addr.S_un.S_addr };
        put(
            buffer,
            offset_of!(IN_PKTINFO, ipi_addr),
            &addr.to_ne_bytes(),
        )?;
        put(
            buffer,
            offset_of!(IN_PKTINFO, ipi_ifindex),
            &self.ipi_ifindex.to_ne_bytes(),
        )
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        Ok(IN_PKTINFO {
            ipi_addr: WinSock::IN_ADDR {
                S_un: WinSock::IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(get(buffer, offset_of!(IN_PKTINFO, ipi_addr))?),
                },
            },
            ipi_ifindex: u32::from_ne_bytes(get(buffer, offset_of!(IN_PKTINFO, ipi_ifindex))?),
        })
    }
}

#[cfg(windows)]
impl AncillaryData for IN6_PKTINFO {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        // SAFETY: every field of the `IN6_ADDR_0` union is plain integers
        // covering the same 16 bytes, so any of them can be read.
        let addr = unsafe { self.ipi6_addr.u.Byte };
        put(buffer, offset_of!(IN6_PKTINFO, ipi6_addr), &addr)?;
        put(
            buffer,
            offset_of!(IN6_PKTINFO, ipi6_ifindex),
            &self.ipi6_ifindex.to_ne_bytes(),
        )
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        Ok(IN6_PKTINFO {
            ipi6_addr: WinSock::IN6_ADDR {
                u: WinSock::IN6_ADDR_0 {
                    Byte: get(buffer, offset_of!(IN6_PKTINFO, ipi6_addr))?,
                },
            },
            ipi6_ifindex: u32::from_ne_bytes(get(buffer, offset_of!(IN6_PKTINFO, ipi6_ifindex))?),
        })
    }
}
