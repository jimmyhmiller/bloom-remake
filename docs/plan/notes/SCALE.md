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

The initial request was 50% higher throughput than Kafka (40,179 rec/s). The active goal is
**25% higher throughput**, provisionally **33,483 rec/s** against Soak 3's 26,786. The original 50% aspiration
remains above that acceptance threshold. Keep the same resources,
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

### Bulk scoped-store retraction

`Store::retract_all` now drains its support rows, empties each secondary index and blob counter once, and builds
the net deleted set in bulk. Current-tick insertions cancel; already-deleted and old present rows remain visible
to old-version readers. Index definitions survive for subsequent inserts. Generation advances once per removed
present row, independent of support multiplicity. The lattice/tiered-store exclusion is unchanged.

Two directed unit cases cover mixed inserts/deletes/reinsertion, multiple supports, old reads and deltas, generation,
index reuse, empty clears and blob retention. Eleven scoped/deletion/blob checks and 27 broader engine/planner/
produce/fetch/read-after-ack tests pass. All six release `kafka3` tests pass with `BLOSSOM_FULL=1` (193 s).
Engine Clippy, workspace formatting and whitespace checks pass.

Matching normal-release run (`~/soak-results/bulk-after-20261013`, same 60/180 s phases and pins as the preceding
pair): **12,124 rec/s**, versus 11,721 (+3.4% observed in one run). Steady ack p50 454.14 ms (was 483.07), fault
window p99 6.10 s (was 6.46), after-recovery p99 944.64 ms (was 842.75). Do not infer a general latency improvement
from these short windows. All **1,209,993** acknowledged records verified after kill/restart, with zero lost,
misplaced or duplicated. The 25%-above-Kafka acceptance goal remains open.

## Rejected performance experiments (2026-10-13)

Two private engine experiments used the same 60 s maximum / 180 s paced workload, 2,000 records/s,
10 topics × 12 partitions, replication factor 3, the same CPU pinning, and kill/restart controls as
`bulk-after-20261013` (12,124 records/s). Equality-only hash indexes, promoted for range queries,
produced 11,509 records/s and verified 1,180,989 records. Hash accumulation of rule head weights,
sorted canonically before application, produced 10,302 records/s and verified 1,109,314 records. Both were reverted: neither showed
a throughput improvement. Record verification found no losses, misplacement, or duplicates.
Artifacts: `~/soak-results/{hash-index,head-hash}-after-20261013` on the benchmark server.

A separate storage experiment returned only the newest eligible version per key from each SST scan;
older versions cannot win the subsequent cross-source merge. Paging still counts physical keys,
including keys with no version eligible as of the read. A direct test checked exactly one emitted
version, alternating puts/deletes, history spanning blocks, and a page boundary. The existing model,
reopen, and crash tests also passed. The matched short run produced 12,094 records/s versus the retained 12,124 baseline, with all
1,213,945 records verified and no losses, misplacement or duplicates. This showed no clear gain;
the storage experiment and its new test were reverted. Artifacts: `~/soak-results/scan-newest-after-20261013`.

The grouped-producer view experiment (`producer-group-after-20261013`) exposed each producer's
materializing batches as a separate indexed group instead of invoking producers_in and producer_batches
from both alternatives. It passed four simulator produce/read-after-ack tests, the default multi-process
Kafka tests, and both explicitly requested ignored full-tier kill/partition/catch-up tests. The latency
measurement remains opt-in. The server verified all 1,131,537 acknowledged records with zero loss,
misplacement, or duplication. Maximum throughput was 10,666 records/s; the view was reverted.

A 20-second leaf-only CPU sample near the end of maximum load crossed the max/drain phase boundary
(`/tmp/blossom-producer-group-leaf.perf`, 2,831 samples, none lost). Value comparison, memcmp, hashing,
SST scans, and relational evaluation remained prominent. This is diagnostic evidence, not a controlled
profile comparison or acceptance result. Next: a symbolized call-stack profile of the retained version
under a longer maximum phase, to identify a larger engine bottleneck.

## Cold-table isolation and cache controls (2026-10-13)

