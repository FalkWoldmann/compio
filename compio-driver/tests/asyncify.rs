use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    time::Duration,
};

use compio_buf::BufResult;
use compio_driver::{Key, OpCode, Proactor, PushEntry, op::Asyncify};

fn take_key<T: OpCode, R>(res: PushEntry<Key<T>, R>) -> Key<T> {
    match res {
        PushEntry::Pending(key) => key,
        PushEntry::Ready(_) => {
            unreachable!()
        }
    }
}

fn wait_for<T: OpCode>(driver: &mut Proactor, mut key: Key<T>) -> BufResult<usize, T> {
    loop {
        _ = driver.poll(Some(Duration::from_millis(1)));
        match driver.pop(key) {
            PushEntry::Pending(k) => key = k,
            PushEntry::Ready(res) => break res,
        }
    }
}

#[test]
fn panic() {
    let mut driver = Proactor::builder().thread_pool_limit(1).build().unwrap();

    // make panicking less noisy
    std::panic::set_hook(Box::new(|_| {}));

    let a = take_key(driver.push(Asyncify::new(|| -> BufResult<usize, ()> {
        panic!("this should not blow up driver's thread pool");
    })));
    let b = take_key(driver.push(Asyncify::new(|| BufResult(Ok(1), ()))));

    let res_b = wait_for(&mut driver, b);
    let res_a = catch_unwind(AssertUnwindSafe(|| wait_for(&mut driver, a)));

    _ = std::panic::take_hook(); // restore to default hook

    assert!(res_b.0.is_ok_and(|res| res == 1));
    assert!(res_a.is_err());
}

#[test]
#[should_panic(expected = "the thread pool is needed but no worker thread is running")]
fn disable() {
    let mut driver = Proactor::builder().thread_pool_limit(0).build().unwrap();

    match driver.push(Asyncify::new(|| -> BufResult<usize, ()> {
        panic!("the asyncify operation should not be executed when the thread pool is disabled");
    })) {
        PushEntry::Pending(_) => {}
        PushEntry::Ready(_) => {
            unreachable!()
        }
    }

    driver.poll(None).unwrap();
}

#[test]
fn small_pool() {
    let mut driver = Proactor::builder().thread_pool_limit(1).build().unwrap();
    let clo = || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        BufResult(Ok(0), ())
    };

    let a = take_key(driver.push(Asyncify::new(clo)));
    let b = take_key(driver.push(Asyncify::new(clo)));

    wait_for(&mut driver, a).unwrap();
    wait_for(&mut driver, b).unwrap();
}

#[test]
fn pool_timeout_race() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use compio_driver::AsyncifyPool;

    // A tiny receive timeout makes idle workers exit constantly, racing with
    // dispatches that claim them.
    let pool = AsyncifyPool::new(4, Duration::from_micros(1));
    let done = Arc::new(AtomicUsize::new(0));
    const TASKS: usize = 20_000;
    for i in 0..TASKS {
        // Let workers go idle and time out between bursts.
        if i % 64 == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
        let mut f = {
            let done = done.clone();
            move || {
                done.fetch_add(1, Ordering::Relaxed);
            }
        };
        while let Err(e) = pool.dispatch(f) {
            f = e.0;
            std::thread::yield_now();
        }
    }
    let start = std::time::Instant::now();
    while done.load(Ordering::Relaxed) < TASKS {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "lost dispatched tasks"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
