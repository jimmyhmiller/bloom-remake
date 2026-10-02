# HD: paying down the S8 debts (working notes)

Branch `slice-hardening`, worktree `.worktrees/hd`. Chosen by the user (2026-10-01) before S9.

## Resume here

- **State (2026-10-02):** items 1–4 done; the adversarial review is done and its findings are fixed (see "Review").
  Next: merge, then S9.

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

### Item 2: the controller's log compacted, the applied commands bounded (done)

- **Compaction.** `compactable` (produce_node.bls) gains the controller: a broker compacts its controller log up to
  `applied − CONTROLLER_KEEP` once that is `CONTROLLER_KEEP` past its snapshot point (steps of KEEP, default 1000).
  Raft's existing InstallSnapshot moves the point to a follower that needs compacted entries.
- **The applied metadata as the snapshot.** The metadata applied at an index is the same on every broker (a function
  of the committed prefix). A broker whose applied index is below its controller snapshot point cannot apply from
  its log, so every heartbeat it sends `meta_want(point)` to every other broker. Any broker that has applied at least
  that far answers with `meta_snap`: its metadata at its own applied index (topics, assignments, reassignments, deleted
  topics, `done`, outcomes, next producer id). The asker adopts the newest copy past its applied index
  (`adopt_state` upserts, `adopt_drop` deletes what the copy lacks, `applied_m` jumps; `to_apply` waits that tick).
  - First draft was a leader push gated on `match_idx < rsnap`. Liveness hole: once InstallSnapshot is acknowledged
    the push stops, so a lost `meta_snap` (or a leader itself behind its point) left a broker stuck for ever. The
    pull asks until it has adopted. No directed test drops exactly the `meta_snap` (the sim cannot single out one
    channel), so the pull is covered by the random drops and partitions of the compacted run below.
- **Bounding `done`/`outcome`/`mdeleted`.** Each keeps its rows for `DONE_WINDOW` entries after they are applied
  (`while applied_m(a) where at + DONE_WINDOW > a`; outcome `while done(origin, tag, _)`). That is safe because a
  command carries `made`, the controller index its origin had applied when it first wanted it. The origin pins it in
  `cmd_made`, so every copy carries the same value. `fresh` skips a command with `made + DONE_WINDOW <= i`
  (stale). Proof: the original was applied at i0 > made. A copy at j ≤ i0 + W still finds its `done` row; a copy at
  j > i0 + W has made + W < j, so it is stale. Producer-id blocks and reassignment completions (`repeatable`) are
  applied however old; applying one twice is harmless.
  - `mark_done` marks stale copies done too (with no outcome): the origin stops sending, and its request times out
    through the usual path (REQUEST_TIMED_OUT, "may still happen", true: it never will).
  - `cmd_made` is not durable. That is fine because every non-repeatable command belongs to a request on a connection,
    which a crash ends (a `Conn` is unique across incarnations).
  - The window size affects only liveness: a request that waits more than `DONE_WINDOW` entries times out. Default
    100000.
- **Tests.**
  - `kafka_topics::a_broker_behind_the_controllers_snapshot_adopts_the_leaders_metadata`: KEEP 4, W 10. Broker 3 is
    stopped 0.3s–1.8s while the others compact past it; it comes back to a snapshot point past what it held.
  - `kafka_topics::admin_requests_stay_linearizable_with_the_controller_compacted`: the three-broker fault run with
    KEEP 3, W 12.
  - `check_runs` now also compares `applied_m`, `mreassign`, `mdeleted`, `done`, `outcome` and `next_pid` across
    brokers. With a window, it also checks the bounds: at most W `done` and `mdeleted` rows, and no outcome without
    its `done` row.
  - `kafka_cluster::a_command_sent_again_past_the_done_window_is_skipped`: the leader's messages to the origin are
    cut and resends come every 50ms. Another broker deletes the topic and pushes the log past W; the late copies
    reach the leader and must not bring the topic back.
  - `kafka_cluster::a_command_sent_again_after_catching_up_from_a_snapshot_is_skipped`: as above, with 8s resends.
    The origin catches up from a snapshot (the `done` row already gone) before it sends again.
