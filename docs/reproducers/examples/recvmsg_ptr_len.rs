// RecvMsg takes msg_control from one as_uninit() call and msg_controllen
// from another. A control buffer whose calls return different slices lets
// the kernel write past an 8-byte buffer. Runs natively (not under Miri):
//   cargo run --example recvmsg_ptr_len
// The canary next to the small buffer gets overwritten.
use std::mem::MaybeUninit;

use compio_buf::{IoBuf, IoBufMut, SetLen};
use compio::net::UdpSocket;

#[repr(C, align(8))]
struct Control {
    empty: [u8; 0],
    small: [MaybeUninit<u8>; 8],
    canary: [u8; 64],
    big: [MaybeUninit<u8>; 256],
    calls: usize,
}

impl IoBuf for Control {
    fn as_init(&self) -> &[u8] {
        &self.empty
    }
}

impl IoBufMut for Control {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        self.calls += 1;
        if self.calls % 2 == 1 { &mut self.small } else { &mut self.big }
    }
}

impl SetLen for Control {
    unsafe fn set_len(&mut self, _: usize) {}
}

#[compio::main]
async fn main() {
    let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Ask for IP_PKTINFO so the kernel returns a 32-byte control message.
    unsafe { rx.set_socket_option(libc::IPPROTO_IP, libc::IP_PKTINFO, &1i32) }.unwrap();
    let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    tx.send_to(b"hi", rx.local_addr().unwrap()).await.unwrap();

    let control = Control {
        empty: [],
        small: [MaybeUninit::new(0); 8],
        canary: [0xAA; 64],
        big: [MaybeUninit::new(0); 256],
        calls: 0,
    };
    let res = rx.recv_msg(Vec::with_capacity(16), control).await;
    let (_, control) = res.1;
    let hit = control.canary.iter().filter(|&&b| b != 0xAA).count();
    println!("{:?}, canary bytes overwritten: {hit}", res.0.map(|r| r.1));
}
