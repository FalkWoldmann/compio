use std::io;

use compio_buf::{IntoInner, IoBuf, IoBufExt};
use compio_io::framed::frame::{Frame, Framer, LengthDelimited};
use proptest::{collection::vec, prelude::*};

fn framer(length_field_len: usize, big_endian: bool) -> LengthDelimited {
    LengthDelimited::new()
        .set_length_field_len(length_field_len)
        .set_length_field_is_big_endian(big_endian)
}

fn max_len(length_field_len: usize) -> u64 {
    match length_field_len {
        8.. => u64::MAX,
        n => (1 << (8 * n)) - 1,
    }
}

fn wire() -> impl Strategy<Value = (usize, bool, u64, Vec<u8>)> {
    let len = prop_oneof![any::<u64>(), u64::MAX - 16..=u64::MAX];
    (0..=8usize, any::<bool>(), len, vec(any::<u8>(), 0..32)).prop_map(|(lfl, be, len, rest)| {
        let mut data = if be {
            len.to_be_bytes()[8 - lfl..].to_vec()
        } else {
            len.to_le_bytes()[..lfl].to_vec()
        };
        data.extend_from_slice(&rest);
        (lfl, be, len & max_len(lfl), data)
    })
}

fn enclosed() -> impl Strategy<Value = (usize, bool, Vec<u8>)> {
    (0..=8usize, any::<bool>()).prop_flat_map(|(lfl, be)| {
        let len = max_len(lfl).min(512) as usize;
        (Just(lfl), Just(be), vec(any::<u8>(), 0..=len))
    })
}

proptest! {
    #![proptest_config(if cfg!(miri) {
        ProptestConfig {
            cases: 8,
            failure_persistence: None,
            ..ProptestConfig::default()
        }
    } else {
        ProptestConfig::default()
    })]

    #[test]
    fn extract((lfl, be, len, data) in wire()) {
        let mut framer = framer(lfl, be);
        let total = usize::try_from(len).ok().and_then(|len| len.checked_add(lfl));
        match (Framer::<Vec<u8>>::extract(&mut framer, &data.clone().slice(..)), total) {
            (Ok(frame), Some(total)) => {
                let expected = (total <= data.len()).then(|| Frame::new(lfl, total - lfl, 0));
                prop_assert_eq!(frame, expected);
            }
            (Err(e), None) => prop_assert_eq!(e.kind(), io::ErrorKind::InvalidData),
            (got, total) => prop_assert!(false, "got {got:?} for frame length {total:?}"),
        }
    }

    #[test]
    fn roundtrip((lfl, be, payload) in enclosed(), trailing in vec(any::<u8>(), 0..16)) {
        let mut framer = framer(lfl, be);
        let mut buf = payload.clone();
        Framer::<Vec<u8>>::enclose(&mut framer, &mut buf);
        prop_assert_eq!(buf.len(), lfl + payload.len());

        for cut in 0..buf.len() {
            let partial = buf[..cut].to_vec().slice(..);
            prop_assert_eq!(Framer::<Vec<u8>>::extract(&mut framer, &partial).unwrap(), None);
        }

        buf.extend_from_slice(&trailing);
        let slice = buf.slice(..);
        let frame = Framer::<Vec<u8>>::extract(&mut framer, &slice).unwrap().unwrap();
        prop_assert_eq!(frame.len(), lfl + payload.len());
        let extracted = frame.slice(slice.into_inner());
        prop_assert_eq!(extracted.as_init(), &payload[..]);
    }
}
