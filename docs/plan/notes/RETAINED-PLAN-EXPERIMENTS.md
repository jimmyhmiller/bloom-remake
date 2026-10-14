# Retained outputs and non-materializing join costing, October 14, 2026

Reference engine/tooling commit: 5dcda98b. These are generic physical execution alternatives; the Kafka source is unchanged. Experimental options default off until measured against the live broker.

## What the measured plan revealed

A checked saturated interval of broker 2's retained trace (ticks 1583–1600, 18 ticks) takes 0.627 s in the initial instrumented reference run; summed maintain calls take 0.515 s. No single relation dominates. `produce_entry` takes 32.6 ms, `entry_check` 20.1 ms, `out_batch$u` 17.6 ms, `sequenced` 15.7 ms and `data_entry` 15.2 ms. These are replay elapsed attribution, not sampled CPU or broker capacity. The initial probe at ticks 50–70 was idle and is not performance evidence.

## Retained differential maps and joins

A pure, nonrecursive, nonaggregate rule may retain its output bag per first positive input row. A primary input's removal retracts the retained bag rather than interpreting its functions and recreating its values. The first prototype covers single-input rules. The extended operator updates retained bags when other positive, negated or lookup dependencies change. It preserves the ordinary telescoping old/new query order: the first dependency's retraction reads the complete previous snapshot; later terms read the primary input at its new version. Missing/evicted primary inputs are evaluated normally. Partial deltas never create a purported complete retained bag.

Only rules doing function/collection computation or generation are admitted; maintaining bags for cheap metadata joins cost more than recomputation in the broad prototype. Copy and native frame/upsert operators retain their existing paths. Time, randomness, blob creation, aggregates, recursion and full rebuilds retain ordinary evaluation. Successful primary terms are admitted only without error contributions. All other error terms use the existing valuation token consolidation, including ghost errors. Reset invalidates retained outputs. Cached rules currently run on the coordinator; other eligible evaluations can still use workers.

A global row budget charges one input and each distinct retained output, with FIFO eviction and direct deletion from its order structure. Oversized results are not admitted. This is a row budget, not a byte/RSS limit; outputs share immutable values but can keep their allocations alive. Cache keys use compiled schema layouts with borrowed lookups, while exact complete-row equality disambiguates keys sharing the physical prefix. This removes neither the head relation nor its indexes: it is a retained execution operator, not full inter-rule pipeline fusion.

Options: storage.map_cache_rows (default 0), storage.map_cache_joins (default false); replay --map-cache-rows and --map-cache-joins. Benchmark environment MAP_CACHE_ROWS/MAP_CACHE_JOINS. Reports label eligible operators retained-map and include cumulative cache hits, misses, evictions and residency since engine creation; those cache counters include the replay prefix, unlike the selected-window work counters.

## Initial evidence

Seven counterbalanced single-input trials on identical checked traffic confirm roughly 11% less replay elapsed time with the physical timers/counters disabled. Initial instrumented `produce_entry` elapsed falls 32.6 → 1.7 ms, and expression steps halve. Source/schema layouts and frozen executables are distinct. Raw results: ~/soak-results/plan-map-replays, plan-map-replays.log; initial plan-reference-max.json and plan-map-max.json. A broad join prototype halves more function work but adds enough bookkeeping to lose part of this gain (one instrumented run 0.578 s). Admission to functions/generators improves the next instrumented probe to 0.539 s. These probes select experiments; they are not acceptance comparisons.

Two matched live pairs, in reversed execution order, use 60-second max and 60-second fixed phases to fit the server's available disk. Other settings match: 10 topics × 12 partitions, four producers/consumers, 1 KiB records, three brokers, two physical cores plus SMT per broker, one evaluation worker, tiered storage, 262144 hot rows, 32 MiB block cache and strict certification. Kill at fixed-phase 20 s; restart 10 s later. Do not directly compare these rates with earlier 120/180-second trials.

| Pair | Reference records/s | Retained maps/joins records/s | Change |
|---|---:|---:|---:|
| Reference → candidate | 14,892 | 15,220 | +2.2% |
| Candidate → reference | 13,219 | 14,536 | +10.0% |

All four trials completed and verified 4,386,216 records, zero loss, misplacement, duplicates, present-unacknowledged or foreign records. Scratch databases were removed only after successful verification. This is not a substantial capacity improvement or a Kafka lead; defaults remain unchanged. Frozen source/binaries and reports are retained on the server under plan-*-source.tar, blossom-plan-* and retained-plan-*. The cache trials are terminal.

## Index costing defect and next experiment

Store::estimate used ensure_index to obtain exact fan-out for every candidate considered by greedy join ordering. Rejected alternatives therefore created permanent indexes that later updates maintained. This is separate from indexes requested by actual reads or required key checks.