The retained implementation's symbolized/frame-pointer build ran a 180-second maximum phase and a
180-second paced phase with the same pinning, 2,000 records/s, kill at 60 seconds and restart 10 seconds later.
`baseline-stacks-20261013`: 11,439 records/s, 2,580,128 records fully verified. Steady ack p50 498.69 ms;
fault-window ack p99 9.40 seconds. Peak broker RSS 1,413 / 781 / 805 MB.
The 30-second broker-2 profile captured 25 seconds into maximum load had 7,156 samples, none lost.
Inclusive shares: tick evaluation 86.9%, relational evaluation 50.5%, store updates 18.4%, cold probes 18.0%.
Shares overlap. Artifact: `/tmp/blossom-baseline-current-max.perf` on the server.

The same build and controls with `TIERED=false` (`in-memory-diagnostic-20261013`) produced 18,663 records/s
(+63.2%), and fully verified 3,862,932 records. Steady ack p50 308.99 ms; fault-window ack p99 10.82 seconds.
Peak broker RSS was 5,691 / 5,420 / 5,157 MB. This isolates substantial cold-table overhead but does **not**
prove a matched-resource win or a solution for state that outgrows RAM. Durability remained strict.

Skipping unification comparisons for columns already matched by an index probe passed 22 planner/scoped/deletion
checks, additional produce/read checks, and the six-test multi-process suite with ignored tests included
(the opt-in latency measurement did not run). `probe-unify-after-20261013` produced 10,742 records/s,
with all 2,432,362 records fully verified. It was reverted: no throughput gain.

`storage.hot_rows` now exposes the existing engine-wide cache row budget through deployments, for both
node and keyed-object executors. Omission keeps the 65,536 default; zero is rejected. The benchmark accepts
`HOT_ROWS`. This is a row budget, not a byte limit, and changing it does not change persistence or durability.
Configuration validation, 13 runtime units, 12 runtime database/server checks and 8 storage model/crash checks
passed, along with Clippy for runtime/node/storage. Two preexisting storage lint violations were corrected.
The next run measures a 262,144-row budget in tiered mode; no throughput claim yet.

Open cache issue recorded in the project pad: distinct empty probe/range answers cost zero rows in the cache
counter, so their metadata can grow without triggering row-budget eviction. This remains unfixed.

## Empty-answer cache bounds and wider-cache diagnostic (2026-10-13)

With the existing per-probe cap, a 262,144-row budget (`hot-262k-diagnostic-20261013`, same 180/180-second
controls and profiling build) produced 11,666 records/s versus the prior 11,439 baseline, a 2.0% single-run
observation. All 2,596,583 acknowledged records verified with zero loss, misplacement or duplication.
Peak RSS was 974 / 1,003 / 897 MB. This is modest diagnostic evidence; no default change or acceptance claim.

The hot cache now bounds answer entries and large-prefix markers as well as rows, using the same engine-wide
budget. Empty probes/ranges participate in LRU eviction; obsolete empty range-key buckets are removed.
Residency reporting continues to count rows separately. Tests cover absent-key pressure, replacements,
growth/deletion, promotion to large-prefix markers, range eviction and preservation of previously returned
snapshots. The regression guard fails on row-only eviction and passes on the new rule. Four engine units,
10 scoped/database/tiered checks (including oracle comparison with 8-row caches, flushes, compactions and
restarts), and Clippy passed. This fixes the previously recorded metadata-growth bug.

The next performance experiment will lift the independent 256-row per-probe cap, letting the existing
one-sixty-fourth budget share govern probe size. The old code comment records that 1,024-row probes cost
one eighth of throughput in a prior 300,000-record Kafka test, so this is an experiment, not an assumed win.
The wider partition workload and larger total budget will be measured, including RSS and full verification;
retain it only if the evidence supports it.

The wider-probe policy (`probe-share-diagnostic-20261013`) regressed badly: 7,727 records/s versus
11,666 with the 256-row cap, and steady ack p50 5.06 seconds versus 459.78 ms. All 1,952,691 acknowledged
records verified with zero loss, misplacement or duplication. Peak RSS 803 / 980 / 868 MB.
A matched-time maximum-load profile had 6,950 samples, none lost; inclusive cold-probe work rose to
24.1% and row decoding to 13.5%. The per-probe cap was restored. Increasing read-ahead here repeats
substantial speculative decoding rather than eliminating the cold path.

