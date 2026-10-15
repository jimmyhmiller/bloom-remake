# Kafka execution experiments, October 15, 2026

Worktree: `.worktrees/kafka-execution`, branch `kafka-execution`, based on `store-experiments` at `0b6d9f0f`. The objective is to outperform Kafka without moving protocol logic out of Bloom, changing relational semantics, or weakening durable acknowledgements. The objective has **not been achieved** by the measurements below. Kafka program semantics are unchanged; the repository formatter repaired pre-existing formatting drift in two source files.

## Measurement contract

Use `scripts/bench-kafka-soak.sh` on `computer.jimmyhmiller.com`, with the same stock Java clients, 10 topics × 12 partitions, replication factor three, four producers and consumers, and 1 KiB records. Brokers receive `0-1,16-17`, `2-3,18-19`, `4-5,20-21`; clients receive `6-7,22-23`. The max phase is 60 seconds of sends plus acknowledgement drain. The fixed phase is 2,000 records/s for 60 seconds, killing the busiest broker after 20 seconds and restarting it after 10 seconds. Reported throughput includes acknowledgement drain. The short fixed phase measures fault recovery, not a separate steady-state latency window.

Bloom uses tiered storage, 262144 hot rows, a 32 MiB decoded-block cache, strict tail certification, 8192 retained-map rows, retained joins, lazy index costing, and one evaluation worker unless explicitly comparing workers. Pipeline batches and inferred-key experiments remain disabled. Builds never overlap timed broker trials. Frozen executables and separate output/scratch directories prevent cross-trial source changes. Scratch databases are removed only after complete record verification.

The earlier matched Kafka controls in the pipeline investigation reached 21,789–24,417 records/s. They are context, not a fresh interleaved control for changes in this document. A new Kafka comparison is required before claiming a lead.

## Rejected signed-change accumulator

Commit `aa620e05` replaced per-output tree insertion with bounded sorted runs and consolidated merges. Seven rotating checked replay pairs improved median elapsed from 0.511403 to 0.483853 seconds (5.4%). Live trials failed to reproduce a consistent gain:

| Trial order | Baseline records/s | Candidate records/s | Candidate change |
|---|---:|---:|---:|
| Baseline → candidate | 14,931 | 13,928 | -6.7% |
| Candidate → baseline | 15,063 | 16,539 | +9.8% |

The four trials verified 4,604,354 records with zero lost, misplaced, duplicated, present-unacknowledged, or foreign records. The accumulator was reverted in `3e93edc8`. A replay improvement is not sufficient broker evidence.

Raw reports: `~/soak-results/execution-batch-{baseline-1,batches-1,batches-2,baseline-2}`. Replay reports: `execution-batch-replays`. Frozen candidate: `blossom-execution-batches`; baseline: `blossom-plan-fusion-fixed`.

## Bounded evaluator scheduling (removed after live comparison)

These scheduling changes were removed from the final implementation. They remain available in commit history; the measurements did not justify retaining the added runtime complexity.

Commit `37de2e58` schedules consecutive independent pure rules against an immutable snapshot on the existing bounded worker pool. Physical store aliases participate in conflict checks. Publication remains in original rule order. Recursive and lattice strata, retained-map cache updates, effectful rules, native copies, upserts and frames remain barriers. Later evaluation errors cannot overtake earlier rule errors. Per-rule elapsed profiling retains sequential scheduling so its attribution stays meaningful.

Seven rotating checked replay comparisons gave median elapsed 0.511322 seconds with one worker, 0.506086 with existing within-rule workers, and 0.510063 with independent-rule workers. This is a generic capability under test, not an established Kafka improvement.

Commit `940d7777` permits retained-map primary insert computations to use that pool. Contiguous eligible inserts are evaluated in groups of at most 256, with worker chunks of 32. Cache insertion, eviction, retraction and join-side cache updates preserve their original coordinator order. Retractions and nonprimary drivers form barriers. The regression covers 320 inputs, multiple groups, duplicate generated outputs, simultaneous join changes, tiny cache eviction, reset, exact function profiles and matching errors.

Raw replay reports: `~/soak-results/execution-stages-replays`. Frozen binaries: `blossom-execution-stages`, `blossom-execution-retained`.

## Live profile and storage changes

The symbolized baseline live profile sampled 11,791 user/kernel cycles samples with no lost samples. Approximately 86% of samples were on the engine thread; expression evaluation accounted for about 10% inclusively. Important inclusive paths overlap and must not be summed. Self samples include canonical Value comparison 6.74%, LSM point lookup 4.29%, Bytes comparison 3.53%, Value hashing 3.18%, engine BLAKE3 3.15%, and table scans 2.80%.

The diagnostic broker run verified 1,169,399 records. Raw profile: `/tmp/blossom-execution-live.perf`; symbolized binary/source: `~/bloom-kafka-execution/target/release/blossom`. Profiling runs are diagnostic, not throughput acceptance trials.

