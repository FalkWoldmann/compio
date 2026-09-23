// Repeat::read fills the whole buffer from index 0 but advances relative to
// the current length, so a non-empty buffer ends up with len > capacity.
use compio_io::AsyncRead;

fn main() {
    futures_executor::block_on(async {
        let mut v = Vec::with_capacity(13);
        v.extend_from_slice(b"abc");
        let cap = v.capacity();
        let (n, v) = compio_io::repeat(42).read(v).await.unwrap();
        println!("read {n}, len {}, capacity {cap}", v.len()); // Vec::set_len precondition violated
    });
}