- **Mutations, all caught:**
  - no `meta_want`;
  - no `send meta_snap`;
  - no `adopt_drop` of topics;
  - no adopting `done`;
  - no controller compaction;
  - `fresh` ignoring staleness (caught by both late-copy tests);
  - no `cmd_made` pinning (caught by the snapshot variant; the 50ms variant cannot see it, because its stale copies
    are marked done first);
  - no window on `done`, on `outcome`, or on `mdeleted`.

### Item 3: acks=all latency (in progress)

- **Measurement.** `kafka3::acks_all_latency` (runs only with `KAFKA3_LATENCY=1`; `--nocapture`, `KAFKA3_TAIL=crc|strict`) sends 300
  sequential one-record acks=all produces to one partition leader on three local brokers.
- **Starting point.** Release build, this Mac: strict p50 150 ms, crc p50 70 ms.
  - One `F_FULLFSYNC` takes about 5 ms here.
  - A sync trace (temporary instrumentation in `vfs.rs`) counted about 30 syncs per produce over the three brokers.
  - The brokers share one disk, so their syncs serialize. The latency here is the sum over all brokers; on separate
    disks only the critical path counts.
- **Where the syncs go.** Each broker does about 1.7 WAL group commits per produce: the leader appends, the follower
  stores, then every replica materializes. Each commit cost 4 syncs in strict mode. On top of that, every Raft entry
  cost 2 blob syncs (the temporary file and the directory) on every broker.
- **Blobs logged in the WAL** (f5ee884): crc 70 → 28 ms, strict 150 → 120 ms. That equals the measured bound with
  blob syncs removed entirely. Notes in FOREIGN-PROTOCOLS §5a.
- **Strict at 3 syncs.** The batch's marker now leads the next batch, as in crc, and the receipt is written at every
  sync with the data end: strict 120 → 90 ms. The guarantee for acknowledged data is unchanged.
  - The marker's separate sync was redundant: the receipt alone tells damage before its frontier (corruption) from
    damage after it (a torn tail).
  - Recovery no longer depends on the certification; `WalScan::scan_certified` is gone.
  - Batches count up by one in both modes.
  - Old strict segments still recover: the extra `last + 1` torn-tail case is always covered by a receipt there
    (see `torn_tail`).
  - Tests: `crash_points.rs` (renamed from `crc_crash_points.rs`) runs both certifications. Mutation: strict writing
    its receipt once per segment, as crc does, fails `synced_marker_and_receipt_media_fault_refused`.
- **User decisions (2026-10-01):** keep strict at 3 syncs (no 2-sync receipt); answer acks=all on commit, with Fetch
  serving committed but unmaterialized entries.
