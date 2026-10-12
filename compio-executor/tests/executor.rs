use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use compio_executor::{Executor, ExecutorConfig, JoinError, JoinHandle};

std::thread_local! {
    static EXE: Executor = Executor::new();
}

fn setup_log() {
    #[cfg(feature = "enable_log")]
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();
}

fn spawn<F: Future + 'static>(f: F) -> JoinHandle<F::Output> {
    EXE.with(|exe| exe.spawn(f))
}

fn block_on<F: Future + 'static>(f: F) -> F::Output {
    EXE.with(|exe| {
        let cx = &mut Context::from_waker(Waker::noop());
        let mut f = pin!(f);
        loop {
            if let Poll::Ready(res) = f.as_mut().poll(cx) {
                return res;
            }
            exe.tick();
        }
    })
}

struct Yield(bool);

impl Future for Yield {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

async fn yield_now() {
    Yield(false).await
}

#[test]
fn test_executor_runs_to_completion() {
    setup_log();

    block_on(async {
        let values = std::rc::Rc::new(RefCell::new(Vec::new()));

        let a = {
            let values = values.clone();
            spawn(async move {
                for i in 0..5 {
                    values.borrow_mut().push(("a", i));
                    yield_now().await;
                }
                10usize
            })
        };

        let b = {
            let values = values.clone();
            spawn(async move {
                for i in 0..3 {
                    values.borrow_mut().push(("b", i));
                    yield_now().await;
                }
                20usize
            })
        };

        let ra = a.await.unwrap();
        let rb = b.await.unwrap();

        assert_eq!(ra, 10);
        assert_eq!(rb, 20);

        let values = values.borrow();
        assert_eq!(values.iter().filter(|(t, _)| *t == "a").count(), 5);
        assert_eq!(values.iter().filter(|(t, _)| *t == "b").count(), 3);
    });
}

#[test]
fn test_cancel_before_poll_returns_canceled() {
    setup_log();

    block_on(async {
        let hit = std::rc::Rc::new(Cell::new(false));
        let hit_task = hit.clone();

        let handle = spawn(async move {
            hit_task.set(true);
            1usize
        });

        let res = handle.cancel().await;

        assert!(res.is_none());
        assert!(
            !hit.get(),
            "future body should never run after cancel-before-poll"
        );
    });
}

#[test]
fn test_cancel_during_execution_returns_canceled() {
    setup_log();

    block_on(async {
        let entered = std::rc::Rc::new(Cell::new(false));
        let done = std::rc::Rc::new(Cell::new(false));

        let entered_task = entered.clone();
        let done_task = done.clone();

        let handle = spawn(async move {
            entered_task.set(true);
            yield_now().await;
            done_task.set(true);
            42usize
        });

        while !entered.get() {
            yield_now().await;
        }

        let res = handle.cancel().await;

        assert!(res.is_none());
        assert!(
            !done.get(),
            "future should be dropped after cancellation before completing"
        );
    });
}

#[test]
fn test_join_handle_drop_cancels_task() {
    setup_log();

    block_on(async {
        let completed = std::rc::Rc::new(Cell::new(false));
        let completed_task = completed.clone();

        let handle = spawn(async move {
            for _ in 0..3 {
                yield_now().await;
            }
            completed_task.set(true);
        });

        drop(handle);

        for _ in 0..8 {
            yield_now().await;
        }

        assert!(
            !completed.get(),
            "dropping JoinHandle should cancel and prevent completion"
        );
    });
}

#[test]
fn test_detach_allows_task_to_continue() {
    setup_log();

    block_on(async {
        let completed = std::rc::Rc::new(Cell::new(false));
        let completed_task = completed.clone();

        let handle = spawn(async move {
            for _ in 0..4 {
                yield_now().await;
            }
            completed_task.set(true);
        });

        handle.detach();

        for _ in 0..10 {
            yield_now().await;
        }

        assert!(
            completed.get(),
            "detached task should continue running to completion"
        );
    });
}

#[test]
fn test_panic_does_not_affect_other_tasks() {
    setup_log();

    block_on(async {
        let run_count = std::rc::Rc::new(Cell::new(0));
        let run_count_task = run_count.clone();

        // This task will panic. We detach it so we aren't waiting on it
        // specifically, but we want to make sure the executor keeps
        // ticking.
        spawn(async {
            panic!("intentional panic");
        })
        .detach();

        // Give the panicked task a chance to run and panic.
        let handle = spawn(async move {
            for _ in 0..5 {
                yield_now().await;
            }
            run_count_task.set(run_count_task.get() + 1);
        });

        handle.await.unwrap();
        assert_eq!(run_count.get(), 1);
    });
}

#[test]
fn test_join_result_resume_unwind() {
    setup_log();

    block_on(async {
        let handle: JoinHandle<()> = spawn(async {
            panic!("resume_unwind panic");
        });

        let JoinError::Panicked(res) = handle.await.unwrap_err() else {
            unreachable!("Future panicked")
        };

        let Some(msg) = res.downcast_ref::<&'static str>() else {
            unreachable!("Panic payload should be a static string")
        };

        assert_eq!(*msg, "resume_unwind panic");
    });
}

