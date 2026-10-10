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

use compio_runtime::{
    FutureExt, Runtime, Scope, scope,
    time::{sleep, timeout},
    try_scope,
};
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
        // About 11000 polls in all, at most 128 between ticks.
        assert!(ticks.get() > 40, "ticker ran {} times", ticks.get());
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

#[test]
fn nested_scopes_keep_other_tasks_running() {
    // The budget holds across nesting, rather than multiplying.
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
            s.spawn(scope(async |inner| {
                for _ in 0..1000 {
                    inner.spawn(async {
                        for _ in 0..10 {
                            yield_now().await;
                        }
                    });
                }
            }));
        })
        .await;
        assert!(ticks.get() > 40, "ticker ran {} times", ticks.get());
        drop(ticker);
    })
}

#[test]
fn child_awaiting_a_failed_sibling_is_dropped() {
    block_on(async {
        let waiter_dropped = Cell::new(false);
        let chained_dropped = Cell::new(false);
        let result: Result<(), &str> = timeout(
            Duration::from_secs(5),
            try_scope(async |s| {
                let failing = s.try_spawn(async {
                    yield_now().await;
                    Err::<(), _>("boom")
                });
                let waiter = s.spawn(async {
                    let _guard = DropFlag(&waiter_dropped);
                    failing.await;
                    unreachable!("awaited a task that failed");
                });
                // Dropping the waiter fails its handle in turn.
                s.spawn(async {
                    let _guard = DropFlag(&chained_dropped);
                    waiter.await;
                });
                Ok(())
            }),
        )
        .await
        .expect("the scope hung");

        assert_eq!(result, Err("boom"));
        assert!(waiter_dropped.get());
        assert!(chained_dropped.get());
    })
}

#[test]
fn body_awaiting_a_failed_task_is_dropped_right_away() {
    // The body's locals are released at once, as with `?`, even though other
    // tasks are still running and wait for that.
    block_on(async {
        let body_dropped = Cell::new(false);
        let result: Result<(), &str> = timeout(
            Duration::from_secs(5),
            try_scope(async |s| {
                let _guard = DropFlag(&body_dropped);
                s.spawn(async {
                    while !body_dropped.get() {
                        yield_now().await;
                    }
                });
                s.try_spawn(async { Err::<(), _>("boom") }).await;
                unreachable!("awaited a task that failed");
            }),
        )
        .await
        .expect("the scope hung");
        assert_eq!(result, Err("boom"));
    })
}

/// Spawns a task setting its flag when dropped, and formats the scope.
struct SpawnOnDrop<'s, 'e, E>(&'s Scope<'s, 'e, E>, &'s Cell<bool>);

impl<E> Drop for SpawnOnDrop<'_, '_, E> {
    fn drop(&mut self) {
        let _ = format!("{:?}", self.0);
        let flag = self.1;
        self.0.spawn(async move { flag.set(true) });
    }
}

#[test]
fn cleanup_spawned_while_a_failed_body_is_dropped_runs() {
    block_on(async {
        let cleaned_up = Cell::new(false);
        let result: Result<(), &str> = try_scope(async |s| {
            let _guard = SpawnOnDrop(s, &cleaned_up);
            s.try_spawn(async { Err::<(), _>("boom") });
            // Not a failed task: the body is only dropped once the tasks are
            // done.
            std::future::pending::<()>().await;
            Ok(())
        })
        .await;
        assert_eq!(result, Err("boom"));
        assert!(cleaned_up.get(), "the cleanup task never ran");
    })
}

/// Panics when dropped.
struct PanicOnDrop(&'static str);

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("{}", self.0);
    }
}

/// Cancels and spawns on the scope when dropped, as a task may.
struct UseOnDrop<'s, 'e>(&'s Scope<'s, 'e>, &'s Cell<bool>);

impl Drop for UseOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.0.cancel();
        self.0.spawn(async {});
        self.1.set(true);
    }
}

#[test]
fn tasks_are_dropped_when_dropping_the_body_panics() {
    let child_dropped = Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        block_on(async {
            let mut scope = Box::pin(scope(async |s| {
                s.spawn(async {
                    let _guard = UseOnDrop(s, &child_dropped);
                    std::future::pending::<()>().await;
                });
                let _bomb = PanicOnDrop("body dropped");
                std::future::pending::<()>().await;
            }));
            poll_fn(|cx| {
                assert!(scope.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(scope);
        })
    }));
    let payload = result.unwrap_err();
    assert_eq!(payload.downcast_ref::<String>().unwrap(), "body dropped");
    assert!(child_dropped.get());
}

thread_local! {
    static SYNC_CHILD_DROPPED: Cell<bool> = const { Cell::new(false) };
}

/// A body that isn't `async`, so it runs before the scope is first polled.
fn sync_body<'s, 'e>(s: &'s Scope<'s, 'e>) -> std::future::Ready<()> {
    // Moved in, so that dropping the task drops it although it never ran.
    let guard = SyncUseOnDrop(s);
    s.spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    panic!("sync body");
}

