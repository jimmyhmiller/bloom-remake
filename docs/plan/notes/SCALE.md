# SCALE: the Kafka soak against Apache Kafka (2026-10-11)

The user asked for "a non-trivial scale (not massive, but reasonable) head to head test of our kafka vs real kafka"
on computer.jimmyhmiller.com. Choices (question tool): a sustained soak (10 topics x 12 partitions, RF 3, 4
producers and a 4-consumer group running throughout; phases max rate, fixed rate, catch-up read); a broker killed
with kill -9 in the fixed phase and every acknowledged record verified afterwards; brokers pinned to the same cores;
if Blossom falls over, diagnose with the trace tooling, fix properly, rerun.

## The harness

- `scripts/kafka-soak/Soak.java`: the load client, Kafka's own Java client (kafka-clients 4.0) for every system.
  `produce` (acks=all, idempotent, paced without coordinated omission: latency is measured from each record's
  slot), `consume` (a group member; end-to-end latency from the slot), `verify` (reads every partition from its
  start; each acknowledged record must be at the offset its acknowledgement gave; no duplicates).
- `scripts/bench-kafka-soak.sh SYSTEM`: the phases, the kill -9 of the broker leading most partitions, per-broker
  CPU/RSS and disk sampling, the catch-up read and the verify. Results in one directory per run.
- `scripts/bench-kafka.sh`: per-broker start/kill/restart (`kill N`, `restart N`), `BROKER_CPUS` pinning,
  `PARTITIONS=0`.
- Pinning on the server (Ryzen AI Max+ 395, 16 cores / 32 threads; CPU n and n+16 are siblings): brokers
  `0-3,16-19`, `4-7,20-23`, `8-11,24-27`; clients `12-15,28-31`.

## Smoke runs (20 s max, 60 s fixed at 2000 records/s, kill at 20 s)

- **Kafka 4.0** (fsync per append): max ~29.6k records/s over 120 partitions; fixed-phase ack p50 120-200 ms before
  the kill (fsync per append across 120 partitions); ~10 s of stalled acks at the kill (KRaft session timeout 9 s);
  verify OK: 830 789 acknowledged, none lost, none duplicated.
