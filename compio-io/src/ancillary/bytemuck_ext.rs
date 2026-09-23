//! Extension module for automatic [`AncillaryData`] implementation via
//! bytemuck.
//!
//! See [`BitwiseAncillaryData`] for details.

pub use bytemuck::{Pod, Zeroable};

use super::{AncillaryData, CodecError};

/// Marker trait to enable automatic `AncillaryData` implementation via
/// bytemuck.
///
/// Types that implement this trait (which requires [`bytemuck::Pod`]) will
/// automatically implement [`AncillaryData`] using a simple byte-wise
/// encoding/decoding.
///
/// # Example
///
/// ```
/// use compio_io::ancillary::bytemuck_ext;
///
/// #[derive(Clone, Copy)]
/// #[repr(C)]
/// struct MyType {
///     value: u32,
/// }
///
/// unsafe impl bytemuck_ext::Zeroable for MyType {}
/// unsafe impl bytemuck_ext::Pod for MyType {}
/// impl bytemuck_ext::BitwiseAncillaryData for MyType {}
///
/// // Now MyType automatically implements AncillaryData
/// ```
pub trait BitwiseAncillaryData: Pod {}

impl<T: BitwiseAncillaryData> AncillaryData for T {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), CodecError> {
        let bytes = bytemuck::bytes_of(self);
        buffer
            .get_mut(..bytes.len())
            .ok_or(CodecError::BufferTooSmall)?
            .copy_from_slice(bytes);
        Ok(())
    }

    fn decode(buffer: &[u8]) -> Result<Self, CodecError> {
        let bytes = buffer
            .get(..size_of::<Self>())
            .ok_or(CodecError::BufferTooSmall)?;
        Ok(bytemuck::pod_read_unaligned(bytes))
    }
}

macro_rules! impl_bytemuck_marker {
    ($($t:ty),* $(,)?) => {
        $(
            impl BitwiseAncillaryData for $t {}
        )*
    };
}

impl_bytemuck_marker!(
    (),
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
);

impl<T: BitwiseAncillaryData, const N: usize> BitwiseAncillaryData for [T; N] {}
