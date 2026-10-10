use std::{cell::Cell, io, net::Ipv4Addr, time::Duration};

use compio::{
    buf::BufResult,
    driver::ErrorExt,
    io::AsyncRead,
    net::{TcpListener, TcpStream},
    runtime::{CancelToken, FutureExt, scope, try_scope},
    time::sleep,
};

/// A connected pair, where reading from either side blocks until the other one
/// writes.
async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (a, (b, _)) =
        futures_util::try_join!(TcpStream::connect(&addr), listener.accept()).unwrap();
    (a, b)
}

#[compio_macros::test]
async fn cancel_reaches_child_io() {
    let (mut a, _b) = tcp_pair().await;
    let a = &mut a;

    let BufResult(res, buf) = scope(async |s| {
        let read = s.spawn(async move { a.read(Vec::with_capacity(16)).await });
        // Let the read start before cancelling it.
        sleep(Duration::from_millis(10)).await;
        s.cancel();
        read.await
    })
    .await;

    assert!(res.is_cancelled(), "{res:?}");
    // The buffer comes back with the cancelled read.
    assert_eq!(buf.capacity(), 16);
}

#[compio_macros::test]
async fn first_error_cancels_sibling_io() {
    let (mut a, _b) = tcp_pair().await;
    let a = &mut a;
    let cleaned_up = Cell::new(false);
    let cleaned_up = &cleaned_up;

    let result: io::Result<()> = try_scope(async |s| {
        s.try_spawn(async move {
            let BufResult(res, buf) = a.read(Vec::with_capacity(16)).await;
            assert!(res.is_cancelled(), "{res:?}");
            assert_eq!(buf.capacity(), 16);
            // Graceful cleanup still gets to do IO-free work, and the scope
            // waits for it.
            sleep(Duration::from_millis(10)).await;
            cleaned_up.set(true);
            Ok(())
        });
        s.try_spawn(async {
            sleep(Duration::from_millis(10)).await;
            Err::<(), _>(io::Error::other("boom"))
        });
        Ok(())
    })
    .await;

    assert_eq!(result.unwrap_err().to_string(), "boom");
    assert!(cleaned_up.get());
}

#[compio_macros::test]
async fn outer_token_cancels_child_io() {
    let (mut a, _b) = tcp_pair().await;
    let a = &mut a;
    let token = CancelToken::new();

    compio::runtime::spawn({
        let token = token.clone();
        async move {
            sleep(Duration::from_millis(10)).await;
            token.cancel();
        }
    })
    .detach();

    let BufResult(res, _) = scope(async |s| {
        s.spawn(async move { a.read(Vec::with_capacity(16)).await })
            .await
    })
    .with_cancel(token)
    .await;

    assert!(res.is_cancelled(), "{res:?}");
}

#[compio_macros::test]
async fn cancel_reaches_nested_scopes() {
    let (mut a, _b) = tcp_pair().await;
    let a = &mut a;

    let BufResult(res, _) = scope(async |s| {
        let inner = s.spawn(scope(async move |inner| {
            inner
                .spawn(async move { a.read(Vec::with_capacity(16)).await })
                .await
        }));
        sleep(Duration::from_millis(10)).await;
        s.cancel();
        inner.await
    })
    .await;

    assert!(res.is_cancelled(), "{res:?}");
}

#[compio_macros::test]
async fn completed_io_is_unaffected() {
    let (mut a, b) = tcp_pair().await;
    let a = &mut a;
    let b = &b;

    let BufResult(res, buf) = scope(async |s| {
        let read = s.spawn(async move { a.read(Vec::with_capacity(16)).await });
        s.spawn(async move {
            use compio::io::AsyncWriteExt;
            let mut b = b;
            b.write_all("hello").await.0.unwrap();
        });
        read.await
    })
    .await;

    assert_eq!(&buf[..res.unwrap()], b"hello");
}
