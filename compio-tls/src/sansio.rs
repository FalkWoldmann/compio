//! **Prototype.** Drive `rustls` directly on compio's own owned-buffer IO
//! traits, with no `futures::AsyncRead` in between.
//!
//! The shipped [`TlsStream`](crate::TlsStream) is generic over
//! `futures_util::AsyncRead + AsyncWrite`, so using it with a compio stream
//! stacks four layers:
//!
//! ```text
//! compio stream (owned buffers, completion)
//!   -> compio_io::compat::SyncStream   (owns a Buffer, re-exposes Read/Write)
//!     -> futures::AsyncRead/AsyncWrite (borrowed slices, readiness)
//!       -> futures_rustls              (readiness adapter)
//!         -> rustls::ConnectionCommon  (sans-io core)
//! ```
//!
//! Two of those exist only to make an owned-buffer stream look like a
//! borrowed-slice one, so that an adapter can turn it back into the sans-io
//! core's "hand me bytes, take bytes back" shape. Talking to the core
//! directly removes both:
//!
//! ```text
//! compio stream (owned buffers, completion)
//!   -> rustls::ConnectionCommon        (sans-io core)
//! ```
//!
//! The reason this works out is that completion IO and sans-io want the same
//! thing. Completion IO says *you* must own the buffer, because the kernel
//! writes into it after the call returns. Sans-io says *you* own the buffers
//! and feed me slices. The `mem::take`/give-back dance below is both at once:
//! the buffer handed to `read` is the buffer handed to `read_tls`.
//!
//! Prototype scope: client and server handshakes, read, write, flush and
//! close-notify, over any compio [`AsyncRead`] + [`AsyncWrite`]. Not wired
//! into [`TlsConnector`](crate::TlsConnector); no vectored writes; no
//! renegotiation handling beyond what a single `process_new_packets` loop
//! gives. See the note on `ensure_init` in `read` for the one place this
//! still pays for an impedance mismatch.

use std::{
    io::{self, Read, Write},
    mem,
};

use compio_buf::{BufResult, IoBuf, IoBufMut, IoBufMutExt, SetLenExt};
use compio_io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use rustls::Connection;

/// How much wire data to ask for in one completion read.
const READ_CHUNK: usize = 16 * 1024;

/// A TLS stream that drives [`rustls`] as a sans-io state machine over a
/// compio stream.
#[derive(Debug)]
pub struct TlsStream<S> {
    stream: S,
    conn: Connection,
    /// Wire bytes read from `stream`, owned across the await that fills them.
    incoming: Vec<u8>,
    /// Records `rustls` wants on the wire, owned across the await that
    /// drains them.
    outgoing: Vec<u8>,
    eof: bool,
}

impl<S> TlsStream<S> {
    /// Wrap a stream around an already-configured connection.
    pub fn new(stream: S, conn: impl Into<Connection>) -> Self {
        Self {
            stream,
            conn: conn.into(),
            incoming: Vec::with_capacity(READ_CHUNK),
            outgoing: Vec::with_capacity(READ_CHUNK),
            eof: false,
        }
    }

    /// The negotiated ALPN protocol, if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn_protocol()
    }

    /// The negotiated TLS protocol version, once the handshake is done.
    pub fn protocol_version(&self) -> Option<rustls::ProtocolVersion> {
        self.conn.protocol_version()
    }

    /// How many certificates the peer presented.
    pub fn peer_certificates_len(&self) -> usize {
        self.conn.peer_certificates().map_or(0, |c| c.len())
    }

    /// Borrow the underlying stream.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Take back the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

fn tls_err(e: rustls::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl<S: AsyncRead + AsyncWrite> TlsStream<S> {
    /// Write out everything `rustls` currently wants on the wire.
    ///
    /// `write_tls` is the sans-io half: it fills a buffer we own. The compio
    /// half then hands that same buffer to the kernel and takes it back.
    async fn flush_outgoing(&mut self) -> io::Result<()> {
        while self.conn.wants_write() {
            let mut out = mem::take(&mut self.outgoing);
            out.clear();
            let res = self.conn.write_tls(&mut out);
            if let Err(e) = res {
                self.outgoing = out;
                return Err(e);
            }

            let BufResult(res, out) = self.stream.write_all(out).await;
            self.outgoing = out;
            res?;
        }
        Ok(())
    }

    /// Read one chunk from the wire and feed it to `rustls`.
    ///
    /// Returns the number of wire bytes read; zero means the peer closed.
    async fn feed_incoming(&mut self) -> io::Result<usize> {
        let mut buf = mem::take(&mut self.incoming);
        buf.clear();
        buf.reserve(READ_CHUNK);

        // The buffer crosses the await owned by the operation, then comes
        // back. This is the shape completion IO requires, and it is also
        // exactly what `read_tls` wants below: a slice of memory we own.
        let BufResult(res, buf) = self.stream.read(buf).await;
        self.incoming = buf;
        let n = res?;
        if n == 0 {
            self.eof = true;
            return Ok(0);
        }

        let incoming = mem::take(&mut self.incoming);
        let mut cursor = &incoming[..n];
        while !cursor.is_empty() {
            match self.conn.read_tls(&mut cursor) {
                Ok(0) => break,
                Ok(_) => {
                    if let Err(e) = self.conn.process_new_packets() {
                        // Give rustls a chance to emit its alert before the
                        // error surfaces.
                        self.incoming = incoming;
                        let _ = self.flush_outgoing().await;
                        return Err(tls_err(e));
                    }
                }
                Err(e) => {
                    self.incoming = incoming;
                    return Err(e);
                }
            }
        }
        self.incoming = incoming;
        Ok(n)
    }

    /// Run the handshake to completion.
    pub async fn handshake(&mut self) -> io::Result<()> {
        while self.conn.is_handshaking() {
            self.flush_outgoing().await?;
            if !self.conn.is_handshaking() {
                break;
            }
            if self.conn.wants_read() && self.feed_incoming().await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "peer closed during TLS handshake",
                ));
            }
        }
        self.flush_outgoing().await
    }
}

