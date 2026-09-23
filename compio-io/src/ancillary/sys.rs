use std::mem::{offset_of, size_of};

#[cfg(unix)]
use libc::CMSG_LEN;
#[cfg(unix)]
pub use libc::{CMSG_SPACE, cmsghdr};
#[cfg(windows)]
pub use windows_sys::Win32::Networking::WinSock::CMSGHDR as cmsghdr;
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::{self, IN_PKTINFO, IN6_PKTINFO};

use super::{AncillaryData, CodecError, copy_from_bytes, copy_to_bytes};

#[cfg(windows)]
#[allow(non_snake_case, dead_code)]
mod windows_macros {
    use std::ptr::null_mut;

    use windows_sys::Win32::Networking::WinSock::{CMSGHDR, WSABUF, WSAMSG};

    const fn CMSG_ALIGN(length: usize) -> usize {
        (length + align_of::<CMSGHDR>() - 1) & !(align_of::<CMSGHDR>() - 1)
    }

    const WSA_CMSGDATA_OFFSET: usize = CMSG_ALIGN(size_of::<CMSGHDR>());

    pub unsafe fn CMSG_DATA(cmsg: *const CMSGHDR) -> *mut u8 {
        unsafe { cmsg.offset(1) as *mut u8 }
    }

    pub const unsafe fn CMSG_SPACE(length: usize) -> usize {
        WSA_CMSGDATA_OFFSET + CMSG_ALIGN(length)
    }

    pub const unsafe fn CMSG_LEN(length: usize) -> usize {
        WSA_CMSGDATA_OFFSET + length
    }

    pub unsafe fn CMSG_FIRSTHDR(msg: *const WSAMSG) -> *mut CMSGHDR {
        unsafe {
            if (*msg).Control.len as usize >= size_of::<CMSGHDR>() {
                (*msg).Control.buf as _
            } else {
                null_mut()
            }
        }
    }

    pub unsafe fn CMSG_NXTHDR(msg: *const WSAMSG, cmsg: *const CMSGHDR) -> *mut CMSGHDR {
        unsafe {
            if cmsg.is_null() {
                CMSG_FIRSTHDR(msg)
            } else {
                let next = cmsg as usize + CMSG_ALIGN((*cmsg).cmsg_len);
                if next + size_of::<CMSGHDR>()
                    > (*msg).Control.buf as usize + (*msg).Control.len as usize
                {
                    null_mut()
                } else {
                    next as _
                }
            }
        }
    }

    pub fn msghdr_from_raw(ptr: *const u8, len: usize) -> WSAMSG {
        WSAMSG {
            Control: WSABUF {
                len: len as _,
                buf: ptr as _,
            },
            ..unsafe { std::mem::zeroed() }
        }
    }
}

#[cfg(windows)]
use windows_macros::CMSG_LEN;
#[cfg(windows)]
pub use windows_macros::CMSG_SPACE;

const HDR: usize = size_of::<cmsghdr>();

/// `CMSG_SPACE(len)`: header plus `len` payload bytes, padded for alignment.
pub(crate) fn cmsg_space(len: usize) -> usize {
    // SAFETY: `CMSG_SPACE` only does integer arithmetic on its argument.
    #[allow(clippy::unnecessary_cast)]
    unsafe {
        CMSG_SPACE(len as _) as usize
    }
}

/// `CMSG_LEN(len)`: the `cmsg_len` of a message with `len` payload bytes.
fn cmsg_len(len: usize) -> usize {
    // SAFETY: `CMSG_LEN` only does integer arithmetic on its argument.
    #[allow(clippy::unnecessary_cast)]
    unsafe {
        CMSG_LEN(len as _) as usize
    }
}

/// `CMSG_ALIGN(len)`: `len` rounded up to the platform's control message
/// alignment, derived from `CMSG_SPACE` so it matches the platform. `None` on
/// overflow.
fn cmsg_align(len: usize) -> Option<usize> {
    let unit = cmsg_space(1) - cmsg_space(0);
    len.checked_next_multiple_of(unit)
}

