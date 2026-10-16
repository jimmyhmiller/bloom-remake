# Where Bloom Kafka's performance gap comes from

Measured 2026-10-15 on metaphysics. Branch: `kafka-gap-analysis`, based on
`compiled-maintenance` at `8750065a`. This is measurement work, not a new speedup.
The result does **not** establish parity with Kafka.

## Result and performance budget

Four fresh controls, in Kafka/Bloom/Bloom/Kafka order:

| Control | Acknowledged records/s | Broker CPU µs/record, all replicas | Attributed block-write bytes/record, all replicas |
|---|---:|---:|---:|
| Kafka 1 | 22,219 | 30.18 | 4,218 |
| Bloom 1 | 14,538 | 195.38 | 10,515 |
| Bloom 2 | 16,383 | 180.56 | 10,221 |
| Kafka 2 | 24,350 | 28.90 | 4,206 |
| Arithmetic mean throughput | Kafka **23,284.5**, Bloom **15,460.5** | | |

Bloom is 33.6% slower by mean throughput; it needs **50.6% more throughput** to
reach this Kafka result. Each acknowledged record costs about **6.4 times as much
broker CPU** and **2.46 times as many attributed block-write bytes**. These ratios
use the means of the per-run costs. They are not device-level NAND amplification.
All four controls passed exact post-crash verification: no lost, misplaced,
duplicate, unacknowledged-present, or foreign records.

The engine accounts for **84.4% of Bloom broker CPU**. Aggregate engine CPU cost is
164.87 and 152.34 µs/acknowledged record. With one engine thread per broker and
three brokers, the conditional ceiling is `3 / engine_seconds_per_record`:
**18,196–19,692 records/s**, even with continuously busy engines. This holds
observed batching and cost per record fixed; it is a budget, not a prediction.
It is below even the slower Kafka control. Removing waits alone cannot close the
measured gap under those assumptions.

At Kafka's mean rate, three engine cores allow **128.84 µs/record**. Bloom needs
15–22% less engine CPU per record even at 100% availability, or roughly one-third
less cost at its observed availability. Alternatively, correct partition-parallel
execution would need roughly 1.18–1.28 engine cores per broker at unchanged cost,
plus other broker work. The allocated two physical cores per broker have room in
principle; synchronization, skew and global barriers could prevent that scaling.

## What was held constant

Same host (AMD Ryzen AI MAX+ 395, 16 physical cores / 32 SMT threads), NVMe-backed
filesystem, broker CPU sets `0-1,16-17`, `2-3,18-19`, `4-5,20-21`, client CPUs
`6-7,22-23`. Each broker receives two physical cores, not four physical cores.
The governor was powersave. No global kernel or durability knobs were changed.

Both systems: 10 topics × 12 partitions, replication factor 3, four producer and
four consumer Java clients, 1 KiB records, acknowledgements from all in-sync
replicas, idempotent producers, default producer batching. The max phase sends for
60 seconds and then drains acknowledgements; reported throughput includes that
drain (roughly 67–74 seconds total). A subsequent 2,000 records/s phase kills the
busiest broker at 20 seconds, restarts it after 10 seconds, catches up and verifies.

Kafka is 4.0.0 with `log.flush.interval.messages=1`, **not default asynchronous
flush Kafka**. Bloom uses strict tail certification, tiered storage, 262,144 hot
rows, 32 MiB block cache, 8,192 retained-map rows, retained joins, lazy indexes and
one evaluation worker. Compiled joins, packed indexes and fusion are enabled;
pipeline batches and inferred keys are unset. Both are exercising durable writes,
but Bloom's strict tail-corruption certification is a different/stronger policy;
this does not prove identical recovery guarantees for every failure model.

Control Bloom binary: `~/soak-results/blossom-fused-maintenance`, SHA-256
`029f02ca6832da01651d0dd97a6958f81b265f1fc74f3bd144dfb3e37c588327`.
The runtime source is `b89488b3`; subsequent baseline commits contain reports.
The profiling binary adds release debug line tables and frame pointers; its runs
are diagnostic, not controls. No compilation overlapped a throughput control.

## Latency and crash-phase observations

These are producer acknowledgement histogram values from the harness; max-load
latencies include substantial client queueing and must not be called broker
service time. The crash-window column is the harness's “fixed, broker down and
back” subwindow, not a no-failure latency benchmark.

| Control | Max ack p50 | Max ack p99 | Crash-window ack p99 |
|---|---:|---:|---:|
| Kafka 1 | 4.35 s | 14.02 s | 9.32 s |
| Bloom 1 | 7.47 s | 13.47 s | 6.50 s |
| Bloom 2 | 6.68 s | 10.22 s | 6.80 s |
| Kafka 2 | 4.35 s | 7.39 s | 8.32 s |

