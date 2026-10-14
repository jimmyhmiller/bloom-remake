# Store experiments, October 14, 2026

Based on retained scale-bench a189454c, isolated branch `store-experiments`. Implemented and measured three candidate paths using temporary release-mode microbenchmarks. Benchmark source, fixture and scratch worktree were discarded after measurement; correctness tests remain. No production merge, push, new Kafka comparison or broker-throughput claim.

## Kept: bounded existence reads and cached witnesses

`Store::any` / `Tiered::any_carried` now use bounded `ColdTables::probe_some` on a cold miss, accounting for overlay deletions and old-version exclusions. A positive incomplete answer caches one witness; complete row probes never consume that entry as a full answer. Catch-up discards a deleted witness rather than turning it into cached absence. Complete answers are admitted only within the existing per-probe size limit. Overlay rows remain visible when the database has no version yet.

The temporary cold backend generated matching two-column rows through the real cold-table interface; this measured materialization/cache work, **not disk I/O or actual database decoding**. Each first-miss trial cleared the hot cache; warm trials reused it. Seven paired runs, alternating order, with no simultaneous build/test jobs:

| Matching rows | First misses, 100 calls: baseline / changed µs | Warm, 10,000 calls: baseline / changed µs |
|---:|---:|---:|
| 1 | 52 / 54 | 599 / 181 |
| 64 | 236 / 35 | 559 / 354 |
| 4,096 | 11,329 / 36 | 775 / 380 |
| 65,536 | 184,028 / 35 | 2,396 / 383 |

A 65,536-row first miss materializes one witness instead of all 65,536 rows. The singleton first-miss timing is essentially neutral/slightly worse; this is not a universal speedup. The backend's materialized-row counter is a stronger structural result than the sub-microsecond timing of a singleton.

Five permanent tests cover bounded first reads, repeated witnesses followed by complete probes, overlay deletion/reinsertion and old exclusions, catch-up removal, pre-version overlays, and oversized exclusion searches/cache eviction.

## Kept: identity-copy reuse and native upsert deletion evaluation

The engine already had native frame maintenance and copy plans. The actual changes are:

- `CopyPlan` recognizes an identity projection once at planning time and returns the same immutable row allocation after its atom checks, avoiding a new tuple and field clones.
- A private `UpsertDelete` plan recognizes the exact normative generated deletion join using `ConstructKind::Upsert` and verified structural checks, and evaluates its full/delta join directly. Delta staging joins old target state; delta target joins new staging state. Unsupported shapes use the general evaluator. It reuses target rows as deletion heads and validates row widths.

The helper relations still exist. This is **not** complete upsert/persistence fusion or elimination of the hundreds of relation-head updates per accepted Kafka batch. It reduces evaluation/allocation work while preserving the expansion, diagnostics, guards, conflicts and tick visibility. Both probed sides are included in row-work accounting.

The temporary full-engine program had `put(k, Vec<u64>)`, keyed table `t`, an identity view over `t`, and an upsert handler. Each trial ran 200 ticks with a fresh 16-element payload per tick, updating every key. Compilation, engine construction and input generation were excluded from timing. It exercised real engine maintenance, not an isolated map/tuple routine.

Seven interleaved ablation trials, median wall time:

| Keys per tick | Baseline µs | Identity copy only | Deletion join only | Both |
|---:|---:|---:|---:|---:|
| 1 | 1,693 | 1,666 (−1.6%) | 1,655 (−2.2%) | 1,552 (−8.3%) |
| 64 | 96,202 | 92,767 (−3.6%) | 93,969 (−2.3%) | 90,422 (−6.0%) |
| 1,024 | 1,809,055 | 1,772,764 (−2.0%) | 1,800,059 (−0.5%) | 1,752,968 (−3.1%) |

Earlier seven paired trials measured the combination 3.9–8.2% faster. A three-pair recheck after runtime width checks measured 1.9%, 3.2% and 8.4% faster at the three sizes. Singleton runs last only about 1.7 ms, so their percentages have limited precision. These are modest, synthetic improvements; isolated contributions are small and non-additive. Final edits also corrected work-counter bookkeeping and strengthened eligibility access checks; no algorithm or representation changed after those trials.

## Rejected: identity-ordered index buckets

The prototype resolved tuple equality in the primary store, pinned the canonical row allocation, and keyed secondary buckets by allocation identity. Equal independently allocated incoming tuples used the primary row's identity. Queries restored canonical value order. It changed in-memory secondary buckets only; it did not replace every support/delta structure or cache every projected key.

Complete store insert/retract loops used 1,024 rows, twenty cycles, one/four/eight indexes and singleton/64-row prefix buckets. Payloads were shared or separately allocated equal 16 KiB bytes. Read trials issued 1,000 indexed prefix probes. Five paired trials:

- With separate equal payloads and 64-row buckets, updates were 34–43% faster.
- A 64-row read with shared payloads was 812 versus 315 µs, **2.6× slower**.
- A 64-row read with separately allocated equal payloads was 29,446 versus 311 µs, **94.7× slower**: sorting by full value order at each read reintroduced expensive comparisons.
- Singleton and smaller-update cases were mixed, including regressions.

This prototype is removed. A future identity-based index would need an efficient maintained canonical order or an explicit proof that a caller does not observe order. Moving sorting onto every read is unsuitable.

## Validation and limitations

The final code passes fourteen engine unit tests; three new integration tests cover randomized composite-key upserts with guarded carry/deletes, conflict code/node/tick agreement with the oracle, and pointer-level identity-view reuse. Existing planner (17), deletion (2), scoped (2), example-corpus oracle, Kafka producer (3), database recovery (3) and tiered-storage (5) tests pass. Clippy across engine/integration targets with warnings denied and formatting checks pass.

The oracle and engine already word conflict details differently; tests compare the semantic error code, node and tick, as the existing suite does. No error-prose rewrite was included.

All measurements ran locally in optimized Rust test binaries. There was no new full Kafka soak, multiprocess suite or server replay in this task. The cold benchmark mock excludes storage I/O; the upsert fixture excludes networking, fsync and larger production state. No inference of a 50%-over-Kafka result is justified. Raw paired outputs and summaries are in `/tmp/blossom-store-results/`; temporary benchmark code is discarded.

Next useful validation before promotion: interleaved longer unrecorded Blossom trials with identical resources/durability, fully verified records, and a profile of the changed cold path under actual database state.