/// Offset and size of a `cmsghdr` field. The size comes from the field's type,
/// which differs between platforms (`cmsg_len` is `usize` on glibc and Windows,
/// `u32` on musl and the BSDs).
macro_rules! field {
    ($f:ident) => {{
        fn size<F>(_: fn(&cmsghdr) -> &F) -> usize {
            size_of::<F>()
        }
        (offset_of!(cmsghdr, $f), size(|h| &h.$f))
    }};
}

fn get(header: &[u8], (offset, size): (usize, usize)) -> u64 {
    let bytes = &header[offset..offset + size];
    match size {
        4 => u32::from_ne_bytes(bytes.try_into().unwrap()).into(),
        8 => u64::from_ne_bytes(bytes.try_into().unwrap()),
        _ => unreachable!("unexpected cmsghdr field size"),
    }
}

fn set(header: &mut [u8], (offset, size): (usize, usize), value: u64) {
    let bytes = &mut header[offset..offset + size];
    match size {
        4 => bytes.copy_from_slice(&u32::try_from(value).expect("value too large").to_ne_bytes()),
        8 => bytes.copy_from_slice(&value.to_ne_bytes()),
        _ => unreachable!("unexpected cmsghdr field size"),
    }
}

/// Checks the preconditions both the iterator and the builder rely on.
pub(crate) fn check_buffer(buf: &[u8]) {
    assert!(buf.len() >= cmsg_space(0), "buffer too short");
    assert!(
        buf.as_ptr().cast::<cmsghdr>().is_aligned(),
        "misaligned buffer"
    );
}

/// Offset of the first header, like `CMSG_FIRSTHDR`.
pub(crate) fn first(buf: &[u8]) -> Option<usize> {
    (buf.len() >= HDR).then_some(0)
}

/// Offset of the header after the one at `offset`, following the `libc`
/// crate's Linux `CMSG_NXTHDR`: `None` if the current `cmsg_len` is shorter
/// than a header, or if a whole header does not fit after it.
fn next(buf: &[u8], offset: usize) -> Option<usize> {
    let len = get(&buf[offset..offset + HDR], field!(cmsg_len));
    let len = usize::try_from(len).ok().filter(|&len| len >= HDR)?;
    let next = offset.checked_add(cmsg_align(len)?)?;
    (next.checked_add(HDR)? <= buf.len()).then_some(next)
}

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

pub(crate) struct CMsgIter<'a> {
    buf: &'a [u8],
    offset: Option<usize>,
}

impl<'a> CMsgIter<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        check_buffer(buf);
        Self {
            buf,
            offset: first(buf),
        }
    }
}

impl<'a> Iterator for CMsgIter<'a> {
    type Item = CMsgRef<'a>;

    fn next(&mut self) -> Option<CMsgRef<'a>> {
        let buf = self.buf;
        let offset = self.offset?;
        let header = &buf[offset..offset + HDR];
        let len = get(header, field!(cmsg_len)) as usize;
        // The payload starts after the aligned header and ends at `cmsg_len`,
        // clamped to the buffer.
        let data_start = (offset + cmsg_len(0)).min(buf.len());
        let data_end = offset.saturating_add(len).clamp(data_start, buf.len());
        self.offset = next(buf, offset);
        Some(CMsgRef {
            level: get(header, field!(cmsg_level)) as u32 as i32,
            ty: get(header, field!(cmsg_type)) as u32 as i32,
            len,
            data: &buf[data_start..data_end],
        })
    }
}

/// Writes one message at `offset` and returns the offset just past it.
pub(crate) fn write_message<T: AncillaryData>(
    buf: &mut [u8],
    offset: usize,
    level: i32,
    ty: i32,
    value: &T,
) -> Result<usize, CodecError> {
    let end = offset
        .checked_add(cmsg_space(T::SIZE))
        .filter(|&end| end <= buf.len())
        .ok_or(CodecError::BufferTooSmall)?;
    let header = &mut buf[offset..offset + HDR];
    set(header, field!(cmsg_len), cmsg_len(T::SIZE) as u64);
    set(header, field!(cmsg_level), level as u32 as u64);
    set(header, field!(cmsg_type), ty as u32 as u64);
    let data_start = offset + cmsg_len(0);
    value.encode(&mut buf[data_start..data_start + T::SIZE])?;
    Ok(end)
}

