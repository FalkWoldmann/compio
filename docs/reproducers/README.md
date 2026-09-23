# Reproducers

Against compio master at 6d40918. Each file in `examples/` is standalone and
short enough to paste into a comment or a gist.

Miri: `cargo +nightly miri run --example <name>`.

## Same root cause as #1053

| Example | Run | Result |
| --- | --- | --- |
| `as_uninit_vec` | Miri | reading uninitialized memory (`Vec<u8>`; `[u8]` is the same) |
| `iter_uninit_slice` | Miri | reading uninitialized memory |
| `copy_within` | Miri | reading uninitialized memory |
| `as_mut_slice_disagree` | Miri | dangling reference in `as_mut_slice` |
| `extend_disagree` | Miri, `--release` | out-of-bounds write in `extend_from_slice` |
| `recvmsg_ptr_len` | native, `cargo run` | kernel writes 32 bytes into an 8-byte buffer, canary overwritten |

## Found along the way

| Example | Run | Result |
| --- | --- | --- |
| `bytesmut_as_uninit` | Miri | retag error in `BytesMut::as_uninit` |
| `repeat_advance` | native or Miri | `Vec::set_len requires that new_len <= capacity()` |

## Ancillary

| Example | Run | Result |
| --- | --- | --- |
| `ancillary_decode_overread` | Miri | dangling reference in `decode_data` |
| `ancillary_push_aliasing` | Miri (SB), `MIRIFLAGS=-Zmiri-tree-borrows` (TB) | retag error (SB), forbidden reborrow (TB) |
| `ancillary_encode_deinit` | `MIRIFLAGS=-Zmiri-disable-stacked-borrows` | reading uninitialized memory |
| `ancillary_walk_hang` | native, `--release` | iterator never ends (debug build aborts in libc) |
| `ancillary_empty_control` | native | `buffer too short` panic on a datagram without control data (N9) |

## Build

`compio-io` with `ancillary` alone doesn't build on Linux with rustix 1.1.5
(N6): `cargo check` in a crate that depends only on `compio-io` fails with
`cannot find timespec in the crate root`. `Cargo.toml` here works around it
by enabling rustix's `fs` feature.