The experimental lazy_indexes strategy keeps costing separate from index construction. Existing indexes provide exact cardinality; probes covering a declared key have a one-row upper estimate; other candidates use a bounded deterministic sample and singleton frequency to estimate distinct keys heuristically. Sample statistics are cached by relation size class. Traversal can be skewed by storage layout; these estimates are not an unbiased statistical guarantee. Actual reads still create exact indexes on demand. New index construction advances the cached order's estimate epoch so later planning can use its exact statistics. No sampling result changes relation contents or predicate evaluation. Tiered stores retain their observed-probe estimates.

Options: storage.lazy_indexes / replay --lazy-indexes / benchmark LAZY_INDEXES. Default false during comparison. A regression shows estimating three candidate key shapes creates no indexes, while executing one probe builds exactly that index; native map/join state and ghost-error comparisons also run with lazy costing. The instrumented checked window reduces built indexes 602 → 497 and secondary-index row updates 183,379 → 149,079 (18.7% fewer). Seven rotated trials with physical measurement off give reference median 0.611482 s, lazy costing 0.595045 s (2.7% less elapsed), and lazy costing plus retained maps/joins 0.508810 s (16.8% less elapsed). These counters do not imply equal CPU savings. Completed combined-plan comparisons follow.


## Verification and limits

Independent oracle comparisons cover simultaneous three-way changes, duplicate generators, negation, lattice lookup updates, ghost errors, actual arithmetic failures, eviction and reset. Tiered recovery/flush/compaction tests run with both operators enabled, along with two workers. A borrowed-query collision test checks exact selection under fixed and nonce hash states. A separate estimator regression checks that rejected candidates create no indexes, actual probes build exact indexes, and index construction invalidates cached join-order statistics.

This iteration does not eliminate the hundreds of maintained relation heads, compile the expression executor, replace database decoding or independently commit partitions. It does not establish a semantic throughput ceiling or achieve the requested Kafka lead. New execution strategies remain explicit experimental settings during measurement.


## Final combined broker result

Eight additional trials use the same 60/60-second configuration, source, durability and assigned cores. Capacity is the aggregate report.csv/summary phase rate, not the sum of individually rounded producer rates (which uses slightly different completion denominators).

| Comparison (execution order) | Reference records/s | Candidate records/s | Candidate change |
|---|---:|---:|---:|
| Combined serial → reference | 13,430 | 15,992 | +19.1% |
| Reference → combined serial | 15,063 | 16,186 | +7.5% |
| Combined parallel → combined serial | 16,084 | 16,862 | +4.8% |
| Combined serial → combined parallel | 15,872 | 15,628 | -1.5% |

Both matched serial comparisons improve, but the size varies substantially. Parallelism adds no repeatable further capacity gain: one pair improves and the reversed pair regresses, while sampled mean broker CPU increases from 106–107% serial to 115–122% parallel. Serial peak sampled RSS is 723/787 MiB versus reference 713/752 MiB; other serial comparison runs reach 704/771 MiB and parallel 756/761 MiB. No consistent memory or fault-latency improvement is established. Fault ack p99 for matched serial pairs: reference 6,394/6,590 ms; candidate 6,545/5,820 ms. Do not select only the favorable pair.

These eight trials verified 9,491,814 records. All twelve live runs in this iteration verified 13,878,030 records with zero loss, misplacement, duplication, present-unacknowledged or foreign records. Every build, replay and benchmark job is terminal; verified scratch databases were removed. Frozen executables, source archives, SHA-256 hashes, client/resource logs and full reports remain at ~/soak-results/{plan-*,retained-plan-*,combined-plan-*}. Local parsed results: /tmp/blossom-plan-tooling/broker-results.json. The initial replay attempt before the lazy binary finished building failed immediately with a missing executable; it produced no valid timing and was rerun after build completion.

The observed gain remains below the requested architectural improvement and far below 50% above the historical Kafka rate. No fresh Kafka comparison was run, and the shorter phases prevent direct capacity comparisons with the earlier 120/180-second runs. The retained operator avoids recomputation and preserves object identity; it does not remove intermediate head storage, most cold decoding or coordinator state updates. Costing stops rejected plans from adding indexes; necessary indexes still remain. The next larger backend change must remove those materialization/storage boundaries rather than claiming these savings will extrapolate to the target.

Implementation is committed as 4e0c03c8 on store-experiments. Strategies remain opt-in: storage.map_cache_rows = 8192, storage.map_cache_joins = true, storage.lazy_indexes = true. Generic program defaults stay unchanged because these workload-specific measurements do not establish a universal improvement. Two workers are not recommended based on this experiment.

Final checks pass: 56 base units, 25 engine units, four CLI tests, 13 runtime units; native store, planner, parallel, retained-map/oracle and runtime configuration integrations; database/tiered recovery (both alternatives enabled) and Kafka produce/crash verification. Clippy denies warnings; formatting, diff checks and WASM compilation pass. All fourteen simulator suites and the server multiprocess liveness/kafka3/raft3 suites were not rerun in this iteration. The explicit zero-arity cell lookup documentation/frontend mismatch found while preparing the regression is recorded in bloom-remake-engine-design and remains open.
