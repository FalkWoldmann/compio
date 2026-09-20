# Unsafe review: buffer traits

Reviewed with [google/rust-skills `unsafe_rust_review`][skill], which treats every
`# Safety` section as a theorem and every `// SAFETY:` comment as a proof, and
classifies each premise by its authority (axiom, precondition, invariant, local
fact, …).

Two of that skill's rules do most of the work here:

- **Rule C — safe trait laws are not safety contracts.** Unsafe code must not
  rely on a caller-provided *safe* trait implementation being semantically
  correct.
- **Reject pattern #8** — reject "SAFETY: the trait tells us N", because a safe
  implementation may lie.

[skill]: https://github.com/google/rust-skills/tree/main/unsafe_rust_review

## Finding: `IoBuf` / `IoBufMut` are safe traits that unsafe code trusts

`IoBuf`, `IoBufMut` and `SetLen` are all declared as safe traits:

```rust
pub trait IoBuf: 'static { fn as_init(&self) -> &[u8]; }
pub trait IoBufMut: IoBuf + SetLen { fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>]; }
```

They are public and users are invited to implement them. Unsafe code then
depends on their return values for memory safety, with nothing enforcing that
dependence. `SetLen::set_len`'s own `# Safety` section is stated in terms of
`as_uninit().len()` and `buf_len()` — so the theorem's premises are supplied by
the very safe methods that cannot be trusted. The proof is circular.

Both consequences below are reachable from **entirely safe** user code.

### 1. `as_init` and `as_uninit` need not agree — in-process UB

`IoBufMutExt::as_mut_slice` takes its length from one safe method and its
pointer from another:

```rust
let len = (*self).buf_len();       // IoBuf::as_init().len()
let ptr = (*self).buf_mut_ptr();   // IoBufMut::as_uninit().as_mut_ptr()
unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len) }
```

An impl reporting 1000 bytes from `as_init` while `as_uninit` exposes 10
produces a `&mut [u8]` running off the end of its allocation. Miri:

```
error: Undefined Behavior: constructing invalid value of type &mut [u8]:
       encountered a dangling reference (going beyond the bounds of its allocation)
   --> compio-buf/src/io_buf.rs:457
```

The reproducer contains no `unsafe` beyond the empty `set_len` body that the
trait signature requires.

### 2. Successive `as_uninit` calls need not agree — kernel-reachable

Worse, because it escapes the process. `buf_mut_ptr()` and `buf_capacity()` each
call `as_uninit()` *separately*, and `RecvMsg::init_control`
(`compio-driver/src/sys/op/socket/unix.rs`) hands both to the kernel:

```rust
ctrl.msg.msg_control    = self.control.buf_mut_ptr() as _;   // one as_uninit()
ctrl.msg.msg_controllen = self.control.buf_capacity() as _;  // another one
```

Nothing requires the two calls to return the same slice. An impl that alternates
between an 8-byte and a 4096-byte buffer yields:

```
msg_control    = 0x7ffe…537 (points into the 8-byte buffer)
msg_controllen = 4096
```

`recvmsg` would then be authorised to write 4096 bytes into an 8-byte
allocation. This was demonstrated by reproducing `init_control`'s two
assignments; the syscall itself was deliberately not invoked.

Note the driver's *other* paths are not affected the same way: ops such as
`Recv` pass `self.buffer.as_uninit()` as a slice, so pointer and length come
from one call and cannot disagree. It is specifically the places that take the
pointer and the length from separate calls that are exposed.

### Severity

Triggering this requires the user's own `IoBufMut` implementation to be
inconsistent. That is not a remote attack; it is a soundness bug. The rule the
codebase currently breaks is that safe code must not be able to cause UB, and an
*accidental* inconsistency — a cached length that drifts from the buffer, a
`as_uninit` that reallocates — is an ordinary bug to write, not a contrived one.

### Fix

The skill prescribes four remedies for Rule C: make the trait unsafe, use an
existing unsafe trait, validate dynamically, or constrain implementations to
trusted types. Two apply:

**Make the traits unsafe** (the thorough fix). `IoBuf` and `IoBufMut` become
`unsafe trait`s stating the implementor's obligations, at minimum:

```rust
/// # Safety
///
/// Implementors must ensure that:
/// 1. `as_uninit` returns the same pointer and length on every call, until the
///    buffer is mutated through `&mut self` by `reserve` or `set_len`.
/// 2. `as_init().len() <= as_uninit().len()`.
/// 3. `as_init()` and `as_uninit()` address the same allocation, with
///    `as_init()` a prefix of it.
```

This is a breaking change for downstream implementors, which is the honest cost
of the guarantee.

**Or validate where it is cheap.** `as_mut_slice` can clamp to the slice it
actually holds, which removes case 1 at the cost of nothing on the happy path:

```rust
let uninit = self.as_uninit();
let n = len.min(uninit.len());
```

That does not fix case 2, where the two lengths come from different calls — that
one needs the pointer and length taken from a single `as_uninit()`, which is a
local change to `init_control` and worth doing regardless of the trait decision.

## Finding: the safety comments in this repo do not meet this standard

The `// SAFETY:` comments added in `unsafe-tooling` — including the ones written
for this review's own branch point — mostly fail the skill's criteria. They
assert a conclusion without naming the operation or its contract:

```rust
// SAFETY: `**self` is the buffer being resized, so the caller's obligation
// transfers verbatim.
unsafe { (**self).set_len(len) }
```

The skill's Reject pattern #2 is precisely this: shifting the burden to "the
caller guarantees it" rather than naming the callee's contract and discharging
each obligation. A conforming comment names the operation, quotes the contract,
classifies each premise, and argues that no intervening code invalidates them.

Being explicit about this because the `deny(clippy::undocumented_unsafe_blocks)`
now set on four crates enforces that a comment *exists*, not that it proves
anything. Passing that lint should not be read as having met this bar.

## What this review did not cover

`compio-driver` (255 unsafe blocks), `compio-executor` and `compio-runtime` were
not reviewed to this standard. The checklist topics most likely to find
something there, on the evidence of the two findings above, are *safe trait
laws*, *reentrancy* (the `OpCode` trait is unsafe, but the futures it polls are
caller-provided) and *temporal scope* (how long the kernel retains a pointer
after a future is dropped).
