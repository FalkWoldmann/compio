# Unsafe review: method and standing gaps

The soundness bugs this review found are in [`soundness.md`](./soundness.md).
This file records how the review was done and what it did *not* cover, so the
next pass can start where this one stopped.

## Method

Reviewed with [google/rust-skills `unsafe_rust_review`][skill]. It treats every
`# Safety` section as a theorem and every `// SAFETY:` comment as a proof, and
requires each premise to be classified by its authority: `AXIOM` (the Reference
or std docs), `DEPENDENCY LEMMA` (an intentionally selected safe dependency),
`PRECONDITION`, `INVARIANT`, `LOCAL FACT`, `TYPE FACT`, `POSTCONDITION`.

[skill]: https://github.com/google/rust-skills/tree/main/unsafe_rust_review

Two of its rules did most of the work:

- **Rule C** — safe trait laws are not safety contracts. Unsafe code must not
  rely on a caller-provided safe trait implementation being semantically
  correct.
- **Reject pattern #2** — reject "the caller guarantees it". A proof names the
  callee's contract and discharges each obligation.

The method's value here was not the checklist but its failure mode: when a proof
cannot be written, that is the finding. Both bugs in `soundness.md` surfaced
while trying to write a safety comment for code that had none.

## Gap: the safety comments do not meet this standard

`compio-buf`, `compio-compat`, `compio-process`, `compio-dispatcher` and
`compio-term` carry `#![deny(clippy::undocumented_unsafe_blocks)]`. That lint
enforces that a comment *exists*, not that it proves anything, and passing it
should not be read as having met this bar.

The comments in `compio-buf` have been rewritten to the standard — each names
the operation, quotes the contract, classifies its premises and argues that no
intervening code invalidates them. Where an obligation genuinely is not
discharged, the comment says so and points at `soundness.md` rather than
asserting a conclusion that is not true.

`compio-compat`, `compio-process` and `compio-dispatcher` still carry the
weaker form: a sentence of intent rather than a proof. `compio-term` has no
`unsafe` blocks left to document.

## Not reviewed

`compio-driver` (255 unsafe blocks), `compio-executor` and `compio-runtime` were
not reviewed to this standard. On the evidence of the two findings, the
checklist topics most likely to turn something up:

- **Safe trait laws** — the pattern that produced both bugs. `OpCode` is
  correctly an `unsafe trait`, but it is worth checking what else unsafe code
  trusts a safe impl for.
- **Temporal scope** — how long the kernel retains a pointer after the future
  that submitted it is dropped. This is the central contract of a
  completion-based runtime and deserves a proof, not a convention.
- **Reentrancy** — the futures the executor polls are caller-provided and may
  panic or re-enter at any suspension point.
