# Shared pipeline storage experiments, October 14, 2026

Implementation: edee5d62 plus recovery correction a4318129 on `store-experiments`, based on c453f195. Both new settings are experimental and default false. This implements derived physical keys, fusion of result consolidation with stage storage, and persistent identity-stage elimination. It does not yet fuse expression evaluation across computed stages or remove their shared row buffers. No substantial performance gain is established by checked replay.

## What the actual producer plan permits

`produce_req → produce_head → produce_entry → entry_check → appends` uses persistent differential views. A request stays at the head while waiting for its answer; these are not tick-local intermediates. Multiple readers consume the decoded request, entries and checks. Joins, retractions, duplicate generators, runtime errors, aggregate completion and logical observations must survive any physical rewrite.

The derived-key analysis proves `head`, `produce_req` and `produce_head` are keyed by connection for a valid snapshot: `answering(c, i, seq)` has key `c`, and `queued(c, i, frame, at)` has key `(c,i)`. The conservative analysis cannot prove that `produce_entries` gives every entry a unique ordinal, so it retains more columns for that generator's output. `entry_check` retains its original physical key in this experiment. No collection member is assumed unique merely because the source collection is determined.

## Implementation

`functional_keys.rs` computes dependency closure through source keys, deterministic bindings, lookups and injective tuple/variant constructors. It requires one writer, no facts, no recursion and row-pure expressions. Channel rows, multi-writer heads and time/random/blob-creation expressions are excluded. A greedy reduction selects a conservative head key; this is not a global optimal-key search. It uses structural expression dependencies, not function-specific inverse proofs.

`storage.infer_keys` / replay `--infer-keys` selects compact physical support/cache keys. These are hints, not new schema constraints. Every distinct full row keeps independent support, even under an invalid upstream key; the ordinary key checker still reports the original language error. Regressions compare both error details and post-failure rows for conflicting carried payloads.

`storage.pipeline_batches` / replay `--pipeline-batches` changes eligible single-writer, non-durable, non-lattice, non-recursive differential/scoped stages. An empty stage adopts the evaluator's consolidated `BTreeMap<Row,i64>` directly; a populated stage merges it. This avoids copying the bag into a second counted support table. The result buffer, signed weights, old/new membership deltas, blob references and observable logical relation remain. Leading-prefix equality/range probes use the ordered result tree directly. Small updates maintain existing indexes; large membership batches invalidate indexes for on-demand reconstruction. The rebuild policy is a heuristic, not a proven optimum.

The same setting aliases persistent identity stages over in-memory, non-lattice source relations, eliminating their own row storage and index maintenance. Alias chains share the upstream canonical rows. Declared durable tables, automatically durable views and mixed views are protected because their write-drain/version lifecycle differs. The protection is computed from all declared durable sources before installing aliases, including durable lattice state kept in memory. Existing scoped identity aliases remain enabled independently.

Store counters separately report `batch_weight_updates`; these are physical updates, even when the logical `support_writes` counter is zero. Inspection identifies batch-backed stages and physical-key hints. Comparison JSON includes optional physical totals and leaves unavailable measurements null.

## Checked replay

The recorded broker workload, program, node membership, bounds and outcome checks are unchanged. Every earlier tick is replayed to establish state and every recorded outcome is checked. The measured window is ticks 1583–1600 inclusive (18 ticks); elapsed time includes replay verification and ordinary logical/function profiling. The seven corrected comparisons alternate previous/candidate order, pinned to the same assigned cores, with physical timers/counters disabled. Earlier prototype and instrumented timings are not substituted for these final medians.

| Plan | Median elapsed | Relative to previous |
|---|---:|---:|
| Previous retained maps/joins + lazy costing | 0.510176 s | baseline |
| Inferred keys + batch stages + safe identity elimination | 0.509186 s | 0.19% faster |

This is a tie. The proposed large improvement has not occurred.

The first prototype invalidated every secondary index for every changing stage and regressed about 12%. Retaining indexes for small changes and using native ordered-prefix probes recovered that regression. An intermediate timing set accidentally overlapped two instrumented replays; those timings are discarded. The corrected seven comparisons run without overlapping benchmarks or builds. Preliminary inferred-key-only trials were also tied; they are not mixed into the corrected paired result.

| Included-window metric | Previous | Candidate |
|---|---:|---:|
| Physical store slots | 1332 | 1280 |
| Window-end resident rows | 115646 | 114853 |
| Counted support writes | 198205 | 109191 |
| Batch weight updates | 0 | 76606 |
| Membership changes | 240774 | 228366 |
| Secondary-index row updates | 149079 | 116619 |
| Secondary-index build rows | 0 | 4359 |
| Expression steps | 2273548 | 2295294 |
| Probe rows | 296022 | 299300 |

Store slots include empty and inactive storage kinds. The 52 eliminated physical stores produce only a 0.7% reduction in resident rows in this window. Counted writes are partly replaced by batch updates, not eliminated. New key statistics/storage access change join order and increase expression/probe work slightly. These counters support neither equal CPU savings nor a large materialization reduction.

## Rejected recovery behavior and reporting correction

