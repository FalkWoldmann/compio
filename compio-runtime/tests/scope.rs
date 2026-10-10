use std::{
    cell::Cell,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::pin,
    ptr,
    rc::Rc,
    sync::atomic::{AtomicPtr, Ordering},
    task::Poll,
    time::Duration,
};

use compio_runtime::{FutureExt, Runtime, scope, time::sleep, try_scope};
use futures_util::future::{Either, poll_fn, select};

/// Sets its flag when dropped.
struct DropFlag<'a>(&'a Cell<bool>);

impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

fn block_on<F: Future>(f: F) -> F::Output {
    Runtime::new().unwrap().block_on(f)
}

#[test]
fn borrows_and_waits_for_children() {
    block_on(async {
        let names = vec!["alpha".to_string(), "beta".to_string()];
        let total = Cell::new(0);
        let late = Cell::new(false);

        let first = scope(async |s| {
            for name in &names {
                s.spawn(async { total.set(total.get() + name.len()) });
            }
            // Never awaited: the scope still waits for it.
            s.spawn(async {
                sleep(Duration::from_millis(20)).await;
                late.set(true);
            });
            s.spawn(async { names[0].clone() }).await
        })
        .await;

        assert_eq!(first, "alpha");
        assert_eq!(total.get(), 9);
        assert!(late.get());
    })
}

#[test]
fn children_spawn_children() {
    block_on(async {
        let count = Cell::new(0);
        let count = &count;
        scope(async |s| {
            for _ in 0..3 {
                s.spawn(async move {
                    sleep(Duration::from_millis(1)).await;
                    s.spawn(async move { count.set(count.get() + 1) });
                    count.set(count.get() + 1);
                });
            }
        })
        .await;
        assert_eq!(count.get(), 6);
    })
}

#[test]
fn many_children() {
    block_on(async {
        let count = Cell::new(0);
        let count = &count;
        scope(async |s| {
            for i in 0..1000 {
                s.spawn(async move {
                    for _ in 0..(i % 4) {
                        compio_runtime::time::sleep(Duration::ZERO).await;
                    }
                    count.set(count.get() + 1);
                });
            }
        })
        .await;
        assert_eq!(count.get(), 1000);
    })
}

#[test]
fn other_tasks_keep_running() {
    // A busy scope must not starve the rest of the executor.
    block_on(async {
        let ticks = Rc::new(Cell::new(0));
        let ticker = compio_runtime::spawn({
            let ticks = ticks.clone();
            async move {
                loop {
                    ticks.set(ticks.get() + 1);
                    yield_now().await;
                }
            }
        });
        scope(async |s| {
            for _ in 0..1000 {
                s.spawn(async {
                    for _ in 0..10 {
                        yield_now().await;
                    }
                });
            }
        })
        .await;
        assert!(ticks.get() > 1, "ticker ran {} times", ticks.get());
        drop(ticker);
    })
}

#[test]
fn first_error_cancels_and_waits() {
    block_on(async {
        let cleaned_up = Cell::new(false);
        let body_dropped = Cell::new(false);

        let result: Result<(), &str> = try_scope(async |s| {
            let _guard = DropFlag(&body_dropped);
            s.try_spawn(async {
                // Stand-in for IO: wait until the scope is cancelled, then
                // clean up after a while.
                s.cancel_token().wait().await;
                sleep(Duration::from_millis(20)).await;
                cleaned_up.set(true);
                Ok(())
            });
            let failing = s.try_spawn(async { Err::<(), _>("boom") });
            // Never completes: the scope drops the body instead.
            failing.await;
            unreachable!("awaited a task that failed");
        })
        .await;

        assert_eq!(result, Err("boom"));
        assert!(cleaned_up.get(), "the scope returned before its children");
        assert!(body_dropped.get());
    })
}

#[test]
fn body_error_cancels() {
    block_on(async {
        let saw_cancel = Cell::new(false);
        let result: Result<(), &str> = try_scope(async |s| {
            s.spawn(async {
                s.cancel_token().wait().await;
                saw_cancel.set(true);
            });
            Err("body")
        })
        .await;
        assert_eq!(result, Err("body"));
        assert!(saw_cancel.get());
    })
}

#[test]
fn first_error_wins() {
    block_on(async {
        let result: Result<(), u32> = try_scope(async |s| {
            s.try_spawn(async { Err::<(), _>(1) });
            s.try_spawn(async {
                sleep(Duration::from_millis(5)).await;
                Err::<(), _>(2)
            });
            Ok(())
        })
        .await;
        assert_eq!(result, Err(1));
    })
}

#[test]
fn try_scope_ok() {
    block_on(async {
        let result: Result<u32, ()> = try_scope(async |s| {
            let a = s.try_spawn(async { Ok(1) });
            let b = s.try_spawn(async { Ok(2) });
            Ok(a.await + b.await)
        })
        .await;
        assert_eq!(result, Ok(3));
    })
}

#[test]
fn outer_cancel_token_cancels_scope() {
    block_on(async {
        let outer = compio_runtime::CancelToken::new();
        compio_runtime::spawn({
            let outer = outer.clone();
            async move {
                sleep(Duration::from_millis(10)).await;
                outer.cancel();
            }
        })
        .detach();

        let inner_cancelled = Cell::new(false);
        scope(async |s| {
            // Nested scopes are cancelled through their parent.
            s.spawn(scope(async |inner| {
                inner.cancel_token().wait().await;
                inner_cancelled.set(true);
            }));
            s.cancel_token().wait().await;
            assert!(s.is_cancelled());
        })
        .with_cancel(outer)
        .await;
        assert!(inner_cancelled.get());
    })
}

#[test]
fn panic_drops_the_rest() {
    let sibling_dropped = Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        block_on(async {
            scope(async |s| {
                s.spawn(async {
                    let _guard = DropFlag(&sibling_dropped);
                    std::future::pending::<()>().await;
                });
                s.spawn(async {
                    yield_now().await;
                    panic!("child panicked");
                });
                std::future::pending::<()>().await;
            })
            .await
        })
    }));
    let payload = result.unwrap_err();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"child panicked"));
    assert!(sibling_dropped.get());
}

#[test]
fn dropping_the_scope_drops_children() {
    block_on(async {
        let child_dropped = Cell::new(false);
        let scope = Box::pin(scope(async |s| {
            s.spawn(async {
                let _guard = DropFlag(&child_dropped);
                std::future::pending::<()>().await;
            });
            std::future::pending::<()>().await;
        }));
        match select(scope, pin!(sleep(Duration::from_millis(5)))).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, scope)) => drop(scope),
        }
        assert!(child_dropped.get());
    })
}

#[test]
fn leaking_the_scope_is_harmless() {
    block_on(async {
        let data = vec![1, 2, 3];
        let mut leaked = Box::pin(scope(async |s| {
            s.spawn(async {
                sleep(Duration::from_millis(5)).await;
                // Would read freed memory if the child ran after the leak.
                data.iter().sum::<i32>()
            })
            .await
        }));
        poll_fn(|cx| {
            assert!(leaked.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        // Leak it like `mem::forget` would, but keep it reachable, so that
        // LeakSanitizer doesn't flag it.
        static LEAKED: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
        LEAKED.store(Box::into_raw(Box::new(leaked)).cast(), Ordering::Relaxed);
        drop(data);
        // The child's timer fires and wakes the scope, which nobody polls.
        sleep(Duration::from_millis(20)).await;
    })
}

async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}
