use compio_io::{AsyncWrite, AsyncWriteAt};
use futures_executor::block_on;
use proptest::{collection::vec, prelude::*};

fn bufs() -> impl Strategy<Value = Vec<Vec<u8>>> {
    vec(vec(any::<u8>(), 0..32), 0..8)
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: if cfg!(miri) {
            None
        } else {
            ProptestConfig::default().failure_persistence
        },
        ..ProptestConfig::default()
    })]

    #[test]
    fn slice_write_vectored(bufs in bufs(), mut dst in vec(any::<u8>(), 0..64)) {
        let mut expected = dst.clone();
        let data = bufs.concat();
        let n = data.len().min(dst.len());
        expected[..n].copy_from_slice(&data[..n]);

        let cap = dst.len();
        let mut slice = &mut dst[..];
        let (len, back) = block_on(slice.write_vectored(bufs.clone())).unwrap();

        prop_assert_eq!(len, n);
        prop_assert_eq!(slice.len(), cap - n);
        prop_assert_eq!(back, bufs);
        prop_assert_eq!(dst, expected);
    }

    #[test]
    fn slice_write_vectored_at(bufs in bufs(), mut dst in vec(any::<u8>(), 0..64), pos in 0..80u64) {
        let mut expected = dst.clone();
        let data = bufs.concat();
        let start = (pos as usize).min(dst.len());
        let n = data.len().min(dst.len() - start);
        expected[start..start + n].copy_from_slice(&data[..n]);

        let (len, back) = block_on(dst[..].write_vectored_at(bufs.clone(), pos)).unwrap();

        prop_assert_eq!(len, n);
        prop_assert_eq!(back, bufs);
        prop_assert_eq!(dst, expected);
    }

    #[test]
    fn array_write_vectored_at(bufs in bufs(), mut dst in any::<[u8; 32]>(), pos in 0..40u64) {
        let mut expected = dst;
        let data = bufs.concat();
        let start = (pos as usize).min(dst.len());
        let n = data.len().min(dst.len() - start);
        expected[start..start + n].copy_from_slice(&data[..n]);

        let (len, back) = block_on(dst.write_vectored_at(bufs.clone(), pos)).unwrap();

        prop_assert_eq!(len, n);
        prop_assert_eq!(back, bufs);
        prop_assert_eq!(dst, expected);
    }
}