- **Answer on commit** (the user's choice).
  - `placed_done` answers once `commit_of` reaches the entry (and the ISR holds it), not once it is materialized
    (`log_end`).
  - Materialization works to `materialize_to`, the commit index the previous tick saw. So the tick that answers
    writes nothing durable, and the answer waits for no sync.
  - Fetch and ListOffsets read up to the commit index:
    - `readable_batch` is the materialized batches plus `pending_batch`, the committed entries not materialized yet;
    - `high_watermark` replaces `log_end`;
    - the timestamp search and MAX_TIMESTAMP consult the pending batches after the materialized log.
  - Without these reads, a read reaching the broker in the tick that materializes an entry (just after its answer)
    would miss an acknowledged record.
  - Measured: strict 90 ms (unchanged), crc 28 → 25 ms. The three brokers share one disk here, so their syncs
    serialize, and the materialization commit moved off the critical path still occupies the device ahead of the
    next produce. On separate disks it saves one leader commit per acks=all produce.
  - Test: `kafka_read_after_ack` drives one broker tick by tick with the manual driver (the simulator cannot place a
    request in that tick). It answers an acks=all produce, then in the very next tick sends a Fetch at the offset
    and three ListOffsets: latest, the batch's first timestamp, and the greatest timestamp. Mutations, all caught:
    no pending batches; ListOffsets using `log_end`; the timestamp search ignoring pending batches; MAX_TIMESTAMP
    ignoring them.
  - `kafka_cluster::reassignments_move_partitions_under_load` then failed on seed 1: the reader did not read every
    batch. The brokers were consistent. The test's Reader marked itself stale on every close, including the ones it
    asked for to move to another partition's leader. It then asked a random broker for metadata and closed again
    unless that broker led the partition. The new schedule left it in that loop for its last half second (twelve
    random picks, none the leader). Fixed in the Reader: a close it asked for keeps the leaders it knows.
- **Options considered** (each sync cut is about 5 ms per hop here):
  - Strict at 2 syncs with an in-place two-slot receipt. This loses detection of a corrupted current receipt slot
    combined with damage to the last acknowledged batch (a double fault). A user decision.
  - Answer acks=all on commit rather than after materializing. This removes the leader's third commit, but Fetch
    must then serve committed but unmaterialized entries, or a read after an ack can miss the record.

### Item 4: idle brokers tick 100 times a second (done)

- **Measurement** (scratch harness: one broker with three partitions under the manual driver, left idle for 5 s):
  - 500 ticks (100/s: the 10 ms `fetch_wake` timer; the others coincide with it);
  - not one of them changes carried state;
  - 10 syncs (the META time reservations);
  - 2.5 ms per tick in debug, 0.27 ms in release.
- **Diagnosis.** No rule spins. The idle CPU is polling by design: `fetch_wake` (every 10 ms) checks a waiting
  Fetch's deadline, and `raft_poll` (every 20 ms) checks election timeouts. Three debug brokers in kafka3 host more
  groups and sit at the check's 2 s of CPU per 4 s, so machine load tips them over.
- **User decision (2026-10-02):** guarded timers plus a spin check (over a check alone, or dynamic deadlines).
  - The new syntax `timer t every D while G` fires only while the guard view G held in the node's latest tick.
  - `fetch_wake` will run only while a Fetch waits.
  - The kafka3 check will count ticks per idle second against the timers' budget, which does not depend on load.
- **Guarded timers, as built** (LANGUAGE §15.2).
  - Syntax: `timer t every d while G;`. The parser takes a `RELPATH` after `while`, and `ast::TimerDecl.guard` holds
    it.
  - Resolution happens in pass 2b (`timer_guards`), once every relation is known: G must be a view or table placed
    at the timer's role (BLS0412, new), and an unknown name is BLS0200. The guard goes in `HRelKind::Timer.guard`,
    then in the IR as `TimerDecl.guard` (serde default `None`, remapped). Lowering sets it after the relations are
    declared (`IrBuilder::set_timer_guard`, as for ACLs).
  - Node: `TimerTable` keeps a `held` bit per guarded timer. `Node::try_tick` observes the guards as it does `halt`,
    and `TimerTable::observe` updates the bits. A dormant timer has no deadline and no firings. A guard that comes
    to hold resumes the timer at its first firing after that tick (`first_after`): missed firings are skipped, so
    `count` stays on the boot timeline. The cluster simulator runs real nodes, so it follows.
  - Synchronous world (`BlsSim`, `SpecSim`): guarded timers are not pre-fed. `SyncConfig.guarded` lists them, and
    the round loop delivers a round's firings (`runtime::firings`) iff the guard held at the end of the node's
    previous round.
  - LDFI refuses guarded timers (LANG-172, beside `halt`'s LANG-052): its lineage treats timer firings as inputs
    that faults cannot change.
  - Kafka:
    - `fetch_wake` (10 ms) runs while `fetch_waiting` (a Fetch at a queue's head);
    - `raft_poll` runs while `election_armed` (a group of this broker's, with a deadline, that it does not lead);
    - an idle single broker went from 100 ticks/s to 20 (only `raft_heartbeat`).
  - `blossom run --stats FILE` writes the node's counters every second (`Stats::snapshot`; temporary file plus
    rename, not synced). kafka3's idle check counts ticks instead of CPU seconds. An idle broker there ticks about
    155/s; the bound is 1 250 per 5 s (the reads span 4 to 6 s), against roughly 400/s for a spinning debug broker.
    A spinner on a heavily loaded machine could pass; an idle broker cannot fail.
  - Tests (`timers_guarded.rs`):
    - the fixture `fixtures/timers/guarded.bls` in the synchronous world, oracle and engine agreeing;
    - the same fixture on a node, under both evaluators: the firings, no deadline while dormant, and the exact
      count of wake-ups;
    - the guard diagnostics;
    - LDFI's refusal.
  - `kafka_cluster::reassignments_move_partitions_under_load` failed on seed 2 with the new schedule: the admin
    client read a broker whose metadata lagged (ListPartitionReassignments is answered from the broker's own
    metadata, by design) and saw the previous wave's target still in progress. The client now accepts a replica
    being added that is in this wave's target or an earlier wave's; one in no requested target still fails.
  - Mutations, all caught:
    - the node ignoring the guard;
    - the node delivering missed firings;
    - the synchronous world ignoring the guard;
    - lowering dropping the guard;
    - LDFI not refusing.

## Review (2026-10-02)

Three reviewers ran in parallel, read-only: storage, the Kafka program changes, and guarded timers plus item 1. The
six findings of the S8 controller review were also rechecked; all were already fixed (S8 item 8; HD item 2;
DescribeCluster now returns 114/115; `NOT_CAUGHT_UP_WAIT` and the InitProducerId late rule).

**Storage** (fixed in 4905922):
- A logged blob's final name could survive a power loss without its bytes, after something else synced the blob
  directory (a large blob's put, the checkpointer, a collection); the node then trusted the name. Logged blobs now
  live under `<name>.log` until synced and renamed, and recovery deletes the provisional files no surviving record
  logs.
- Recovery replayed an unsynced tail (left in the page cache by a process crash) without syncing it. It now syncs
  and certifies the newest segment first.
- Smaller fixes:
  - `restore_logged` propagates real I/O errors;
  - `sync_logged_below` hard-errors on a vanished pending file;
  - the inline-blob budget counts the delta.
- Tests:
  - a power loss after a directory sync;
  - a replayed tail surviving a later power loss;
  - an old-scheme strict segment still recovering.

**Kafka program**:
- **High.** Whether a `done` row counted at the window's edge depended on tick timing (retention is filtered on each
  tick's state). For repeatable commands (pid blocks), brokers could disagree about applying a late copy, ending in
  duplicate producer ids. `fresh` and `mark_done` now use `done_by(origin, tag, i)` (`at + DONE_WINDOW >= i`, which
  retention always keeps), and the snapshot views are filtered by the applied index alone.
- **Medium.** `meta_snap` was one row; past the 16 MiB frame limit it would be dropped and the asking broker stuck.
  The snapshot is now a format (`MetaSnapshot`), encoded and sent as `meta_part(at, k, n, chunk)` of at most
  `META_CHUNK` bytes. The asker reassembles the parts in `meta_parts` and adopts the newest whole one. Asks go out
  every `META_ASK` (500 ms) through a guarded timer (`while meta_stuck`). The stopped-broker test uses 64-byte
  parts; the mutation "send only the first part" fails it.
- **Low–medium.** A deleted topic's Raft state leaked on a broker that adopted metadata past the `mdeleted` window.
  The cleanup is now keyed on the metadata itself: a data group with a term whose topic id no longer names a topic.
  `mdeleted` is gone. `check_runs` asserts that no broker keeps Raft state for a deleted topic.
- **Low.** Controller compaction could pass this broker's commit index after an adoption. It is now bounded by
  `commit_of`.
- **Low, accepted.** An origin more than `DONE_WINDOW` entries behind (an asymmetric partition, still latched as
  caught up) has every command skipped as stale, and its requests time out. Stamping `made` at the leader would
  break the duplicate proof (copies appended by different leaders would differ).

**Guarded timers and item 1**:
- **Medium (item 1).** The program's expressions inside a format (element arguments, conditions, defaults) ran
  unmetered, and a metered function called from a format got a fresh budget per call (N × 10^7). Now:
  - such expressions take no closure and no `range` (BLS0301);
  - every metered call of one evaluation spends from its one budget (`Fuel` no longer refreshes inside unmetered
    frames, in both evaluators);
  - the two evaluators' budget messages are identical;
  - tests: a metered condition per array item adds up to BLSR012 (and the mutation restoring fresh budgets fails
    it); the BLS0301 refusals.
- **Medium-low.** A guard over events or `now()` could make the node and the synchronous world disagree. BLS0412 now
  requires a guard that depends only on carried state, checked on the lowered IR (`unsteady_guards`). Kafka's
  `election_armed` was rewritten over `rterm` and `won` (it read `eff`, which folds in message terms).
- **Low.** Fixed:
  - `SpecSim::step` and exhaustive certification refuse guarded timers (LANG-172);
  - the IR validator checks a guard's class and placement;
  - `timer_guards` runs after `imports`;
  - the stats temporary file is `<path>.tmp`;
  - kafka3's idle check also asserts at least 80 ticks and live processes.
- **Documented.** Request deadlines now rely on the unguarded `raft_heartbeat` (50 ms) instead of `fetch_wake`
  (10 ms); a comment on `raft_heartbeat` says it must stay unguarded.
- **Not fixed.**
  - A duplicate timer's guard resolving to the first timer: diagnostics only, on a program already rejected with
    BLS0201.
  - The loosened reassignment check accepting an earlier wave's target from any broker: kept; its reason is in the
    test.

## Test tiers (2026-10-02, the user's choice)

The verification of this slice took hours on the laptop: debug-built simulator suites, throttled, re-run in full
after every change. The user chose fast and full tiers, with long full runs on their server (computer.jimmyhmiller.com,
32 cores) in the background.
- `blossom_integration_tests::{full_tier, seeds, seeds_of, scaled, scaled_of}`: `BLOSSOM_FULL=1` runs every seed. The
  fast tier runs each multi-seed simulation's first seed, and a threshold summed over seeds is scaled to the seeds
  that run.
- `#[ignore = "full tier"]` (reported as ignored, never skipped silently), from per-test timings on the server:
  - kafka_cluster's four heaviest fault runs (290–480 s each);
  - three LDFI crash and long-run proofs in bls_raft_ldfi (270–450 s);
  - the two kill -9 process tests in the CLI suite (150–275 s);
  - raft_kv's per-tick work test (104 s);
  - the 1.6M-item format decode (85 s).
- `scripts/test-tiers.sh fast|full`; CI's gate tier sets `BLOSSOM_FULL=1` and runs the ignored tests; the policy is
  in CONVENTIONS §8.
- The process-cluster tests of the CLI suite (`kill9`, `kafka_kill9`, `raft3`, `kafka3`) hold one lock
  (`one_cluster`) and so run one at a time: their ports are picked by binding port 0 and releasing it, which a test
  running at the same time could take (the first full-tier run failed on `Address already in use`). The latency
  measurement is no longer an ignored test (`--include-ignored` ran it): it runs with `KAFKA3_LATENCY=1`.
- The corpus (full tier only) passes: core 195, lattices 47, async 36, net 3. Lattices takes about 20 minutes in a
  debug build, on main as on this branch.
