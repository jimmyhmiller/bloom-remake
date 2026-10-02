# HD: paying down the S8 debts (working notes)

Branch `slice-hardening`, worktree `.worktrees/hd`. Chosen by the user (2026-10-01) before S9.

## Resume here

- **State (2026-10-01):** items 1 and 2 done; item 3 in progress (blobs logged in the WAL, strict at 3 syncs, both
  committed). Next for item 3: decide whether to go further (see its notes), then item 4.

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

- **Measurement.** `kafka3::acks_all_latency` (ignored; `--ignored --nocapture`, `KAFKA3_TAIL=crc|strict`) sends 300
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
- **Options left** (each sync cut is about 5 ms per hop here):
  - Strict at 2 syncs with an in-place two-slot receipt. This loses detection of a corrupted current receipt slot
    combined with damage to the last acknowledged batch (a double fault). A user decision.
  - Answer acks=all on commit rather than after materializing. This removes the leader's third commit, but Fetch
    must then serve committed but unmaterialized entries, or a read after an ack can miss the record.