- **Blossom** fell over: of ~162k records offered in the max phase ~75k were acknowledged (the rest timed out after
  the client's 120 s delivery timeout); the fixed phase acknowledged ~10k of 120k; brokers stayed at 100% CPU after
  the load stopped, RSS grew 0.5 -> 3.4 GB, DescribeGroups timed out. Stopped; evidence in
  `~/soak-evidence/smoke1` on the server.

## Diagnosis

Reproduced with no consumers: 4 soak producers at 2000 records/s in all over the 10 topics, 20 s
(`~/soak-exp2.sh 4 2000 20 0`): ack p50 0.75-1.8 s, p99 up to 115 s, records failing at 120 s, while the brokers
were only 25-30% busy. A single `kafka-producer-perf-test` producer at 4000 records/s over 120 partitions does not
show it (fewer, fuller batches: fewer blobs).

1. The Java client's log (WARN) showed 906 NOT_LEADER_OR_FOLLOWER and 861 OUT_OF_ORDER_SEQUENCE_NUMBER on 95 of the
   120 partitions: leaders kept changing, and an idempotent producer stalls a partition until its delivery timeout
   after an out-of-order sequence.
2. Recorded traces (`RECORD=1`; `blossom trace history … "won(_, _)"` and `"stepped_down(_, _)"`): 153 leaderships
   lost in 20 s, every one a CheckQuorum step-down, in bursts of 6-30 groups at once.
3. For one deposed group, both followers' last acknowledgements were 312 ms before the step-down. In that window
   every broker kept ticking but took no acknowledgement; `beat`/`ack` histories showed whole peer links silent for
   ~300 ms (b2 -> b1: nothing, then 70 beats and 174 acks in one tick).
4. A node releases a tick's messages only once the committer has synced its WAL record. The committer also made the
   WAL's truncation after each database flush: `BlobStore::sync_logged_below`, an `fdatasync` and a rename for every
   blob logged since the previous flush (every Kafka record batch is a blob). New stats (`wal_truncate_*`, then
   `blob_promote_*`, `blobs_promoted`): each truncation took up to **1.06-1.08 s** for ~700 blobs/s per broker
   (~5700 blobs in the run). For that long nothing a broker sent left it, so its peers' leaders heard no
   acknowledgements for a CheckQuorum period and stepped down, all at once.

## Fix 1 (done): blob promotion off the committer

- `blossom-runtime`: a `promoter` thread makes a flush's blobs durable; the committer hands it each truncation
  (`Promotion`) and drops the WAL segments once it reports back (it waits for work at most `TRUNCATE_POLL`, 100 ms,
  so it also takes finished promotions while idle). Shutdown has no channel cycle: the promoter's channel closes
  with the committer.
- `blossom-store::BlobStore`: safe for that concurrency, which the inline call never exercised. A pending blob's
  provisional file is never written again (logged anew, its earlier record outlives the file's sync; rewriting it
  while a promotion was between its sync and its rename could leave unsynced bytes under the blob's name and lose the
  blob once the WAL went), and promotions take turns (`promoting`): a truncation's and a put's promotion of one blob
  never interleave (the loser could otherwise find the file gone while the blob is still pending, a WAL failure).
  Tests: `a_pending_blob_logged_again_keeps_its_file_and_its_first_record`,
  `promotions_run_beside_logging_and_puts` (threads; a crash after; both fail with the old `write_logged`).
- Result: no record times out (40 000 of 40 000 acknowledged; the producers finish in 27 s, not 125+), but WAL syncs
  now take up to **0.56-0.92 s**: ~10 400 blob `fdatasync`s per broker compete with the WAL's on the one disk, so
  leaders still step down (~650 NOT_LEADER / out-of-order errors) and ack p99 is ~7.5 s.

## The cost of a sync per blob (server, ext4 on NVMe, 5000 blobs of 1 KiB)

| How | Time | Per blob |
|---|---|---|
| `fdatasync` + rename each, serially (today) | 1.613 s | 323 us |
| the same on 8 threads | 0.413 s | 83 us |
| the same on 32 threads | 0.193 s | 39 us |
| one file, one `fdatasync` | 0.019 s | ~4 us |

## Also seen (not yet looked at)

- Ack p99 at a fixed 1000 records/s is 1-3 s even with 6 partitions (S10 measured p99 14 ms at 2000 records/s):
  probably the same sync stalls (flushes come every few seconds under load).
- The soak's Blossom smoke run: RSS grew to 3.4 GB under the collapse (queues behind stalled links?).

## How others do it, and the choice

Nobody makes small values durable with a file and an fsync each. Kafka and Redpanda append to per-partition segment
files (Kafka leaves flushing to the OS by default; Redpanda folds many appends into one fdatasync per segment);
WiscKey, Badger and TiKV Titan keep large values in an append-only value log with one sync per batch and a garbage
collector; RocksDB's integrated BlobDB writes values into blob files during flushes and compactions, one sync per
file; Bitcask has append-only data files and hint files for a fast boot. On liveness, etcd documents slow fdatasync
causing elections (dedicated disk, WAL fsync p99 under 10 ms); Raft lets a leader replicate while its own write is in
flight; etcd's raft has AsyncStorageWrites; Redpanda/Scylla and CockroachDB give background I/O lower priority. A
follower's acknowledgement of entries waits for their sync; heartbeats wait for nothing (in Blossom every message of
a tick waits for its WAL sync).

User choice (question tool): **BlobDB-style packs**; storage first, then measure again before touching output commit or
I/O priority.

## Fix 2: pack files

- `<store>/blobs/p<seq:016x>.pack`: append-only; an entry is a header (magic `BLPK`, length `u64` LE, hash 32 bytes)
  and the bytes, so each entry checks itself. A pack is sealed past `pack_bytes` (64 MiB) and never appended again.
- Logged blobs (`write_logged`, `restore_logged`) are appended without a sync and are pending. `sync_logged_below`
  syncs every pack with unsynced bytes (one `fdatasync` each) and the directory if a pack is new, and then every entry
  those syncs cover is durable; a sealed pack synced whole gets `p<seq>.idx` (its live entries and a hash of them),
  so opening it needs no scan. `put_all` appends and syncs the same way.
- `delete` only drops index entries; a sealed pack with no live entry goes (and its `.idx`). `compact` (the promoter,
  after each promotion) copies the live entries of a sealed, indexed pack at least half dead into the active pack,
  syncs, points the index at the copies, and removes the old pack; reads that race it look the blob up again.