Commit `b73890e9` prunes a point lookup's disk tables only when their version bounds prove that they cannot improve its newest eligible result. An exact as-of memory hit returns immediately. Tombstones are treated like puts for version selection. Compacted table order is never assumed to equal version order. Historical reads, amendments, flushes, compaction and crash/recovery comparisons pass.

Snapshot scan reduction additionally keeps only the newest eligible version per key from each source. Entire older blocks containing only an already-selected key can be skipped. Page boundaries continue counting all distinct keys, including deleted and future-only keys. Compaction and persisted history are unchanged. Historical model tests and a regression with hot-key histories crossing blocks and page sizes 1–3 check this distinction.

## Shared byte views

Large byte slices can share immutable backing storage. A view computes its digest only when hashing or making a content address requires it. Equality checks actual bytes even on digest collisions; canonical ordering remains lexicographic. Exact `Arc<[u8]>` ownership materializes a partial view on demand. Small slices copy fewer than 128 bytes so small headers release large request buffers. Large views can extend backing-buffer lifetime; live RSS is part of acceptance.

The engine uses views for `Bytes.slice` and `Blob.read`; the independent oracle retains copying implementations. Tests compare nested views with owned bytes, hashes, ordering, serialization, content addresses and concurrent hashing. An end-to-end regression turns an interior slice into a blob and reads it in later ticks on both evaluators.

## Repeated live point-pruning and worker results

| Variant | Records/s | Max ack p99 ms | Fault ack p99 ms | Peak broker RSS MiB |
|---|---:|---:|---:|---:|
| Baseline 1 | 14,833 | 12,967.94 | 5,890.05 | 735 |
| Point pruning 1 | 15,529 | 10,829.82 | 6,217.73 | 778 |
| Point pruning + workers 1 | 15,403 | 12,017.66 | 5,615.62 | 725 |
| Point pruning + workers 2 | 15,949 | 9,732.10 | 5,152.77 | 803 |
| Point pruning 2 | 14,861 | 11,460.61 | 6,275.07 | 705 |
| Baseline 2 | 15,556 | 11,239.42 | 6,279.17 | 739 |

Baseline and point-pruning means are effectively identical (15,194.5 and 15,195 records/s). The worker mean is 15,676 (+3.2%); two runs and this variability do not establish a substantial or reliable lead. All six trials verified 6,924,350 records, with every correctness discrepancy counter zero. Reports: `~/soak-results/execution-final-*`, consolidated `execution-final-summary.json`.

## Repeated live byte and scan results

| Variant (one worker throughout) | Records/s | Max ack p99 ms | Fault ack p99 ms | Peak broker RSS MiB |
|---|---:|---:|---:|---:|
| retained-1 | 14,465 | 13,918.21 | 5,992.45 | 720 |
| byte-views-1 | 15,406 | 14,352.38 | 6,606.85 | 706 |
| visible-scans-1 | 16,577 | 10,412.03 | 7,000.06 | 720 |
| visible-scans-2 | 16,152 | 11,673.60 | 6,782.98 | 752 |
| byte-views-2 | 16,968 | 9,568.26 | 6,443.01 | 713 |
| retained-2 | 15,499 | 10,854.40 | 6,582.27 | 768 |

All six runs verified 7,161,051 records with every discrepancy counter zero. Mean throughput is 14,982 records/s for the one-worker retained implementation, 16,187 for byte views (+8.0%), and 16,364.5 for byte views plus visible-version scans (+9.2% against control, +1.1% against byte views). Byte views improve in both orderings. The incremental scan difference is not repeatable enough to claim an additional broker capacity gain. Large shared slices did not increase peak RSS in these two comparisons.

Reports: `~/soak-results/execution-bytes-*`, consolidated `execution-bytes-summary.json`. The control is `blossom-execution-retained` at one worker. Candidates: `blossom-execution-byte-views` and `blossom-execution-visible-scans`.

## Probe reuse, equality indexes and work-based worker grain (rejected)

All three changes below were reverted after the live comparisons.

`4e1e9f5c` uses complete cached key-probe answers to establish a table row's cold membership. Counted durable views and incomplete witnesses retain cold support reads. The answer follows existing cold-cache updates; it does not add a stale negative membership cache.

`f3b2da3e` uses deterministic hash indexes for equality probes and promotes an index to canonical key order on its first range query. Per-key buckets preserve all distinct rows and their order. Concurrent first-time builds cannot replace a promoted ordered index with a late hash build. Tests cover old/new tick queries, multiplicities, erasure, reinsertion and concurrent promotion.

`188c3123` uses existing per-rule expression-work counters to reduce worker chunk size for expensive short batches. Cold rules retain the original row grain; prior work supplies an estimate after execution. No wall-clock measurements or program-specific identifiers affect scheduling. A small expensive retained computation starts serially, then uses workers while matching serial results and function profiles.

