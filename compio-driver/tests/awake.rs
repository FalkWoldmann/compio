#![cfg(io_uring)]

use std::{io, os::fd::AsRawFd, time::Duration};

use compio_driver::Proactor;

fn is_readable(driver: &Proactor) -> bool {
    let mut fd = libc::pollfd {
        fd: driver.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let res = unsafe { libc::poll(&mut fd, 1, 0) };
    assert!(res >= 0);
    res > 0
}

#[test]
fn wake_after_timed_out_poll() {
    let mut driver = Proactor::new().unwrap();
    let waker = driver.waker();

    let err = driver.poll(Some(Duration::ZERO)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    waker.wake_by_ref();
    assert!(!is_readable(&driver));

    assert!(driver.flush());
    waker.wake_by_ref();
    assert!(is_readable(&driver));
}
