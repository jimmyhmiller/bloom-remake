# 08 — The Hydro Project: Hydroflow/DFIR, the Datalog Frontend, Hydroflow+/Hydro, Flo, and the Compiler/Runtime

Research report for **bloom-remake**. Scope: the Hydro project, the BOOM group's current successor to Bloom/Bud/Dedalus. It covers the vision paper, the Hydroflow model, the DFIR runtime and compiler, the Datalog frontend, Hydroflow+/Hydro, the formal semantics (Flo and Gyatso), the CALM/CRDT query model, the protocol-rewriting work, the performance claims, and what we should adopt.

Method: I read the papers in full (text extracted from the PDFs) and read the code. The main code source is a clone of `github.com/hydro-project/hydro` at commit `9e2a120` (2026-09-24). I also read tag `hydroflow_lang-v0.10.0` (Nov 2024, the last release with the Datalog frontend in-tree), `github.com/hydro-project/dfir-datalog` (the frontend's current home), `github.com/rithvikp/autocomp` (the Dedalus Paxos/2PC programs from the SIGMOD'24 paper) and `github.com/hydro-project/hydro-optimize`. Every claim below cites its source. Where I could not find something, the text says so.

---

## 0. Executive summary (for implementers)

1. **The execution model is the one from Dedalus and Bloom, and Hydro kept it.** Each node is a single-threaded *transducer* with a local logical clock. Each tick it takes a batch of inputs, runs the local program to fixpoint, sends asynchronous outputs, and advances the clock. Across ticks, state exists only if it is explicitly persisted (`'static`, `persist`, `defer_tick`). Messages between nodes are asynchronous, can be delayed arbitrarily, and are not ordered.
2. **The engine is a compiled dataflow, not an interpreter.** The team called Bud "interpreted" and Hydroflow "compiled". The DFIR compiler builds a flat operator graph and splits it into *in-out trees*: pull inputs feed a single "pivot" operator, which pushes to the outputs. Each tree becomes monomorphized Rust that rustc/LLVM inlines. As of 2026 the runtime scheduler is gone. The compiler emits one async closure per tick that runs the subgraphs **once each, in topological order**. Intra-tick buffers are arena-allocated `Vec`s, and `defer_tick` buffers are double-buffered. The team reports a **35–50% Paxos throughput gain** from this change alone.
3. **The big semantic departure is recursion.** Current DFIR **rejects intra-tick cycles**; iteration has to go through `defer_tick` (a new tick) or explicit `loop { }` blocks. Older Hydroflow (≤0.10) did support in-tick recursive cycles and stratified negation, using a strata scheduler. A Datalog language needs recursion inside a timestep, so this is exactly where our design must differ from current DFIR.
4. **Lattices (Bloom^L) are first class.** Hydro has a `lattices` crate: a `Merge` trait whose merge returns whether the value changed, plus bottom/top, morphisms and bimorphisms. DFIR has lattice operators: `lattice_fold`, `lattice_reduce`, `state`, `lattice_bimorphism`.
5. **CALM became a type system.** Hydro's live collections carry boundedness (`Bounded`/`Unbounded`, plus `Monotonic` on singletons), ordering (`TotalOrder`/`NoOrder`) and retries (`ExactlyOnce`/`AtLeastOnce`) in their types. APIs that would be nondeterministic need an explicit `nondet!` guard with a written justification. Aggregations over unordered or duplicated streams need commutativity or idempotence proofs. The formal basis is **Flo** (POPL'25): *eager execution* plus *streaming progress*.
6. **What Hydro dropped:** the declarative logic language as the main surface. The team's stated non-goal is a new language, so Hydro is embedded in Rust through staging. The Datalog frontend was moved out of the main repo as experimental in 2025. There is no lineage-driven fault injection (Molly); the substitute is a deterministic simulator that explores the marked nondeterminism points.
7. **Adopt:** the transducer tick loop; the DFIR-style compiled push/pull dataflow; symmetric hash joins with persistent half-join state; the persist/delta algebra for incrementalizing Dedalus persistence; lattice operators; stream-type markers as our CALM analysis; explicit channel fault models; clusters with member IDs; a deterministic simulator; and the SIGMOD'24 decoupling/partitioning rewrites. **Diverge** by keeping in-tick recursion with semi-naive evaluation, and by making Datalog the primary surface language, not an add-on.

---

## 1. Genealogy and timeline