An additional cache regression test found that one LRU pass can remove many old empty answers without
freeing any rows. Trimming now repeats until both counters fit. Its progress guard checks both counters,
so a decrease in entries alone does not stop reclamation early. The new test fails on the previous
single-pass rule and passes after the fix. Five engine units, eight database/tiered regressions and
Clippy pass with the original 256-row cap retained.

## Requested-range reads (2026-10-13)

Range misses no longer first decode a speculative whole prefix. A complete prefix already cached is still
used; otherwise the cold side reads the requested range directly and keeps small range answers.
A source-counter guard fails on the previous rule and passes on the new one, demonstrating one range read,
no whole-prefix probe, and reuse of the cached answer. Six engine units, 25 planner/database/tiered checks,
Clippy, and the six-test multi-process Kafka suite (ignored fault cases included; opt-in latency measurement
skipped) passed.

`range-first-after-20261013`, the same profiling build and 180/180-second controls with tiered storage,
256-row per-probe cap and 262,144 total budget: 12,145 records/s versus 11,666 (+4.1% in one comparison).
All 2,683,234 acknowledged records verified with zero loss, misplacement or duplication. Peak broker RSS
1,273 / 886 / 947 MB. Steady ack p50 494.59 ms (was 459.78); fault-window p99 7.75 s (was 8.65);
after-recovery p99 2.28 s (was 0.82). This does not prove a general latency improvement, repeatability,
or the Kafka acceptance target. The code is retained for its demonstrated read reduction and observed
throughput gain; the final matched soak must still assess recovery and memory behavior.

The maximum-load profile (`/tmp/blossom-range-first-current-max.perf`, 6,979 samples, zero lost) put inclusive
cold-probe work around 11%, with row decoding below the report's 3% threshold. Shares overlap and are relative.
Next larger investigation: compact dynamic value representation. Current Value is 48 bytes, alignment 16;
rare wide scalar/opaque/member/modular/blob variants constrain every scalar's layout. Any public representation
change must follow ARCHITECTURE §1.6 and preserve wire/JSON/postcard encoding, typed fingerprints, numeric
semantics and canonical ordering. Prototype separately and benchmark before retaining it.

## Compact dynamic Value experiment — rejected (2026-10-13)

Isolated `compact-value-bench` prototype reduced Value from 48 bytes/alignment 16 to 32/alignment 8 using
safe two-word full-width integers and immutable handles for bulky variants. All workspace targets compiled;
legacy JSON/postcard/hash/fingerprint/debug cases matched; arithmetic/ordering/encoding properties,
core/runtime units, 50 integration checks, full-tier Kafka tests and additional keyed/web-member checks passed.
The new binary opened each old store and read 229,245 batches per broker without initialization.

`compact-values-after-20261013`, same 180/180-second profiling workload, strict durability, tiered mode,
256-row probe cap, 262,144 total budget and pinning: **10,962 records/s** versus **12,145** (-9.7% observed).
Every **2,466,995** acknowledged record verified, zero lost, misplaced or duplicated. Peak broker RSS
753 / 816 / 1,005 MB (previous 1,273 / 886 / 947): memory improvement was uneven across brokers.
Steady ack p50 464.38 ms, fault-window p99 7.70 s, after-recovery p99 1.43 s. The maximum-load stack profile
(`/tmp/blossom-compact-values-current-max.perf`, 7,186 samples, zero lost) still showed relational work,
store updates and value comparison dominating. A smaller enum did not deliver higher throughput.

The prototype is rejected for the active throughput goal. It is preserved only on its isolated experimental
branch, with its API 3 amendment/version/migration kept together; it is not merged into scale-bench.
The retained value API remains version 2. The goal remains open.

Archived experiment commit: `b19a0f42` on `compact-value-bench`; do not merge into the retained branch.

## Recorded function-work diagnostic (2026-10-13)

`rule-trace-20261013` recorded the retained profiling binary with the same tiered/cache/pinning controls,
30 seconds maximum and 90 seconds paced, kill at 30 seconds, restart 10 seconds later. All 625,294
acknowledged records verified, zero lost, misplaced, duplicated, unacknowledged or foreign. Recording
changes performance, so its rate is not acceptance evidence. Traces occupy 2.2 GB on the server.

