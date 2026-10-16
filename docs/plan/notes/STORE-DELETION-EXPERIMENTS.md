# Store maintenance experiments, October 14, 2026

Starting point: `store-experiments` 033b9d53. This iteration measures old-version deletion probes and redundant support-map lookups. The retained changes are support-map entry reuse and old-version deletion lookup improvements. Two final matched producer pairs show a modest capacity improvement; changes remain isolated and there is no new Kafka comparator or production merge.

## Old-version deletion probes

Before: `Store::old_rows`, old-version existence and range probes filter the entire tick's deletion set, even when the present/cold side uses an indexed point lookup. Repeating N point probes over N deletions performs quadratic scan work.

The final code seeks leading-column prefixes directly in the existing ordered deletion set, avoiding duplicate indexes. Non-leading probes keep deletion buckets by requested columns, built on first use, maintained on net deletions/cancellations, and discarded when the delta is cleared/replaced or bulk-retracted. Buckets use the existing ordered row representation; no identity order or per-read resorting. The delta set is private and exposed read-only to other engine code. Point and existence reads use their matching bucket; ranges narrow by prefix and filter that bucket. This does not introduce a full ordered range index over deletions.

Temporary optimized unit benchmarks used 64/1,024/4,096 rows, two integer columns and a 1 KiB byte payload, deleting every row before timing. Query trials performed ten rounds of one old-row lookup and one old-version existence query per key, including the first index construction. Mixed trials performed ten reinsertion/deletion/query rounds, including index maintenance. Seven paired runs alternated order, with no simultaneous local builds. Both executables have distinct hashes and test inventories.

| Deleted rows | Query baseline / indexed µs | Mixed baseline / indexed µs |
|---:|---:|---:|
| 64 | 468 / 103 | 622 / 440 |
| 1,024 | 115,481 / 1,499 | 92,548 / 7,767 |
| 4,096 | 1,828,749 / 5,213 | 1,416,866 / 34,816 |

The deliberately scan-heavy case improves strongly; it does not establish a comparable gain in the producer path. Index construction and upkeep can cost more than scanning when probes are few. Temporary benchmark sources were removed; raw outputs remain under `/tmp/blossom-deleted-results/`.

Three permanent tests compare mixed support changes, indexed old reads, range bounds and bulk retractions against a fixed tick snapshot, and exercise cancellation, direct delta replacement, clear, compound equality with independent allocations, and leading-prefix row order. Existing oracle, Kafka producer, recovery and tiered tests pass, as do seventeen engine tests and Clippy.

## One support-map lookup

Before: a support change looks up the row with `entry`; when its count reaches zero, `remove(&row)` hashes/probes the entire row again. The prototype removes through the occupied entry instead. A shared private helper handles ordinary rows, lattice contributions and volatile support on tiered tables. Existing support transitions and errors remain unchanged.

The temporary store benchmark used 1,024 rows and twenty cycles of support transitions 1→0→1→2→1. Payloads were vectors of zero, sixteen or 256 integers; zero or four maintained secondary indexes. Initial row/index construction was excluded. Seven paired optimized trials, alternating order:

| Vector width | Indexes | Baseline / changed µs | Reduction |
|---:|---:|---:|---:|
| 0 | 0 | 5,593 / 5,175 | 7.5% |
| 0 | 4 | 25,772 / 24,904 | 3.4% |
| 16 | 0 | 15,619 / 13,374 | 14.4% |
| 16 | 4 | 35,730 / 33,304 | 6.8% |
| 256 | 0 | 175,466 / 141,799 | 19.2% |
| 256 | 4 | 197,686 / 164,221 | 16.9% |

An initial attempt used a shared Cargo target across worktrees: Cargo marked the baseline fresh and both copied executables were identical. Those timings were discarded. The results above use separate target directories and verified distinct executable hashes. Raw outputs and provenance are under `/tmp/blossom-support-results/`. Permanent correctness checks were also rebuilt in the support worktree's own target.

The complete-engine recheck used a keyed upsert table and identity view, 200 ticks, 64 or 1,024 keys updated per tick, and sixteen-integer payload vectors. Compilation, input construction and engine reset were excluded; complete engine maintenance was timed. Final table width and payloads were checked. Seven paired optimized trials used separate target directories and distinct executable copies, with no simultaneous local builds:

| Keys per tick | Baseline / one-lookup µs | Reduction |
|---:|---:|---:|
| 64 | 92,171 / 86,264 | 6.4% |
| 1,024 | 1,807,330 / 1,729,052 | 4.3% |

Every pair improved in both sizes, although the larger case varied between runs. This synthetic full-engine result includes the existing copy/upsert optimizations from 033b9d53. It excludes storage I/O and networking and is not a Kafka producer-throughput measurement. Temporary fixture/source were discarded. Raw results are under `/tmp/blossom-support-engine-results/`. The standalone support change is also committed as 9af70a87; the main experiment combines it with deletion indexing for final correctness checks.

## Final combined engine recheck