| Date | Artifact | What it is |
|---|---|---|
| 2010–2012 | Bloom/Bud (Ruby DSL), Dedalus, Bloom^L | Predecessors: BOOM, CALM, lattices. |
| Jan 2021 | **"New Directions in Cloud Programming"**, Cheung, Crooks, Hellerstein, Milano, CIDR'21 ([pdf](https://hydro.run/papers/new-directions.pdf), [arXiv 2101.01159](https://arxiv.org/abs/2101.01159)) | The Hydro vision: PACT facets; the stack Hydraulic → HydroLogic → Hydrolysis → Hydroflow. |
| Aug 2021 | **"Hydroflow: A Model and Runtime for Distributed Systems Programming"**, Mingwei Samuel (MS report UCB/EECS-2021-201, advisors Cheung and Hellerstein) ([pdf](https://hydro.run/papers/hydroflow-thesis.pdf)) | A lattice-flow model that unifies dataflow and reactive programming, with delta and cumulative edges. |
| Oct 2021 / Feb 2022 | Design docs: architecture; time and strata (`design_docs/` in the repo) | Two-layer "scheduled + compiled" architecture; stratification added so Dedalus programs with negation can run. |
| 2022–2024 | Hydroflow surface syntax (`hydroflow_syntax!`), `hydroflow_datalog` | Strata scheduler; Datalog frontend used for the SIGMOD'24 Paxos/2PC work. |
| Jun 2023 | ApPLIED'23 "Initial Steps Toward a Compiler for Distributed Programs" ([pdf](https://hydro.run/papers/joe-applied-2023.pdf)) | Hand-applied transformations; shopping-cart lattices. |
| Oct 2022 / 2023 | "Keep CALM and CRDT On", VLDB'23 ([arXiv 2210.12605](https://arxiv.org/abs/2210.12605)) | A monotone query model for CRDTs. |
| Feb 2024 | Chu et al., "Optimizing Distributed Protocols with Query Rewrites", SIGMOD'24 ([pdf](https://hydro.run/papers/david-sigmod-2024.pdf), [TR arXiv 2404.01593](https://arxiv.org/abs/2404.01593)) | Decoupling and partitioning rewrites on Dedalus, compiled to Hydroflow. |
| 2024 | Hydroflow+ / "Suki" (CP'24 extended abstract, [arXiv 2406.14733](https://arxiv.org/abs/2406.14733)) | Staged, choreographic Rust dataflow. |
| Dec 2024 (v0.11) | Renames: Hydroflow → **DFIR** (`dfir_lang`, `dfir_rs`); Hydroflow+ → **Hydro** (`hydro_lang`) | Changelogs. |
| 2025 | **Flo**, POPL'25 ([arXiv 2411.08274](https://arxiv.org/abs/2411.08274)); Laddad PhD thesis ([EECS-2025-85](http://www2.eecs.berkeley.edu/Pubs/TechRpts/2025/EECS-2025-85.pdf)) | Formal semantics; Gyatso (distributed Flo); stageleft; e-graph incrementalization. |
| Jul 2025 (v0.14) | "Move datalog from repo" (dfir_rs CHANGELOG, issue #1809) | Datalog frontend now at [github.com/hydro-project/dfir-datalog](https://github.com/hydro-project/dfir-datalog). |
| 2026 (v0.16–0.17) | Inline codegen replaces the scheduled runtime; `DelayType::Stratum` removed; `loop {}` scopes re-added | `dfir_lang/CHANGELOG.md`. |

**Naming note for readers of older papers.** "Hydroflow" in 2021–2024 papers is today's DFIR. "Hydroflow+", "HF+" and "Suki" are today's Hydro (`hydro_lang`). "HydroLogic", "Hydraulic" and "Hydrolysis" are vision-paper layers. Hydrolysis survives as the compilation phase from Hydro to DFIR. HydroLogic was never built as a standalone language; the Hydro Rust API took its place.

---

## 2. "New Directions in Cloud Programming" (CIDR'21): PACT and HydroLogic

Source: [new-directions.pdf](https://hydro.run/papers/new-directions.pdf).

### 2.1 PACT facets
- **P — Program semantics.** "Lift and Support": lift familiar code (actors, futures, MPI, sequential code) into a declarative IR (HydroLogic) by verified lifting, and keep whatever cannot be lifted as UDFs.
- **A — Availability.** Declarative spec "available in the face of *f* independent failures across failure domains" (VM/rack/DC/AZ), for example `availability: default: { domain = AZ, failures = 2 }`.
- **C — Consistency.** Consistency is declared per handler. It can be history-based (serializable, linearizable, causal, …) or an application invariant (for example `vaccine_count >= 0`). The paper names three enforcement mechanisms:
  1. prove no enforcement is needed (monotonicity or invariant confluence);
  2. wrap state in lattice metadata (the Cloudburst/Hydrocache approach);
  3. use heavyweight coordination.
  
  It also names two further problems: "Metaconsistency" (checking consistency across composition paths) and "consistency placement" (for example Conway-style sealing moved to the client).
- **T — Targets for optimization.** Latency, cost and GPU requirements, for example `target: default: { latency = 100ms, cost = 0.01units }`. Deployment is posed as integer programming over machine types.

### 2.2 HydroLogic semantics (the parts relevant to us)
- **Event loop = Bloom's transducer model.** "Each iteration ('tick') of the loop uses the developer's program specification to compute new results from the snapshot, and atomically updates state at the end of the tick. All computation within the tick is done to fixpoint. The snapshot and fixpoint semantics together ensure that the results of a tick are independent of the order in which statements appear in the program."
- **Queries** are named views over lattices (relations included). Several queries with the same name are merged (a union, as in Datalog). If a query shares its name with a data variable, the query implicitly replaces that variable at the end of the tick; this mutation is monotone iff the query is.
- **Mutations** are deferred to the end of the tick. There are three kinds: lattice `merge` (monotone); `:=` (arbitrary and non-monotone); and the query-named replacement just described.
- **Handlers** (`on name(args):`) are sugar that maps statements over a mailbox for the current tick. Each handler has an implicit `<response>` mailbox.
- **Send** is an asynchronous merge into a mailbox. It is not visible in the current tick and may be "delayed an unbounded number of ticks, appearing non-deterministically in the specified mailbox at any later tick".
- **UDFs** are black boxes, possibly stateful, that "cannot access HydroLogic variables". "Each UDF is invoked once per input per tick (memoized by the runtime), in arbitrary order." Compare Bud, where Ruby blocks may run an *arbitrary* number of times during fixpoint (bud `docs/ruby_hooks.md`).
- Footnote: "HydroLogic supports recursion and non-monotonic operations (with stratified negation) for both relations and lattices. These features are based on Bloom^L."

Running example (verbatim, Figure 3):
```
class Person: (pid: int, country: string,
    contacts: Set(&Person), covid: bool, vaccinated: bool,
    key=pid, partition=country)
table people: Person
var vaccine_count: int

on add_person(pid: int):
    people.merge(Person(pid)) # monotonic mutation
    return OK

on add_contact(p: Person, p1: Person):
    p.contacts.merge(p1) # monotonic mutation
    p1.contacts.merge(p) # monotonic mutation
    return OK

query transitive(p: Person, p1: Person): # monotonic query
    {(p, p1) for p in people for p1 in p.contacts}
    {(p, p2) for (p, p1) in transitive for p2 in p1.contacts}

on trace(p: Person):
    return (p2 for (p, p2) in transitive(p, _)

on diagnosed(pid: int):
    people[pid].covid.merge(true) # monotonic mutation
    send alert(p: Person) {p for p in trace(pid)}

from covid_xmission_model import covid_predict
on likelihood(pid: int):
    return covid_predict(people[pid])

on vaccinate(pid: int, consistency={serializable;
        vaccine_count >= 0; people.has_key(pid)}):
    people[pid].vaccinated.merge(True) # monotonic mutation
    vaccine_count := vaccine_count - 1 # NON-monotonic mutation
    return OK

availability:
    default: { domain = AZ, failures = 2 }
    likelihood: { domain = AZ, failures = 1 }
target:
    default: { latency = 100ms, cost = 0.01units }
    likelihood: { processor = GPU, cost = 0.1units }
```
The paper argues that `vaccinate` can stay serializable without making the other handlers strong. It is the only handler that touches `vaccine_count`, and every access to `people` is monotone.

### 2.3 Goals set for the Hydroflow IR (§8)
- Unify dataflow, lattices and reactive programming. For example, the output of COUNT must pipeline the way a set does.
- **Monotonicity typechecking**: "an explicit monotone type modifier, and a compiler that can typecheck monotonicity."
- **Flows beyond collections**: operators must be able to view their inputs "differentially or all-at-once", with clear semantics either way.
- Copy efficiency through Rust ownership, following Timely Dataflow.
- Execution "within a transducer network", with all state thread-local and "no locks, atomics, or other coordination" (the Anna design).

Appendix A lifts actors (with blocking receive modelled by a status variable across ticks), promises/futures (a condition variable across ticks) and MPI collectives into HydroLogic. The working code was at `github.com/hydro-project/cidr2021`; I did not open that repo.

---

## 3. The Hydroflow model (Samuel, 2021 tech report)

Source: [hydroflow-thesis.pdf](https://hydro.run/papers/hydroflow-thesis.pdf). This is the "Hydroflow: A Model and Runtime…" paper. The implementation it describes, a Rust-trait/`Op` pipeline library, was **replaced** by the surface-syntax compiler in §4. The concepts carried forward.

- **Lattice definitions.** ACI merge ⊔; partial order a ⊑ b ≡ a ⊔ b = b; bottom and top.
- **Morphism:** f(a ⊔ b) = f(a) ⊔ f(b). Morphisms are *differentially computable*: z' = z ⊔ f(δ).
- **Monotone function:** a ⊑ b ⇒ f(a) ⊑ f(b).
- **"Monotone tricky" (MTT) functions** are monotone but not morphisms. Example: set cardinality, since card({0,1}) + card({1,2}) ≠ card({0,1,2}). They need the *whole* value, which the paper calls reactive, whole-value recomputation.
- **Split binary morphism:** f(a ⊔ δa, b) = f(a,b) ⊔ f(δa, b), and symmetrically in b. Computing it differentially **is the symmetric hash join** (Fig. 4). Later docs call this a *lattice bimorphism*.
- **Edge types.**
  - A *delta edge* carries arbitrary lattice elements, which ideally are small and non-redundant.
  - A *cumulative edge* carries a sequence in which each element dominates the previous one.
  - A `StateMergeOp` turns deltas into cumulatives. It also emits minimized deltas: for sets, δx' = δx \ x.
  - Cumulative edges may **not** cross node boundaries, because the network reorders. To ship one, turn it into deltas, send those, and re-merge on the receiver.
- **Top-stoning (⊤-stoning).** Once an edge reaches ⊤, upstream operators can be removed or garbage-collected. Example: a `Max<bool>` vote-threshold pipeline.
- **Non-monotonicity as taint.** Operators that make non-monotone observations are "tainted", and the taint spreads downstream. Tainted operators are Bloom's "points of order", resolved by coordination, a single node, or "memories, guesses and apologies".
- **Cycles.** A monotone cycle needs no fixpoint guarantee. Unbounded monotone cycles (like Datalog's "unsafe rules") are the programmer's problem.
- **Lattice type vs. representation.** Types are separated from physical representations: `Union<T>` can be backed by a HashSet, BTreeSet, Vec batch, `[T;N]`, `Single<T>` or `Option<T>`. A zero-cost `Hide<Delta|Cumulative, LatRepr>` wrapper blocks non-monotone access at compile time.
- **The KVS lesson (§5.1).** "One-off reads of monotonically-growing state are simply not monotonic". A read joined against growing state behaves as a subscription. Read/write races are the canonical non-monotone point. Later Hydro solved this with snapshots, `nondet!` and `atomic`.

---

## 4. DFIR (Hydroflow) runtime and compiler

Sources: `dfir_lang/src/graph/**`, `dfir_rs/src/**`, `docs/docs/dfir/**` at commit `9e2a120`; tag `hydroflow_lang-v0.10.0` for the older scheduler; design docs `2021-10_architecture_design_doc.md` and `2022-02_time_strata_design_doc.md`.

### 4.1 Process model: "The Life and Times of a DFIR Process"
Quoted from `docs/dfir/concepts/life_and_times.md`:
1. "Given events and messages buffered from the operating system, ingest a batch of data items and deliver them to the appropriate `source_xxx` operators."
2. "Run the DFIR spec. If the spec has cycles, continue executing it until it reaches a 'fixpoint' on the current batch."
3. "Once the spec reaches fixpoint and messages have all been sent, advance the local clock and then start the next tick."

DFIR time (from `distributed_time.md`) differs from Lamport time in three ways:
- **batched events**: one tick ingests many events;
- a **required fixpoint** between events;
- **consecutive ticks** that never skip.

Lamport and vector clocks are left to user code; `examples/lamport_clock` and `examples/vector_clock` exist in v0.10.

Runtime API (the `Dfir` type):
- `run_tick()` runs one tick and returns whether any work was done.
- `run_available()` keeps running ticks while work is immediately available: external input has arrived, or a non-lazy `defer_tick` holds data.
- `run().await` runs forever, sleeping on wakers.
- `context.current_tick()` is available inside closures.

**Ticks are lazy:** no input and no pending deferred data means no tick.

### 4.2 Surface syntax
The grammar comes from `dfir_lang/src/parse.rs`.
- **Statements:** `use …;` | `name = pipeline;` | `pipeline;` | `loop [ident] { statements };`
- **Pipelines:** `a -> b -> c`, parenthesized sub-pipelines, and named references. Order of statements does not matter; forward references are allowed.
- **Ports:** input ports as a prefix, `-> [0]my_join`, `[pos]diff`, `[neg]diff`, `[input]gate`, `[signal]gate`, `[build]`/`[probe]`; output ports as a suffix, `my_tee[print]`, `my_state[items]`, `my_state[state]`, `my_partition[fizz]`, `my_demux[Square]`. Variadic operators (`union`, `tee`) accept arbitrary port names.
- **Generics:** persistence lifetimes and types, in that order: `join::<'static, 'tick>()`, `fold_keyed::<'static, K, V>(…)`, `_lattice_join_fused_join::<'tick, Min<_>, Max<_>>()`. With one persistence argument, it applies to both inputs.
- **Pseudo-operators** (2026): `handoff()` forces a subgraph boundary and gives a `Vec` buffer; `singleton()` and `optional()` give an `Option` slot that panics if it receives more than one item.
- **References** into those buffers from closures: `#var` (`&T`), `#mut var` (`&mut T`), and `#{N} var` / `#{N} mut var` for ordered "access groups". Groups run in ascending order, enforced by partitioning barriers. The rules: ambiguous multiple `#mut` is an error, and mixing `#var` with `#mut var` in one group is an error. `iter_ref(#buf)` iterates a handoff without copying.
- **Context:** closures can use a `context` object (`current_tick()`, wakers, and so on).

### 4.3 Operator catalogue (current `dfir_lang/src/graph/ops`)
Persistence arguments are `'tick` (the default) or `'static`. Unless noted, operators are streaming.

| Category | Operators and semantics |
|---|---|
| Sources | `source_iter(iter)`: emits everything on the **first tick only**. `source_stream(rx)`, `source_stream_serde(rx)` → `(T, SocketAddr)`, `source_stdin()`, `source_file(path)`, `source_json(path)`, `source_interval(dur)` (emits `()`, first immediately), `initialize()` (one `()` on the first tick), `spin()` (a unit every tick, and it always schedules the next tick), `iter_ref(#h)`. External-input sources are `is_external_input: true`. |
| Sinks | `for_each(f)`, `dest_sink(sink)` (async `Sink`), `dest_sink_serde(sink)` (takes `(T, addr)`), `dest_file(path, append)`, `null()`. |
| Map/filter | `map`, `filter`, `filter_map`, `flat_map`, `flatten`, `flat_map_stream_blocking`, `flatten_stream_blocking`, `inspect`, `identity::<T>()`, `enumerate::<'tick\|'static>()` (the index resets each tick under `'tick`), `unzip`, `_counter(tag, dur)`, `assert(pred)`, `assert_eq([..])` (remembers its position across ticks). |
| Multi-in | `union()` (any interleaving); `chain()` (all of `[0]` before `[1]`); `chain_first_n(n)`; `zip` and `zip_longest` (per tick; `'static` may buffer without bound). |
| Joins | `join::<L,R>()`: equijoin on `(K,V1)`,`(K,V2)` → `(K,(V1,V2))` with **set** semantics (dedups inputs). `join_multiset()`: multiset. `join_multiset_half()`: build side accumulated, probe side streamed, and probe order is preserved. `cross_join()` (set), `cross_join_multiset()`. `cross_singleton()`: pairs each item with the first element of the `[single]` input and short-circuits if it is empty. `join_fused(Fold/Reduce/FoldFrom, …)`, `join_fused_lhs`, `join_fused_rhs`: aggregation fused into the join state. `_lattice_join_fused_join::<Lat1, Lat2>()`. |
| Negation | `difference::<L,R>()`: `[pos]` minus `[neg]`; set semantics on `neg` only, and duplicates in `pos` pass through. `anti_join::<L,R>()`: `(K,T)` pos against `K` neg, multiset on pos. |
| Aggregation | `fold(init, f)`, `reduce(f)`, `fold_keyed(init, f)` ("GROUP BY"), `reduce_keyed(f)`, `fold_no_replay`, `reduce_no_replay` (do not re-emit the accumulator on ticks with no new input), `scan(init, f)` (running state; returning `None` terminates), `scan_async_blocking`, `sort()`, `sort_by_key(f)` (per tick), `unique::<'tick\|'static>()`, `multiset_delta()` (removes elements that were present in the previous tick's input multiset). |
| Lattices | `lattice_fold(init)` = `fold(init, Merge::merge)`; `lattice_reduce()`; `state::<Lat>()` / `state_by::<Lat>(map, factory)`, whose outputs are `[items]` (only inputs that **changed** the state, i.e. deltas) and `[state]` (the accumulated value); `lattice_bimorphism(func, #lhs, #rhs)`; `_lattice_fold_batch`. |
| Persistence and time | `persist::<'static>()`: replays **all history** every tick. `defer_tick()`: releases buffered items at the next time boundary and makes that tick fire. `defer_tick_lazy()`: the same without forcing a tick. `defer_signal()`: releases `[input]` when `[signal]` is non-empty. Older versions also had `persist_mut` / `persist_mut_keyed` (input `Persist(x)`/`Delete(x)` applied in order) and `next_stratum()`. |
| Multi-out | `tee()`, `partition(\|x, [a,b,c]\| …)` (named) or `partition(\|x, n\| idx)` (indexed), `demux_enum::<Enum>()`. |
| Async | `resolve_futures`, `resolve_futures_ordered`, `resolve_futures_blocking`, `resolve_futures_blocking_ordered`. |
| Loops (windowing) | Inside `loop { }`: `batch()` (fires the loop if non-empty), `batch_lazy()` (never fires it), `batch_eager()` (always fires; root loops only); `all_iterations()` (un-windowing). |

**Replay semantics of `'static`.** This is critical. From `dfir_rs/tests/surface_join.rs`, test `replay_static`: `join::<'static,'static>()` over constant inputs outputs the **complete join result on every tick that runs**. It does not output only new matches. `static_static` shows the output growing across ticks. In `static_tick`, the `'static` side is remembered and joined with each tick's `'tick` side.

The codegen (`ops/join.rs`) keeps a `HalfJoinState` per side. `'tick` sides are `clear()`ed at tick end. `symmetric_hash_join(..., is_new_tick=true)` first drains both inputs into state and then iterates all matches.

This is Dedalus persistence taken literally: a persisted fact exists at every timestep, so derivations from it recur. It is also a performance hazard, handled in §4.8 and §10.

### 4.4 Compiler pipeline
1. **Parse**, then **flat graph** (`FlatGraphBuilder`). This step resolves names and forward references and checks port arity against each operator's `OperatorConstraints`:
   - `hard_range_inn` / `soft_range_inn` and the `_out` equivalents;
   - `persistence_args` and `type_args`;
   - `ports_inn` / `ports_out` (fixed or variadic);
   - `input_delaytype_fn(port) -> Option<DelayType>`;
   - `flo_type` (`Source`, `Windowing`, `WindowingLazy`, `WindowingEager`, `Unwindowing`);
   - `write_fn`.
   
   The flat graph is followed by `eliminate_extra_unions_tees`.
2. **Partition into in-out trees** (`flat_to_partitioned.rs`, `docs/dfir/architecture/in-out_trees.md`). An in-out tree is an in-tree (pull) joined to an out-tree (push) at a single root, the "pivot". Pull composes like Rust iterators, including fan-in (`chain`, `zip`). Push handles fan-out without buffering, since `tee` just calls both outputs. A handoff buffer sits wherever a push feeds a pull.
   
   The v0.10 greedy algorithm is in `find_subgraph_unionfind` and `can_connect_colorize`. Every edge starts as a candidate handoff. Nodes are coloured Pull (multi-in), Push (multi-out), Comp (the pivot: multi-in and multi-out) or Hoff. 1-in/1-out nodes take the colour of a neighbour. Edges are merged with a union-find, repeating until no progress. The merge rules:
   - Pull→Pull, Pull→Comp, Pull→Push, Comp→Push and Push→Push may merge.
   - Comp→Pull, Push→Pull and Push→Comp may not; they become handoffs.
   - An edge is never merged if that would put a barrier-crossing pair into one subgraph.
   
   The 2026 version (`SubgraphMerge`) partitions and topologically sorts online, and "ensure[s] that no subgraphs are merged that could create a subgraph-level cycle". This matters because reference edges do not obey the in-out-tree property.
3. **Codegen.** Each operator's `write_fn` returns four pieces:
   - `write_prologue`: runs once, sets up state and channels;
   - `write_iterator`: the pull `Stream` or push `Sink` expression, chained;
   - `write_iterator_after`: flushes after the pipeline;
   - `write_tick_end`: resets `'tick` state.
   
   Pull and push are the custom `dfir_pipes` traits. `PullStep::{Ready(item,meta), Pending, Ended}` uses type-level `CanPend`/`CanEnd` toggles (`Yes`, and `No = Infallible`), so impossible branches compile away.

### 4.5 Scheduling, old: stratified scheduled runtime (≤ v0.16)
From `hydroflow_lang-v0.10.0/.../flat_to_partitioned.rs` and `hydroflow/src/scheduled/graph.rs`.
- **Delay types on operator inputs:**
  - `Stratum`: blocking. Used by `difference[neg]`, `anti_join[neg]`, `fold`, `reduce`, `fold_keyed`, `sort`, `persist_mut`, `next_stratum`, and singleton references.
  - `MonotoneAccum`: `lattice_fold`, where "cycles are actually fine".
  - `Tick`: `defer_tick`.
  - `TickLazy`: `defer_tick_lazy`.
- **Stratification** (`find_subgraph_strata`):
  1. Build a graph of subgraphs, ignoring Tick/TickLazy edges.
  2. Topologically sort its SCCs.
  3. Give each subgraph `stratum = max over preds(pred.stratum + [edge is a Stratum barrier])`, or 0 with no predecessors.
  4. A `Stratum` edge with `dst_stratum <= src_stratum` is an error: "Negative edge creates a negative cycle which must be broken with a `defer_tick()` operator." The source itself says this check is insufficient (issue #1115).
  5. A tick edge that points forward (`src_stratum <= dst_stratum`), or any lazy tick edge, gets an extra identity subgraph at `max_stratum + 1`, so its data lands in the next tick.
  6. External-input operators are moved into stratum-0 subgraphs of their own.
- **Runtime.**
  - There is a FIFO queue per stratum. `run_stratum()` pops subgraphs, runs each, and schedules any successors whose handoff is non-empty.
  - Scheduling a successor in an **earlier** stratum from a non-lazy subgraph sets `can_start_tick`.
  - `next_stratum()` moves to the next non-empty stratum. At the end of the tick it resets `'tick` state, increments the tick, receives external events, and continues only if `can_start_tick`.
  - Within a stratum, subgraphs re-run until the stratum is quiescent, which is **how in-tick recursive cycles reached fixpoint.**
- The Feb 2022 design doc explains why this was built: "Important to note is that positive (monotonic) loops are easy… We don't even have a way to compute a non-monotonic operation as simple as set difference". Stratification was chosen over retractions à la Timely. The doc also observes that Dedalus time "ensures this will be locally stratified": loops through negation are allowed when they pass through `defer_tick`.

### 4.6 Scheduling, new: inline DAG codegen (2026)
From `dfir_lang/CHANGELOG.md` 0.16–0.17 and `meta_graph.rs::as_code`.
- "Emit this graph as runnable Rust source code tokens that execute inline. Generates a flat `async move |df: &mut Context|` closure where subgraph blocks are inlined in topological order, using local `Vec<T>` buffers instead of runtime handoffs. Each call to the closure runs one tick."
- "With the inline DAG codegen, each subgraph runs exactly once per tick."
- "replace stratification with plain topo sort, remove next_stratum"; "remove `DelayType::Stratum`, `MonotoneAccum`". Blocking operators no longer need barriers: the topological order guarantees their producers have finished.
- Intra-tick buffers use `bumpalo`, reset between ticks, which also allows `&` references inside handoffs. `defer_tick` handoffs are double-buffered and captured across ticks.
- **Intra-tick cycles are compile errors:** "Cyclical dataflow within a tick is not supported. Use `defer_tick()` or `defer_tick_lazy()` to break the cycle across ticks."
- **`loop { }` scopes.**
  - A root-level loop becomes an `if` gate, firing at most once per tick.
  - A nested loop becomes a `while` gate that iterates to fixpoint, conditioned on the entry handoff having data or the non-lazy defer back-buffer being non-empty.
  - Inside a nested loop, `defer_tick()` means "next iteration" (`DelayType::Loop`).
  - Loop bodies must be DAGs apart from `defer_tick` back-edges. Windowing operators may appear only at loop entry and un-windowing only at loop exit. Sources must be at root level.
- **Result:** "switch all codegen paths to inline DFIR execution … **35-50% throughput improvement on the Paxos benchmark** — zero application code changes."
- A related bug is informative. After sorting by stratum only, `defer_tick_lazy` in a cycle delayed data by *two* ticks, because consumers ran before producers inside a stratum. They switched to a full topological sort of subgraphs.

### 4.7 Stratification lessons for us
DFIR's rule is: insert a stratum barrier at **every blocking input**, not only at negation as classical Datalog does; the strata graph must be acyclic; and cycles through negation are legal only when broken by `defer_tick`. This matches Dedalus local stratification. The rule is to put a handoff at the start of each stratum so a blocking operator always reads a *complete* batch (2022 design doc). Blocking inputs are exactly the non-monotone ones ("If an operator is monotone with respect to an input, that input is streaming. If an operator is non-monotone, it is blocking." — `stratification.md`).

### 4.8 Performance techniques in DFIR
- Monomorphized, inlined push/pull pipelines with no virtual dispatch inside a subgraph (the Click-router analogy in ApPLIED'23).
- Symmetric hash join with `HalfSetJoinState` / `HalfMultisetJoinState` per side; half-joins for ordered probes; nested-loop `cross_join_multiset` (changelog: "up to 1.7x" over an SHJ keyed on `()`).
- `join_fused*` folds aggregation state into the join's hash table, so one map serves both.
- `state[items]` emits only deltas that changed the lattice, the minimized-delta idea from §3.
- `unique::<'static>`, `multiset_delta`, `fold_no_replay` and `persist` let programs choose replay or delta explicitly.
- `lattices::ght` is a Generalized Hash Trie and COLT "from the Wang/Willsey/Suciu Freejoin work": trie indexes for worst-case-optimal-style multiway joins.
- Arena-allocated per-tick buffers; zero-copy `iter_ref` / `#var`; batching per tick ("automatic vectorization" in the README).
- Single-threaded, shared-nothing processes, following Anna. Parallelism means more processes (SPMD), never shared memory.

---

## 5. The Datalog frontend (`hydroflow_datalog` → `dfir-datalog`)

Sources: `hydroflow_datalog_core/src/{grammar.rs, lib.rs, join_plan.rs}` at `hydroflow_lang-v0.10.0`, and [dfir-datalog](https://github.com/hydro-project/dfir-datalog) (same grammar; last commit Apr 2025). Used as the `datalog!(r#"…"#)` macro. This is the Dedalus dialect the SIGMOD'24 protocols were written in.

### 5.1 Grammar (rust-sitter; paraphrased faithfully)
```
Program     := Declaration*
Declaration := ".input"   Ident `pipeline`            # external source, DFIR code in backticks
             | ".output"  Ident `pipeline`            # sink
             | ".persist" Ident                       # relation persists across ticks
             | ".async"   Ident `send_pipeline` `recv_pipeline`   # channel; send consumes (node, data)
             | ".static"  Ident `rust_expr`           # constant EDB, replayed every tick
             | Rule
Rule        := Target RuleType Atom ("," Atom)* "."?
RuleType    := ":-"   (same tick)  | ":+" (next tick)  | ":~" (async, requires @node)
Target      := Ident ("@" TargetExpr)? "(" TargetExpr,* ")"
TargetExpr  := IntExpr | Aggregation | "index()"
Aggregation := min(x) | max(x) | sum(x) | count(*) | count(x,…) | choose(x) | collect_vec(x,…)
Atom        := "!" Rel | Rel | "(" IntExpr BoolOp IntExpr ")"       # predicate in parens
Rel         := Ident "(" ExtractExpr,* ")"
ExtractExpr := Ident | "_" | "*" ExtractExpr (flatten) | "(" ExtractExpr,* ")" (untuple)
IntExpr     := Ident | Integer | "(" IntExpr ")" | IntExpr (+|-|*|%) IntExpr
BoolOp      := < <= > >= == !=
Comments    := "#…" | "//…"
Magic relation: less_than(s, max)  # enumerates 0..max, used for hole filling in Paxos
```

### 5.2 Translation to DFIR (exact scheme)
- Every relation `R` becomes `R_insert = union() -> unique::<'tick>()`, then `R = R_insert -> tee()`. So relations have **set semantics per tick**.
- A **`.persist R`** relation compiles to `R_insert -> [pos] R; R = difference::<'tick,'static>() -> tee(); R -> defer_tick() -> [neg] R`. The read side therefore emits **only facts new this tick** (a delta), and the `'static` negative side remembers everything already emitted. Outputs of persisted relations get `persist::<'static>()` so that they replay.
- **Joins** are left-deep, in body order (source comment: "TODO(shadaj): smarter plans"). Each join is `join::<L,R, HalfMultisetJoinState>()`, with a `'static` side for each input that is persisted. Two persisted sides make a `('static,'static)` join whose output counts as persisted.
- **Negation** `!R(x)` becomes `anti_join()`, applied after all positive joins. A persisted negative side gets `persist::<'static>()`.
- **Predicates** become a `filter` after the joins. Repeated variables inside one atom (`R(x,x)`) become a local filter. `_` is a wildcard. `*x` flattens a column. `(a,b)` destructures a tuple.
- **Aggregation**: the head's non-aggregate expressions form the group key, and the rule becomes `fold_keyed::<'tick or 'static>` over `Option`-wrapped accumulators. It is `'static` when the body is persisted. `count(*)` counts rows, `count(x…)` counts distinct values, `choose` picks the first, and `collect_vec` collects distinct values.
- **`index()`** becomes `enumerate::<'tick>` (or `'static` for a persisted body with no aggregation).
- **Rule types:**
  - `:-` feeds `R_insert` directly.
  - `:+` inserts `defer_tick()`.
  - `:~` requires `@node`; it maps to `(node, (fields…))` and sends into the `.async` send pipeline.
  - Using `@` with `:-` or `:+` panics: "Rule must be async to send data to other nodes".
- Stratification is inherited from DFIR, so negation or aggregation through a cycle is rejected unless it passes through `:+`.

### 5.3 Verbatim test programs from the frontend (`lib.rs` tests)
```
.input edges `source_stream(edges)`
.input seed_reachable `source_stream(seed_reachable)`
.output reachable `for_each(|v| reachable.send(v).unwrap())`
reachable(x) :- seed_reachable(x).
reachable(y) :- reachable(x), edges(x, y).
```
```
result(x, z) :- ints_1(x, y), ints_2(y, z), !ints_3(y)
result(max(a), b) :- ints(a, b)
result(count(a), b) :- ints(a, b)
result(sum(a), b) :+ ints(a, b)
result2(choose(a), b) :- ints(a, b)
result(a % 2, sum(b)) :- ints(a, b)
result@b(a) :~ ints(a, b)          # with .async result `...send...` `...recv...`
result(a, b, index()) :- ints(a, b)
result(collect_vec(a, b)) :- ints1(a), ints2(b)
result(a, b) :- ints1(a, *b)        # flatten
result(a, b, c, d) :- ints1((a, b), (c, d))
```

### 5.4 Real protocols written in this dialect (SIGMOD'24 artifacts)
From [autocomp](https://github.com/rithvikp/autocomp) `rust/examples/{multipaxos,twopc,voting,...}`. The **MultiPaxos acceptor**, verbatim with debug outputs elided:
```
.input id `repeat_iter(my_id.clone()) -> map(|p| (p,))`
.async p1a `null::<(u32,u32,u32,)>()` `source_stream(p1a_source) -> map(...)`
.async p1b `map(|(node_id, v)| (node_id, serialize_to_bytes(v))) -> dest_sink(p1b_sink)` `null::<...>()`
.async p1bLog `...` `...`
.async p2a `null::<...>()` `source_stream(p2a_source) -> map(...)`
.async p2b `...dest_sink(p2b_sink)` `null::<...>()`

ballots(id, num) :+ ballots(id, num)
.persist log

ballots(id, num) :- p1a(pid, id, num)
MaxBallotNum(max(num)) :- ballots(id, num)
MaxBallot(max(id), num) :- MaxBallotNum(num), ballots(id, num)
LogSize(count(slot)) :- p1a(_,_,_), log(p, slot, ballotID, ballotNum)
p1b@pid(i, size, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p1a(pid, ballotID, ballotNum), LogSize(size), MaxBallot(maxBallotID, maxBallotNum), id(i)
p1b@pid(i, 0, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p1a(pid, ballotID, ballotNum), !LogSize(size), MaxBallot(maxBallotID, maxBallotNum), id(i)
LogEntryMaxBallotNum(slot, max(ballotNum)) :- p1a(_,_,_), log(p, slot, ballotID, ballotNum)
LogEntryMaxBallot(slot, max(ballotID), ballotNum) :- p1a(_,_,_), LogEntryMaxBallotNum(slot, ballotNum), log(p, slot, ballotID, ballotNum)
p1bLog@pid(i, payload, slot, payloadBallotID, payloadBallotNum, ballotID, ballotNum) :~ p1a(pid, ballotID, ballotNum), log(payload, slot, payloadBallotID, payloadBallotNum), LogEntryMaxBallot(slot, payloadBallotID, payloadBallotNum), id(i)
log(payload, slot, ballotID, ballotNum) :- p2a(pid, payload, slot, ballotID, ballotNum), MaxBallot(ballotID, ballotNum)
p2b@pid(i, payload, slot, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p2a(pid, payload, slot, ballotID, ballotNum), id(i), MaxBallot(maxBallotID, maxBallotNum)
```
The **2PC coordinator**, verbatim core:
```
voteToParticipant@addr(client, id, p) :~ participants(addr), clientIn(client, id, p)
AllVotes(client, id, payload, src) :+ AllVotes(client, id, payload, src), !committed(client, id, _)
AllVotes(client, id, payload, src) :- voteFromParticipant(client, id, payload, src)
NumYesVotes(client, id, count(src)) :- AllVotes(client, id, payload, src)
committed(client, id, payload) :- NumYesVotes(client, id, num), AllVotes(client, id, payload, src), numParticipants(num)
logCommit(client, id, payload) :- committed(client, id, payload)
logCommitComplete(client, id, payload) :+ committed(client, id, payload)
commitToParticipant@addr(client, id, payload) :~ logCommitComplete(client, id, payload), participants(addr)
AllAcks(client, id, payload, src) :+ AllAcks(client, id, payload, src), !completed(client, id, _)
AllAcks(client, id, payload, src) :- ackFromParticipant(client, id, payload, src)
NumAcks(client, id, count(src)) :- AllAcks(client, id, payload, src)
completed(client, id, payload) :- NumAcks(client, id, num), AllAcks(client, id, payload, src), numParticipants(num)
logCommit(client, id, payload) :- completed(client, id, payload)
clientOut@client(v) :~ completed(client, id, v)
```
Idioms to support natively:
- **persist-until-GC**: `R :+ R, !done(key)`;
- **"log then act next tick"**: `logX :- …; logXComplete :+ …; send :~ logXComplete`, which models fsync-before-send;
- **heartbeat timeouts** as `.input` timer streams;
- **leader election** by `max()` over received ballots;
- the **hole-filling** trick with `less_than`.

The MultiPaxos leader, about 150 rules including stable leader election, p1b log reconciliation, noop hole filling and `index()` slot assignment, is at `autocomp/rust/examples/multipaxos/leader.rs`.

### 5.5 Frontend limitations (observed in source)
- No join-order optimization.
- No lattice-typed columns or lattice merge in the head: Dedalus relations only.
- No Dedalus `choose`/`delay` built-ins. `:~` hands delay to the runtime.
- No explicit location or time columns; they are implicit.
- Aggregation is the only non-monotone construct besides `!`.
- Error reporting relies on panics in places.
- The `.persist` delta trick plus `unique::<'tick>` makes persistence cheap on the insert side, but joins between two persisted relations still use `'static,'static` **replay**.
- The frontend was demoted: moved out of the main repo in 2025 (issue #1809: experimental code "should be cut out into their own repositories … to speed up our testing workflow").

---

## 6. Hydroflow+ → Hydro (`hydro_lang`)

Sources: `docs/docs/hydro/reference/**`, `hydro_lang/src/**`, Laddad dissertation ch. 3–5, the Suki paper, and v0.10 `hydroflow_plus` docs.

### 6.1 Architecture: staged choreographic dataflow
- A Hydro program is a normal Rust function run **on the developer's laptop**. It builds a global IR through `FlowBuilder`. That IR is *projected* into one DFIR program per location, compiled with rustc, and deployed (dissertation §4.2).
- UDFs are quoted with `q!(…)` (the **stageleft** library), stored as syn ASTs and spliced into the generated code. Free variables are captured through the `FreeVariable` trait, so a closure's environment is known statically.
- **Locations:**
  - `Process<Tag>`: one thread on one machine.
  - `Cluster<Tag>`: SPMD; size chosen at deploy time; `MemberId<Tag>`; `CLUSTER_SELF_ID` is usable only in cluster code, which is checked at compile time.
  - `External`: a client outside the program.
  - `Tick<L>`: a logical location for one iteration of a local loop.
  - `Atomic<L>`.
- **The IR** (`compile/ir/mod.rs`) is an expression tree plus sharing and cycle nodes. Node kinds:
  - sources: `Source`, `SingletonSource`, `CycleSource`, `Tee`, `Reference`, `PartitionSide`;
  - atomicity: `BeginAtomic`, `EndAtomic`, `Batch`, `YieldConcat`;
  - combining: `Chain`, `MergeOrdered`, `ChainFirst`, `CrossProduct`, `CrossSingleton`, `Join`, `JoinHalf`, `Difference`, `AntiJoin`;
  - futures: `ResolveFutures*`;
  - element-wise: `Map`, `FlatMap`, `Filter`, `FilterMap`, `DeferTick`, `Enumerate`, `Inspect`, `Unique`, `Sort`;
  - aggregation: `Fold`, `Scan`, `FoldKeyed`, `Reduce`, `ReduceKeyed`, `ReduceKeyedWatermark`;
  - networking: `Network`, `VersionedNetwork`, `ExternalInput`;
  - others: `Counter`, `Cast`, `ObserveNonDet`, `Placeholder`.
  
  `Network` is **not a sink**. It can sit in the middle of expressions, so optimizers can reason across machines. Cycles use `CycleSource`/`CycleSink` pairs; no fixpoint operator is needed.

### 6.2 Live collections and their type markers
`Stream<T, Loc, Bound, Order = TotalOrder, Retries = ExactlyOnce>`, `KeyedStream<K, V, Loc, Bound, Order, Retries>`, `Singleton<T, Loc, Bound>`, `Optional<T, Loc, Bound>`, `KeyedSingleton<K, V, Loc, Bound>`.
- **Boundedness.** `Bounded`: complete, "immediately available". `Unbounded`: may still change. Singletons also have `Monotonic`, which "marks values that only grow over time (for example, `count()` on an unbounded stream returns `Singleton<usize, _, Monotonic>`)" and allows `threshold_greater_or_equal`. Bounded converts to unbounded freely with `.into()`. Unbounded converts to bounded **only** through a slice or batch plus `nondet!`.
- **Order.** `TotalOrder` vs `NoOrder`. Receiving from a cluster via `.values()` gives `NoOrder`. `fold`/`reduce` on `NoOrder` need `commutative = manual_proof!(/** … */)`. `first`/`last` need `TotalOrder`.
- **Retries.** `ExactlyOnce` vs `AtLeastOnce`. Aggregation over `AtLeastOnce` needs idempotence. `sample_every` produces `AtLeastOnce`.
- **Weakening is safe; strengthening needs `nondet!`.** Examples: `assume_ordering::<TotalOrder>(nondet!(…))`, `assume_retries::<ExactlyOnce>(nondet!(…))`.
- **API** (method names from source): `map`, `filter`, `filter_map`, `flat_map_ordered/unordered`, `flatten_*`, `inspect`, `enumerate`, `scan`, `generator`, `fold`, `reduce`, `count`, `first`, `last`, `max`, `min`, `unique`, `sort`, `limit`, `join`, `anti_join`, `filter_not_in`, `cross_product`, `cross_singleton`, `chain`, `interleave`, `merge_ordered/unordered`, `partition`, `into_keyed`, `keys`, `values`, `entries`, `resolve_futures*`, `sample_every`, `timeout`, `batch`, `all_ticks`, `defer_tick`, `across_ticks`, `atomic`, `end_atomic`, `send`, `demux`, `broadcast`, `for_each`, `dest_sink`. Keyed APIs include `fold_early_stop`, `get_max_key`, `lookup_keyed_singleton`, `join_keyed_singleton`, `value_counts`, `key_count` and `reduce_watermark`.

### 6.3 Nondeterminism guards (`nondet!`)
"Every **safe** API in Hydro guarantees eventual determinism." Non-deterministic APIs require a `NonDet` value, which only `nondet!(/** explanation */)` can create. The doc comment is **mandatory**. Guards are either discharged locally (with an explanation of why the output stays deterministic) or forwarded to the caller as a `nondet_*: NonDet` parameter.

Example: `paxos_core(..., nondet_leader: NonDet, nondet_commit: NonDet)`. "All non-determinism in a Hydro program originates at a `nondet!` invocation." In the dissertation this was Rust `unsafe`; it became `nondet!` later. Hydro's analogue of CALM "points of order" is therefore **explicit and checked by the compiler**, not inferred by a separate analysis tool as in Bud.

### 6.4 Ticks, slices and atomicity
- **Old API** (Hydroflow+ 0.10): `tick_batch()` / `all_ticks()`, and `persist()`, `unpersist()`, `delta()`, `cycle()`, `tick_cycle()`, `defer_tick()`. At that time streams had a window type parameter `W` (`Async`/`Windowed`) that blocked aggregation over un-windowed streams.
- **Current API:**
  - `let tick = process.tick();`
  - `stream.batch(&tick, nondet!)`: a bounded batch.
  - `singleton.snapshot(&tick, nondet!)`.
  - `.all_ticks()`: concatenates each tick's output into an unbounded stream.
  - `.defer_tick()`.
  - `tick.cycle()` / `cycle_with_initial()`: feedback that "automatically defers values by one tick".
  - `across_ticks(|s| s.count())`: stateful operators that keep memory across ticks.
- **`sliced!` block** (current preferred form):
  - Hooks `use::batch(coll, nondet!)` and `use::snapshot(coll, nondet!)`.
  - `use::atomic(coll, nondet!)`: a snapshot consistent with outputs already released by `end_atomic()`.
  - `let mut s = use::state(|l| init)` / `use::state_null::<T>()`: state carried across iterations.
  
  Guarantees: "Batches partition the input" (exactly once, in order); "Snapshots are monotone"; "all hooks in one `sliced!` block are sliced together, at the same logical point in time". Returned bounded collections are "unsliced": streams are concatenated, and singletons are latest-value.
- **Atomic collections.** `x.atomic()` starts an atomic context and `end_atomic()` releases outputs only after all computation in that context's tick is done. This gives read-after-write consistency within a single location; the docs say it has "No distributed atomicity". In Dedalus terms: derive the ack and the state update in the same timestep, and let readers snapshot that timestep.

### 6.5 Networking and fault models
`send(&loc, NET)`, `demux(&cluster, NET)` (for `(MemberId, T)` input), `broadcast(&cluster, NET, nondet!)` (membership may change), `source_cluster_membership_stream`. From a cluster you receive a `KeyedStream<MemberId, T>`.

`NET` is built from a transport and a fault model:

| Config | Delivery | Resulting type | Needs `nondet!` |
|---|---|---|---|
| `TCP.fail_stop()` | "the recipient receives a **prefix** of the sent messages in order" | preserves order | no |
| `TCP.lossy_delayed_forever()` / `UDP.lossy_delayed_forever()` | drops "modeled as being **indefinitely delayed**" | `NoOrder` | no |
| `TCP.lossy(nondet!)` | arbitrary drops | keeps `TotalOrder` | yes |
| `UDP.lossy(nondet!)` | drops and reordering | `NoOrder` | yes |

Serialization is `.bincode()` or `.embedded()`. `.name("x")` names a channel for versioned deployments.

### 6.6 Deployment, simulation, optimization
- **Hydro Deploy.** The README lists Localhost, GCP and Azure; the source also has `aws.rs`. Other backends: containerized Docker/ECS, Maelstrom (Jepsen), and "embedded" (user wires the channels).
- **Simulator.** `flow.sim().exhaustive(async || …)`, `.fuzz(…)` (libfuzzer, reproducers under `sim-failures/`) and `.deterministic(…)` (every decision scripted through hooks). It explores exactly the `nondet!` points (batch boundaries, snapshot versions, orderings) plus message interleavings. `lossy_delayed_forever` needs `.test_safety_only()`.
- The Raft implementation (`hydro_test/src/cluster/raft.rs`) has regression tests `concurrent_elections_never_fork_the_committed_log` and `fully_concurrent_run_never_forks_the_committed_log`.
- **Optimizer** (`hydro-optimize`, "DistOptimize", under submission). Automatic decoupling and partitioning over Hydro IR, driven by profiling (perf) and an ILP (Gurobi), with a per-operator partitionability table (§9.3).

### 6.7 Hydro → DFIR lowering (current)
- Operators at a top-level location get `'static` (cross-tick) state.
- Operators inside a `Tick` get `'tick`.
- `batch` into a tick becomes `batch_eager()` into the tick's `loop { }`.
- A bounded singleton entering a tick is first `persist::<'static>()`ed at the root.
- Sources inside a tick are emitted at root and windowed in.

(`compile/ir/mod.rs`: `tick_state_lifetime` → `'tick`, `cross_tick_state_lifetime` → `'static`, with a note that tick regions are moving to `loop` blocks with `'none`/`'loop`.)

---

## 7. Formal semantics: Flo (POPL'25) and Gyatso (thesis ch. 3)

Source: [flo.pdf](https://hydro.run/papers/flo.pdf) / [arXiv 2411.08274](https://arxiv.org/abs/2411.08274); Laddad dissertation ch. 3.

### 7.1 Flo definitions (precise)
- **Event loop** (Fig. 1). Loop: Δ ← new input batches; inputs := inputs ++ Δ; run "an arbitrary number of small-steps"; send "an arbitrary part of O". Execution is not required to reach quiescence each iteration.
- **Collection language** L_C = (C, ++, E_C, T_C, ⟦⟧, ⌊⌋, type, fix).
  - `fixed(c) ≜ ∀c'. c ++ c' = c`, i.e. no more data can be added.
  - ∅ is a right identity.
  - Types are closed under ++.
  - `++` need **not** be monotone, associative or commutative. This is what lets retractions and Z-sets fit.
- **Stream type** (T, B|U), with the subtyping (C,B) ≤ (C,U).
- **Operator language.**
  - A small-step relation (I, e) →δ (I', e', O'). Outputs are concatenated: O ++ O'.
  - Required properties: **confluence**; type preservation; each step decreases a finite, downward-closed partial order ≺. Hence Lemma 3.1: every operator reaches a stuck state in finitely many steps.
- **Eager execution** (Def 3.1). If (I, op, O) → (I', op', O') and (I ++ Δ, op, O) → (I'', op'', O''), then both (I' ++ Δ, op', O') and (I'', op'', O'') reach the same stuck state. Informally: processing partial input and then more gives the same result as processing everything at once. This is the determinism and incrementality property. It covers monotone operators, LVars thresholds, and DBSP's bilinear Z-set join.
- **Output maximality** (Def 3.2) and **streaming progress** (Def 3.3). With the bounded inputs fixed, the stuck-state outputs must be maximal: fixing the unbounded inputs as well would only *fix* the outputs and never change their contents. Bounded outputs must be fixed.
  - Consequence: `fold` must have a **bounded** input. `scan` or a lattice fold is allowed on unbounded input.
  - "LVar → sequence" is illegal. Emitting the current value breaks eager execution; waiting for fixedness breaks streaming progress. A **threshold** operator is the only legal read.
- **Graphs:** `e ::= e | e  (parallel) | e ; e  (sequential) | {S}[op]`. Determinism, eager execution and streaming progress are proven compositionally by structural induction (Lemmas 4.2–4.4). The only proof burden is **per operator**.
- **Nesting.**
  - Nested stream collection `[(S_0…S_n)]`: an ordered sequence of tuples of inner streams, with a terminator ⊗. Every tuple except the newest must have its bounded inner streams fixed.
  - `nest(g)` runs inner graph g on each inner tuple. It moves on only when g is stuck and all outputs, including `write_defer` inputs, are fixed.
  - `write_defer(k)` / `read_defer(k, init)` pass state to the next iteration. A substructural context makes each key written exactly once.
  - Worked examples: reachability within a fixed radius, and "dynamic radius" with nested cycles (`repeat_nested`, `nest_once`, `last`, `zip`).
- **Case studies.** Flink (`window` operator emitting bounded inner streams, then `nest` + `fold`); LVars (`fold_lattice` → LVar collection; `thresh`); DBSP (Z-sets with `++` = pointwise addition; join via (a+a')⋈(b+b') expansion, which is eager because it is bilinear).

### 7.2 Gyatso (distributed Flo)
- Stream types gain a location: (T, B|U, Process[Tag] | Cluster[Tag]). An operator's inputs must all be at one location.
- Clusters are modelled with *multibuffers* (member id → buffer), and a nondeterministic choice of which member steps.
- **Eventual determinism** (Thm 3.4.1): outputs converge to a deterministic value once all messages are processed, assuming liveness.
- **Monotone outputs** (Thm 3.4.2): if machines crash, the outputs are a subset, in the collection's natural order, of the intended outputs. So there are "no unexpected side effects".
- **Network operators as collection types:**
  - `network_o2o` (TCP: in order, exactly once);
  - `network_o2o_retry` over `[T]dup` (equivalence modulo duplicates);
  - `network_o2o_unord` over `[T]unord` (equivalence modulo order);
  - `fold_idempotent` / `fold_commutative` consume these types;
  - `network_o2m`, `network_m2o` for clusters.
  
  **This is the formal justification for the NoOrder and AtLeastOnce markers.**

---

## 8. "Keep CALM and CRDT On" (VLDB'23)

Source: [arXiv 2210.12605](https://arxiv.org/abs/2210.12605).
- CvRDT = join semilattice + monotone operations. CmRDT (op-based) logs are themselves a lattice: the DAG of operations as a grow-only set. But CRDT **queries** are unconstrained ("Schrödinger consistency").
- Potato/Ferrari example: a 2P-Set (A, R), queried as A − R during checkout, can be read "too early".
- **Monotone query:** ∀ i ≤ j: Q(i) ⇒ Q(j), for boolean threshold queries that return `true` or ABORT. Examples: `cardinality({txn | giftcard, amount>100}) > 50`; |A| + |R| > 100 on a 2P-Set.
- By CALM, monotone queries are *exactly* those that are safe on a single replica without coordination. Non-monotone queries must coordinate, following the Bernstein–Goodman spectrum (write-one/read-all, and so on), or knowingly accept stale reads.
- Agenda: a SQL/Datalog-like query language over lattices where monotonicity is syntactic; a CRDT data store with pluggable CRDTs; logical/physical separation (compression, delta gossip, GC); lineage to automate "apologies".

**In Hydro today:** `Monotonic` singletons and `threshold_greater_or_equal` implement threshold queries, and `use::snapshot` + `nondet!` implements a knowingly stale read. The "Free Termination" paper (ICDT'25, [arXiv 2502.00222](https://arxiv.org/abs/2502.00222)) adds the dual completeness question: when can a node *terminate* without coordination? It shows that under acyclic state modification only threshold queries have free termination, and that group- or ring-based updates (IVM) cannot have it.

---

## 9. Chu et al., SIGMOD'24: rule-driven rewrites of Dedalus protocols

Source: [david-sigmod-2024.pdf](https://hydro.run/papers/david-sigmod-2024.pdf); full proofs are in the TR [arXiv 2404.01593](https://arxiv.org/abs/2404.01593), which I did **not** read, only the paper.

### 9.1 Dedalus as they use it
Every IDB relation has trailing (L, T) attributes. Body literals share l and t. Rule forms:
- **synchronous**: head t = body t, same l;
- **sequential**: head `t' = t+1`, same l;
- **asynchronous**: different l', and t' from a `delay((tuple…, l, t, l'), t')` literal, where `delay` respects happens-before (t < t').

Persistence is `r(…, l, t') :- r(…, l, t), t'=t+1`. Library functions (`hash`, `sign`) are infinite EDB relations usable only with bound inputs. Aggregates put the group-by in the non-aggregated head attributes. A *component* is the rule set that runs on one node. Inputs are referenced but not defined; outputs are defined but not referenced.

**Correctness** means equivalence of concurrent histories as for linearizability: the optimized program's outputs, *with timestamps*, match some run of the original. The fault model is asynchronous networking plus general omission (a decoupled node's partial failure equals an omission failure of the original).

### 9.2 Rewrites (preconditions are the part to implement)
- **Mutually independent decoupling.** C1 and C2 reference disjoint relations and neither references the other's outputs. Mechanism: add a `forward(l, l'')` redirection EDB to the producing rules.
- **Monotonic decoupling.** C1 is independent of C2 and C2 is monotone. Sufficient test: C2's inputs are persisted and it has no negation or aggregation (the TR relaxes this). Mechanism: redirect, and add persistence rules for C2's inputs.
- **Functional decoupling.** C2 has no aggregation or negation, and "each rule body in C2 has at most one IDB relation", so it is stateless. Mechanism: redirect only.
- **Asymmetric monotonic decoupling** (TR only): C2 is monotone but C1 depends on it (Paxos p2b proxy leaders).
- **Partitioning with co-hashing.** Find a distribution policy D that is *parallel-disjoint-correct*: all facts in one proof tree map to one node. Co-hashing sends facts that share join, group or antijoin keys to the same partition, consistently across **all** rules of the component.
- **Partitioning with dependencies.** Functional dependencies (A → B within a relation, for example `hash.1 → hash.2`) and co-partition dependencies (g: A ↪ B between relations joined through a function) loosen co-hashing.
- **Partial partitioning** (plus **sealing**). Replicate the relations that cannot be partitioned. Broadcast inputs that modify them, and buffer other inputs until every node has the replicated fact. This coordination is off the critical path (the Paxos acceptor ballot).

### 9.3 Results and follow-on
All protocols are "implemented as Dedalus programs and compiled to Hydroflow", on GCP n2-standard-4 machines.

| Protocol | Before | After | Setup |
|---|---|---|---|
| Voting | 100k | 250k (2×) | 26 machines |
| 2PC | 30k | 160k (5×) | 46 machines |
| Paxos | 50k | 150k (3×) | 29 machines; 130k with 20 machines |

Scala BasePaxos reached 25k and Scala CompPaxos 130k (their measurement; 150k as reported by its authors). Dedalus CompPaxos reached 160k. Things the rewrites cannot express include shared proxy leaders, nack messages, independent acceptor partitions (not linearizable in general), batching, thriftiness and flexible quorums. Extended to BFT (PBFT 5×) in "Bigger, not Badder" (PaPoC'24).

`hydro-optimize` automates this on Hydro IR. It never decouples inside an atomic region. It solves an ILP with operator placement variables, per-message serialization/deserialization costs and a decoupling penalty. Its partitionability table:
- always partitionable: `Map`, `Filter`, `FlatMap`, `Tee`, `Chain`, `Unique`, `Sort`, `DeferTick`, `Persist`, `Delta`;
- partitionable on the key: `Join`, `FoldKeyed`, `ReduceKeyed`, `Difference`, `AntiJoin`;
- never partitionable: `Fold`, `Reduce`, `Scan`, `Enumerate`, `CrossProduct`, `CrossSingleton`.

It also runs an input-dependency analysis through UDFs (`partition_syn_analysis.rs` analyses the Rust closure ASTs).

---

## 10. Incrementalization: persist/delta algebra and "time-travelling rewrites"

Sources: v0.10 `hydroflow_plus/src/rewrites/persist_pullup.rs`; Laddad dissertation ch. 7.
- Operators:
  - `persist`: output at tick i is ++_{j≤i} c_j.
  - `delta`: its inverse, outputting δ_i such that c_{i−1} ++ δ_i = c_i; valid only where δ is unique.
  - `old`: history before this tick.
  - `prev`: the input of the previous tick.
- **Rewrite rules** (egg syntax, verbatim from ch. 7):
  - `(delta (persist ?a)) <=> ?a`
  - `(persist ?a) <=> (chain (old ?a) ?a)`
  - `(cross_product (chain ?a ?b) ?c) <=> (chain (cross_product ?a ?c) (cross_product ?b ?c))`, and symmetrically
  - `(chain (chain ?a ?b) ?c) <=> (chain ?a (chain ?b ?c))`
  - `(chain (prev ?a) (prev ?b)) <=> (prev (chain ?a ?b))`, and the same for `cross_product`. This is the determinism rule: an operator whose inputs all come from the previous tick can be shifted back in time.
  - `(old ?a) <=> (prev (persist ?a))`: "unrolling". Together with e-graph equivalence cycles, which act as inductive proofs, this derives the streaming symmetric-hash-join form automatically. The chapter says this "generalizes the rewrites in DBSP to any collection in Flo".
  
  The acknowledged limitation is that e-graphs struggle across shared computation (tees and diamonds, §7.5).
- **`persist_pullup`**, as shipped in v0.10:
  - `Unpersist(Persist(x)) → x` and `Delta(Persist(x)) → x`;
  - `Map/Filter/FilterMap/FlatMap/Network(Persist(x)) → Persist(op(x))`: do the work once per element and replay the results;
  - `Union(Persist a, Persist b) → Persist(Union(a,b))`;
  - `Join(Persist a, Persist b) → Persist(Delta(Join(Persist a, Persist b)))`, and the same for `CrossProduct` and `Unique`;
  - persist is also pulled through tees.

**Relevance.** Dedalus persistence `p(X)@next :- p(X)` is exactly `persist`. Downstream rules evaluated naively over persisted relations recompute everything each tick, which is DFIR's `'static` replay. These rules turn per-tick recomputation into O(|Δ|) work. We need this, or an equivalent hand-built delta-maintenance compiler.

---

## 11. Performance claims (as stated; sources)
- "Hydroflow is as fast or faster than handwritten code in languages like C++ and Scala." CompPaxos in Dedalus → Hydroflow is "a bit better than Michael Whittaker's state-of-the-art (as of 2021) handwritten Scala code". Anna in Hydroflow is "in the same order of magnitude as C++ Anna", with linear scaling. ([Data in Beta blog, May 2023](https://databeta.wordpress.com/2023/05/09/hydroflow-performance-update-whoosh/))
- ApPLIED'23 makes stronger claims: CompPaxos in Hydroflow "provides better latency and peak throughput than the original handwritten Scala"; Anna in Hydroflow "outperforms the original handwritten C++ code and matches its linear scaling under conflict"; "Raw performance is no longer one of our primary concerns; optimization is the next challenge."
- SIGMOD'24 numbers are in §9.3. Suki (CP'24): Paxos "exceeding 50 kops/s out of the box".
- 2026 inline codegen: "35-50% throughput improvement on the Paxos benchmark" (dfir_lang changelog 0.16).
- Techniques the team credits: compiled monomorphized push/pull subgraphs (in-out trees), batching per tick, thread-local shared-nothing state with no locks or atomics (the Anna lineage), Rust ownership for copy efficiency, compile-time serialization, and staged projection giving "zero-overhead" per-location binaries.
- Anna background (original C++ system, not Hydro): the speakerdeck [Hydroflow talk](https://speakerdeck.com/jhellerstein/hydroflow-a-compiler-target-for-fast-correct-distributed-programs) repeats "Up to 700x faster than Masstree and Intel TBB on multicore".

---

## 12. What Hydro kept from BOOM, what it dropped, and why Bloom/Bud was slow

### 12.1 Kept
- Dedalus's transducer or "tick" model: local logical clock, fixpoint per tick, deferred state update, asynchronous channels with unbounded delay. Sources: DFIR docs, New Directions §3.1, Flo's event loop.
- The Datalog rule forms `:-` / `:+` / `:~` and `@node`, in the Datalog frontend.
- Bloom's collection kinds, now as lifetimes: `'tick` ≈ scratch, `'static` ≈ table, `defer_tick` ≈ `<+`, network send ≈ `<~`. Deletion (`<-`) ≈ `persist_mut` or the `:+ R, !del` idiom.
- Bloom^L lattices, morphisms, threshold reads.
- CALM and stratified negation, with blocking inputs forcing strata in the older runtime.
- Points of order, as `nondet!` and type markers. Blazes-style order and seal annotations, as the Order and Retries markers.
- Graph visualization: DFIR emits mermaid and dot; Hydro has viz.

### 12.2 Dropped or changed
- **The logic language as the primary surface.** "An explicit non-goal is to create a new programming language—doing so would miss decades of investment into libraries and tooling" (Laddad dissertation §1). The Datalog frontend is experimental and moved out of the main repo in 2025.
- **In-tick recursive fixpoint** (current DFIR). Cycles must go through `defer_tick` or `loop {}`. Hydro's `tick.cycle()` always defers by a tick.
- **Bud's Ruby-hook escape hatch.** Replaced by `q!` Rust closures that are assumed deterministic, plus `nondet!` for acknowledged nondeterminism.
- **Per-node program organization.** Suki: "languages like Bloom … syntactically scatters pieces of a distributed protocol according to where they are run. This makes the programs harder to read and gets in the way of modularity." Replaced by choreographic, location-typed single functions.
- **Molly/LDFI and lineage.** I found no lineage-driven fault injection or provenance system in the Hydro repos; searching for "lineage", "provenance", "molly" and "LDFI" matched only unrelated text. The replacement is the deterministic simulator with exhaustive, fuzz and scripted exploration of `nondet!` points and network interleavings. Lineage is named only as future work (Keep CALM §4.4; ApPLIED'23 §1.1 cites LDFI).
- **Dynamic graphs.** Proposed in the 2021 thesis (cumulative edges for replay to late attachers); not implemented ("we will NOT implement this", 2021 design doc).

### 12.3 Why Bloom/Bud was slow (evidence) and what Hydro did about it
- Bud was never meant to be fast. From `bloom-lang/bud/docs/intro.md`: "The first limitation is performance: Bud alpha is not intended to excel in single-node performance in terms of either latency, throughput or scale… many of the known performance problems have known solutions."
- Interpretation vs. compilation. From the Hydroflow thesis §1.1: "Unlike Bloom, Hydroflow is imperative and low-level, and Bloom is interpreted while Hydroflow is compiled. We hope to eventually use Hydroflow as a compilation target to speed up Bloom-style programs." **Response:** a compiled Rust dataflow, emitted by a proc macro or by staging, then inlined by LLVM.
- Semantics that allowed redundant work. Bud's fixpoint could re-run Ruby blocks "an *arbitrary* number of times during a single Bloom timestep" (`docs/ruby_hooks.md`). **Response:** each operator runs once per tick in topological order (2026), deltas flow through symmetric hash joins, and persistence and replay are explicit.
- Scheduling granularity. 2021 design doc: "Rust-style iterators … work well on linear or simple tree-shaped operator graphs … this does not work well on complex graphs"; "Timely does much better with complex graph topologies". **Response:** hybrid compiled subgraphs plus a scheduler (2021–2025), then fully static scheduling (2026, +35–50%).
- I found **no** Hydro document that directly benchmarks Bud against Hydroflow; the causes above are the team's stated reasons.

---

## 13. Recommendations for bloom-remake (concrete)

**R1. Runtime = the DFIR model, compiled.**
- Per node: single-threaded, shared-nothing, one local clock.
- Tick: ingest the queued batch, evaluate the local program to fixpoint, flush outbound messages, reset `'tick` state, swap the defer buffers, advance the clock.
- Lazy ticks: run only when external input exists or non-lazy deferred data exists.
- Provide `run_tick` / `run_available` / `run`.
- Parallelism comes from more nodes (SPMD clusters), never from shared-memory threads.

**R2. Compile Datalog to a DFIR-like IR, then to monomorphized Rust.**
- The IR is a flat operator graph: named nodes, ports, tee/union, lifetimes. Partition it into in-out trees (pull fan-in → pivot → push fan-out) and order subgraphs topologically at compile time. Buffers are per-tick arena `Vec`s; defer buffers are double-buffered.
- Keep a second backend that *interprets* the same IR with columnar batches, for REPL, tests, the simulator and Molly-style fault injection.
- Both backends must be semantically identical; the operator contracts in §4.3 serve as the spec.

**R3. Diverge from current DFIR by supporting intra-tick recursion.**
- Condense the rule graph into SCCs. Order the SCCs topologically, and put strata barriers at *every* non-monotone input: negation, aggregation, and non-threshold lattice reads.
- Evaluate each recursive SCC as a DFIR-style nested `loop {}` with semi-naive delta, new and total buffers. The loop iterates while any delta is non-empty.
- Use symmetric hash joins whose half-states persist for the whole SCC evaluation, so each new tuple is joined once against the accumulated other side. This is exactly Hydroflow's streaming semi-naive, and `unique`/set semantics dedup.
- Reject cycles through negation or aggregation unless an `@next` or async edge breaks them (Dedalus local stratification). Use a real SCC check; DFIR's own check was knowingly insufficient (issue #1115).

**R4. Make persistence incremental.**
- Recognize `p(X)@next :- p(X).` (Bloom `table`) and `p(X)@next :- p(X), !del(X).` (the persist-until idiom). Compile them to stateful tables with explicit deletion, like `persist_mut`, not to per-tick rederivation.
- Downstream rules over persisted relations should be maintained incrementally, via the persist/delta algebra (`persist_pullup` rules; Join(P,P) → P(Δ(Join))).
- Offer explicit "replay" semantics only where the program observes the whole relation at each timestep.

**R5. Lattices are first-class column and relation types (Bloom^L).**
- `Merge::merge(&mut self, other) -> bool` (returns whether it changed), `Default` = ⊥, `IsBot`/`IsTop`, `LatticeOrd`, `Atomize`.
- Built-ins: Max, Min, SetUnion, MapUnion (nested), Pair, DomPair, WithBot, WithTop, VecUnion, UnionFind, Conflict, Point, set/map union with tombstones.
- Declared morphisms and bimorphisms are evaluated differentially. Unannotated monotone functions are evaluated as whole-value reactive recomputation (the MTT class). Threshold reads are the only way to observe lattices monotonically.
- A `state` operator emits the deltas that changed the value, following the `state[items]` idea.

**R6. CALM analysis as a type and effect system, following Hydro's markers.**
- Every relation or channel gets:
  - boundedness: bounded, unbounded, or monotone singleton;
  - order: total or none;
  - retries: exactly-once or at-least-once.
- Operators state their requirements: aggregation over unordered input needs a commutative aggregate; over at-least-once input it needs an idempotent one; negation or aggregation over unbounded input needs a stratum or a tick boundary.
- Any construct that strengthens a guarantee needs an explicit `nondet "reason"` annotation, which the compiler and simulator track. The Bloom "points of order" report becomes: list every `nondet` and every non-monotone channel crossing.

**R7. Channels with declared fault models.**
- Async rules (`@async`, `:~`) are parameterized by transport and fault model: `fail_stop` (ordered prefix), `lossy_delayed_forever` (receiver sees unordered input), `lossy` (needs `nondet`).
- The fault model sets the receiving relation's type markers and drives the simulator.

**R8. Locations and choreographic modules.**
- Relations are typed by location: process, cluster, external.
- A protocol is one module spanning several locations, fixing Bloom's scattering. Cluster member ids and self id are built in, with broadcast, demux and membership streams.
- Projection generates one binary or program per location.

**R9. Atomicity.** Give the language an `atomic` construct, the equivalent of Hydro `atomic()`/`end_atomic()`/`use::atomic`. Outputs such as acks are released only after the same timestep's state updates are visible to later snapshot reads. This gives read-after-write consistency within one node. Keep all state of interlocked protocols (Raft) in one node's tick: Hydro's Raft moved to one unified step per tick after a two-component design lost committed entries in simulation.

**R10. Deterministic simulator.**
- Same compiled program. Explored nondeterminism: batch boundaries, message delay, order, duplication and loss per channel model, snapshot timing, and node crashes.
- Modes: exhaustive, coverage-guided fuzz, and fully scripted deterministic. Save failing traces as reproducers.
- Put Molly/LDFI on top: it can use the same hooks to drop the messages chosen by lineage.

**R11. Optimizer.**
1. Datalog join ordering. Hydro has none; this is an easy win over it.
2. Generalized hash tries (Free Join, the `lattices::ght` structure) for multiway joins.
3. The SIGMOD'24 decoupling and partitioning rewrites, with their preconditions: monotonicity, functionality, co-hashing, FD/CD analysis.
4. A profile-driven ILP as in hydro-optimize.
5. E-graph incrementalization later.

**R12. Surface syntax.**
- Start from the proven `hydroflow_datalog` features: `.input/.output/.async/.persist/.static`, `:-`/`:+`/`:~`, `@node`, `!`, predicates, arithmetic, the aggregates `min/max/sum/count/count(*)/choose/collect_vec`, `index()`, `*` flatten, tuple destructuring, `_`, `less_than`.
- Add lattice columns, modules, and types.

---

## 14. MUST-IMPLEMENT CHECKLIST

Each item gives a one-line precise spec and its source.

**Execution model**
1. **Per-node tick loop.** Ingest a batch → fixpoint → emit async → advance clock. Consecutive integer ticks; state updates atomic at tick end. (DFIR `life_and_times.md`; New Directions §3.1)
2. **Lazy ticks.** A tick runs only on external input or non-lazy deferred data. Provide `run_tick`, `run_available` and async `run`. (dfir_rs `context.rs`; v0.10 `graph.rs`)
3. **`'tick` vs `'static` operator state.** `'tick` state is cleared at tick end; `'static` persists. The default is `'tick`. With two arguments, one per input port. (ops `persistence_args`; `join.rs`)
4. **Replay semantics of `'static` joins and folds.** They emit the full result each tick that runs; `*_no_replay` variants emit only on new input. (`surface_join.rs` `replay_static`; `fold_no_replay`)
5. **`defer_tick` / `defer_tick_lazy`.** Buffer until the next tick. Non-lazy forces the next tick; lazy waits for another trigger. Inside a nested loop, "next tick" means next iteration. (ops `defer_tick*`; CHANGELOG 0.17)
6. **`persist` / delta / `multiset_delta` / `unique::<'static>`.** Explicit history replay and delta extraction. (ops docs)
7. **Deletion-capable persistence.** `Persist(x)`/`Delete(x)` applied in arrival order. (v0.10 `persist_mut`)

**Stratification and recursion**
8. **Barrier at every non-monotone input.** `difference[neg]`, `anti_join[neg]`, fold/reduce/fold_keyed, sort, singleton refs. (v0.10 `input_delaytype_fn`; `stratification.md`)
9. **SCC-based stratification.** stratum = max(pred + barrier). A cycle through a barrier is an error unless broken by a tick or async edge. (v0.10 `find_subgraph_strata`)
10. **Intra-tick recursive fixpoint** for monotone SCCs, via semi-naive / symmetric-hash-join streaming; set semantics dedup. (v0.10 reachability examples; DFIR `cyclic_flows.mdx`) *(Current DFIR forbids this; we must diverge.)*
11. **`loop {}` regions.** `batch`/`batch_lazy`/`batch_eager` ingress, `all_iterations` egress. Root loops fire at most once per tick; nested loops iterate while data exists. (CHANGELOG 0.17)
12. **External inputs placed at stratum 0 / root level.** (v0.10 `separate_external_inputs`; flat_graph_builder loop validation)

**Operators and their contracts**
13. **Equijoin** `(K,V1)⋈(K,V2)→(K,(V1,V2))`, in set and multiset variants, with a per-side persistence lifetime. (`join.rs`, `join_multiset.rs`)
14. **Half-join** that accumulates the build side and streams the probe side in order. (`join_multiset_half.rs`)
15. **Cross join**, set and multiset (nested loop), and **cross_singleton**, which short-circuits on empty. (ops docs)
16. **`difference`** (set on neg, multiset on pos) and **`anti_join`** (keys on neg). (ops docs)
17. **`fold`, `reduce`, `fold_keyed`, `reduce_keyed`, `scan`, `sort`, `sort_by_key`, `enumerate`, `unique`**, each with tick/static lifetime. (ops docs)
18. **Fused join-aggregate** (`join_fused` with Fold, Reduce and FoldFrom) sharing one hash table. (`join_fused.rs`)
19. **union / chain / chain_first_n / zip / zip_longest / tee / partition / demux_enum / unzip** port semantics. (ops docs)
20. **Sources and sinks:** `source_iter` (first tick only), `source_stream`, `source_interval`, `initialize`, `spin`, `for_each`, `dest_sink`, and serde variants. (ops docs)
21. **Singleton and optional buffers with by-reference access**, with read and mutable access groups ordered by the compiler. (CHANGELOG 0.17 `#var`/`#mut`/`#{N}`)

**Compiler and codegen**
22. **Flat graph with named nodes, ports and forward references**; arity and port validation from operator constraint tables. (`flat_graph_builder.rs`, `ops/mod.rs`)
23. **In-out tree partitioning** into pull fan-in → pivot → push fan-out, with handoffs at push→pull edges and no merges that create subgraph cycles. (`in-out_trees.md`; `flat_to_partitioned.rs`)
24. **Static topological schedule of subgraphs**, each run once per tick (outside recursive regions), with per-tick arena buffers and double-buffered defer buffers. (CHANGELOG 0.16/0.17)
25. **Operator codegen phases:** prologue, iterator, after and tick_end. (`OperatorWriteOutput`)
26. **Monomorphized native code generation**, with a second, semantics-equivalent interpreter backend. (ApPLIED'23 §1.2; recommendation R2)

**Datalog surface (Dedalus dialect)**
27. **Rule types:** `:-` same tick, `:+` next tick, `:~` async with required `@node` head location. (`grammar.rs`)
28. **Declarations** `.input`, `.output`, `.async send recv`, `.persist`, `.static`. (`grammar.rs`)
29. **Body atoms:** positive, `!` negated, parenthesized comparison predicates, arithmetic `+ - * %`, `_`, `*` flatten, tuple destructuring, and repeated variables as equality constraints. (`grammar.rs`, `join_plan.rs`)
30. **Head aggregates** `min, max, sum, count(*), count(x…)` (distinct), `choose`, `collect_vec`, plus `index()`. Grouping is by the non-aggregated head terms. (`lib.rs` `apply_aggregations`)
31. **Per-tick set semantics** for every IDB relation (`union → unique::<'tick>`). (`lib.rs`)
32. **Magic range relation** `less_than(x, n)`, which enumerates 0..n when n is bound. (`lib.rs` `MAGIC_RELATIONS`)

**Lattices**
33. **`Merge` trait** returning a changed flag; ⊥ = Default; IsBot/IsTop; LatticeOrd agreeing with merge; property-test helpers for ACI. (`lattices` README)
34. **Built-in lattices:** Max, Min, SetUnion, MapUnion, Pair, DomPair, WithBot, WithTop, VecUnion, UnionFind, Conflict, Point, tombstone variants. (`lattices/src`)
35. **Lattice operators:** lattice_fold/reduce, `state` (emits changed deltas plus the accumulated value), `lattice_bimorphism` (differential in both arguments), and threshold reads. (ops docs; `lattice_math.md`)
36. **Morphism/bimorphism annotations** that enable differential evaluation; everything else is recomputed reactively on the whole value. (Hydroflow thesis §2–3)

**Distributed safety types**
37. **Boundedness, order, retries and monotonic markers** on every relation or stream, with the operator requirements (commutative, idempotent, bounded) enforced. (Hydro docs; Flo; Gyatso §3.5)
38. **Explicit `nondet` annotations** with mandatory justification, forwardable through module interfaces. (`nondet.md`)
39. **Channel fault models** fail_stop / lossy_delayed_forever / lossy, which set the received relation's markers. (`network-configuration.md`)
40. **Clusters:** SPMD location with member ids, self id, broadcast, demux (send to member), receive as keyed-by-sender, and a membership stream. (`clusters.md`)
41. **Atomic regions:** outputs released only after the same tick's state is visible to atomic snapshots. (`atomic-collections.mdx`)
42. **Slices/snapshots:** batch partitions input exactly once and in order; snapshots are monotone; all hooks share one point in time. (`slices.mdx`)

**Tooling and optimization**
43. **Deterministic simulator:** exhaustive, fuzz and scripted modes over nondet points and network schedules; failing traces become reproducers. (simulation docs)
44. **Decoupling rewrites** (mutually independent, monotonic, functional) with their preconditions. (SIGMOD'24 §3)
45. **Partitioning rewrites** (co-hashing, FD/CD dependencies, partial with sealing), checked by parallel-disjoint correctness. (SIGMOD'24 §4)
46. **Persist/delta incrementalization** (`persist_pullup` rules, at least the Join(P,P) → P(Δ(Join)) family). (v0.10 `persist_pullup.rs`; dissertation ch. 7)
47. **Graph visualization** (mermaid or dot) of the compiled dataflow with strata and loops. (`graph_write.rs`; DFIR docs)

---

## 15. TEST PROGRAMS (from the literature and code; expected behavior)

1. **DFIR reachability** (`hydroflow/examples/example_5_reachability.rs`, verbatim in the v0.10 tree).
   - Setup: origin 0; edges (0,1),(2,4),(3,4),(1,2),(0,3),(0,3),(4,0).
   - Expected: "Reached" prints {0,1,2,3,4}, each once, after `unique()`.
   - Tests in-tick recursion, set-semantics dedup and termination.
2. **Unreachability** (`example_6_unreachability.rs`).
   - Setup: pairs (5,10),(0,3),(3,6),(6,5),(11,12).
   - Expected: unreached = {11,12} (vertices 0,3,6,5,10 are reachable).
   - Tests a stratum barrier on `difference[neg]` after a recursive SCC.
3. **Flip-flop** (`defer_tick` op doc).
   - `state = union() -> assert(even tick ⇒ true, odd ⇒ false) -> map(!x) -> defer_tick() -> state`.
   - Expected: exactly one value per tick, alternating; must never assert.
4. **Cross-tick dedup** (`defer_tick` doc). `inp -> [pos]diff; inp -> defer_tick() -> [neg]diff`. Tick 1 input {1,2,3,4}, tick 2 input {3,4,5,6}. Expected output: 1 2 3 4, then 5 6.
5. **Join lifetimes** (`dfir_rs/tests/surface_join.rs`). Expected outputs are exactly the test asserts:
   - `('tick,'static)`: tick 0 gives (7,(1,0)),(7,(2,0)); tick 1 gives nothing.
   - `('static,'tick)`: ticks 0/1/2 give the pairs with 0/1/2 respectively.
   - `('static,'static)`: output grows cumulatively.
   - `replay_static`: identical full output on every tick.
6. **fold_keyed / unique / multiset_delta lifetimes** (op docs).
   - `fold_keyed::<'tick>`: second run shows only "palo alto, ".
   - `unique::<'tick>`: 3 is re-emitted in the next tick.
   - `multiset_delta`: tick 2 emits 5 and one 3.
7. **persist replay.** `source_iter(["hello"]) -> persist::<'static>()` emits "hello" every tick. The join with persisted inputs prints oakland, oakland, then san francisco (op doc).
8. **Datalog frontend snapshot programs** (`dfir-datalog` `lib.rs` tests): transitive_closure, join_with_self, local_constraints (`out(x,x) :- input(x,x)`), test_anti_join, test_max/test_max_all, test_aggregations_and_comments (`:+` with sum), test_index, test_collect_vec, test_persist, test_persist_uniqueness (count over a persisted relation must count distinct facts once), flatten/detuple variants. Expected: the snapshot DFIR graphs and their outputs.
9. **Dedalus 2PC** (autocomp `twopc/{leader,participant}.rs`, verbatim in §5.4).
   - Expected: each client (client,id,payload) is logged, committed after all N votes, and completed after all N acks, with a client reply.
   - `AllVotes`/`AllAcks` are garbage-collected after commit/complete.
   - Under message delay or reordering in the simulator: no commit without all votes.
10. **Dedalus voting** (autocomp `voting`). Leader broadcasts, collects votes from all participants, replies once per payload.
11. **Dedalus MultiPaxos** (autocomp `multipaxos/{leader,acceptor}.rs`).
   - Expected: stable leader elected via ballots and `iAmLeader` heartbeats; p1b log reconciliation re-proposes the highest-ballot entries; holes filled with noops; payloads assigned consecutive slots via `index()`; `allCommit` after 2f+1 matching p2bs; replicas receive (payload, slot).
   - Safety: no two different payloads committed in one slot under any interleaving.
12. **Rewritten protocols** (autocomp `auto*` examples): ScalableVoting, Scalable2PC, AutoMultiPaxos with p2a/p2b proxy leaders and acceptor coordinators. Expected: same client-observable histories as the base versions (SIGMOD'24 correctness notion) and higher throughput.
13. **Hydro Paxos, 2PC and Raft** (`hydro_test/src/cluster/{paxos.rs, two_pc*.rs, raft.rs}`, examples `paxos.rs`, `two_pc.rs`, `raft.rs`). The Raft simulation tests `concurrent_elections_never_fork_the_committed_log` and `fully_concurrent_run_never_forks_the_committed_log` must pass. Expected: every member's committed log is a prefix of every other's.
14. **Counter with read-after-write** (`atomic-collections.mdx`). Increment → ack → get must return ≥ 1 in *all* simulated schedules with `use::atomic`, and must be *able* to fail with a plain snapshot.
15. **collect_quorum** (`hydro_std/src/quorum.rs`). Keys reaching `min` successes are emitted exactly once regardless of batch boundaries. With min < max, each key is output once and its state is dropped after `max` responses.
16. **Shopping cart** (ApPLIED'23; `hydroflow/examples/shopping` in v0.10). Six variants: original Vec group_by, BP lattice, SSIV lattice, push group_by through join, client- or server-side decoupling, replicated servers via broadcast. Expected: identical final carts per session; checkout emitted only once the SSIV/BP lattice reaches ⊤, in every variant and under network reordering.
17. **COVID tracker** (New Directions Fig. 3). `transitive` over contacts; `diagnosed` sends alerts to the transitive closure; `vaccinate` must never make `vaccine_count` negative when run concurrently.
18. **Chat with history** (dissertation ch. 7 Figs. 7.1–7.3).
   - The tick-local cross product broadcasts only same-tick pairs.
   - persist ⋈ persist → delta broadcasts every message to every member exactly once, including history for late joiners.
   - The incrementalized plan must produce the same output as the naive one.
19. **Flo nested iteration** (Flo §5, Figs. 8–9). Reachability within radius k via nest/read_defer/write_defer yields k growing layers; the dynamic-radius version bootstraps from the previous query's `last`.
20. **CRDT/threshold queries** (Keep CALM). 2P-Set Potato/Ferrari: A − R at checkout is non-monotone and must be flagged or need coordination. `|A| + |R| > 100` and "giftcard count > 50" are monotone thresholds, safe locally, and never retract.
21. **Distributed deadlock detector** (`hydroflow/examples/deadlock_detector`). Peers gossip waits-for edges. Every node eventually reports each cycle in canonical order; purely monotone.
22. **RGA / collaborative text** (`hydroflow/examples/rga`). Kleppmann's Datalog RGA, hand-compiled. Expected: identical total order of characters at all replicas.
23. **Replicated KVS** (`hydroflow/examples/kvs_replicated`; Anna-style). PUTs gossip and converge; GETs are tick-scoped. Expected: convergence after quiescence.
24. **Lamport and vector clocks** (`hydroflow/examples/{lamport_clock,vector_clock}`). Message stamps respect happens-before.
25. **Maelstrom broadcast** (`hydro_test/src/maelstrom/broadcast.rs`, "implements the Maelstrom broadcast workload", fly.io dist-sys 3a/3b; its intra-cluster channel is `TCP.lossy(nondet!(…))`). Expected: passes the Maelstrom broadcast checker, including under partitions: every node eventually reads every broadcast value.

---

## 16. Sources

Papers read in full (text extracted from PDFs):
- [New Directions in Cloud Programming (CIDR'21)](https://hydro.run/papers/new-directions.pdf)
- [Hydroflow: A Model and Runtime for Distributed Systems Programming (UCB/EECS-2021-201)](https://hydro.run/papers/hydroflow-thesis.pdf)
- [Flo: a Semantic Foundation for Progressive Stream Processing (POPL'25)](https://arxiv.org/abs/2411.08274)
- [Keep CALM and CRDT On (VLDB'23)](https://arxiv.org/abs/2210.12605)
- [Optimizing Distributed Protocols with Query Rewrites (SIGMOD'24)](https://hydro.run/papers/david-sigmod-2024.pdf)
- [Initial Steps Toward a Compiler for Distributed Programs (ApPLIED'23)](https://hydro.run/papers/joe-applied-2023.pdf)
- [Suki (CP'24)](https://arxiv.org/abs/2406.14733)
- [Laddad, Programming Models for Correct and Modular Distributed Systems (EECS-2025-85)](http://www2.eecs.berkeley.edu/Pubs/TechRpts/2025/EECS-2025-85.pdf): chapters 1, 3, 4 and 7 read; 5 and 6 skimmed.
- [Free Termination (ICDT'25)](https://arxiv.org/abs/2502.00222): intro only.
- [Wrapping Rings in Lattices (PaPoC'24)](https://hydro.run/papers/conor-papoc-2024.pdf): intro only.

Code:
- `github.com/hydro-project/hydro` @ `9e2a120`: `dfir_lang`, `dfir_rs`, `dfir_pipes`, `lattices`, `hydro_lang`, `hydro_std`, `hydro_test`, `design_docs/`, `docs/`.
- The same repository at tag `hydroflow_lang-v0.10.0`: `hydroflow_lang`, `hydroflow`, `hydroflow_datalog_core`, `hydroflow_plus`.
- [hydro-project/dfir-datalog](https://github.com/hydro-project/dfir-datalog)
- [rithvikp/autocomp](https://github.com/rithvikp/autocomp)
- [hydro-project/hydro-optimize](https://github.com/hydro-project/hydro-optimize)
- [bloom-lang/bud docs](https://github.com/bloom-lang/bud) (intro.md, ruby_hooks.md)

Other:
- [Hydroflow Performance Update: Whoosh! (Data in Beta)](https://databeta.wordpress.com/2023/05/09/hydroflow-performance-update-whoosh/)
- [Hydroflow talk slides (speakerdeck)](https://speakerdeck.com/jhellerstein/hydroflow-a-compiler-target-for-fast-correct-distributed-programs)
- [hydro.run research page](https://hydro.run/research/)
- [David Chu McElroy's publications page](https://davidchuyaya.github.io/): lists "DistOptimize" as under submission; the paper itself was not available.

Not accessed:
- the SIGMOD'24 technical report appendix with the full rewrite proofs (arXiv 2404.01593);
- Conor Power's dissertation (downloaded, not read);
- the `cidr2021` code repository;
- Katara (OOPSLA'22) beyond its abstract;
- any direct Bud-vs-Hydroflow benchmark (none found).