/// Like `UseOnDrop`, with a flag that outlives the scope.
struct SyncUseOnDrop<'s, 'e>(&'s Scope<'s, 'e>);

impl Drop for SyncUseOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.0.cancel();
        self.0.spawn(async {});
        SYNC_CHILD_DROPPED.set(true);
    }
}

#[test]
fn tasks_are_dropped_when_a_sync_body_panics() {
    let result = catch_unwind(|| block_on(scope(sync_body)));
    assert!(result.is_err());
    assert!(SYNC_CHILD_DROPPED.get());
}

#[test]
fn panicking_destructors_dont_stop_the_teardown() {
    let dropped = [Cell::new(false), Cell::new(false)];
    let result = catch_unwind(AssertUnwindSafe(|| {
        block_on(async {
            let mut scope = Box::pin(scope(async |s| {
                for flag in &dropped {
                    s.spawn(async move {
                        let _flag = DropFlag(flag);
                        let _bomb = PanicOnDrop("child dropped");
                        std::future::pending::<()>().await;
                    });
                }
                std::future::pending::<()>().await;
            }));
            poll_fn(|cx| {
                assert!(scope.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(scope);
        })
    }));
    let payload = result.unwrap_err();
    assert_eq!(payload.downcast_ref::<String>().unwrap(), "child dropped");
    assert!(dropped.iter().all(Cell::get));
}

#[test]
fn a_panic_survives_panicking_destructors() {
    let result = catch_unwind(AssertUnwindSafe(|| {
        block_on(async {
            scope(async |s| {
                s.spawn(async {
                    yield_now().await;
                    panic!("child panicked");
                });
                let _bomb = PanicOnDrop("body dropped");
                std::future::pending::<()>().await;
            })
            .await
        })
    }));
    let payload = result.unwrap_err();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"child panicked"));
}

#[test]
fn stale_wakers_after_slot_reuse() {
    block_on(async {
        let stash = Cell::new(None);
        scope(async |s| {
            // The first task leaves its waker behind, and its slot is reused.
            s.spawn(poll_fn(|cx| {
                stash.set(Some(cx.waker().clone()));
                Poll::Ready(())
            }))
            .await;
            let second = s.spawn(async {
                yield_now().await;
                2
            });
            stash.take().unwrap().wake();
            assert_eq!(second.await, 2);
        })
        .await;
        // And after the scope is gone.
        if let Some(waker) = stash.take() {
            waker.wake();
        }
    })
}

#[test]
fn is_finished() {
    block_on(async {
        let result: Result<(), ()> = try_scope(async |s| {
            let ok = s.spawn(async {});
            let failed = s.try_spawn(async { Err::<(), _>(()) });
            let pending = s.spawn(yield_now());
            assert!(!ok.is_finished() && !failed.is_finished() && !pending.is_finished());
            yield_now().await;
            assert!(ok.is_finished() && failed.is_finished());
            Ok(())
        })
        .await;
        assert_eq!(result, Err(()));
    })
}

#[test]
fn try_spawn_in_a_plain_scope() {
    block_on(async {
        let value = scope(async |s| s.try_spawn(async { Ok(1) }).await).await;
        assert_eq!(value, 1);
    })
}

#[test]
fn scope_inside_a_spawned_task() {
    block_on(async {
        let value = compio_runtime::spawn(async {
            let data = [1, 2, 3];
            scope(async |s| {
                let sum = s.spawn(async { data.iter().sum::<i32>() });
                sum.await
            })
            .await
        })
        .await
        .unwrap();
        assert_eq!(value, 6);
    })
}

#[test]
fn woken_from_another_thread() {
    block_on(async {
        let value = scope(async |s| {
            s.spawn(compio_runtime::spawn_blocking(|| {
                std::thread::sleep(Duration::from_millis(10));
                7
            }))
            .await
            .unwrap()
        })
        .await;
        assert_eq!(value, 7);
    })
}

#[test]
fn waker_dropped_on_another_thread() {
    // Under LeakSanitizer: the clone mustn't keep the scope's token, and with
    // it the runtime, alive.
    block_on(async {
        scope(async |s| {
            s.spawn(poll_fn(|cx| {
                let waker = cx.waker().clone();
                std::thread::spawn(move || drop(waker)).join().unwrap();
                Poll::Ready(())
            }))
            .await;
        })
        .await;
    })
}

#[test]
fn nested_try_scope_errors_propagate() {
    block_on(async {
        let outer_cancelled = Cell::new(false);
        let result: Result<(), &str> = try_scope(async |s| {
            s.spawn(async {
                s.cancel_token().wait().await;
                outer_cancelled.set(true);
            });
            s.try_spawn(try_scope(async |inner| {
                inner.try_spawn(async { Err::<(), _>("inner") });
                Ok(())
            }));
            Ok(())
        })
        .await;
        assert_eq!(result, Err("inner"));
        assert!(outer_cancelled.get());
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