Broker 2 replay over maximum-load epoch seconds 1791888656–1791888657 covered 17 ticks and 2,826,927
expression steps. Own function steps: BatchHeader::decode 314,825 over 1,799 calls; after_append 310,276
over 3,758 calls; producers_in 97,490 over 5,590 calls. Rule leaders remain materialized_producer,
entry_check, sequenced, data_entry and produce_req. These are work counts, not CPU percentages.
Profile: `/tmp/rule-trace-profile-20261013.txt`; traces: `/tmp/soak-rule-trace-20261013/blossom/traces`.

The next isolated investigation (`function-plan-bench`, `.worktrees/function-plans`) is compiled function
evaluation plans (ARCHITECTURE §3.2). Preserve evaluation order, short circuits, scoped bindings,
function budgets, work counters, errors and time/random/blob tracking; no Kafka-name specialization
or unbounded memoization. No implementation or performance claim yet.

## Compiled function control-flow prototype (2026-10-13)

Isolated `function-plan-bench` prototype compiles let/if/match/typed control flow once per program into
immutable flat instructions. Leaves retain the established evaluator (including borrowed field reads,
arithmetic, library/host calls, closures, budgets and time/blob/random tracking). No input-dependent
memoization, Kafka function-name specialization, public ABI change or semantic IR mutation.

Ten engine units pass, including four direct tree/plan equivalence checks for results, arithmetic errors,
exact expression counters, time flips, failed partial patterns, guards, nested matches and restoration
of displaced bindings. All 51 selected function/planner/Kafka codec/record/produce/read-after-ack integration
checks pass. Engine all-target Clippy passes with warnings denied. Captured workload replay and matched
180/180-second throughput diagnostic remain pending; this is not retained on scale-bench yet.

Prototype follow-up checks: all workspace targets compile, and the six-test multi-process Kafka suite
passes with ignored fault cases included (359 seconds; opt-in latency measurement skipped). Captured
broker-2 replay through epoch 1791888657 verifies the recorded outcome digests and reproduces the entire
rule/function work report byte-for-byte. Three pinned replay runs per version, interleaved: baseline
user CPU 4.85 / 4.74 / 4.73 seconds; prototype 4.79 / 4.77 / 4.75. Essentially unchanged. The matched soak
is running; no throughput conclusion yet.

## Compiled control-flow result — not retained (2026-10-13)

`function-plan-after-20261013`, same 180/180-second profiling build workload, strict durability, tiered
mode, 256-row probe cap, 262,144 cache budget and pinning: **12,256 records/s** versus retained
**12,145** (+0.9% observed). Every **2,687,819** acknowledged record verified with zero loss,
misplacement, duplicates, unacknowledged or foreign records. Steady ack p50 479.49 ms; fault-window
p99 7.82 s; after-recovery p99 953.34 ms. Catch-up read 2,687,819 records in 15.2 seconds. Peak RSS 1,444 / 898 / 855 MB (retained comparison: 1,273 / 886 / 947 MB).

The three replay CPU comparisons showed essentially no change. This one soak's 0.9% difference does
not establish a useful throughput gain, so the prototype is not retained on scale-bench. It remains
archived on `function-plan-bench`, implementation commit `17a99e28`, with its correctness checks and
measurements. The large Kafka throughput gap remains. Next work must address relation/index execution
and materialization costs, rather than merely flattening function control flow.

Archived branch tip: `115fe467`; no prototype code merged into scale-bench.

## Singleton index-bucket prototype (2026-10-13)

Isolated `singleton-index-bench` replaces the per-key BTreeSet in in-memory and tiered-overlay indexes
with an empty/single-row/tree bucket. A second distinct row promotes it; removing all but one demotes
it and releases the removed allocations. Distinct rows under the same key are always retained; no
schema uniqueness shortcut. Iteration remains canonical, equal separately allocated rows deduplicate,
and support counting remains in the existing Store.

Nine engine units include a 10,000-operation ordered-set model comparison, promotion/demotion/lifetime
checks and an indexed support/old-version/range/retraction check. All 33 selected planner/deletion/scoped,
Kafka produce/read-after-ack and database/tiered integration checks pass. Engine Clippy passes. Release
build, captured replay comparisons, Kafka multi-process checks and throughput measurement are next;
no performance claim or merge into scale-bench yet.

Captured broker-2 replay passed and reproduced the full rule/function work report exactly. Three pinned,
interleaved user-CPU timings: retained 4.81 / 4.73 / 4.71 seconds; singleton buckets 4.57 / 4.56 / 4.57.
Peak replay RSS about 291 MB versus 321–324 MB. This is modest repeatable evidence on one trace, not
an acceptance result. All workspace targets compile. The matched 180/180-second soak remains live.