All four fixed-rate controls acknowledge all 120,000 sent records with zero
producer failures. Both systems sustain the requested rate across the complete
phase including drain. Bloom's crash-window tail is lower here despite its lower
max throughput; this small sample does not establish general recovery superiority.
Full phase summaries, producer counts, consumer rates, catch-up and verification
results for all eight live runs are preserved in `kafka-gap-data/live-summaries.json`.

## Live CPU: maintenance and representation dominate

A separate 30-second max-load profile sampled user cycles at 199 Hz across all
three brokers: 13,493 samples, weighted by sample period. Engine threads receive
90.9% of sampled user cycles (different denominator from total CPU above).

The following exclusive stack partition assigns each sample to its first matching
category, in priority order: LSM, expression evaluation, row support, other engine
store, other engine, serialization, other. It sums to 100%; nested ownership is
resolved by that priority rather than counted twice.

| Exclusive stack owner | User cycles |
|---|---:|
| Other engine store operations | 31.35% |
| Other engine evaluation/maintenance | 23.12% |
| LSM access and maintenance | 19.63% |
| Row support | 8.08% |
| Expression evaluation | 7.54% |
| Serialization | 1.67% |
| Other | 8.62% |

An independent **leaf mechanism** partition finds Value equality/order/hash at
16.93%, byte comparison 8.48%, allocation/free 8.06%, Value clone/drop 7.62%, and
copying 2.76%. Together these are about 43.8% of sampled user cycles. They cross
engine and storage ownership: do not add them to the table above. Byte comparison
is not proof of repeatedly comparing entire record payloads: stacks show encoded
LSM key comparisons in scan/probe and key-tree update paths.

Useful **overlapping inclusive** paths are Store 53.89%, delta drivers 37.76%,
joins 26.76%, LSM 19.63%, row support 8.08%, expression evaluation 7.54%, and
compiled kernels 6.85%. Store.add alone appears on 18.83% of cycles; new_rows on
10.89%, LSM scan_page on 8.63%, Store.any on 5.49%, range_rows on 4.98%, and
change_count on 4.93%. These inclusive numbers must never be summed.

This explains the small previous improvement. The previous “compiled” path is a
source-column relational kernel, not native compilation of the whole engine. Its
current execution occupies only 6.85% of sampled user cycles. Expression evaluation
is only 7.54%. Even deleting all expression evaluation would offer only an 8.2%
user-CPU speedup under fixed costs; it would not remove storage/support/index work
or synchronous waits. That is an illustration of limited scope, not a bound on a
compiler that changes representation and materialization too. The fusion pass
selects one inactive `fresh_grant` view in these windows and eliminates essentially
no hot work. The earlier noisy 4% result was not evidence that this architecture
had been broadly optimized.

## Live waiting: synchronous META reservation blocks the engine

In the 30-second CPU/wait diagnostic, each engine performs 29 fdatasync and 29
fsync calls. Combined syscall time per engine is **4.14–4.45 seconds**, or
13.8–14.8% of the window. Uninterruptible off-CPU time is 4.32–4.63 seconds.
Runqueue delay is only 3.2–7.1 milliseconds: CPU scheduling contention is not the
main explanation for those blocked intervals.

The source path is `server.rs`'s engine-side `self.meta.write(&self.record)`,
`MetaStore::write`, then `vfs::atomic_write`: write temporary file, sync its data,
rename, sync the directory. `node.rs` reserves timestamp bounds in one-second
steps. A separate direct uprobe on `MetaStore::write` confirms 30 calls per engine
in 30 seconds, totaling **2.23–2.84 seconds per engine** (7.4–9.5%). The amount
varies across runs; the caller is established, but a fixed 14% gain is not.

A proper optimization is asynchronous **durable pre-reservation**: continue only
within already durable bounds and publish new bounds only after their durable
acknowledgement. Removing fsync or changing the recovery promise is not a valid
optimization. A blocked engine also delays protocol/produce/fetch work for unrelated
partitions; moving these reservations off its critical path merits an isolated test.

Kafka's diagnostic has 100,873 fsync/fdatasync calls in 30 seconds, versus Bloom's
4,128. Kafka accumulates 171.74 thread-seconds inside those calls versus Bloom's
42.74; concurrent thread times overlap and are not wall-time fractions. Kafka's
replica/request threads average about 1.6–1.8 ms per call, while Bloom's committers
average about 4.7–7.2 ms and its engines about 71–77 ms. Fewer sync calls alone do
not imply higher throughput: Kafka spreads these waits over many request threads.
These counters exclude mmap `msync`; Kafka's index flushes can use that path.

