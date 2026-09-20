# Benchmarking and performance

How compio is measured, what tooling exists, and what the measurements have
turned up so far.

## Tooling

### criterion — end-to-end, wall clock

Already in place before this work: `compio/benches` (fs, net), `compio-quic/benches`,
`compio-executor/benches/schedule.rs`. `compio-runtime` carries an optional
`criterion` feature so async benchmarks can drive a real runtime.

Criterion measures what users feel — whole operations, wall clock — and that is
the right default for I/O paths where a syscall dominates. Its weakness is CI:
wall-clock numbers on a shared runner are noisy enough that a few percent of
regression disappears into the error bars.

### iai-callgrind — instruction counts, CI-stable

`compio-executor/benches/schedule_iai.rs`. Runs under callgrind and counts
instructions, L1/LL/RAM hits and estimated cycles.

The point is determinism. A re-run of an unchanged tree reports literally `No
change`, so a 1% regression is visible and attributable rather than lost in
noise. That makes it the right tool for the executor's hot path, where changes
are small and frequent. It needs `valgrind` on the machine.

```
cargo bench -p compio-executor --bench schedule_iai
```

Benchmarks: `local_wake` (wake an already-spawned task), `spawn_ready` (spawn
and immediately complete), `idle_tick` (poll loop with nothing to do), each at
n=1, 100, 1000.

### divan — allocation profiling

`compio-io/benches/copy.rs`. Lower boilerplate than criterion, and it reports
allocation counts and bytes per iteration alongside timings. Used for the
in-memory copy paths, where the question is usually "did this allocate?" rather
than "how many nanoseconds".

```
cargo bench -p compio-io --bench copy
```

### dial9 — runtime flight recorder

`compio-executor/src/dial9/`, behind the `dial9` feature.

dial9 is normally described as Tokio telemetry, and its `dial9-tokio-telemetry`
crate does hook Tokio's runtime hooks — which compio does not have. The usable
layer is underneath: `dial9-core` plus `dial9-trace-format` are runtime-agnostic,
so compio emits its own events (spawn, poll, wake, park) into the same trace
format from the same places the `console` feature already instruments.

```
cargo run -p compio-executor --features dial9 --example dial9
```

writes `/tmp/compio-trace.0.bin`, readable by dial9's viewer. Unlike aggregate
metrics, this records individual events, so it answers "what actually happened
to this task" rather than "what is the p99".

## Measured results

### Executor hot path: one slot lookup instead of three

`TaskQueue` keeps tasks in a `SlotMap` with two intrusive doubly-linked lists
(hot and cold) threaded through it. `make_hot`/`make_cold` — the per-wake path —
looked the same key up three times:

1. `map.get(key)` to read `is_hot`,
2. `unlink` → `map.get(key)` to read `prev`/`next`,
3. `link_tail` → `map.get_mut(key)` to write the new links.

`relink()` reads the old links and writes the new ones under a single `get_mut`,
then fixes up the neighbours. Neighbours still need their own lookups, because
`SlotMap` hands out one mutable borrow at a time — that part is irreducible.

| benchmark | before | after | delta |
| --------- | -----: | ----: | ----: |
| `local_wake` n100 | 34,211 | 29,663 | **−13.3%** |
| `local_wake` n1000 | 332,111 | 287,063 | **−13.6%** |
| `spawn_ready` n100 | 55,227 | 50,624 | −8.3% |
| `spawn_ready` n1000 | 570,200 | 524,182 | −8.1% |
| `idle_tick` | 38,403 | 37,403 | −2.6% |

Instruction counts, so these are exact rather than sampled. `spawn_ready` and
`idle_tick` improve because they also relink.

Correctness for this change rests on tests written *before* it, against the old
implementation, so they encode existing behaviour rather than the refactor:
`queue::tests` walks both lists checking `prev`/`next` are mutual, that no node
appears twice or in the wrong list, and that `head`/`tail` agree with the walk.
The 5 loom models and Miri also pass.

### A deeper variant exists: `Place::Running`

A second approach lives on `bench-tooling-prototype`. A task used to be in one
of the two lists at all times, *including while being polled*, which put it in
the cold one. A wake arriving during its own poll — what every future that
yields does — then walked it back out of cold and onto the hot tail, so a
poll-and-self-wake cycle paid for two full list migrations. Adding a third
state, `Place::Running`, takes the task out of both lists for the duration of
the poll; a wake that arrives meanwhile only records that it happened.

The two changes are complementary rather than competing: `Running`
short-circuits self-wakes, while the fused relink pays off on genuine
cold-to-hot moves. Measured on `local_wake` n1000:

| variant | instructions | vs master |
| ------- | -----------: | --------: |
| master | 332,111 | — |
| fused relink (this branch) | 287,063 | −13.6% |
| `Place::Running` only | 284,994 | −14.2% |
| both together | 281,994 | **−15.1%** |

All three pass the 5 loom models and Miri.

`Place::Running` is not on this branch because it costs more than instructions:
it changes the executor's queue API (`take`/`reset` become
`start_run`/`finish_run`) and it changes observable ordering — a self-woken task
used to reach the hot tail at the moment of the wake, ahead of tasks woken
during its own poll, and now arrives when the poll returns, behind them. That is
defensible but it is a semantic decision, not a free win, so it is kept separate
for review on its own terms.

### Rejected: inlining the link into `insert`

`insert` calls `link_tail` after `insert_with_key`, which re-looks-up the slot it
just wrote. Fusing them measured **−0.76%** on `spawn_ready`.

