# Physical-plan inspection and checked workload comparison

The tooling inspects the value engine actually used by the broker. It does not dump the separate blossom-plan IR and imply that the engine executes it.

## Inspect a program

```sh
blossom plan examples/kafka/broker.bls --nodes b1=Broker,b2=Broker,b3=Broker --node b1 > kafka-plan.json
blossom plan --report kafka-plan.json --dot --focus sequenced:main --depth 1 > append.dot
dot -Tsvg append.dot -o append.svg
```

`--deploy deploy.toml` compiles a deployment instead. JSON identifies the program digest and node, rule dependencies, regimes, native eliminations, strata, aggregates, semantic barriers and pure-worker eligibility. Physical stores list readers, actual non-eliminated writers, resident row counts and currently built secondary indexes. Static indexes can be empty because indexes are lazy; that is not proof a join scans. The default backend materializes heads; the listed reasons are structural observations, not proofs that every boundary is necessary. Graph dashed edges mark native eliminations.

The current Kafka program compiled for three Brokers has 1,741 rule plans and 1,332 physical store slots, including empty/unused slots and guarded rules. These are not 1,741 active rules per tick. `sequenced:main` has three readers; `append_outcome:main`, `placed_at:main` and `data_proposals:main` are separate physical heads. This confirms the execution boundaries, not their CPU cost.

## Attach measured work

```sh
blossom trace plan node.blstrace --deploy deploy.toml --first 100 --last 200 --workers 1 > serial.json
blossom trace plan node.blstrace --deploy deploy.toml --first 100 --last 200 --workers 2 > parallel.json
blossom plan --compare serial.json parallel.json > comparison.json
blossom plan --report serial.json --dot --focus sequenced:main --depth 1 > measured-append.dot
```

A recorded simulation can use `--program FILE --node NAME[:ROLE] --param NAME=VALUE` instead of --deploy. Every replayed outcome is checked against the recording, including the prefix needed to establish state before --first. The window is inclusive. Empty windows, reversed bounds, zero workers, diverging outcomes and torn full traces fail. Comparison requires the same trace content hash, bounds, tick count and program digest. Candidate binaries may implement different execution strategies for that same program; workers is an immediately selectable strategy. This does not implement a fused candidate backend.

Clock-free per-rule metrics: returned probe rows, expression IR steps, counted head/support writes and evaluated queries. Optional per-rule elapsed nanoseconds cover maintain calls, including skip checks, evaluation and applying results. They are elapsed time, not CPU; worker CPU cannot be inferred by summing them. Stratum settlement, invariant/key checking, boot, replay verification and profiling overhead are outside those rule times. Replay elapsed time includes verification and profiling; it is not a broker capacity measurement.

Optional per-store metrics: probe API calls, membership changes (including native boolean membership), secondary-index row update attempts, rows traversed to build secondary indexes, bulk-retracted rows and bulk index clear operations. Probe API counts include nested API calls. Index counters concern in-memory secondary indexes; they exclude compact support buckets, deletion indexes, tiered overlays and durable indexes. Bulk clear operations are separate from per-row updates. Store snapshots describe window-end residency, not peak memory. Counters and timers start at the included window; logical work is summed only within it.

## Assess instrumentation overhead

```sh
blossom trace plan node.blstrace --deploy deploy.toml --first 100 --last 200 --no-measurement > untimed.json
blossom plan --compare untimed.json serial.json > overhead.json
```

--no-measurement disables the new physical counters and per-rule clocks. Existing logical/function profiling remains enabled in both modes. Compare logical totals for agreement and repeat wall-time measurements with counterbalanced ordering, builds and other heavy work idle. A single replay's elapsed difference is not a performance conclusion. Execution/measurement options are retained in the report.

## Choosing an architectural experiment

First inspect an expensive measured path and its materialization fan-out, store churn and semantic barriers. Account for the replacement's costs: a pipeline can introduce repeated computation, buffers or extra coordination. Use this report alongside a CPU profile; do not treat a large rule elapsed share as entirely removable. Implement a complete candidate path, preserve the recorded outcome checks, compare identical windows, then run bounded counterbalanced live broker trials with identical durability and full record verification.

Not yet measured here: allocation counts/bytes, decoded bytes, cold-probe I/O/cache behavior, durability wait time, distinct partition keys, peak residency, or executed dynamic join-order traces. Those require additional hooks in their owning subsystems; no fabricated zeros or estimated speedups substitute for them. The first version supplies actual plan structure, store operations, elapsed attribution and checked comparison rather than a predictive cost model.

Validation: an actual six-tick upsert trace is generated and replayed over a three-tick interior window using one/two workers and measurement on/off. Outcomes and logical work agree; differing windows, empty windows, zero workers and corrupted outcomes are rejected. Exact store-counter tests distinguish support-only changes, membership transitions, index build hits, row updates and bulk retraction. Engine units, CLI tests, planner/native-store/parallel differential suites, Clippy and formatting pass.


## Experimental execution strategies

Checked replay now accepts `--map-cache-rows N`, `--map-cache-joins`, and `--lazy-indexes`. Corresponding deployment storage settings select bounded retained output bags and non-materializing join cost estimates. `blossom plan --deploy` reflects the deployment's worker/storage/operator choices. Map-cache counters include the prefix since engine creation; ordinary work/timing and store counters describe the selected window. [Retained-plan experiments](RETAINED-PLAN-EXPERIMENTS.md) document eligibility, semantics and measured results. These switches select implemented strategies; they do not enable full pipeline fusion.


## Experimental shared stage storage

`storage.infer_keys = true` / replay `--infer-keys` infers compact physical keys of single-writer deterministic derived views. JSON `physical_key` describes the selected hint. It does not add language constraints or remove distinct conflicting payload rows.

`storage.pipeline_batches = true` / replay `--pipeline-batches` lets eligible single-writer non-durable stage stores adopt or merge the evaluator's consolidated output bag. Small updates maintain indexes; large batches invalidate indexes for on-demand construction. Leading-prefix equality/range probes read the stage's ordered bag directly. Persistent identity stages over non-durable, non-lattice derived sources alias their source, eliminating their own stores. Original logical views and error/aggregate/recursion barriers remain observable.

`batch_weight_updates` counts published nonzero consolidated weights. These updates replace counted support writes and are still physical work. `batch_backed` distinguishes buffered stages from eliminated aliases. Comparison JSON includes optional physical totals separately from logical support-write totals; unavailable counters remain null. Both settings default false pending measurements; result-bag adoption does not eliminate intermediate row buffers or compile rule expressions.
