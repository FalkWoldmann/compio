// AncillaryData is a safe trait, but encode gets &mut [MaybeUninit<u8>] over
// bytes that push then marks as initialized. A safe impl can de-initialize them.
// push also trips the aliasing bug, so turn aliasing checks off to reach this:
//   MIRIFLAGS=-Zmiri-disable-stacked-borrows cargo miri run --example ancillary_encode_deinit
use std::mem::MaybeUninit;

use compio_io::ancillary::{AncillaryBuf, AncillaryData, CodecError, ancillary_space};

struct Deinit;

impl AncillaryData for Deinit {
    const SIZE: usize = 4;

    fn encode(&self, buffer: &mut [MaybeUninit<u8>]) -> Result<(), CodecError> {
        buffer[0] = MaybeUninit::uninit(); // safe
        Ok(())
    }

    fn decode(_: &[u8]) -> Result<Self, CodecError> {
        Ok(Deinit)
    }
}

fn main() {
    let mut buf = AncillaryBuf::<{ ancillary_space::<Deinit>() }>::new();
    buf.builder().push(1, 2, &Deinit).unwrap();
    let sum: u32 = buf.iter().map(|&b| b as u32).sum(); // UB: reads uninitialized memory
    println!("{sum}");
}