Open independent storage bug recorded in the project pad: decoded-block cache retention is charged
`BlockRef.len` (encoded bytes), rather than decoded Vec<Entry>/key allocation capacities. Its documented
decoded-byte budget can therefore be exceeded. Fix accounting before treating cache_bytes as a decoded
memory budget; it is not an RSS limit. This remains unfixed and is not part of the bucket prototype.

## Singleton buckets — retained (2026-10-13)

`singleton-index-after-20261013`, same 180/180-second profiling controls, strict durability, tiered mode,
256-row cap and 262,144 total cache budget: **12,338 records/s**, versus **12,145** (+1.6% in one run).
Every **2,730,259** acknowledged record verified with zero loss, misplacement, duplication, unacknowledged
or foreign records. Peak RSS **1,154 / 820 / 816 MB**, versus **1,273 / 886 / 947 MB**. Steady ack p50
482.30 ms (was 494.59), steady p99 2.57 s (was 1.09); fault p99 7.82 s (was 7.75); after-recovery p99
1.48 s (was 2.28). Catch-up read all records in 15.2 seconds. No general latency or Kafka acceptance claim.

The nine engine units, 33 integration checks, full workspace check and engine Clippy pass. All six
multi-process Kafka checks pass with ignored fault cases included (362 seconds; opt-in latency skipped).
The maximum-load profile `/tmp/blossom-singleton-current-max.perf` captured 7,087 samples, zero lost:
relational evaluation 45.1%, store updates 20.5%, Value comparison 16.5%, memcmp 14.1% inclusive, overlapping.

Retain the bucket change for its repeatable captured-workload CPU reduction, lower memory use and small
observed throughput gain. Broad repeatability and latency must still be assessed in final matched soaks.
The active Kafka target remains far away; next investigation is accurate decoded-cache accounting and
whether a larger bounded block cache can reduce the tiered-storage gap without in-memory-mode resource use.

## Decoded block-cache accounting and controls (2026-10-13)

Isolated `block-cache-bench` fixes the cache's encoded-size charge: retention now charges Vec<Entry>
capacity, each key's capacity and the Vec header. Allocator/Arc/cache-index overhead and externally
pinned blocks are excluded explicitly; this is not an RSS cap. The regression checks oversized spare
capacity, replacement, LRU eviction, disabled retention and preservation of pinned snapshots.

The documented `storage.cache_bytes` setting is now parsed and passed to both ordinary nodes and keyed
member databases. Omission keeps the LSM's 32 MiB default; zero disables retention; negative bytes are
rejected. The harness accepts BLOCK_CACHE_BYTES. Defaults and persistence/wire formats do not change.

All 40 storage units, 13 runtime units, 20 database/tiered/server/configuration integration checks,
eight LSM model/history/flush-compaction crash checks and both WAL crash-point tests pass. Runtime/store
all-target Clippy passes with warnings denied. The long segment-roll crash test was CPU-active and
finished; no test remains hung. The next measurements compare corrected 32 MiB and 256 MiB budgets
under the same 180/180-second soak controls; no performance claim yet.

## Corrected 32 MiB cache — verified and retained (2026-10-13)

`cache-32m-corrected-20261013` uses strict tail certification, tiered storage, 262,144 hot rows and
33,554,432 decoded cache bytes with the same 180/180-second controls and CPU pinning. **12,326 records/s**
versus the preceding singleton run's **12,338** (essentially unchanged). Every **2,740,863** acknowledged
record verified, zero loss, misplacement, duplicates, unacknowledged or foreign records. Peak RSS
796 / 839 / 893 MB. Steady ack p50 503.81 ms; steady p99 1.21 s; fault p99 7.30 s; after-recovery
p99 2.31 s. Catch-up took 15.2 seconds. These varying latency results are not a general improvement claim.

All six Kafka multi-process tests pass with ignored fault cases included (357 seconds; opt-in latency
skipped). Workspace all-target check passed. The accounting regression was explicitly changed back to
encoded charging: it failed (16 bytes charged versus 1,688 decoded bytes), then passed after restoration.
Retain the correct accounting and configuration wiring; no cache default change or Kafka target claim.

