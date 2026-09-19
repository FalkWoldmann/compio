//! Benchmarks for the in-memory IO paths, with allocation counts.
//!
//! Everything here runs over `&[u8]` and `Vec<u8>`, which implement
//! [`AsyncRead`] and [`AsyncWrite`] directly, so no runtime, no driver and no
//! socket are involved: the numbers are the cost of the adapter layer itself —
//! the buffer handoff, the `Slice` bookkeeping and the futures — rather than of
//! the kernel underneath it. That also makes them runnable anywhere, including
//! the sandboxes where the socket benchmarks in `compio/benches/net.rs` cannot
//! open a socket at all.
//!
//! [`divan`] rather than criterion, for the allocation profiler: every case
//! reports the allocations and the bytes it made per iteration next to the
//! time, which is what tells a buffer being reused from one being reallocated
//! on every round of the copy loop.
//!
//! ```sh
//! cargo bench -p compio-io --bench copy
//! cargo bench -p compio-io --bench copy -- copy_with_size   # one group
//! ```
//!
//! [`AsyncRead`]: compio_io::AsyncRead
//! [`AsyncWrite`]: compio_io::AsyncWrite
//! [`divan`]: https://docs.rs/divan

use compio_io::{
    AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter, copy, util::copy_with_size,
};
use divan::{Bencher, black_box};
use futures_executor::block_on;

/// Counts every allocation the benchmarked code makes, which divan reports per
/// iteration alongside the timings.
#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Payload sizes: one well under the 8 KiB default copy buffer, one just over
/// it, and one that takes many rounds of the loop.
const SIZES: [usize; 3] = [1 << 10, 1 << 14, 1 << 20];

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| i as u8).collect()
}

/// A payload that outlives the benchmark, so that a case writing it does not
/// clone it per iteration.
///
/// `write_all` takes its buffer by value, and a `Vec` handed to it would have
/// to be cloned for every iteration and dropped inside the timed region — at
/// the sizes here, that allocation dwarfs the write path it is meant to
/// measure. `&'static [u8]` is an `IoBuf` too, and copying that is copying a
/// fat pointer.
fn leaked(len: usize) -> &'static [u8] {
    Box::leak(payload(len).into_boxed_slice())
}

/// [`copy`] end to end, at the default 8 KiB buffer size.
///
/// The allocation count is the interesting number: the copy loop should
/// allocate its buffer once and reuse it, so this should stay flat as the
/// payload grows, with only the sink's growth on top.
#[divan::bench(args = SIZES)]
fn copy_default(bencher: Bencher, len: usize) {
    let src = payload(len);
    bencher
        .with_inputs(|| (src.as_slice(), Vec::with_capacity(len)))
        .bench_local_values(|(mut src, mut dst)| {
            block_on(copy(&mut src, &mut dst)).unwrap();
            black_box(dst);
        });
}

/// [`copy_with_size`] across buffer sizes, at a fixed payload.
///
/// Shows the trade-off the copy buffer makes: a smaller one means more rounds
/// of the read/write loop and more future machinery per byte, a larger one
/// means a bigger up-front allocation.
#[divan::bench(args = [1 << 9, 1 << 12, 1 << 13, 1 << 16])]
fn copy_with_buf_size(bencher: Bencher, buf_size: usize) {
    const LEN: usize = 1 << 20;
    let src = payload(LEN);
    bencher
        .with_inputs(|| (src.as_slice(), Vec::with_capacity(LEN)))
        .bench_local_values(|(mut src, mut dst)| {
            block_on(copy_with_size(&mut src, &mut dst, buf_size)).unwrap();
            black_box(dst);
        });
}

/// `read_exact` into a freshly reserved buffer, one call per iteration.
///
/// This is the shape most protocol code has: read a header, then read a body of
/// the size it names.
#[divan::bench(args = SIZES)]
fn read_exact(bencher: Bencher, len: usize) {
    let src = payload(len);
    bencher
        .with_inputs(|| (src.as_slice(), Vec::with_capacity(len)))
        .bench_local_values(|(mut src, buf)| {
            let (res, buf): (_, Vec<u8>) = block_on(src.read_exact(buf)).into();
            res.unwrap();
            black_box(buf);
        });
}

/// `write_all` of one buffer, which the `Vec` sink takes in a single write.
///
/// `write_all` slices the buffer on every round of its loop, so a sink that
/// takes everything at once is the floor for that bookkeeping.
#[divan::bench(args = SIZES)]
fn write_all(bencher: Bencher, len: usize) {
    let src = leaked(len);
    bencher
        .with_inputs(|| Vec::with_capacity(len))
        .bench_local_values(|mut dst| {
            let (res, _) = block_on(dst.write_all(src)).into();
            res.unwrap();
            black_box(dst);
        });
}

/// Many small `write_all` calls through a [`BufWriter`], the case the buffered
/// writer exists for.
#[divan::bench(args = [16usize, 256, 4096])]
fn buf_writer_small_writes(bencher: Bencher, chunk: usize) {
    const LEN: usize = 1 << 18;
    let chunks = LEN / chunk;
    let data = leaked(chunk);
    bencher
        .with_inputs(|| BufWriter::new(Vec::with_capacity(LEN)))
        .bench_local_values(|mut w| {
            block_on(async {
                for _ in 0..chunks {
                    let (res, _) = w.write_all(data).await.into();
                    res.unwrap();
                }
                w.flush().await.unwrap();
            });
            black_box(w);
        });
}

/// Many small `read_exact` calls through a [`BufReader`], the mirror of the
/// case above.
#[divan::bench(args = [16usize, 256, 4096])]
fn buf_reader_small_reads(bencher: Bencher, chunk: usize) {
    const LEN: usize = 1 << 18;
    let chunks = LEN / chunk;
    let src = payload(LEN);
    bencher
        .with_inputs(|| BufReader::new(src.as_slice()))
        .bench_local_values(|mut r| {
            block_on(async {
                let mut buf = Vec::with_capacity(chunk);
                for _ in 0..chunks {
                    let (res, b): (_, Vec<u8>) = r.read_exact(buf).await.into();
                    res.unwrap();
                    buf = b;
                    buf.clear();
                }
            });
            black_box(r);
        });
}