Seven rotated checked replay medians: visible-scan control 0.483215 s; new indexes/probe reuse with one worker 0.457125 s (-5.4%); two workers 0.454176 s; four workers 0.460207 s. The one/two difference is just 0.6%; live results must decide. Frozen candidate: `blossom-execution-hash-workers`. Raw reports: `execution-hash-replays`. Full source archive: `~/soak-results/bloom-execution-188c3123.tar.gz`.

## Repeated live index and worker results

| Variant | Records/s | Max ack p99 ms | Fault ack p99 ms | Peak RSS MiB |
|---|---:|---:|---:|---:|
| control-1 | 16,625 | 9,469.95 | 6,127.62 | 739 |
| one-1 | 15,422 | 12,877.82 | 6,488.06 | 703 |
| two-1 | 14,998 | 12,140.54 | 5,582.85 | 749 |
| two-2 | 16,763 | 11,575.30 | 6,275.07 | 742 |
| one-2 | 16,373 | 10,665.98 | 6,615.04 | 719 |
| control-2 | 15,319 | 10,993.66 | 6,471.68 | 717 |

All six trials verified 7,195,551 records with zero discrepancy counters. Means: control 15,972 records/s, new indexes/probe reuse with one worker 15,897.5, with two workers 15,880.5. No useful improvement; all new scheduling, indexes, and probe reuse were removed. The final code retains only shared byte views, point-version pruning and visible-version scans.

Final runtime source: `35c2264e`; archive `~/soak-results/bloom-execution-final-35c2264e.tar.gz`. Frozen Linux executable: `~/soak-results/blossom-execution-final`, SHA-256 `ae36b9a93abea6155bfd20924d875e4cc06c9512145e4907e44800e89bcd05ad`. Raw comparison reports: `execution-index-*`; consolidated `execution-index-summary.json`.

## Independent final Kafka comparison

The reduced implementation was rebuilt before all four runs, with no builds overlapping timed runs. Trial order was Kafka → Bloom → Bloom → Kafka. Settings and full correctness verification were identical to the measurement contract above.

| Variant | Records/s | Max ack p99 ms | Fault ack p99 ms | Peak broker RSS MiB |
|---|---:|---:|---:|---:|
| kafka-1 | 24,547 | 7,692.29 | 9,494.53 | 1,274 |
| blossom-1 | 14,707 | 13,099.01 | 6,692.86 | 756 |
| blossom-2 | 15,754 | 10,428.42 | 6,807.55 | 760 |
| kafka-2 | 24,596 | 9,404.42 | 9,248.77 | 1,275 |

Mean throughput: **Bloom 15,230.5 records/s; Kafka 24,571.5 records/s**. Bloom is **38.0% lower**, requiring another 61.3% increase to match this Kafka result. The requested performance objective is not achieved. All four runs verified **5,751,039 records** with every lost, misplaced, duplicate, present-unacknowledged and foreign counter zero. Bloom used less peak RSS and had lower fault-window p99 in these samples, but that does not establish a throughput win.

The earlier paired byte-view experiment showed an 8% mean gain. The final reduced implementation averaged only 15,230.5 records/s, so these results do **not** establish a reliable overall improvement for the final branch. Do not promote the exploratory result as a guaranteed broker speedup. Raw reports: `~/soak-results/execution-acceptance-*`; consolidated `execution-acceptance-summary.json`; combined log `execution-acceptance-final.log`.

## Status and validation

All planned trials are complete. The faster-than-Kafka objective remains open; this branch is an experiment with validated generic byte/storage changes, not a demonstrated overall performance win. Final focused parallel, retained-map, function/oracle, Kafka produce, database and tiered tests pass after removing the experiments, as does final workspace Clippy.

Validation so far: 64 value units, 30 engine units, 40 store units, 11 wire units, 10 LSM historical/crash tests, 14 function/oracle integrations, retained-map and parallel integrations, seven blob durability/crash tests, tiered and database recovery tests, and Kafka produce/idempotence/crash tests. Workspace Clippy with all targets/features and warnings denied passes, as does engine WASM compilation; subsequent index/grain changes passed focused Clippy and WASM checks. The fast workspace suite completed in two parts: all packages before syntax passed; a pre-existing formatting failure in two Kafka files was repaired in `df0f4dbd`, then syntax and all remaining packages passed. All workspace doctests passed. Stock-client Kafka, multi-process failure tests, historical storage and crash tests passed. Final workspace Clippy with all targets/features and warnings denied and Rust formatting pass. Full-tier exhaustive schedules have not been run.

The final engine also passes `cargo check -p blossom-engine --target wasm32-unknown-unknown`. No protocol logic moved out of Bloom, no FFI or workload-specific fast path was added, and acknowledgement or history semantics were not weakened.
