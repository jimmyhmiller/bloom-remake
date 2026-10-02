# HD: paying down the S8 debts (working notes)

Branch `slice-hardening`, worktree `.worktrees/hd`. Chosen by the user (2026-10-01) before S9.

## Resume here

- **State (2026-10-01):** item 1 done. Next: item 2 (controller compaction).

## Items

1. Decoding a huge well-formed request exceeded the step budget (BLSR012 aborts the tick: a DoS).
2. The controller's log is never compacted; `done`/`outcome`/`mdeleted` grow with every admin request.
3. Produce latency: ~130 ms per `acks=all` request on real disks.
4. `kafka3`'s idle-CPU check fails under heavy machine load.

### Item 1: formats are bounded by their input (done)

- `FnProps::metered` (IR, serde default true): a format's generated functions are unmetered; both evaluators'
  `Fuel` save the budget at each call, spend nothing inside an unmetered call, and give a metered call inside an
  unmetered one a budget of its own (a condition such as `flexible(...)` stays metered).
- Sound because every loop in the generated code is bounded by the bytes left (counts are checked against them, and
  array items take at least one byte, checked since the SL review) or by the encoded value.
- Test: `bls_extensions::formats_are_bounded_by_their_input_not_the_step_budget` (1.6M items, ~11M steps, both
  evaluators; a metered condition past its budget is still BLSR012); mutation: metering format functions again makes
  the decode BLSR012.