Seven interleaved optimized trials compared retained 033b9d53, combined support/deletion changes before leading-prefix seeking, and the final combined code. The same 200-tick upsert fixture and correctness checks described above were used; all binaries have distinct hashes and their own build targets.

| Keys per tick | Retained µs | Combined before prefix seek µs | Final µs | Final median reduction |
|---:|---:|---:|---:|---:|
| 64 | 90,558 | 85,050 | 85,358 | 5.7% |
| 1,024 | 1,764,952 | 1,678,595 | 1,681,703 | 4.7% |

The first 64-key trial of each combined variant was slower than the baseline (about 102 versus 91 ms); later trials improved. Reported medians include those trials. These are synthetic engine timings, not a broker-capacity claim. The leading-prefix revision preserves the measured engine gain while avoiding redundant prefix-index storage. Raw paired outputs and binary hashes: `/tmp/blossom-combined-engine-results/`.

## Matched producer trials

Frozen retained 033b9d53 and deletion-index executables, identical retained Kafka source; three brokers, ten topics × twelve partitions, four producers and four live consumers, 1 KiB records, strict tail certification and tiered storage. Each broker receives two physical cores plus SMT; clients receive two cores plus SMT. Hot row budget 262,144 and block cache 32 MiB. No recording; no overlapping builds. Max production runs for 180 seconds, followed by a 60-second 2,000/s phase with kill at twenty seconds and restart ten seconds later; catch-up and every-record verification follow. Trial order: retained, indexed, indexed, retained.

The initial older-binary smoke test overlapped builds and is excluded. A 5.2 GiB unused Cargo incremental cache was removed early in the first indexed trial to provide disk headroom; note this small first-trial disturbance. Sources and frozen executables are preserved. Each experiment-owned scratch database is removed only after successful complete record verification; broker metadata/stats/logs and reports are retained.

The initial deletion-index-only prototype is effectively tied in throughput: retained 13,402 / 13,268 records/s versus indexed 13,353 / 13,607. Two-run median difference is +1.1%, within the observed variation; this is not a repeatable broker-capacity gain. Peak per-broker RSS over the whole workload was 1,001 / 882 MiB retained versus 1,395 / 1,577 MiB indexed. The max-load phase itself was similar (779 / 785 versus 803 / 792 MiB), so the larger peaks were transient around restart. All four verified 10,651,448 records, zero lost, misplaced, duplicated, unacknowledged or foreign.

That memory evidence prompted the leading-prefix seek revision. Two final pairs tested the complete combined changes against retained, in final/retained then retained/final order, with the same workload and resources:

| Pair | Retained / final records/s | Change | Retained / final sampled peak broker RSS MiB | Retained / final fault ack p99 ms |
|---:|---:|---:|---:|---:|
| 1 | 12,892 / 13,889 | +7.7% | 1,036 / 1,058 | 7,066 / 7,778 |
| 2 | 13,057 / 14,125 | +8.2% | 833 / 925 | 7,107 / 6,431 |

Both matched pairs favor the final code in capacity. The final runs also exceed the earlier retained runs (13,402 / 13,268), although comparisons across those earlier runs are not the matched-pair result. Observed RSS is back near the retained range; the five-second sampler can miss brief peaks. Fault p99 moves in opposite directions, so no consistent fault-latency improvement is established. The faster max phase leaves more records to recover, so fault comparisons do not hold dataset size constant.

All eight measured trials verified **21,339,478 records**, zero lost, misplaced, duplicated, present-unacknowledged or foreign. Every benchmark/build process is terminal. Scratch databases were removed after successful verification; frozen binaries, source snapshots, build logs, client logs, stats and reports remain. Temporary benchmark sources/fixtures and both scratch worktrees were removed.

Server evidence: `~/soak-results/store-next-{retained,indexed,final}-*/`, `store-next-pairs.log`, `store-next-final-pair.log`, `store-next-final-repeat.log`, `store-next-source/` (initial prototype), and `store-final-source/` on `computer.jimmyhmiller.com`. Local copies and parsed summaries: `/tmp/blossom-producer-store-results/`. The build-overlapped smoke remains explicitly excluded.


## Reporting repair

`Soak report` emitted comma-bearing fixed-phase labels without CSV quoting, shifting fields in ordinary CSV readers. The max-capacity rows have no comma in their phase and were unaffected. Historical reports remain unchanged; analysis explicitly rejoins their known phase-label fields. The formatter now quotes phase cells and escapes embedded quotes; Java output round-trips through Python's independent CSV reader for ordinary, comma-bearing, quoted and multiline cells. The final pair uses this corrected formatter for both variants. Fix: 6116c1cb.

## Validation and scope

The final combined code passes seventeen engine tests and existing oracle/planner/deletion/scoped/example, Kafka producer, database recovery and tiered-storage tests. Clippy across engine/integration targets denies warnings; formatting and diff checks pass. Temporary benchmark code and fixtures were removed. No full simulator sweep, multiprocess test suite or new Kafka comparator was run in this iteration. Two counterbalanced final producer pairs support a roughly 8% gain on this workload. They do not establish a universal gain or the requested 50% lead over Kafka. Do not sum separate microbenchmark percentages.
