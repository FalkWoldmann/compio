# TaskQueue: the two hot-path variants

A decision record for the executor's scheduling queue. Two independent changes
were measured; one is uncontroversial, the other trades observable behaviour for
speed. This is the argument for and against each, so the second can be accepted
or rejected on its merits rather than on its benchmark number.

## The structure as it stands

`TaskQueue` holds every live task in a `SlotMap<TaskId, Item>` and threads two
intrusive doubly-linked lists through it:

- **hot** — woken, waiting to be polled
- **cold** — parked, waiting for a wake

`Item` carries `prev`, `next`, the `Task` itself, and a flag saying which list it
is in. Links are `TaskId`s rather than pointers, so every traversal step is a
`SlotMap` lookup: an index plus a generation check. Cheap, but not free, and the
wake path runs once per wake.

The executor's tick, before either change:

```rust
for id in queue.iter_hot().take(max_interval) {
    queue.make_cold(id);                 // migration 1: hot -> cold
    let task = queue.take(id).expect(..);
    let res = unsafe { task.run() };
    if done { queue.remove(id) } else { queue.reset(id, task) }
}
```

A task is in one of the two lists *at all times*, including while it is being
polled — during its own poll it sits in cold.

## Variant A — fuse the relink (on this branch)

### What it does

`make_hot`/`make_cold` looked the same key up three times: once to read the
list flag, once inside `unlink` to read `prev`/`next`, once inside `link_tail`
to write the new links. `relink()` reads the old links and writes the new ones
under a single `get_mut`, then fixes up the neighbours.

Neighbour lookups stay. `SlotMap` hands out one mutable borrow at a time, so
touching `prev`, `next` and the destination tail needs separate lookups — that
part is irreducible without changing the data structure.

### Pros

- **No API change.** `make_hot`/`make_cold` keep their signatures.
- **No behaviour change.** Same lists, same order, same everything — only fewer
  lookups to get there.
- **Small.** ~50 lines, contained entirely within `Inner`.
- **Covered by tests written before it.** The invariant tests were written
  against the old implementation and passed there first, so they encode existing
  behaviour rather than the refactor. They walk both lists checking `prev`/`next`
  are mutual, that no node appears twice or in the wrong list, and that
  `head`/`tail` agree with the walk.

### Cons

- **Leaves the real waste in place.** It makes each migration cheaper without
  questioning why there are two migrations per poll-and-self-wake cycle at all.
- **Slightly more intricate.** The read-and-rewrite-in-one-borrow pattern is
  less obvious than `unlink` then `link_tail`, and the comment carries that
  weight.

## Variant B — `Place::Running` (`queue-place-running-prototype`)

### What it does

Replaces the two-state flag with three:

```rust
enum Place {
    Hot,
    Cold,
    Running { woken: bool },
}
```

A task being polled is removed from *both* lists for the duration. The tick
becomes:

```rust
let task = queue.start_run(id);   // unlink from hot, mark Running { woken: false }
let res = unsafe { task.run() };
if done { queue.remove(id) } else { queue.finish_run(id, task) }
```

`finish_run` links the task into hot if it was woken during its own poll, and
cold otherwise.

### Why it is faster

Consider the commonest thing a future does: poll, return `Pending`, and wake
itself later — or yield, which wakes immediately.

| | before | after |
| --- | --- | --- |
| entering the poll | full hot→cold migration | unlink from hot only |
| self-wake during the poll | full cold→hot migration | set a `bool` |
| leaving the poll | — | one link into hot or cold |

Two list migrations become one link plus one unlink. The `bool` is the point:
a wake arriving while the task is in neither list has no list work to do.

### Pros

- **Removes work rather than shrinking it.** This is the structural fix that
  Variant A works around.
- **Arguably better fairness** (see below).
- **Makes an invalid state unrepresentable.** "Being polled" was previously
  encoded as "in the cold list", which is a lie — the task is not parked, it is
  running. The third state says what is true.

### Cons

- **Changes the queue API.** `take`/`reset` become `start_run`/`finish_run`.
  Internal, but it is a real churn cost and it couples the queue more tightly to
  the executor's tick.
- **Changes observable ordering.** See below. This is the substantive objection.
- **Bigger.** ~120 lines including the executor's tick, versus ~50.
- **No dedicated tests.** Variant A's invariant tests are written against the
  two-state `is_hot` flag and do not port unchanged; nothing currently exercises
  the `Running` transitions directly. The 7 executor tests and 5 loom models pass,
  but they were not written with this state machine in mind.

### The ordering change, concretely

Task `S` is polled. During its poll it wakes tasks `A` and `B`, and also wakes
itself (it yields).

**Before:** `S`'s self-wake calls `make_hot(S)` at the moment it happens, which
is *during* the poll. So `S` reaches the hot tail first, then `A`, then `B`:

```
hot tail: ... S A B
```

**After:** `A` and `B` are linked at their wake time as usual. `S` only sets
`woken = true`; it is linked when the poll returns, so it lands behind them:

```
hot tail: ... A B S
```

Which is correct? Neither, strictly — the old order was an accident of when the
link happened, not a designed guarantee. The new order is arguably fairer: a task
that yields should not jump ahead of the tasks it just woke, and round-robin
progress is the usual expectation for a cooperative scheduler.

But "arguably fairer" is not "documented as unchanged". Anything relying on the
old order — a test asserting a wake sequence, a benchmark, a user's mental model
of fairness — would shift. That is why this is a semantic decision rather than a
perf patch.

## Measurements

`local_wake` n1000, instruction counts under iai-callgrind. Deterministic: a
re-run of an unchanged tree reports `No change`, so these deltas are exact and
attributable.

| variant | instructions | vs master |
| ------- | -----------: | --------: |
| master | 332,111 | — |
| A: fused relink | 287,063 | −13.6% |
| B: `Place::Running` | 284,994 | −14.2% |
| **A + B** | **281,994** | **−15.1%** |

They compose. B short-circuits self-wakes; A pays off on genuine cold→hot moves,
which B leaves untouched. Neither subsumes the other.

All four configurations pass the 5 loom models and Miri.

## Recommendation

**Take A unconditionally.** It is a pure win: no API change, no behaviour change,
and it is covered by tests that passed against the old implementation first.

**Take B as a separate, deliberate decision.** The extra 1.5 points over A alone
is not the reason to take it; the reason is that "being polled" stops being
encoded as a lie about which list the task is in. The reason to refuse it is the
ordering change, which is defensible but real, and the fact that nothing
currently tests the `Running` transitions.

If B is taken, it should come with:

1. Tests for the `Running` transitions specifically — wake-during-poll landing
   in hot, no-wake landing in cold, and cancel-during-poll.
2. A note in the executor docs that self-woken tasks queue behind tasks woken
   during their poll, so the order is documented rather than incidental.

## Open, not yet measured

- **`start_run` still takes two lookups** — `unlink::<HOT>(key)` then
  `get_mut(key)` to set `Running` and take the task. The same fusing as Variant A
  applies and has not been tried.
- **`remove()` has the three-lookup shape** Variant A fixed in the wake path.
  Lower value: removal runs once per task, not once per wake.
- **Links are `TaskId`s, so every step is a generation-checked lookup.** The
  deeper question neither variant asks is whether the lists should hold pointers
  instead. That is a much larger change with real safety implications, and it
  should not be attempted without the loom models that already exist being
  extended first.
