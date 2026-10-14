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
