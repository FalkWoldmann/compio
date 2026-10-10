use std::{cell::Cell, io, net::Ipv4Addr, time::Duration};

use compio::{
    buf::BufResult,
    driver::ErrorExt,
    io::{AsyncRead, AsyncWriteExt},
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

#[compio_macros::test]
async fn inner_with_cancel_doesnt_shield_from_the_scope() {
    let (mut a, _b) = tcp_pair().await;
    let (mut c, _d) = tcp_pair().await;
    let (a, c) = (&mut a, &mut c);
    let own = CancelToken::new();

    let scope = scope(async |s| {
        let read = s.spawn(async move {
            a.read(Vec::with_capacity(16))
                .with_cancel(own.clone())
                .await
        });
        // A nested scope polled with a token of its own still follows this one.
        let nested = s.spawn(
            scope(async move |inner| {
                inner
                    .spawn(async move { c.read(Vec::with_capacity(16)).await })
                    .await
            })
            .with_cancel(CancelToken::new()),
        );
        sleep(Duration::from_millis(10)).await;
        s.cancel();
        (read.await, nested.await)
    });
    let (BufResult(res, _), BufResult(nested, _)) =
        compio::time::timeout(Duration::from_secs(5), scope)
            .await
            .expect("the cancellation didn't reach the reads");

    assert!(res.is_cancelled(), "{res:?}");
    assert!(nested.is_cancelled(), "{nested:?}");
}

#[compio_macros::test]
async fn reads_with_data_ready_fail_after_cancel() {
    let (mut a, mut b) = tcp_pair().await;
    b.write_all("hello").await.0.unwrap();
    let a = &mut a;

    let BufResult(res, buf) = scope(async |s| {
        s.cancel();
        s.spawn(async move { a.read(Vec::with_capacity(16)).await })
            .await
    })
    .await;

    assert!(res.is_cancelled(), "{res:?}");
    assert_eq!(buf.capacity(), 16);
}

#[compio_macros::test]
#[cfg(target_os = "linux")]
async fn personality_reaches_tasks() {
    use compio::driver::DriverType;

    if compio::runtime::Runtime::with_current(|r| r.driver_type()) != DriverType::IoUring {
        return;
    }
    let (_a, b) = tcp_pair().await;
    let b = &b;

    // No personality is registered with this id.
    let res = scope(async |s| {
        s.spawn(async move {
            let mut b = b;
            b.write_all("hello").await.0
        })
        .await
    })
    .with_personality(0x7ff0)
    .await;

    assert_eq!(
        res.unwrap_err().raw_os_error(),
        Some(nix::errno::Errno::EINVAL as i32)
    );
}
