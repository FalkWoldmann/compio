use std::mem::MaybeUninit;

use compio_buf::*;

/// A safe implementation whose `as_init` (1000 bytes) and `as_uninit`
/// (8 bytes) disagree.
struct Inconsistent {
    init: Vec<u8>,
    uninit: [MaybeUninit<u8>; 8],
}

impl Inconsistent {
    fn new() -> Self {
        Self {
            init: vec![0; 1000],
            uninit: [MaybeUninit::new(0); 8],
        }
    }
}

unsafe impl IoBuf for Inconsistent {
    fn as_init(&self) -> &[u8] {
        &self.init
    }
}

unsafe impl SetLen for Inconsistent {
    unsafe fn set_len(&mut self, _len: usize) {}
}

unsafe impl IoBufMut for Inconsistent {
    unsafe fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        &mut self.uninit
    }
}

/// Debug builds should say so loudly rather than quietly hand back a
/// shorter slice than `buf_len()` advertised.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "describe the same buffer")]
fn as_mut_slice_asserts_when_the_impls_disagree() {
    let _ = Inconsistent::new().as_mut_slice();
}

/// The soundness property, which must hold with assertions compiled out:
/// the slice never runs past what `as_uninit` actually exposes. Before the
/// clamp this produced a 1000-byte slice over 8 bytes of storage, which
/// Miri reported as a dangling reference.
#[test]
#[cfg(not(debug_assertions))]
fn as_mut_slice_clamps_when_the_impls_disagree() {
    assert_eq!(
        Inconsistent::new().as_mut_slice().len(),
        8,
        "as_mut_slice must not exceed what as_uninit exposes"
    );
}

/// `buf_len()` is 1000 and `buf_capacity()` is 8. The old
/// `buf_capacity() - init` wrapped to ~1.8e19, so `reserve` said Ok and
/// `extend_from_slice` wrote 992 bytes past an 8-byte allocation.
#[test]
fn extend_from_slice_refuses_when_the_impls_disagree() {
    assert!(Inconsistent::new().extend_from_slice(b"abcd").is_err());
}

/// The same disagreement must not make `reserve` itself claim capacity.
#[test]
fn reserve_refuses_when_the_impls_disagree() {
    assert!(Inconsistent::new().reserve(4).is_err());
}
