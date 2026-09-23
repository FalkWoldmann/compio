use compio_buf::*;

fn buf() -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&[1, 2, 3, 4]);
    v
}

#[test]
#[should_panic(expected = "initialized prefix")]
fn refuses_to_move_spare_capacity_into_the_prefix() {
    buf().copy_within(4..8, 0);
}

#[test]
#[should_panic(expected = "initialized prefix")]
fn refuses_a_partial_overlap_with_spare_capacity() {
    buf().copy_within(2..6, 0);
}

#[test]
fn allows_copies_that_keep_the_prefix_initialized() {
    let mut v = buf();
    v.copy_within(1..4, 0);
    assert_eq!(v, [2, 3, 4, 4]);

    let mut v = buf();
    v.copy_within(0..4, 4);
    assert_eq!(v, [1, 2, 3, 4]);

    let mut v = buf();
    v.copy_within(0..4, 2);
    assert_eq!(v, [1, 2, 1, 2]);

    let mut v = buf();
    v.copy_within(6..6, 0);
    assert_eq!(v, [1, 2, 3, 4]);
}
