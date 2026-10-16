# Compiled relational maintenance

Branch `compiled-maintenance`, based on `kafka-execution` at `c3afbb85`.

The earlier Kafka comparison found 15,230.5 records/s for Bloom against 24,571.5 for Kafka. The live CPU profile attributed roughly 86% of samples to the engine thread, with generic relational maintenance and data representation prominent. Expression evaluation alone accounted for about 10% inclusively. Those samples motivate compiling relational maintenance; they do not establish that compilation alone will close the throughput gap.

## Execution changes

`compiled_joins` compiles each eligible delta driver's planned join order into source-column references, exact probes, row-local equality checks, anti-joins and projections. It avoids constructing and repeatedly interpreting variable environments. Identity projections reuse source rows. Snapshot selection, signed delta consolidation, publication and retained-map capture keep their existing semantics. Expressions, aggregates and unsupported steps use the existing evaluator. These are compiled physical kernels executed by the Rust engine, not machine-code generation or a complete native compiler.

`packed_indexes` compiles equality-key layouts from schemas into scalar words and shared byte references. Unsupported layouts retain ordinary indexes. Full rows remain in each bucket, preserving distinct payloads. A range request promotes the arrangement to canonical Value ordering. Wrong-shape inserts also promote without discarding existing data. Store replacement preserves schema/configuration, including across reset.

`fuse_relations` substitutes eligible transient relational producers into their consumers and skips maintaining their intermediate stores. A producer must expose every positive-atom variable exactly once in its head, with no wildcard hiding a witness. This makes the producer row identify its complete body valuation, so removing its set boundary preserves support counts. Only single-writer, tick-scoped, nonrecursive, full-key IDB relations qualify. Durable, interface, explicitly materialized, partitioned, lattice and final-output relations remain boundaries. Consumers must use the relation positively and contain only pure relational literals; expressions, negative consumers and native constructs remain boundaries. Expansion is bounded at 128 literals; exceeding the bound leaves the original materialized producer intact.

Logical observations reconstruct fused views using full kernels. Execution plans report `fused-observation`, while program identity remains the source program digest. The physical rewrite is revalidated before execution. Fusion implies compiled relational kernels. All three options default off and are independently exposed through deployment storage settings and trace-plan CLI flags. Benchmark environment variables are `COMPILED_JOINS`, `PACKED_INDEXES`, and `FUSE_RELATIONS`.

No Kafka logic moved out of Bloom. No FFI, protocol-specific operator or weaker acknowledgement/storage contract was introduced. Bloom source is unchanged. Partition-aware concurrent mutation and native code generation are not implemented by this change.

## Validation

Differential tests compare the candidate with the existing evaluator and independent oracle, including duplicate-producing joins, simultaneous source changes, repeated variables, constants, anti-joins, wildcard substitution, self-joins, reset and one/two worker modes. Lossy projection, negative-consumer, expression-consumer and persistent-view barriers are checked. Existing planner/oracle regressions also run with the new options, including recursion and error ordering.

Arrangement tests cover composite signed/byte keys, distinct payloads, insertion/retraction, canonical range promotion, unexpected-shape promotion, concurrent lazy index construction, old/new snapshots and emptied-store configuration. Trace tests check selected-window compiled counters and option reporting.

## Measurements

Checked Kafka trace replay selects ticks 1583–1600 of `b2-1.blstrace` from `/tmp/soak-rule-trace-20261013/blossom/traces`, with three Broker nodes, one worker, 8192 retained-map rows, retained joins, lazy indexes and physical measurement disabled. Outcomes are verified against the recorded trace. Trials run pinned to CPUs `0-1,16-17`, with no overlapping builds.

Seven rotated repetitions per mode, frozen binary `~/soak-results/blossom-packed-maintenance`:

| Mode | Median selected replay time |
|---|---:|
| Interpreter | 486.98 ms |
| Compiled joins | 479.91 ms |
| Packed arrangements | 468.29 ms |
| Both | 465.22 ms |

Combined reduction: 4.47%. The combined run exercised 69,719 compiled terms and 291 packed arrangements. Raw reports are in `~/soak-results/packed-maintenance-replays` on `computer.jimmyhmiller.com`. These timings are not broker throughput measurements and do not establish a lead over Kafka.

A second seven-rotation run using `blossom-fused-maintenance` (`b89488b3`) gave interpreter 491.89 ms, compiled+packed 470.74 ms, and compiled+packed+fusion 461.23 ms. All outcomes matched. Fusion selected only `fresh_grant`, with no support writes in this window; all modes recorded 198,205 support writes. The difference between combined and fused timing therefore does not demonstrate a benefit from eliminating intermediate maintenance. Raw reports: `~/soak-results/fused-maintenance-replays`.

The current fusion proof does not reach the Kafka hot path. Important maintenance still passes through expression-bearing, projected, aggregate or persistent boundaries. Extending fusion requires proving how those boundaries preserve deduplication, support, error reachability and recovery, rather than simply inlining all rules. This remains a limitation of this implementation.

## Live broker results

Four trials in ABBA order used the same frozen binary, with the three new flags disabled for baseline and enabled for candidate. The workload and crash/durability contract are the same as `KAFKA-EXECUTION-EXPERIMENTS.md`; no builds overlapped trials.

| Trial | Records/s | Max ack p99 ms | Fault ack p99 ms | Fault consumer p99 ms | Peak broker RSS MiB |
|---|---:|---:|---:|---:|---:|
| baseline-1 | 15139 | 11780.10 | 6299.65 | 6279.17 | 738 |
| candidate-1 | 16358 | 10928.13 | 6352.90 | 9478.14 | 727 |
| candidate-2 | 15980 | 11255.81 | 5750.78 | 11034.62 | 695 |
| baseline-2 | 15958 | 11067.39 | 6717.44 | 11296.77 | 700 |

Mean baseline throughput was 15,548.5 records/s; candidate 16,169 (+3.99%). The forward pair improved 8.05%; the reverse pair improved only 0.14%. This establishes neither a large nor a reliable gain. Recovery consumer latency is variable and worsened substantially in the first pair; not every metric improved. All four runs verified 4,823,637 records with zero lost, misplaced, duplicated, present-unacknowledged or foreign records.

Reports: `~/soak-results/maintenance-live-{baseline-1,candidate-1,candidate-2,baseline-2}`; consolidated `maintenance-live-summary.json`, on `computer.jimmyhmiller.com`. Scratch databases were deleted only after verification. The earlier Kafka reference mean was 24,571.5 records/s; no fresh Kafka trials were run in this slice and no Kafka lead is claimed. The original faster-than-Kafka objective remains unmet. The implementation remains opt-in.

Frozen measured binary SHA-256: `029f02ca6832da01651d0dd97a6958f81b265f1fc74f3bd144dfb3e37c588327`.

## Enabling the implementation

Add to a deployment's existing storage section:

```toml
[storage]
compiled_joins = true
packed_indexes = true
fuse_relations = true
```

For checked replay, use `trace plan --compiled-joins --packed-indexes --fuse-relations`. Keep the workload, existing tuning and physical-measurement setting identical between comparisons. `plan` reports compiled terms and packed arrangement counts so eligibility is distinguishable from actual use.

## Final validation

Final validation: the fast workspace suite passed (983 tests passed, 75 intentionally ignored, including doctests). Workspace Clippy with all targets/features and warnings denied, engine wasm32-unknown-unknown compilation, Rust formatting and diff checks passed. Full-tier exhaustive schedules, browser/service tiers were not run. All live trial records verified after broker crashes.
