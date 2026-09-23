// recv_msg returns an empty control buffer for a datagram without control
// messages, and AncillaryIter::new panics on it with "buffer too short".
// Runs natively: cargo run --example ancillary_empty_control
use compio::net::UdpSocket;
use compio_io::ancillary::{AncillaryBuf, AncillaryIter};

#[compio::main]
async fn main() {
    let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    tx.send_to(b"hi", rx.local_addr().unwrap()).await.unwrap();

    let res = rx.recv_msg(Vec::with_capacity(16), AncillaryBuf::<64>::new()).await;
    let ((_, control_len, _, _), (_, control)) = res.unwrap();
    println!("control_len = {control_len}");
    let n = unsafe { AncillaryIter::new(&control) }.count(); // panics
    println!("{n} messages");
}
