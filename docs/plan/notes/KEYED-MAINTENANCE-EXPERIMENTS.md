# Keyed maintenance and compact support, October 14, 2026

Baseline: store-experiments d8e49356. Pending editor files were committed separately on main as f9610d49; all other worktrees were clean.

## Direct next-state contributions

For a non-lattice framed table whose only other next-state writer is the exact generated identity copy of its upsert staging relation, the engine reads that contribution directly from staging. It no longer evaluates or applies that copy rule, or maintains a duplicate next-state support map. Recognition checks construct membership, full identity shape, absence of atom metadata, and the complete set of running next-state writers. Tables with other next-state writers retain the existing evaluator. Observations, frame guards, deletion relations, conflict validation and recovery semantics are retained. This is an incremental native maintenance operator, not full upsert deletion or arbitrary producer pipeline fusion.

## Compact schema support

Supported keyed schemas compile scalar columns into primitive words and Bytes into shared immutable references. Support is hashed by that physical key, without traversing payload vectors/tuples. The common bucket holds one canonical Row and its count; conflicting payloads promote to an ordered full-row map until the ordinary FD check rejects them. Different allocations with equal rows coalesce correctly. Exact primary-key probes read these same buckets, avoiding a second generic-key index; range and secondary indexes retain canonical order. Unsupported key schemas, lattices and tiered stores use the existing representation. Reset preserves the selected layout. This does not yet replace generic executor values or database decoding end-to-end.

## Temporary complete-engine microbenchmark

200 ticks, 64 or 1,024 keys updated each tick, sixteen-integer payload vectors, a keyed table and identity view. Compilation/input construction are excluded; complete engine maintenance is timed. Final table width is checked. Seven interleaved release trials rotate baseline, direct-next only, and direct-next plus compact support. Each uses its own Cargo target; three copied executables have distinct SHA-256 hashes. No concurrent builds or test suites during measurement.

| Keys/tick | Baseline median µs | Direct next µs | Both µs | Both time reduction |
|---:|---:|---:|---:|---:|
| 64 | 85,014 | 75,406 | 56,168 | 33.9% |
| 1,024 | 1,649,055 | 1,449,791 | 1,112,456 | 32.5% |

Head writes: 89,280 → 63,744 at 64 keys; 1,428,480 → 1,019,904 at 1,024 keys (28.6% fewer). Resident rows: 448 → 384 and 7,168 → 6,144 respectively. These figures concern the synthetic in-memory engine path, not broker throughput. Raw outputs, hashes and discarded benchmark source/fixture: /tmp/blossom-maintenance-results/.

## Verification

Twenty engine units, including randomized compact support/conflict/demotion/drain checks; seventeen planner tests, two scoped tests, five store-path tests, three Kafka produce tests, three database recovery tests, and five tiered storage tests pass. New tests check zero copy-rule writes and no duplicate resident next-state rows across reset, and oracle agreement when another next-state writer prevents fusion. Clippy all engine/integration targets denies warnings. Broker measurement is pending.

## Remaining larger changes

Full construct/pipeline fusion, packed executor/database rows, and multicore partition execution remain. The initial changes above are tested foundations, not completion of the larger architecture or evidence that Blossom is faster than Kafka.

## Identity fusion and broker check

Commit d6b88bf6 additionally aliases eligible tick-local identity chains to their physical source. The logical relations remain observable; duplicate support/index maintenance and tick clearing are omitted. Only acyclic, single-writer, non-durable, non-lattice, fact-free identity relations qualify. Mixed writers and projections retain their own stores. Scoped classification remains logical, even when clearing is omitted physically. Oracle checks include chains, negation, multiple writers, quiet ticks and reset.

The initial server pair used the same retained Kafka source, two physical cores plus SMT per broker, four producers/consumers, ten topics × twelve partitions, 1 KiB records, tiered storage, strict certification, 262,144 hot rows and 32 MiB cache. Each ran 180 seconds at max followed by 60 seconds at 2,000/s, kill at 20 seconds and restart ten seconds later. Retained d8e49356 binary: 14,017 records/s; d6b88bf6: 14,109 (+0.66%, effectively tied). Both verified every record: 2,780,794 and 2,802,923 records, zero loss, misplacement, duplication, present-unacknowledged or foreign records. First fault ack p99 7,365 → 7,000 ms; one pair is insufficient to claim a latency improvement. This broker result does not support extrapolating the large local micro gain.

A missed coverage boundary explains why the next iteration expands compact support: most derived set/view schemas default to every column as a key, so large structs and vectors were still hashed. The physical support layout now selects supported scalar/Bytes columns without assuming uniqueness. Every distinct row retains its own support within canonically ordered buckets; multiple rows per scalar key are valid for ordinary sets. All-complex keys retain generic storage; scalar-only sets with no omitted columns remain generic too. A differential fixture tests multiple vector payloads under the same scalar key, changing membership and aggregation. This remains an internal support/probe representation, not packed database decoding or a fully typed expression executor.

## Bounded read-only workers

The evaluator now optionally runs disjoint additive valuation terms on a fixed worker pool. Row state is immutable while terms run; synchronized caches may build indexes/keep hot answers. Partial heads, aggregate tuples, errors and profiling are combined in driver-group order before the coordinator applies changes. Large pure full evaluations split at their first pivot atom; delta terms retain their old/new dependency positions. Time/random/blob-producing rules remain serial. Workers receive the same evaluation-stack bound and per-function fuel contract as the engine thread. Tick state mutation, durability and release are unchanged. Deployment storage.eval_workers selects the pool; default 1 remains serial. EVAL_WORKERS exposes it in the soak script.

Seven interleaved local release trials, 20 ticks, 128 or 1,024 inputs per tick, a pure function folding 256 scalar values: serial medians 80,968 / 653,530 µs; two workers 48,509 / 408,720 (1.67× / 1.60× throughput). Outputs and activated term counts are checked. This CPU-heavy fixture excludes disk/network and is not the broker capacity result. A matched seven-trial check of synchronized caches in serial mode against d6b88bf6 is neutral: 51,544 / 993,673 versus 51,933 / 989,321 µs. An initial cache-check attempt lacked its temporary fixture and was discarded; successful measurements restored the exact fixture. Raw outputs, sources and hashes: /tmp/blossom-parallel-results/.

Worker differential checks include large batches, simultaneous changes, aggregates, negation, reset, profiling, arithmetic errors and cancellation of ghost errors between old/new join terms in separate worker groups. Recovery/tiered tests also run with two workers. Scheduling uses [Rayon's bounded ThreadPool](https://docs.rs/rayon/1.12.0/rayon/struct.ThreadPool.html); acceleration caches use [parking_lot locks](https://docs.rs/parking_lot/0.12.5/parking_lot/).

Full upsert deletion fusion, arbitrary producer pipelines and packed database/expression rows remain further work. The new worker path parallelizes read-only relational evaluation across partitions and other independent valuations; it does not create independently committing partition engines.

The expanded set/view support layout has its own seven paired complete-engine trials against d6b88bf6: at 64 keys median 51,171 → 34,690 µs (32.2% less time); at 1,024 keys 986,894 → 714,104 (27.6%). Head-write/resident-row counts are identical between these two variants, separating cheaper representation from the earlier write elimination. The first expanded 64-key trial was nearly tied (50,443 vs 49,979); all later trials improved. Medians include all seven. These are additional measured micro improvements, not percentages to add to broker capacity.