- Opening: `.idx` files are trusted (written after their pack synced whole); a pack without one is scanned, its
  entries checked against their hashes, cut at the first bad one (a torn tail), synced and indexed. So every entry
  found is durable, and a blob only some lost record logged is kept (unreferenced, it is collected after the next
  flush, as recovery already makes every stored blob no row holds a candidate). `drop_unrestored` goes. Stores from
  before: blob files `<hash>-<len>` are adopted as loose blobs (read, deleted, never written again); provisional
  `.log` files are removed (their surviving records log them again).

## The full soak after packs: overload collapse (Raft resends)

Kafka 4.0 full soak (fsync per append; `~/soak-results/kafka-1`): max phase ~19.6k records/s (4 x 4.9k, ack p50
5.5 s: unthrottled producers queue); fixed 2000/s for 30 min with a broker killed: ack p50 202 ms, p99 2.5 s, p99.9
11 s (the kill: KRaft's 9 s session timeout); 9 637 669 acknowledged, verify OK (none lost, misplaced or duplicated);
brokers ~16% CPU, ~1.3 GB RSS; 29 GB on disk.

Blossom with packs (`~/soak-results/blossom-1`, stopped): under the unthrottled max phase acks fell from ~5k/s to 0
within 100 s, peers' messages waited up to 15.8 s in the engine's queue, RSS reached 8.5 GB, ~272k records failed; 14
minutes after the load stopped every broker was still at 100% CPU and the consumers had not moved.

Recorded (`~/soak-rec.sh 30 60`: 30 s of overload, then 60 s with no client), read with `blossom trace`:
- `out_from` + `rsent` histories on b2 over 5 s of load: 960 of 1920 batch sends started below what the leader had
  already sent (resends of 4-8 entries in flight). Two rules did it: at every heartbeat the leader sent each non-idle
  group's followers everything after what they had acknowledged, and every acknowledgement short of the log sent
  again from it, although pipelined sends after `rsent` already carried those entries.
- `trace profile` of b2 10-20 s after the load stopped: 114 ticks in 10 s, 60 976 incoming entries stored, nearly all
  copies the follower already held (each hashed, written, logged): with acknowledgements seconds late, every heartbeat
  re-sent every group's in-flight entries, and the queues never drained.

## Fix 3: Raft replication without resends of in-flight entries (examples/kafka/raft.bls)

As etcd's raft: the leader pipelines new entries after `rsent` while fewer than `RAFT_INFLIGHT` (256) are sent past
what the follower holds (what it acknowledged, or `base`: the leader's last index when it began leading or tracking
it, since `match_idx` is 0 then). It goes back to what a follower acknowledged (`go_back`, `go_back_missed`):
- at once when a heartbeat finds the follower short of what the leader had sent before it (`beat` carries `upto`; a
  connection delivers in order, so the follower lost a batch and says `missed`);
- when the follower has acknowledged nothing new for `RAFT_RESEND` (500 ms, doubled each time in a row up to
  `RAFT_RESEND_MAX`, 8 s) and is heard alive: a lost acknowledgement, or a conflicting suffix the heartbeat cannot see.
A follower skips a batch whose last entry it holds with the same term (Log Matching: it holds all of it) and entries
it holds, and acknowledges them all the same.

Found on the way, by the suites: `initial_lead` (every Kafka partition's first leader) set `rsent`/`match_idx` but no
`repl_wait`, so its window never opened (kafka_groups' Calm scenario: a group never settled); a timer alone (500 ms)
recovered from lost batches too slowly under kafka_topics' faults (only 32 requests answered; the old code went back
at every 50 ms heartbeat), hence `missed`; an empty batch made `batch_held` underflow (`BLSR004`).

Test: `raft_groups::followers_slower_than_a_heartbeat_are_sent_each_entry_once` (40-60 ms latency each way, a tap on
the simulator's peer messages, `Cluster::tap_peers`): every entry goes to each follower exactly once; the old rules
sent entry 19 to b2 43 times. Suites: raft_groups, raft_kv, kafka_sim, kafka_groups, kafka_topics, kafka_ldfi (fast
tier), kafka_retention, node_blobs, node_database pass.

Same overload experiment after (30 s of 4 unthrottled producers, 60 s quiet): ~210k records acknowledged in 30 s
(~7k/s, ~96k before), the four producers now even (46-63k each); peers' longest wait 157-235 ms (was 4.0-13.5 s);
10-12% busy after the load (it recovers). Left: WAL syncs up to 1.8 s on the shared disk under this load, ticks up to
317 ms, so leaders still step down (~6300 NOT_LEADER in 30 s): every message of a tick waits for its WAL sync.

## Why leaders still stepped down: failure detection measured load

Traced overload runs (bpftrace on fdatasync, jbd2 commit stats and block requests, 4 runs): the WAL's fdatasync took
at most ~26 ms, journal commits never spent over 1 ms flushing ordered data or 16 ms logging, no device request took
over 200 ms (the 0.3-1.8 s syncs seen before did not recur; the probes stay in the harness). A run without step-downs
acknowledged ~12k records/s with no NOT_LEADER. The step-downs that remained (bursts: b3 40 groups, then b2 16, b1 3)
were CheckQuorum false positives: with every engine near 100% CPU, ticks took 100-290 ms; b3's heartbeat to b1 left
139 ms after the previous one, waited 285 ms for b1's tick, and b1's acknowledgement left after that tick's sync and
reached b3 after its 300 ms check. Each group's failure detection was measuring tick latency, i.e. load.

How others avoid it: Kafka's partitions detect no failures; brokers heartbeat the controller and a broker is fenced
after its session (9 s); CockroachDB put store liveness (one heartbeat stream per node pair) under its ranges' Raft.
Blossom's own quiet groups already count their leader's broker liveness (`alive`).

## Fix 4 (in progress): leadership follows broker liveness

Program side (examples/kafka/raft.bls, uncommitted): `led_by(g, t, l, boot)`: while the leader of a follower's term
is alive in that incarnation, a member there and not `deposed`, no election starts and no vote is granted
(`leader_alive`, `recently_led`, `timed_out`); CheckQuorum counts members whose brokers are alive; a leader's message
of an older term is answered with the newer one (`newer_term`, §5.1), so a leader cut off from its group's new leader
hears of it through a member both reach. Test: `raft_groups::a_loaded_cluster_keeps_its_leaders` (every broker 150 ms a
tick: the leader keeps its group 6 s and commits; the previous rules lost it after 1 s).

With `alive` sent by the program, liveness itself rides the loaded path (a tick makes it, it waits for the tick's WAL
sync, queues behind data, and is noticed at the receiver's next tick), so its session had to be 2 s. User choice
(question tool): runtime-level liveness instead, so the session can be ~500 ms:
- each peer connection carries a runtime heartbeat frame, sent by the connection's writer thread (no tick, no WAL sync,
  not behind queued frames' engine work), stamped on arrival by the reader thread;
- it carries the sender's incarnation and when its engine last finished a tick: a broker counts as alive only while its
  engine makes progress (a live process with a wedged engine must not keep its leaderships);
- the engine sees each peer's liveness as an input of its ticks (recorded in traces, replayed; the simulator models
  the heartbeats deterministically), read by programs as a built-in relation; raft.bls reads it in place of `alive`.

### Fix 4 as built (1654e29d): runtime liveness

- Language: `R.up(n: Node<R>, incarnation: u64)` / `R.down(n: Node<R>)` (HRelKind::Peer, EventSource::Peer), read at
  a cluster role (BLS0404 elsewhere), written only by the runtime (BLS0400); LANGUAGE §7.15, ARCHITECTURE §5.13.
- Runtime (`blossom-runtime::liveness`): an `ALIVE` frame (0x06) on a liveness connection per peer, sent every
  `[liveness] period_ms` (50) while the engine is not inside one tick for longer than the session
  (`EngineProgress`); `PeerViews` stamps arrivals and a checker takes a peer down after `session_ms` (500); changes go
  to the engine as `Control::Peer`, offered as inputs (recorded). A node reports itself up at its first tick, so a
  program learns its incarnation (the store's restart count, 1 at the first boot).
- Simulators: `Cluster` (heard while running, the link uncut and no tick longer than `liveness_session`; up one
  latency after, down a session after); the synchronous rounds (up at the first/restart round, down in the crash
  round, incarnation 1 then 2); LDFI's hazards (`Origin::Peer`: a crash raises `down`, a restart `up`).
- raft.bls reads `Broker.up/down` (`peer_up`, `my_boot`); the `alive` channel and `RAFT_SESSION` went; cluster.bls's
  re-telling of restarted brokers compares runtime incarnations.
- A simulator bug found by raft_groups/kafka_groups: a due change for an observer that was down kept the next
  instant in the past, so the simulation crept forward a nanosecond at a time; `liveness_next` skips down observers.
- Tests: `blossom-cli` `liveness` (three processes through proxies: kill -9 -> down after >= 300 ms, back in
  incarnation 2; SIGSTOP -> down, SIGCONT -> back; a one-way cut -> down at the far end only), `raft_groups`
  `a_loaded_cluster_keeps_its_leaders`, front-end diagnostics, liveness unit tests.

## Fix 5 (uncommitted): in-sync replicas follow liveness; a stuck limit apart from the session

- examples/kafka/cluster.bls `in_sync`: a follower counts only while its broker is up (`peer_alive`); one taken down
  leaves at once (Kafka's controller shrinks the ISR of a broker it fences), rather than after `REPLICA_LAG_MAX`
  (30 s in the soak), during which every acks=all produce to the partitions it followed waited for it. Test:
  `blossom-cli` `kafka3::a_killed_follower_leaves_the_in_sync_replicas_with_its_session` (REPLICA_LAG_MAX 30 s; after
  a kill -9 of a follower a produce is acknowledged within 6 s, the next ones within 2 s; the restarted follower
  rejoins). Checked on the server against the old rule: it fails there ("after the kill: not acknowledged within
  6s"), passes on the new one.
- `[liveness] stuck_ms` (10 s; `LivenessConfig::stuck`, sim `liveness_stuck`): a node stops sending `ALIVE` only once
  one tick has run that long. It used the session (500 ms), so a saturated broker whose ticks ran past 500 ms was taken
  down by its peers and back up again, and leadership flapped. Test: `raft_groups::
  a_broker_whose_ticks_outlast_the_session_stays_up_until_stuck` (a leader at 1.5 s a tick stays up and keeps its
  group; at a tick longer than `liveness_stuck` its peers take it down and elect another). It fails with the stuck
  limit equal to the session ("took the slow leader down after 1200 ms").
- A simulator bug the new test hit (again the one Fix 4 found, another path): `liveness_next` raised a stuck node's
  "words stop" instant for the whole tick, also after it had passed; clamped to `now + 1`, the simulation crept a
  nanosecond at a time for the rest of the tick (a 100 s tick: ~10^11 steps; the test ran 6 h before it was killed).
  Fixed: the instant is raised only while it is ahead. And the clamp is gone: a liveness change due at or before now
  is an internal error, so this class of bug fails the run at once ("a liveness change fell due at ... not after
  now") instead of hanging it.
- That error found a second such path at once (5 tests in raft_groups, kafka_groups, kafka_topics, kafka_cluster, all
  restarting a node): a down node kept `live_since` for the peers it could not hear, so restarted it raised their
  `R.up` from a moment seconds past (hidden by the clamp: it fired a nanosecond after the restart). A down node now
  forgets its views wholly (as `liveness_step`'s doc said), and restarted hears each peer one latency after its first
  word, as the runtime does. Every simulator suite, the sim/runtime unit tests and the multi-process liveness, kafka3
  and raft3 tests pass on the server (`~/bloom-logs/simcheck.log`).
- The new test's run cost: the harness's leaders append without end (no compaction) and the safety observer reads
  every node's whole state at every step, so a run's cost grows with the square of its length; with the default 10 s
  stuck limit the test took 264 s. It uses a 4 s stuck limit (the property is relative: session < tick < stuck stays
  up; tick > stuck goes down): 50 s. The observer's per-step full-state read is a known simulator cost, not addressed.

## Soak 3 (both fixes, 2026-10-12 21:39-22:52; `~/soak-results/{blossom,kafka}-3`)

Same harness and phases as above; brokers `0-1,16-17`, `2-3,18-19`, `4-5,20-21`, clients `6-7,22-23` (half the box).
Both verify OK: every acknowledged record present at its offset, none lost, misplaced or duplicated.

| phase | Blossom | Kafka 4.0 |
|---|---|---|
| max (4 unthrottled producers, 300 s) | 6 736 rec/s, ack p50 17.8 s, p99 24.1 s | 26 786 rec/s, ack p50 4.7 s, p99 7.9 s |
| fixed 2000/s, steady (572 s) | ack p50 1 056 ms, p99 2 022 ms, max 3.2 s | ack p50 179 ms, p99 1 523 ms, max 2.2 s |
| fixed, broker killed and back (120 s) | ack p50 2 134 ms, p99 12.7 s, max 15.3 s | ack p50 173 ms, p99 7.9 s, max 10.0 s |
| fixed, after recovery (1 080 s) | ack p50 1 260 ms, p99 2 681 ms | ack p50 184 ms, p99 1 518 ms |
| catch-up read, a new group of 4 | 5.76 M records in 26.4 s (~218k rec/s) | 11.8 M records in 15.2 s (~777k rec/s) |
| acknowledged in all | 5 758 330 | 11 814 643 |

Read: no failures and no loss on either side, and Blossom now rides through the kill -9 (soak 2 left partitions
leaderless). The gap is throughput and latency: ~4x lower max rate, ~6x the steady ack p50 at a rate both hold, and a
longer stall at the kill. Next, per the plan: bounded per-tick work.

## Performance target and CPU investigation (2026-10-13)

The initial request was 50% higher throughput than Kafka (40,179 rec/s). The active goal was subsequently revised
to **25% higher throughput**, provisionally **33,483 rec/s** against Soak 3's 26,786. Keep the same resources,
workload, strict durability, Java
clients, kill/restart and full record verification. Track acknowledgement latency and catch-up reads separately.
The target remains open.

Bounded CPU profiles on the server (`/tmp/blossom-{20261013,partition-max,seek-max}.perf`) show value comparison,
hashing and relational evaluation taking substantial CPU. Under max load, the original storage build with the
first partition-view change spent ~12% of sampled CPU in `DetHasher::write`, ~8% in `Value::cmp`, ~6% in `memcmp`,
and additional time in codecs, storage probes and allocation/reference counting. These are sample shares, not
independent speedups that can be added together.

Changes:

- Expose `partition_commit(tid, p, c)` from Raft's tuple-shaped group key. `note_commit`, `pending_batch` and
  `placed_done` can then probe by partition even when their change is driven by the commit index. The first two
  rules each spent ~14 million expression steps in a 30-second exploratory run at 2,000 rec/s; the explicit
  partition columns removed them from the slow-rule report. That alone did not materially improve steady latency.
- SSTable point and range reads seek within sorted blocks with binary search, rather than scanning from the
  beginning. Historical versions of a key spanning blocks remain supported. The eight LSM tests pass, including
  the versioned model, flush/compaction crash points, and a new directed cross-block historical-read test.
- The soak harness used `|| true` for verification and report errors, and ended successfully after an incomplete
  catch-up. It now preserves the summary/evidence and exits nonzero on those failures. Its final stage was checked
  with successful and failing verifier/report commands and an incomplete catch-up.
- A short soak could report a negative after-recovery duration, and count quiet drain time in the fixed fault
  window. Reports now clip fixed windows to the fixed phase's end and omit empty windows. Full-duration windows
  are unchanged.

### Short paired smoke (not the acceptance soak)

Server results: `~/soak-results/{baseline,seek}-smoke-20261013`. Same half-box pinning as Soak 3, 10 topics × 12
partitions, RF3, 4 producers and 4 consumers, 1 KiB records. Max phase 30 s, fixed 2,000/s for 90 s, kill at 30 s,
restart after 10 s. The max rate includes time to acknowledge the producers' outstanding records.

| build | max rec/s | max ack p50 | verified records | lost / misplaced / duplicated |
|---|---|---|---|---|
| 3e697298 | 8,973 | 10.928 s | 591,136 | 0 / 0 / 0 |
| partition joins + SST binary seeks | 9,714 | 10.215 s | 615,256 | 0 / 0 / 0 |

Observed max-rate gain: **8.3%** in this single short pair. This is not repeatability evidence or a comparison
against Kafka's full-soak rate. The fixed phase has only ~1 s after its 30 s warm-up before the kill, so its steady
percentiles are not useful acceptance evidence. Catch-up includes startup and is too short for comparison. An
earlier exploratory partition-view-only smoke overlapped a release build during catch-up; do not use its read rate.

Validation of the final changes: 24 Kafka integration tests across nine suites pass (four full-tier
`kafka_cluster` tests not included), all six release `kafka3` tests pass with `BLOSSOM_FULL=1` (190 s), and all
eight LSM tests pass. Shell syntax, changed Rust files' formatting, and whitespace checks pass. The original and
clipped reporters produce byte-for-byte identical output for the full Soak 3 artifacts.

The runtime still uses the value-level interpreted engine; `blossom-codegen` and `blossom-kernel` are placeholders.
The architecture's native executors, specialized codecs, word-level stores and bulk fingerprints are not implemented
there. Repeated hashing/comparison of immutable payloads and interpreted evaluation are candidates for the larger
performance work. The profiles do not establish that the requested target is impossible, or that these changes
will reach it. The simulator's full-state safety observer cost also remains open.

### Caller stacks and immutable byte hashes

The previous perf files had essentially flat call chains. A separate profiling build with release optimization,
debug symbols and frame pointers (`target/perf`) produced usable stacks (`/tmp/blossom-stacks-max.perf`, 4,734
samples, none lost). Inclusive CPU: engine tick ~90.6%, rule maintenance ~72.5%, evaluation ~43.9%, relation
insertion/update ~29.1%, cold database probes ~17.1%. Expression evaluation was only ~5.8%; missing expression
codegen is not the immediate dominant cost. `DetHasher::write` was ~13.2%, with substantial work hashing nested
values on relation insertion/removal. These shares overlap and must not be summed.

- `Value::Bytes` now holds immutable `BytesValue` (value API 2, architecture/DECISIONS amendments in the same
  commit). Payloads >=128 bytes share a construction-time BLAKE3-256 digest across clones. Collection hashing
  reads only the length and digest; `Blob::of` reuses the full content address. Small byte values retain one Arc.
  Equality still checks contents on a digest collision, ordering remains lexicographic, and the wire/serde bytes
  and typed fingerprints are unchanged. No global cache or interior mutability. The extra large-value allocation
  and construction-time hash are included in the measurement below.
- Tuple decoding appends fields arriving in schema order directly, without building implicit field-number arrays
  or optional-slot arrays. Reordered known fields switch to indexed slots; unknown, duplicate, missing and
  mistyped fields retain their existing checks. Explicit unordered schemas use the general path.

Clean normal-release pair: `~/soak-results/bytes-{before,after}-20261013`, same half-box pins, 10×12 partitions,
RF3, four producers/consumers, 1 KiB records, 60 s max, 180 s fixed at 2,000/s, kill at 60 s for 10 s. No profiling
or compilation ran beside these measured phases.

| build | max rec/s | steady ack p50 | verified records | lost / misplaced / duplicated |
|---|---|---|---|---|
| partition joins + SST seeks | 10,908 | 454.66 ms | 1,143,860 | 0 / 0 / 0 |
| plus byte hashes + ordered decoding | 11,721 | 483.07 ms | 1,190,073 | 0 / 0 / 0 |

Observed max-rate gain: **7.5%** in this single pair. Steady latency did not improve; the acceptance target remains
open. This is still shorter than the full soak and is not evidence of 25% higher throughput than Kafka.

The new normal-release binary also reopened the before run's three old stores without initialization and verified
all 1,143,860 acknowledged records, with zero loss/misplacement/duplication. Verification log:
`~/bloom-logs/scale-bytes-upgrade-verify.log`.

Validation: all workspace targets check; 85 value/wire/standard-host unit tests pass; 59 selected engine, function,
codec, blob and database regression tests pass (35 relevant tests rerun after the final immutable-digest/content-
address implementation); all six release `kafka3` tests pass with `BLOSSOM_FULL=1` (192 s). The previously ignored
large-format guard was also run separately with `BLOSSOM_FULL=1` and passes (35 s). Clippy on the value, wire,
engine and standard-host crates passes with warnings denied;
workspace formatting and whitespace checks pass. Two pre-existing formatting differences from 3e697298 were
normalized while migrating the touched tests.

Next concrete cost found: `Store::retract_all` removes every scoped row through `add(-count)`, repeating hash
probes, per-index removals and blob traversal although the whole store is being emptied. Its bulk operation must
preserve old-version rows, net deltas, generation, index definitions and blob retention; lattice/tiered stores
remain excluded. This is the next relation-update optimization to implement and measure.
