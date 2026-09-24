//! Benchmarks for delimiter-based framing.
//!
//! `extract` measures a single [`Framer::extract`] call on a buffer that holds
//! one complete frame (`hit`) or only a partial one (`miss`). `framed` drives a
//! whole [`Framed`] stream over an in-memory reader that hands out data in
//! fixed-size chunks, so large frames are scanned once per chunk received.

use std::{hint::black_box, io::Cursor};

use compio_buf::{BufResult, IoBufExt, IoBufMut, bytes::Bytes};
use compio_io::{
    AsyncRead, AsyncReadAt,
    framed::{
        Framed,
        codec::bytes::BytesCodec,
        frame::{AnyDelimited, Framer, LineDelimited},
    },
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures_executor::block_on;
use futures_util::StreamExt;

const EXTRACT_SIZES: [usize; 4] = [16, 256, 4096, 65536];

/// `len` bytes of printable payload containing no `\n` or `\r`.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| b'a' + (i % 26) as u8).collect()
}

fn frame(len: usize, delimiter: &[u8]) -> Vec<u8> {
    let mut buf = payload(len - delimiter.len());
    buf.extend_from_slice(delimiter);
    buf
}

fn bench_extract_with<F: Framer<Vec<u8>>>(
    c: &mut Criterion,
    name: &str,
    delimiter: &[u8],
    mut framer: F,
) {
    let mut group = c.benchmark_group(format!("extract/{name}"));
    for len in EXTRACT_SIZES {
        group.throughput(Throughput::Bytes(len as u64));

        let hit = frame(len, delimiter).slice(..);
        group.bench_with_input(BenchmarkId::new("hit", len), &hit, |b, buf| {
            b.iter(|| framer.extract(black_box(buf)).unwrap())
        });

        let miss = payload(len).slice(..);
        group.bench_with_input(BenchmarkId::new("miss", len), &miss, |b, buf| {
            b.iter(|| framer.extract(black_box(buf)).unwrap())
        });
    }
    group.finish();
}

fn bench_extract(c: &mut Criterion) {
    bench_extract_with(c, "line", b"\n", LineDelimited::new());
    bench_extract_with(c, "crlf", b"\r\n", AnyDelimited::new(b"\r\n"));
}

/// A reader that returns at most `chunk` bytes per read, like a socket.
struct ChunkedReader {
    data: Cursor<Vec<u8>>,
    chunk: usize,
}

impl AsyncRead for ChunkedReader {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        let pos = self.data.position() as usize;
        let end = (pos + self.chunk).min(self.data.get_ref().len());
        let BufResult(res, buf) = self.data.get_ref()[..end].read_at(buf, pos as u64).await;
        if let Ok(n) = res {
            self.data.set_position((pos + n) as u64);
        }
        BufResult(res, buf)
    }
}

/// Reads `total` bytes of `line_len`-byte lines in `chunk`-byte reads.
fn read_lines(total: usize, line_len: usize, chunk: usize) -> usize {
    let data = frame(line_len, b"\n").repeat(total / line_len);
    let reader = ChunkedReader {
        data: Cursor::new(data),
        chunk,
    };
    let mut framed =
        Framed::new::<Bytes, Bytes>(BytesCodec::new(), LineDelimited::new()).with_reader(reader);
    block_on(async {
        let mut count = 0;
        while let Some(line) = framed.next().await {
            black_box(line.unwrap());
            count += 1;
        }
        count
    })
}

fn bench_framed(c: &mut Criterion) {
    const TOTAL: usize = 1 << 20;

    let mut group = c.benchmark_group("framed/lines");
    group.sample_size(20);
    group.throughput(Throughput::Bytes(TOTAL as u64));
    // (line length, bytes per read)
    for (line_len, chunk) in [
        (64, 64 * 1024),
        (4096, 64 * 1024),
        (64 * 1024, 4096),
        (TOTAL, 4096),
    ] {
        group.bench_function(format!("line={line_len}/read={chunk}"), |b| {
            b.iter(|| assert_eq!(read_lines(TOTAL, line_len, chunk), TOTAL / line_len))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_extract, bench_framed);
criterion_main!(benches);
