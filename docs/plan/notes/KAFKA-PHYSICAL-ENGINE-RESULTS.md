# Kafka physical engine implementation: results and remaining work

The complete redesign is **not finished**, and the measured implementation is **not at Kafka parity**.
Implementation is isolated on branch `kafka-physical-engine`, under
`.worktrees/kafka-physical-engine`. Main and the earlier analysis worktree are unchanged.
The [implementation plan](KAFKA-PHYSICAL-ENGINE-PLAN.md) records each package separately.

## Implemented

- A bounded metadata writer completes reservations only after the atomic metadata write
  and directory sync. The engine receives explicit success/failure asynchronously.
- Every persistent writer retains the directory lock through its lifetime. Shutdown
  joins workers even if the engine panics. Partial-startup listener cleanup remains an
  open resource-lifetime issue; writer exclusion is protected independently.
- Replay borrows its historical blob map instead of cloning it on every tick. Outcome,
  prefix and program-fingerprint checks remain enabled.
- Equality arrangements use deterministic hashed packed keys and borrowed probes.
- Fusion proves that a producer's output determines its source-row combination. Pure
  expression consumers can use the expanded rule. Logical observations reconstruct
  eliminated relations; explicit observation previously returned an empty physical store.
- Source-column kernels now support tuple/struct field binding, constant/repeated-variable
  patterns, comparisons and range probes. Infallible conjuncts can be scheduled separately;
  fallible guards stay whole to preserve error order.
- Kafka Fetch checks that the requested offset is below the watermark before probing
  candidate batches. The protocol and read-after-ack behavior are unchanged.
- The committer syncs payload packs once per group before writing referencing WAL records.
  This removes the normal duplicate payload in the WAL. Recovery still reads old inline
  payload records. Crash cuts test missing, surviving and torn writes between both barriers.
- LSM memtables store each key once with ordered version histories. Historical reads seek
  a visible version directly; visible memory hits avoid probing older disk tables. The
  disk format and retained history remain unchanged.

These changes are generic engine/storage mechanisms or Bloom source. There is no
Kafka-name fast path, hard-coded workload answer or application FFI implementation.
The source-column executor is not a whole-update native compiler.

## Measured artifacts

The first live candidate used source at `88802e4c` with the engine binary built after
`71830236` (the only intervening change was Bloom Fetch source). Subsequent physical
changes were tested with the binary at `5f585176`:

```
61c018e6195b4ca5011f05ff61e46cd510cb947134b681cdf6bc735ec2c78e47
~/soak-results/blossom-physical-verified
```

Built on the benchmark host with release optimization, debug information level 1 and
`RUSTFLAGS="-C force-frame-pointers=yes"`. Later commits change shutdown/lock ownership,
formatting and evidence; they are covered by targeted tests but are not represented as
part of this benchmark binary.

Historical input: `~/soak-results/gap-trace/traces/b2-1.blstrace`, BLAKE3
`5c291a417b69f262a82e2a1e6f3d2cac00493d892beb2fc9a6fbc54fb4dd713b`.
Replay uses the original program source in `~/bloom-kafka-stages`; the updated Fetch source
has a different program fingerprint and is tested by new live runs instead.

## Initial live measurements on the original NVMe

| Run | Records/s | All broker CPU µs/record | Engine CPU µs/record | Block-write bytes/record |
|---|---:|---:|---:|---:|
| First implementation | 16,363 | 179.10 | 157.63 | 7,178 |
| Updated implementation, first control | 16,394 | 172.09 | 150.81 | 7,104 |
| Fresh Kafka, first control | 21,929 | 30.04 | — | 4,223 |

The first implementation verified 1,229,892 records after a kill/restart; the updated
implementation verified 1,217,726. Both had zero lost, misplaced, duplicated,
present-unacknowledged or foreign records. Kafka verified 1,677,288.

The second updated Bloom control exhausted the original filesystem during the paced
crash/recovery phase. It was aborted and is **invalid for acceptance**. Its logs and an
`INVALID.txt` marker are retained at `~/soak-results/physical-controls-bloom-2`; its synthetic
broker stores were removed after the processes stopped. Four intermediate binaries were
preserved as gzip archives, freeing their uncompressed disk space.

Fresh interleaved controls completed with both systems' stores on `/mnt/data`, a separate
physical WD_BLACK SN7100 1TB NVMe with approximately 777 GB initially free. The earlier
filesystem is a WD_BLACK SN7100 2TB. Results from these drives are not pooled.

## Fresh acceptance comparison on the roomy NVMe

The order was Kafka, Bloom, Bloom, Kafka. Every run used three brokers, two physical
cores plus their SMT siblings per broker, the same four Java producers and four consumers,
10 topics × 12 partitions, replication factor 3, 1 KiB records, `acks=all` and idempotence.
The maximum-rate phase was 60 seconds plus outstanding acknowledgement drain. A subsequent
60-second phase at 2,000 records/s killed the busiest broker after 20 seconds and restarted
it 10 seconds later. Kafka used `flush.interval.messages=1`; Bloom retained strict WAL
certification, pack and WAL durability barriers, and one evaluation worker.