/// Pull whatever plaintext `rustls` has ready into `buf`.
///
/// Deliberately not `async`: the borrow `ensure_init` takes must not span an
/// await, or it would have to live as long as the buffer's own `'static`
/// bound.
fn pull_plaintext<B: IoBufMut>(conn: &mut Connection, buf: &mut B) -> io::Result<usize> {
    // `rustls::Reader` is `std::io::Read`, which needs an initialized
    // `&mut [u8]`. `ensure_init` zeroes the tail to provide one.
    //
    // This is the one place the prototype still pays for an impedance
    // mismatch, and note that it is std's `Read`, not sans-io: a
    // `read_buf`-shaped entry point taking `&mut [MaybeUninit<u8>]` would let
    // the plaintext land in the caller's buffer with nothing zeroed first.
    // `(*buf)`, not `buf`: `&'static mut B` itself implements `IoBufMut`, so
    // method resolution on `&mut B` finds that impl first and pins the borrow
    // to `'static`. The same reason the crate writes `(*self).buf_len()`.
    let slice = <B as IoBufMutExt>::ensure_init(buf);
    // `<[u8]>::is_empty`, fully qualified: with the `IoBuf` ext traits in
    // scope, a bare `slice.is_empty()` resolves to `IoBufExt::is_empty` on
    // `&'static mut [u8]` -- the same `&'static mut B` impl that forces the
    // `(*self)` idiom elsewhere in the crate -- and pins the borrow to
    // `'static`.
    if <[u8]>::is_empty(slice) {
        return Ok(0);
    }
    match conn.reader().read(slice) {
        Ok(n) => Ok(n),
        // No plaintext ready yet; the caller feeds more wire data.
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
        Err(e) => Err(e),
    }
}

impl<S: AsyncRead + AsyncWrite> AsyncRead for TlsStream<S> {
    async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        if (&mut buf).buf_capacity() == 0 {
            return BufResult(Ok(0), buf);
        }

        loop {
            match pull_plaintext(&mut self.conn, &mut buf) {
                Ok(n) if n > 0 => {
                    // SAFETY:
                    // Operation: `SetLenExt::advance_to(n)`.
                    // Contract: `n <= as_uninit().len()` and `[buf_len(), n)`
                    // must be initialized.
                    // Evidence:
                    // - POSTCONDITION: `n` came from a `Read::read` into the
                    //   slice `ensure_init` returned, which spans the whole
                    //   extent, so `n` is at most its length.
                    // - POSTCONDITION: `ensure_init` left every byte of that
                    //   slice initialized.
                    unsafe { buf.advance_to(n) };
                    return BufResult(Ok(n), buf);
                }
                Ok(_) => {}
                Err(e) => return BufResult(Err(e), buf),
            }

            if self.eof {
                return BufResult(Ok(0), buf);
            }
            if let Err(e) = self.flush_outgoing().await {
                return BufResult(Err(e), buf);
            }
            match self.feed_incoming().await {
                Ok(0) => return BufResult(Ok(0), buf),
                Ok(_) => continue,
                Err(e) => return BufResult(Err(e), buf),
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite> AsyncWrite for TlsStream<S> {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        // `as_init` is safe: plaintext in, no uninitialized memory involved.
        let n = match self.conn.writer().write(buf.as_init()) {
            Ok(n) => n,
            Err(e) => return BufResult(Err(e), buf),
        };
        match self.flush_outgoing().await {
            Ok(()) => BufResult(Ok(n), buf),
            Err(e) => BufResult(Err(e), buf),
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.conn.writer().flush()?;
        self.flush_outgoing().await?;
        self.stream.flush().await
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        self.conn.send_close_notify();
        self.flush_outgoing().await?;
        self.stream.shutdown().await
    }
}