/// Offset for the next message after one ending at `end`, if a header fits.
pub(crate) fn after(buf: &[u8], end: usize) -> Option<usize> {
    (end.checked_add(HDR)? <= buf.len()).then_some(end)
}

#[cfg(unix)]
impl AncillaryData for libc::in_addr {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        unsafe { copy_to_bytes(self, buffer) }
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        unsafe { copy_from_bytes(buffer) }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl AncillaryData for libc::in_pktinfo {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        let mut pktinfo: libc::in_pktinfo = unsafe { std::mem::zeroed() };
        pktinfo.ipi_ifindex = self.ipi_ifindex;
        pktinfo.ipi_spec_dst.s_addr = self.ipi_spec_dst.s_addr;
        pktinfo.ipi_addr.s_addr = self.ipi_addr.s_addr;
        unsafe { copy_to_bytes(&pktinfo, buffer) }
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        let pktinfo: libc::in_pktinfo = unsafe { copy_from_bytes(buffer) }?;
        Ok(libc::in_pktinfo {
            ipi_ifindex: pktinfo.ipi_ifindex,
            ipi_spec_dst: libc::in_addr {
                s_addr: pktinfo.ipi_spec_dst.s_addr,
            },
            ipi_addr: libc::in_addr {
                s_addr: pktinfo.ipi_addr.s_addr,
            },
        })
    }
}

#[cfg(unix)]
impl AncillaryData for libc::in6_pktinfo {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        let mut pktinfo: libc::in6_pktinfo = unsafe { std::mem::zeroed() };
        pktinfo.ipi6_ifindex = self.ipi6_ifindex;
        pktinfo.ipi6_addr.s6_addr = self.ipi6_addr.s6_addr;
        unsafe { copy_to_bytes(&pktinfo, buffer) }
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        let pktinfo: libc::in6_pktinfo = unsafe { copy_from_bytes(buffer) }?;
        Ok(libc::in6_pktinfo {
            ipi6_ifindex: pktinfo.ipi6_ifindex,
            ipi6_addr: libc::in6_addr {
                s6_addr: pktinfo.ipi6_addr.s6_addr,
            },
        })
    }
}

#[cfg(windows)]
impl AncillaryData for IN_PKTINFO {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        let mut pktinfo: IN_PKTINFO = unsafe { std::mem::zeroed() };
        unsafe {
            pktinfo.ipi_addr.S_un.S_addr = self.ipi_addr.S_un.S_addr;
        }
        pktinfo.ipi_ifindex = self.ipi_ifindex;
        unsafe { copy_to_bytes(&pktinfo, buffer) }
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        let pktinfo: IN_PKTINFO = unsafe { copy_from_bytes(buffer) }?;
        Ok(IN_PKTINFO {
            ipi_addr: WinSock::IN_ADDR {
                S_un: WinSock::IN_ADDR_0 {
                    S_addr: unsafe { pktinfo.ipi_addr.S_un.S_addr },
                },
            },
            ipi_ifindex: pktinfo.ipi_ifindex,
        })
    }
}

#[cfg(windows)]
impl AncillaryData for IN6_PKTINFO {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        let mut pktinfo: IN6_PKTINFO = unsafe { std::mem::zeroed() };
        unsafe {
            pktinfo.ipi6_addr.u.Byte = self.ipi6_addr.u.Byte;
        }
        pktinfo.ipi6_ifindex = self.ipi6_ifindex;
        unsafe { copy_to_bytes(&pktinfo, buffer) }
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        let pktinfo: IN6_PKTINFO = unsafe { copy_from_bytes(buffer) }?;
        Ok(IN6_PKTINFO {
            ipi6_addr: WinSock::IN6_ADDR {
                u: WinSock::IN6_ADDR_0 {
                    Byte: unsafe { pktinfo.ipi6_addr.u.Byte },
                },
            },
            ipi6_ifindex: pktinfo.ipi6_ifindex,
        })
    }
}