The 30-second maximum profile began around 78 seconds into maximum load, captured 6,986 samples, zero
lost. Inclusive scan-page work 13.3%, block access 9.1%, block reads 5.1%; shares overlap. It was later
than earlier profiles, so use a matching offset for the 256 MiB comparison. Artifact:
`/tmp/blossom-cache-32m-current-max.perf`. Next run uses the same binary and 268,435,456 cache bytes.

## Cache profiling timing correction (2026-10-13)

The 256 MiB sample started at epoch 1791892273.09, 103.0 seconds after its maximum phase began,
versus 78.3 seconds for the 32 MiB sample. The profiling timer slept from invocation after the phase
had already begun. Do not use these as matched-time CPU comparisons; each is a standalone 30-second
sample. Future timers must target the phase-start timestamp plus the offset and reject missed deadlines.
The two soaks themselves retain matched workload/pinning/durability/phases/fault controls. Final 256 MiB
throughput, memory and complete verification are still pending.

## 256 MiB decoded-cache diagnostic (2026-10-13)

`cache-256m-corrected-20261013`, the same binary, 180/180-second phases, CPU pinning, strict tail
certification, tiered mode and 262,144 hot rows, with 268,435,456 decoded-cache bytes: **13,168 records/s**
versus corrected 32 MiB **12,326** (+6.8% observed). All **2,853,821** acknowledged records verified,
zero loss, misplacement, duplication, unacknowledged or foreign records. Peak broker RSS **1,902 /
1,140 / 1,175 MB**, versus **796 / 839 / 893 MB** at 32 MiB. Steady ack p50 434.18 ms (was 503.81),
steady p99 1.71 s (was 1.21); fault p99 8.90 s (was 7.30); after-recovery p99 1.57 s (was 2.31).
Catch-up read all records in 15.2 seconds. Both benchmark processes completed with successful reports
and full verification. All correctness checks are terminal and passing.

This is a measured tuning option, not a default change or Kafka acceptance result. Larger retention
improves observed throughput but raises memory and worsens fault-window and steady p99 in this pair.
The 256 MiB profile has 6,783 samples, zero lost, but its 103-second offset differs from the 32 MiB
profile's 78-second offset; do not attribute differences in their shares solely to cache capacity.
Artifact: `/tmp/blossom-cache-256m-current-max.perf` and its profile timing file on the server.

The Kafka target remains unachieved. The large remaining gap needs changes to relational execution,
physical row/index representation and intermediate materialization, beyond cache capacity alone.

## Projected index ordering prototype (2026-10-13)

Isolated `projected-index-bench` lets a multi-row bucket compare the suffix after a leading run of indexed
columns. Those columns are equal within the bucket by construction. Full rows break suffix ties and
prefix width participates in the internal total order, so even independently compared keys cannot
collapse distinct rows. Single-row buckets remain allocation-free. No schema uniqueness assumption,
hash ordering, wire/persistence change or public ABI change.

Eleven engine units pass: mixed ordered-set models over seven projections including reordered/duplicate
columns, non-prefix columns and variable row arity; total-order/Eq checks; support, old-version and
range checks; removal and payload release. All 33 selected planner/deletion/scoped/Kafka/database/tiered
integration checks and engine Clippy pass. Release build and captured replay timing are next. No
performance claim or retention yet.

## Projected index ordering — not retained (2026-10-13)

Captured broker-2 replay outcomes pass and the rule/function work report remains byte-for-byte identical.
Three pinned, interleaved user-CPU timings: retained 4.66 / 4.57 / 4.56 seconds; projected 4.61 / 4.64 /
4.65. Peak RSS after warm-up about 291 MB for both. No repeatable CPU reduction is established; the
prototype is slightly slower on this captured workload. Do not merge it into scale-bench. Archive on
`projected-index-bench`, implementation d00d837e. All eleven engine units, 33 integration checks,
workspace all-target compilation and engine Clippy pass. No full throughput/fault soak was run for this
rejected prototype. No process remains running.

The planner, kernel and codegen crates remain placeholders; the retained engine uses dynamic Value
rows and interpreted relational operators. Those files were inspected directly. Next larger work must
build the physical execution path rather than assume a public Value layout or comparison tweak can
supply the remaining Kafka throughput gap. Final target remains open.