## File-write attribution

An additional 30-second diagnostic measures successful VFS write return bytes,
grouped by file basename across all three brokers. Of **3,500,211,172 logical
bytes**, **36.13%** are LSM table temporary files, **32.33%** WAL segments, and
**31.44%** blob packs. Other files account for 0.10%. File naming was checked
against the LSM, WAL and blob-store writers. The WAL/blob double path is present
in the live measurements, and LSM table output is also substantial.

These are logical bytes returned by `vfs_write`/`vfs_iter_write`, not physical
writes, and exclude mmap writes. The diagnostic has different timing and batching
from the controls; its proportions must not be mechanically multiplied into the
control block-byte ratio. It does not separate initial LSM flush from compaction.
The current WAL holds blob payloads and the committer also appends them to packs;
this supports investigating a shared durable payload representation, not removing
either copy without a recovery/reclamation design.

Engine blocked kernel stacks confirm both filesystem journal-commit waiting
(`jbd2_log_wait_commit` through `ext4_sync_file`) and file writeback waiting
(`folio_wait_writeback` through `ext4_sync_file`). The source and direct META
uprobes establish the caller; kernel stacks establish the kind of I/O wait.

## Relational work per stored batch

A fresh, fully verified run recorded broker 2's complete trace (1.2 GB, no torn
trace tail). Selected five-second windows replay their entire prefix and check
recorded outcomes. These replays expose **1,741 rules and 1,332 physical stores**.
A batch below is a newly inserted durable Kafka `batch` row, not a single record.

| Work | Max early (96 ticks) | Max late (84 ticks) | Fixed rate (799 ticks) | Recovery (735 ticks) |
|---|---:|---:|---:|---:|
| Stored batches | 4,320 | 4,000 | 2,960 | 686 |
| Support writes | 607,221 | 582,408 | 845,243 | 435,837 |
| Support writes / batch | 140.6 | 145.6 | 285.6 | 635.3 |
| Membership changes | 746,477 | 717,421 | 984,251 | 597,399 |
| Secondary-index row updates | 496,046 | 483,473 | 669,341 | 468,781 |
| Probe API entries (including nesting) | 2,114,507 | 2,076,688 | 3,244,625 | 1,496,039 |
| Probe rows | 1,085,872 | 862,051 | 1,326,470 | 846,915 |
| Expression steps | 9,430,441 | 8,149,432 | 11,302,079 | 4,713,572 |

At max load, each stored batch induces about **115–121 secondary-index row
updates** as well as 140–146 support writes. This is already incremental execution;
“make it incremental” alone misses the cost of maintaining its many intermediate
relations, support counts, indexes and carried-state transitions.

Early-window rule maintenance takes 1.323 seconds: generic relational 651 ms,
retained maps 305 ms, compiled relational 190 ms, copy rules 133 ms, native
upsert/delete 40 ms; fused observation is only 0.0029 ms. No single rule dominates.
`fetch_first` is the largest at 56 ms, with 193,689 probe rows and no support writes;
`data_entry`, `entry_check`, `out_batch$u`, `pending_batch`, `data_proposals` and
`materialized_producer` follow. This requires both read/probe and update-path work.

The highest index-update counts include `ack:sent` (32,520), `beat:main` and
`beat:sent` (24,050 each), `rlog` (21,640), `pending_batch` (17,600), and
`produce_entry` (15,360). There is substantial protocol bookkeeping, not just
payload handling. High-load delivered messages include 5,480 acknowledgements,
5,972 heartbeats, 923 entries and 361 quiesces in the early window. The trace
reports actual boundary event types, function calls, own expression steps and
store operations in the accompanying JSON.

Only 29,519 early-window membership changes and 62,924 index updates are marked
as *declared durable main stores* by plan reasons. This does **not** mean the rest
is disposable: next-state stores, auxiliary maintenance, protocol state, duplicate
support, negation and aggregation semantics can all require state. Eliminating a
materialization must prove its use/observation and maintenance requirements.

## Measurement bug found: replay time is not engine time

`Replay::next` clones the entire historical blob BTreeMap each tick. This creates
history-dependent preparation and destruction overhead in the offline harness,
not the live broker. It is recorded as an open Bloom bug in the project pad.

