// AncillaryBuilder::push writes through pointers that Miri rejects:
//   cargo miri run --example ancillary_push_aliasing
//     Stacked Borrows: payload written through a pointer derived from &mut cmsghdr
//   MIRIFLAGS=-Zmiri-tree-borrows cargo miri run --example ancillary_push_aliasing
//     Tree Borrows: write through an older pointer after a new &mut borrow
use compio_io::ancillary::{AncillaryBuf, ancillary_space};

fn main() {
    let mut buf = AncillaryBuf::<{ ancillary_space::<u32>() }>::new();
    buf.builder().push(1, 2, &7u32).unwrap();
}