| Run | Records/s | Broker CPU µs/record | Engine CPU µs/record | Block-write bytes/record |
|---|---:|---:|---:|---:|
| Kafka 1 | 43,108 | 21.17 | — | 4,213 |
| Bloom 1 | 19,238 | 167.39 | 145.99 | 7,039 |
| Bloom 2 | 19,049 | 169.01 | 147.77 | 7,070 |
| Kafka 2 | 42,732 | 20.69 | — | 4,215 |
| **Bloom mean** | **19,143.5** | **168.20** | **146.88** | **7,054.6** |
| **Kafka mean** | **42,920** | **20.93** | **—** | **4,214.1** |

Bloom delivers **44.60%** of Kafka's throughput here; parity requires approximately
**124.2% more throughput**. Broker CPU per record is about **8.04×** Kafka's, and block
writes are **1.67×**. The original nearly-full-drive measurements substantially understated
Kafka's performance under these storage conditions. The old-drive before/after figures
therefore cannot establish a code-only speedup on this drive.

All four runs verified every acknowledged record after the fault, with zero lost,
misplaced, duplicated, present-unacknowledged or foreign records. The verified counts
were 2,847,613; 1,413,232; 1,403,425; and 2,827,594 respectively (8,491,864 total).

At the measured 146.88 aggregate engine CPU µs/record, three fully busy serial engine
threads have a conditional ceiling around 20.4k records/s. At unchanged work per record,
reaching 42.9k/s with three engine threads would require approximately 70 µs/record.
Parallel execution changes that ceiling but does not eliminate the underlying CPU work.

Raw evidence: `/mnt/data/bloom-physical-results/physical-nvme-{kafka-1,bloom-1,bloom-2,kafka-2}`
on the benchmark host. Compact counters, sampled bounds, configs, event logs and summaries
are copied into [kafka-physical-data](kafka-physical-data). The sampler exposes its actual
sample interval and endpoint error; CPU figures are normalized by acknowledged maximum-phase
records, not the later paced/catchup phases.

## Replay findings

The checked early window is ticks 1949–2044, 96 ticks. Broader fusion eliminates 53
intermediates, versus the previous single inactive view. Expanded column kernels execute
410,970 terms versus 185,356 after fusion but before the expanded kernel. The original
baseline executed 215,324 compiled terms. Support updates fall from 607,221 to 575,293:
only a 5.26% reduction, despite eliminating 53 views. That is 140.56 to 133.17 updates
per newly inserted batch (4,320 batches). The window still takes about 1.54 seconds of
engine execution. These are instrumented replay timings, not live CPU
or throughput measurements. They establish broader coverage, not a large speedup.

The four-way physical-option replay checked the same prefix and outcomes. Each timing
below is a single instrumented window, so small differences are not a reliable speedup claim.

| Inferred keys | Pipeline batches | Engine seconds | Support writes | Batch weight updates | Index row updates |
|---|---|---:|---:|---:|---:|
| off | off | 1.550 | 575,293 | 0 | 496,046 |
| off | on | 1.573 | 314,666 | 232,338 | 375,130 |
| on | off | 1.510 | 575,293 | 0 | 489,936 |
| on | on | 1.506 | 314,666 | 232,338 | 375,130 |

Pipeline batches transfer much of the counted work into batch-weight updates; the support
counter alone would give a misleading improvement. Neither option closes the measured gap,
and neither was enabled in the live acceptance runs. Full plans and commands remain in
`/mnt/data/bloom-physical-results/replay-matrix`.

Late maximum-load (84 ticks), paced-load (799 ticks) and recovery (735 ticks) windows
also pass full prefix/outcome verification with the measured candidate. Engine times were
1.468 s, 1.852 s and 1.228 s respectively. Full reports and exact commands are retained in
`/mnt/data/bloom-physical-results/checked-windows`; the compact timing record is in
`kafka-physical-data/checked-windows.json`.

## Validation

- Workspace test run: 849 passed, 75 ignored, one formatting failure in the edited Fetch
  source. Formatted the source and reran that check successfully.
- Remaining workspace packages after the failed package: 133 tests passed; system package
  checks and workspace documentation tests also completed successfully.
- Latest compiled-execution/reference/oracle and planner suite: 18 tests passed, including
  retractions, duplicates, self joins, reset, negation, recursion and error ordering.
- Latest LSM snapshot/model/paging/crash suite: 10 passed. Store unit suite: 42 passed.
- Runtime server/stream suite: 14 passed; runtime plus node-blob crash checks: 22 passed.
- Metadata completion ordering/failure tests, Fetch and immediate-read-after-ack tests pass.
- Clippy with `-D warnings` passes for changed engine, runtime, store, simulator and value
  crates; Rust formatting and diff whitespace checks pass.

## Remaining architecture

The typed physical maintenance IR, native generation covering support/index mutation,
unboxed rows, comprehensive Kafka state redesign and partition-owned execution are
unfinished. The placeholder planner/codegen/build crates remain placeholders. Existing
read-only valuation workers do not supply partition ownership. The current changes have
not met the <=100 aggregate engine CPU µs/record target.

The evidence still points to structural work: reduce stored intermediate maintenance and
specialize the state that must remain. A materialization decision must include reverse
probe costs: eliminating a tuple-destructuring view can turn an indexed partition lookup
into a scan unless the plan supplies an inverse key or compatible arrangement.