New opt-in phase accounting separates preparation, `Executor::step`, and other
verification, internal report extraction and cleanup within `Replay::next`.
Outer CLI aggregation/JSON output is outside this timer. In the fixed-rate window, **2.43 of 5.79 seconds**
is engine execution; preparation is 1.63 s and other work 1.73 s. Recovery is
1.57 s engine, 1.81 s preparation and 1.92 s other work. Assigning all replay time
to relational execution would have produced the wrong diagnosis.

Max-early measured engine time is 1.639 s versus 1.610 s with store-operation
measurement off; max-late 1.589 versus 1.561 s (~1.8% difference in these single
pairs). Function profiling remains enabled in both. These are not uninstrumented
production times, and the replay uses in-memory storage without live network/disk
waiting. Counts and verified outcomes are the primary evidence; rule timers guide
where to investigate, not a substitute for the live CPU profile.

## Implementation priorities and acceptance criteria

1. **Remove engine-blocking META persistence correctly.** Pre-reserve asynchronously
   with explicit durability handoff and exhaustion behavior. Verify crash/restart
   monotonicity and existing recovery guarantees. Measure engine blocked time and
   throughput; do not assume this alone reaches Kafka.
2. **Compile the physical maintenance plan, not merely expressions.** Use inferred
   types and keys for compact rows and indexes, reduce generic Value compare/hash/
   clone work, and fuse actually hot producer/replication/materialization chains.
   Keep Bloom source relational and small; choose transformations from dependencies,
   keys and observation semantics, never Kafka-name special cases. Validate against
   the reference engine including retractions, duplicate support, recursion,
   aggregates and negation. Require reductions in measured support/index operations
   and CPU per acknowledged record, not just counts of “compiled rules.”
3. **Reduce persistent access and duplicate payload movement.** LSM is 19.6% of
   sampled user cycles and writes are 2.46× Kafka. The current committer serializes
   logged blobs into the WAL and appends them to blob packs before eventual promotion.
   A shared durable representation is a legitimate storage-engine direction only
   if crash recovery, tail certification and reclamation remain correct. Separate
   payload bytes from table/index rewrite costs before selecting that redesign.
4. **Evaluate partition-parallel execution after quantifying its synchronization.**
   There is spare allocated CPU, but global strata/negation and shared state must
   remain correct. This is a compiler/runtime capability, not an FFI implementation
   of Kafka. Report per-partition skew, engine utilization and cost, not throughput
   alone.

A concrete next performance gate is aggregate engine cost below **129 µs/record**
at full availability, or around **106 µs/record** at 82% availability, followed by
repeated end-to-end controls beating 23.3k records/s on this contract. Percentages
from different profiles are not additive speedup promises. Winning this benchmark
would still not establish superiority on every Kafka workload.

## Evidence, reproducibility and limits

Local data: [`kafka-gap-data/`](kafka-gap-data/), including controls, CPU partitions,
sync histograms, perf counters, environment/configuration and six replay reports.
Scripts and accounting definitions: [`scripts/kafka-analysis/`](../../../scripts/kafka-analysis/).
Remote retained raw artifacts on `computer.jimmyhmiller.com`:

- `~/soak-results/gap-control-{kafka-1,bloom-1,bloom-2,kafka-2}`:
  summaries, correctness verification, process counters, device sampling, logs.
- `~/soak-results/gap-diagnostic-{bloom,kafka}`: perf counters, Bloom stacks/data,
  sync/off-CPU histograms and window timestamps.
- `~/soak-results/gap-trace`: full broker-2 trace, timeline, selected windows,
  direct META timings and replay JSON (`replays-before-timing` preserves old runs).
- `~/soak-results/gap-write-path`: additional file-write and blocked-kernel stacks.

Process accounting uses samples nearest phase boundaries (reported errors roughly
half a second or less); one-second engine stats may lag further. Per-thread deltas
can omit threads born and dead inside the interval; process totals include them.
The sampler's worst observed pass was 34 ms; it is not zero overhead. CPU includes
Kafka JVM compilation/warmup. Hardware counters were multiplexed (~71% active).
The host filesystem was nearly full (about 12 GB free before the first controls; the final diagnostic
reached 2.8 GB free before its verified scratch was removed). All systems shared that environment, but storage state and run
order still matter. Two runs per system give a useful budget, not a confidence
interval or long-run steady-state distribution. Crash verification does not prove
all possible failures safe. Raw trace replay covers one broker and selected windows.

Validation of measurement code: CLI trace-plan tests (2 passed), Clippy for CLI,
sim and trace all targets with warnings denied, timeline example Clippy. Every
reported live run and selected replay passed its outcome checks. No production
optimization is claimed by this branch.