The initial persistent-identity extension checked declaration durability alone. Non-durable views can be mounted automatically as durable or mixed views. Aliasing them lets two logical relation ids drain the same underlying persistence changes. A forward-declared alias regression fails on edee5d62 at tick 3 in 0.04 seconds: base and peak rows disappear. The fix in a4318129 computes possible durable/mixed ownership and protects both sources and destinations. The regression covers repeated flushes and four crash/recovery cycles. The original bounded-memory fixture remains unchanged; a separate fixture supplies the alias cases.

Four initial Blossom broker runs and their replay results are rejected as acceptance evidence for the corrected candidate, even though full record verification found no loss in that workload. They remain archived, not silently replaced. Corrected replay and broker results use a4318129.

An earlier reporting branch labeled virtual aliases of batch sources as batch-backed. Physical-store lists/counters and execution were unaffected. The fix prioritizes elimination and has an alias-chain regression. Re-extracting an archive preserved a source mtime older than the prior Cargo fingerprint, so an incremental build missed this reporting-only fix. The corrected server build touches Rust sources after extraction, recompiles them and freezes a new executable/hash. Its rule labels and physical-store count agree: 52 more aliases, 52 fewer stores.

## Live comparison

Two corrected counterbalanced pairs compare the previous combined plan with a4318129. Rates are the canonical aggregate phase rates from `report.csv`, not sums of individually rounded client rates.

| Pair / execution order | Previous rec/s | Candidate rec/s | Candidate change |
|---|---:|---:|---:|
| Previous → candidate | 14985 | 14378 | −4.1% |
| Candidate → previous | 14585 | 14388 | −1.4% |

The candidate fails the performance criterion. It is not recommended or promoted. Measured row-storage/index reductions did not produce an end-to-end gain.

| Trial | Fault-window ack p99 ms | Mean sampled broker CPU % of one core | Peak sampled broker RSS MiB | Verified records |
|---|---:|---:|---:|---:|
| Previous 1 | 6402 | 98.9 | 745 | 1155718 |
| Candidate 1 | 6472 | 92.9 | 753 | 1094772 |
| Candidate 2 | 6009 | 96.3 | 744 | 1100284 |
| Previous 2 | 5960 | 103.1 | 741 | 1112874 |
| Kafka 1 | 13435 | 16.0 | 1295 | 1731151 |
| Kafka 2 | 8929 | 15.6 | 1276 | 1572058 |

Fresh Kafka aggregate rates are 24417 and 21789 records/s. These use the harness's `KAFKA_FSYNC=every` default (`log.flush.interval.messages=1`), with `acks=all` clients. The corrected candidate remains substantially slower; the requested 50% Kafka lead is not achieved. CPU samples include I/O waiting behavior, so the CPU percentages are not an isolated implementation-efficiency benchmark.

Each trial uses 10 topics × 12 partitions, four producers/readers, 1 KiB records, three brokers, identical assigned cores, 60-second max and fixed phases, 2000/s fixed input, `kill -9` at 20 seconds and restart after 10 seconds, catch-up and full record verification. Blossom uses strict certification, tiered storage, 262144 hot rows, 32 MiB block cache, one evaluation worker and retained-map budget 8192. Short phases cannot be directly compared with earlier 120/180-second capacity runs. Two pairs show the observed regression, not a precise population effect size.

Corrected Blossom trials verify 4463648 records, and Kafka verifies 3303209: 7766857 records with zero loss, misplacement, duplication, present-unacknowledged or foreign records. Including the four rejected prototype runs, all ten trials verify 12529257 records. Prototype record verification did not cover the view-ownership bug and does not make that prototype acceptable. Every build/replay/benchmark process is terminal. Only successfully verified scratch databases/classes were removed; reports, source archives, hashes and diagnostics remain. Server root has about 15 GiB free after cleanup.

## Validation and remaining work

Passing checks: 27 engine units; three new pipeline/key oracle/conflict tests; two retained-map tests (now exercising both settings); eight store-path tests; 17 planner tests; two parallel tests; six tiered recovery/flush/compaction tests with both settings enabled; three database, seven runtime-database/configuration and three Kafka produce/crash tests; 13 runtime units; four CLI tests; Clippy with warnings denied, formatting/diff checks and WASM compilation. Kafka simulator tests use their existing executor defaults; live trials and real checked replay exercise the new broker options. All fourteen simulator suites and multiprocess liveness/kafka3/raft3 were not rerun.

Full cross-rule fusion of computed stages remains open. The shared-bag backend and identity elimination are not that complete implementation. The current candidate still invokes the generic rule evaluator and buffers each computed stage. The tested changes are rejected as a performance fix and remain disabled in defaults on the experimental branch. It must not be described as the completed compiler/partition pipeline or as achieving the Kafka target. Any further implementation needs measured reduction of that remaining work, rather than accepting a favorable isolated stage timing.

Artifacts: `~/soak-results/plan-fusion-{source.tar,tuned-source.tar,final-source.tar.gz,*replays*,*instrumented*}`, corrected source `blossom-plan-fusion-fixed.tar.gz`, frozen `blossom-plan-fusion*` executables and SHA-256, corrected `plan-fusion-fixed-*` replay/inspection artifacts and `fusion-plan-fixed-*` live reports/logs. Parsed local results: `/tmp/blossom-plan-tooling/fusion-fixed-results.json`. The corrected measurement binary is built from a4318129. Earlier executable hashes and unsafe-prototype reports remain separate.