Not kept. It duplicates `link_tail`'s list manipulation at a second call site, in
an intrusive linked list that loom is used to verify, and 0.76% does not buy that
maintenance risk. Recorded here so the number does not have to be re-derived if
someone disagrees.

### Rejected: fusing `remove()`

`remove()` had the same three-lookup shape the wake path did — `get(key)` for
`is_hot`, `unlink`'s own `get(key)`, then `map.remove(key)`. Since
`SlotMap::remove` hands the whole item back, links included, fusing looks
obvious.

It is a **regression**:

| benchmark | before | after | delta |
| --------- | -----: | ----: | ----: |
| `spawn_ready` n1000 | 524,182 | 535,183 | **+2.1%** |
| `local_wake` n1000 | 287,063 | 288,075 | +0.35% |

Isolating the two halves explains why. The refactor that extracted a shared
`detach` helper measured *exactly* neutral — `No change` on both benchmarks — so
the whole regression came from the fusing itself. The lookups were never the
cost here: `SlotMap::remove` moves the entire `Item` (including the `Task`) out
by value, and doing that before the neighbour fix-ups is worse than reading a
single `bool` and letting `unlink` work in place.

Worth stating plainly, because it is the same reasoning that produced a −13.6%
win on the wake path. "Fewer lookups" is not a law; it happened to be the
binding constraint in one place and not the other. Measure each one.

## Where the remaining time actually goes

From `callgrind_annotate` over the iai profiles, rather than from reading code.

### `spawn_ready`: about a third is the allocator

| function | instructions | share |
| -------- | -----------: | ----: |
| `_int_malloc` | 137,573 | 26.3% |
| `Executor::tick` (queue.rs) | 63,084 | 12.0% |
| `malloc` | 45,000 | 8.6% |
| `__memcpy_avx_unaligned_erms` | 32,032 | 6.1% |
| `Task::drop` | 28,000 | 5.3% |

**~35% of spawning is malloc.** That is `TaskAlloc<F>` — one heap allocation per
spawn — not queue bookkeeping. Further micro-optimisation of the list code will
not move this number; pooling or arena-allocating task storage would, and that is
a real design change rather than a tweak.

### `local_wake`: no allocation at all

The wake path shows no `malloc` in its profile. The cost is spread across
`Executor::tick`, `TaskQueue`, and `Task::schedule`, with no single site above
18%. The −13.6% above took the concentrated win; what is left is diffuse, and
the next percent there will cost far more effort than the last thirteen did.

## Open opportunities

Not yet measured — listed with the mechanism so the next person can start from a
hypothesis rather than a hunch.

- **Driver submission batching** (`compio-driver`, io_uring SQ/CQ) has no
  instruction-count benchmark. It is also the layer where syscall count, not
  instruction count, dominates, so it needs a different measurement approach
  before anything is attempted.
- **`compio-buf` per-operation allocation.** The divan benches cover in-memory
  copies; they do not yet cover the buffer lifecycle across a completion.
- **Driver submission batching** (`compio-driver`, io_uring SQ/CQ) is untouched
  by this work and has no instruction-count benchmark. It is also the layer where
  syscall count, not instruction count, dominates — so it needs a different
  measurement approach than the executor benches.
- **`compio-buf` per-operation allocation.** The divan benches cover in-memory
  copies; they do not yet cover the buffer lifecycle across a completion.

## Closed: pooling task allocations

This was listed as the biggest remaining lever, on the strength of the 35%
`malloc` share above. It does not work, and it is worth recording why so nobody
spends the effort again.

**There is no redundant allocation to remove.** `compio-executor/tests/allocs.rs`
counts allocations with a counting global allocator: spawning is **exactly 1.00
allocations per task, ~150 bytes**. The 35% is one `TaskAlloc<F>` box, not a pile
of avoidable ones. So the only available move is to stop returning the block to
the allocator between tasks.

**A thread-local cache is slower than the allocator.** Two variants were built
and measured on `spawn_ready` n1000:

| variant | instructions | vs baseline |
| ------- | -----------: | ----------: |
| baseline (no pool) | 524,182 | — |
| single-slot `Cell` cache | 528,182 | +0.76% |
| bounded `RefCell<Vec<_>>` cache, keyed by layout | 536,678 | +2.4% |

Both lose. glibc's tcache already recycles a 150-byte block on a very short fast
path, and a thread-local access plus a layout check costs more than it saves. The
cheapest possible Rust-side cache — one slot, a `Cell`, no scan — still loses.

**An executor-local pool would avoid the thread-local cost, and is unsound.**
`Task::drop` stores null into the header's `shared` pointer before the reference
count reaches zero, so the executor is already unreachable from `dealloc`. That
nulling is not incidental: `wait_for_scheduling` only waits for scheduling that
is already in flight, and its own comment says late wakers from other threads
"will see the null pointer and return early". A task allocation can therefore
outlive `Shared`, and a pool living in `Shared` would be a use-after-free on
exactly the path the null is there to protect.

What is left, if this is ever revisited, is changing the allocation rather than
caching it: inline storage for small futures, so that no separate block is taken
at all. That is a much larger change and should start from a measurement of how
many real workloads have futures small enough to benefit.

## A note on the numbers in this document

Every figure here came from a run on one machine. Instruction counts are stable
across machines in a way wall-clock numbers are not, which is why the executor
results are quoted in instructions. Re-derive rather than trust the absolute
values; the deltas are the durable part.