#[test]
fn test_num_alive_tasks() {
    setup_log();

    let exe = Executor::new();
    assert_eq!(exe.num_alive_tasks(), 0);

    // A task is alive as soon as it is spawned, before it ever runs.
    let finishes = exe.spawn(async {});
    let pending = exe.spawn(std::future::pending::<()>());
    let cancelled = exe.spawn(std::future::pending::<()>());
    assert_eq!(exe.num_alive_tasks(), 3);

    // The finished task is gone, the ones waiting to be woken still count.
    exe.tick();
    assert!(finishes.is_finished());
    assert_eq!(exe.num_alive_tasks(), 2);

    // Detaching does not change the count, the task keeps running.
    pending.detach();
    assert_eq!(exe.num_alive_tasks(), 2);

    // Dropping the handle cancels the task; it is removed when the
    // executor processes the cancellation.
    drop(cancelled);
    exe.tick();
    assert_eq!(exe.num_alive_tasks(), 1);

    exe.clear();
    assert_eq!(exe.num_alive_tasks(), 0);
}

#[test]
fn test_tick_runs_task_spawned_by_last_task() {
    setup_log();

    let ran = Rc::new(Cell::new(false));
    let inner_ran = ran.clone();
    spawn(async move {
        spawn(async move { inner_ran.set(true) }).detach();
    })
    .detach();

    let has_hot = EXE.with(|exe| exe.tick());
    assert!(ran.get());
    assert!(!has_hot);
}

#[test]
fn test_tick_runs_task_woken_by_last_task() {
    setup_log();

    let ready = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<Waker>));

    let (waiter_ready, waiter_waker) = (ready.clone(), waker.clone());
    let waiter = spawn(std::future::poll_fn(move |cx| {
        if waiter_ready.get() {
            Poll::Ready(())
        } else {
            *waiter_waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    }));
    spawn(async move {
        ready.set(true);
        waker.borrow_mut().take().expect("waiter ran first").wake();
    })
    .detach();

    let has_hot = EXE.with(|exe| exe.tick());
    assert!(waiter.is_finished());
    assert!(!has_hot);
}

#[test]
fn test_tick_runs_lone_self_waking_task_once() {
    setup_log();

    let exe = Executor::new();
    let polls = Rc::new(Cell::new(0));
    let counter = polls.clone();
    exe.spawn(async move {
        loop {
            counter.set(counter.get() + 1);
            yield_now().await;
        }
    })
    .detach();

    assert!(exe.tick());
    assert_eq!(polls.get(), 1);

    exe.clear();
}

#[test]
fn test_tick_stops_at_max_interval() {
    setup_log();

    let exe = Executor::with_config(ExecutorConfig {
        max_interval: 8,
        ..Default::default()
    });
    let polls = Rc::new(Cell::new(0));
    for _ in 0..2 {
        let counter = polls.clone();
        exe.spawn(async move {
            loop {
                counter.set(counter.get() + 1);
                yield_now().await;
            }
        })
        .detach();
    }

    assert!(exe.tick());
    assert_eq!(polls.get(), 8);

    exe.clear();
}

#[test]
fn test_tick_stops_at_max_interval_for_spawn_chain() {
    setup_log();

    fn chain(exe: Rc<Executor>, polls: Rc<Cell<u32>>) {
        let next = exe.clone();
        exe.spawn(async move {
            polls.set(polls.get() + 1);
            chain(next.clone(), polls);
        })
        .detach();
    }

    let exe = Rc::new(Executor::with_config(ExecutorConfig {
        max_interval: 8,
        ..Default::default()
    }));
    let polls = Rc::new(Cell::new(0));
    chain(exe.clone(), polls.clone());

    assert!(exe.tick());
    assert_eq!(polls.get(), 8);

    exe.clear();
}

#[test]
fn test_nested_tick_wakes_running_task() {
    setup_log();

    let handle = spawn(async {
        let mut child = pin!(spawn(async {}));
        std::future::poll_fn(|cx| {
            if child.as_mut().poll(cx).is_ready() {
                return Poll::Ready(());
            }
            EXE.with(|exe| exe.tick());
            child.as_mut().poll(cx).map(drop)
        })
        .await
    });

    EXE.with(|exe| exe.tick());
    let res = pin!(handle).poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(res, Poll::Ready(Ok(()))));
}
