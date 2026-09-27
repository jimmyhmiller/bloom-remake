# 09 — Fast Datalog Engine Techniques (Soufflé, DD/DDlog/DBSP, FlowLog, Ascent, WCOJ, …)

**Audience:** the people building the bloom-remake evaluation engine in Rust.
**Question this answers:** how do we make the fastest engine for Dedalus/Bloom-style programs? Those programs run many small ticks, apply incremental updates each tick, use networking and lattices, and need provenance for LDFI.

**Method.** I read the primary papers (full text via PDF → `pdftotext`) and the relevant source code (Soufflé `src/ram`, `src/ast2ram`, `src/include/souffle/datastructure`; datafrog; Ascent's `ascent_macro`; egglog's `core-relations`/`egglog-bridge`; Bud `lib/bud.rb`; Molly `DedalusRewrites.scala`; Hydro DFIR operator docs). Each claim cites its source. Anything I could not get to is listed in §0.

---

## 0. Sources I could not fully access (read this first)

- **Hu, Zhao, Jordan, Scholz, "An efficient interpreter for Datalog by de-specializing relations", PLDI 2021.** I did not get the PDF itself. I read the Soufflé summary page (https://souffle-lang.github.io/pldi21.html) and Xiaowen Hu's 2020 honours thesis, *An Efficient Interpreter for Soufflé* (https://souffle-lang.github.io/pdf/xiaowenthesis.pdf), which is the precursor of the paper. The STI details below come from the thesis. The headline numbers come from the paper's abstract on the page.
- **Szabó, Bergmann, Erdweg, Voelter, "Incrementalizing Lattice-Based Program Analyses in Datalog" (IncA/Laddder, DRedL), OOPSLA 2018.** Metadata only (https://dl.acm.org/doi/10.1145/3276509). Its algorithm is **not** described here beyond the name "DRedL" (DDlog's related-work section cites it as the IncA algorithm).
- **Naiad/timely dataflow (SOSP 2013).** Not read directly. Timely concepts (frontiers, product timestamps) are described only as they appear in the DD CIDR'13 paper and the Shared Arrangements VLDB'20 paper.
- **EmptyHeaded, LogicBlox SIGMOD'15, NPRR PODS'12.** Not read. They are mentioned only as the LFTJ, Nemo and FlowLog papers cite them.
- **"A Differential Datalog Interpreter" (arXiv 2307.14810)** and **David Zhao's PhD thesis** failed to download. **Soufflé CCPE 2020 ("Specializing Parallel Data Structures") and the APLAS 2021 "choice" paper** were not read.
- **Egglog timestamp-based semi-naïve** is documented from **source code** (`core-relations/src/table/mod.rs`, `egglog-bridge/src/rule.rs`), not from the PLDI'23 paper, which does not describe it.

---

## 1. Executive summary: what the literature says we should build

1. **Semi-naïve evaluation is necessary but not sufficient.** Every serious engine uses it: Soufflé, Ascent, Crepe, datafrog, egglog, Nemo/VLog, RecStep, FlowLog, and DD, where it is implicit. The winners differ in three places: *data structures*, *join planning* and *parallel scheduling*. They do not differ in the fixpoint algorithm.
2. **Soufflé's recipe is the batch baseline to beat.** It lowers Datalog to an imperative relational-algebra IR (RAM), turns scans into indexed range queries, picks a *provably minimal* set of lexicographic indexes (minimum chain cover), and evaluates pipelined index-nested-loop joins over specialized B-trees and tries. It parallelizes only the outermost loop. Soufflé is still roughly the memory-efficiency leader (FlowLog uses 2–3× more memory). It is **not** incremental, and it scales poorly on workloads with many small iterations (FlowLog, SPLASH'26 demo).
3. **Differential dataflow (DD) gives incrementality and asynchrony. The cost is memory.** DDlog, which compiles to DD directly, uses about 3.5–6× more memory than FlowLog. FlowLog puts a logical IR in front of DD (operator fusion, structural join planning, semijoin pre-filtering, subplan sharing, Boolean-specialized diffs). It is currently the fastest published general engine at 64 threads: fastest in 36/41 benchmark cases vs Soufflé, RecStep, DDlog, DuckDB and Umbra (VLDB'26).
4. **Our workload is different from everyone else's.** Dedalus/Bloom means many *small* ticks per node. Each tick sees a small delta (messages) against a large persistent state. The key engine properties are:
   - (a) per-tick cost proportional to the **delta**, not the state;
   - (b) near-zero fixed overhead per tick and per stratum;
   - (c) cheap retraction of tick-local facts;
   - (d) lattice-aware semi-naïve;
   - (e) provenance capture that is cheap when off and complete when LDFI needs it.
   No existing engine is designed for exactly this. Hydro/DFIR (`'tick` vs `'static` persistence) and Bud (invalidation/rescan) are the closest designs.
5. **Recommended architecture** (details in §14):
   - Columns are interned to `u64`/`u32`.
   - Tables are append-only, with a *tick/iteration stamp* so that "delta" is a contiguous row range (the egglog core-relations technique).
   - Every table has a hash dedup index. Secondary indexes are either (i) permuted-column sorted runs merged geometrically (datafrog/DD "spine") for prefix, range and multiway joins, or (ii) hash indexes built lazily (Free Join's COLT) for tiny deltas.
   - Index sets are chosen by Soufflé's chain-cover algorithm.
   - Rules compile to Free Join plans, which subsume left-deep index nested loops and Generic Join. Plans come from a structural, worst-case-aware optimizer plus SIP.
   - The same kernel library serves a de-specialized interpreter and Rust codegen.
   - Per-tick incremental maintenance uses lifetime analysis: `'tick` relations are dropped wholesale and `'static` relations are continued semi-naïvely. Signed-diff (DBSP/DD) maintenance is used only on non-monotone paths, with elastic fallback to recompute.
   - Provenance has three tiers: off; Soufflé-style (rule, height) annotations at about 1.3× time and 1.8× memory; or a Molly-style per-rule firing log restricted to the backward slice of the goal relations.

---

## 2. What the workload needs from the engine

Dedalus semantics (see 02-dedalus.md) as it matters to the engine:

- **Deductive rules** fire within timestep *t*. **Inductive (`@next`)** rules write to *t+1*. **Asynchronous (`@async`)** rules write to a nondeterministic later time at another location.
- Persistence is explicit, e.g. `p(X)@next :- p(X), !del_p(X).` Derived (deductive) facts do **not** persist. At *t+1* they must be re-derived from the state at *t+1*.

This framing is the most important one for engine design. If the persisted base relations only **grow** from *t* to *t+1*, every derived relation at *t+1* is a superset of its value at *t*. We can keep it and run semi-naïve from the new base facts only. That is a *continuation* of the fixpoint, not a recomputation. If base facts are **removed** (deletions, or transient/tick-local inputs such as the messages that arrived at *t*), derived facts must be retracted. That is classical incremental view maintenance (DRed, counting, DD/DBSP; §6).

Cross-tick incrementality is therefore IVM with the tick as the outer timestamp. This is exactly DD's product timestamp (outer epoch, inner iteration) (§5.1). Hydro's DFIR exposes the same distinction as operator lifetimes: "With `'tick`, pairs will only be joined with corresponding pairs within the same tick. With `'static`, pairs will be remembered across ticks and will be joined with pairs arriving in later ticks" (DFIR `join` operator docs, hydro.run). Bud calls it scratch vs table, plus an invalidation/rescan scheme (§6.6).

Other requirements from the rest of the project that affect engine design:

- **Lattices** (Bloom^L): morphisms vs monotone functions (§3.5).
- **Aggregation and negation**, stratified within a tick. Negation through `@next` is allowed across time.
- **Networking**: an outbox and inbox per tick.
- **Provenance for LDFI** (Molly): all derivations of goal facts back through message sends (§11).
- **Deterministic simulation** of many nodes and many failure scenarios. Parallelism across executions matters more than inside one tiny tick (§12).

---

## 3. Semi-naïve evaluation: precise algorithms and variants

### 3.1 Classic semi-naïve with SCC scheduling

Build the precedence graph over relations. Strongly connected components (SCCs) are mutually recursive relations. Evaluate SCCs in topological order. Non-recursive SCCs are evaluated once. Recursive SCCs are evaluated with a fixpoint loop (Scholz et al. CC'16 §4.1, https://souffle-lang.github.io/pdf/cc.pdf).

Each recursive relation R keeps `R` (full), `ΔR` (tuples new in the previous iteration) and `newR` (tuples discovered in this iteration). Soufflé's lowering of TC, verbatim (CC'16 Fig. 5):

```
// rule: path(X,Y) :- edge(X,Y).
insert search edge do project (edge[0], edge[1]) into path;
merge path into delta_path;
loop
  purge new_path;
  // rule: path(X,Z) :- edge(X,Y), path(Y,Z).
  insert search edge do
    search delta_path where ((edge[1] = delta_path[0]) and
                             ((edge[0], delta_path[1]) not in path)) do
      project (edge[0], delta_path[1]) into new_path;
  exit counttuples(new_path) = 0;
  merge new_path into path;
  swap new_path and delta_path;
endloop
```

Note the **dedup-at-derivation-time** filter `(…) not in path`. A tuple enters `new` only if it is not already in `full`.

### 3.2 Soufflé's versioning scheme for multi-atom recursive rules (exact)

Take a rule `H :- A1, …, An` whose body contains *k* atoms from the current SCC at positions r1 < … < rk. Soufflé emits **k versions**. In version *j*:

- atom r_j reads **Δ**;
- atoms r_l with **l > j** get an extra filter `NOT (tuple ∈ Δ)`, so they read "old" = full \ Δ;
- atoms r_l with l < j read **full** (which includes Δ).

This is verified in source: `src/ast2ram/seminaive/ClauseTranslator.cpp`, `addBodyLiteralConstraints` loops `for (i = version + 1; i < sccAtoms.size(); i++) op = addNegatedDeltaAtom(...)`, and it also adds `addNegatedAtom(head)` for recursive clauses.

Consequence: each combination of delta and old body tuples is derived **exactly once**, in the version whose index is the *last* Δ-sourced atom. The number of versions is linear in k.

Ascent's implementation (`ascent_macro/src/ascent_mir.rs`, `versions_base`) also emits k versions, but mirrored. In version j, earlier dynamic atoms read `Total`, which in Ascent means prior iterations excluding Δ, i.e. old. Later atoms read `TotalDelta` (full). Here each combination is derived in the version of its *first* Δ atom. The BYODS formal semantics instead describes the naive-looking set of all 2^k − 1 combinations: "All the combinations of τ and Δ are present except all-τ" (OOPSLA'23 §3). Either linear scheme is fine; implement one of them, not the exponential one.

Soufflé also:

- stops recursive nullary heads early with an emptiness check (`createCondition`: "if it contains already the null tuple, don't re-compute");
- allows a separate join order per version via `.plan`.

### 3.3 Timestamped tables: delta as a row range (egglog core-relations; VLog blocks)

egglog's new backend stores tables that "can also be 'sorted by' a column" (`core-relations/src/table_spec.rs`). Rules are "(sets of) core-relations rules parameterized by a range of timestamps used as constraints during seminaive evaluation" (`egglog-bridge/src/rule.rs` header). Rows are written in timestamp order, so a constraint `ts ≥ c` (or `=`, `<`) on the sort column is answered by **binary search into a dense `OffsetRange`** (`fast_subset` in `core-relations/src/table/mod.rs`).

Delta, old and full are therefore row ranges. No separate Δ tables are copied or merged. VLog does something similar: every rule application "(step number, rule, and table) [is stored] in one block, and [VLog keeps] a separate list of blocks for each IDB predicate". Blocks are sorted columnar tables compressed with run-length encoding (Urbani, Jacobs, Krötzsch AAAI'16 §3, https://arxiv.org/pdf/1511.08915). Nemo keeps the idea: "we avoid costly table updates by storing the fresh results of each rule application in separate delta tables … mitigated by caching such unions" (Nemo KR'24).

**Why this matters for us:** use a stamp of (tick, iteration). Then "everything new since the last tick" and "delta of iteration i" are both range queries. Semi-naïve inside a tick and continuation across ticks become the same mechanism.

### 3.4 datafrog's `Variable`: stable/recent/to_add with geometric merging

datafrog (McSherry; `datafrog/src/variable.rs`) keeps `stable: Vec<Relation>` (sorted, deduplicated runs), `recent: Relation` (= Δ) and `to_add: Vec<Relation>`. `changed()`:

- (1) moves `recent` into `stable`, merging with the last stable run while `last.len() <= 2 * recent.len()`. This keeps logarithmically many runs, "no two within a factor of two" (McSherry blog).
- (2) merges all `to_add`, then removes tuples already present in any stable run. It gallops when the run is more than 4× larger, otherwise it does a linear merge.
- (3) sets `recent = to_add`.

Joins are **sort-merge with galloping** (`join_helper`, `gallop` doubles the step, then halves). A two-way join computes `recent1 ⋈ stable2 + stable1 ⋈ recent2 + recent1 ⋈ recent2` (`join_delta`). DD's arrangement "spine" and Free Join's lazy tries are the same idea at larger scale.

### 3.5 Lattice-aware semi-naïve (Flix, Bloom^L, Ascent, Datalog°, DFIR)

Lattice relations are *functional*: key columns map to one lattice value. Ascent: "a lattice … is denoted as a partial map from the non-lattice columns … to the lattice column" (CC'22 §3.1). egglog: `(function path (i64 i64) i64 :merge (min old new))`.

- **Flix's Δ definition** (Madsen, Yee, Lhoták PLDI'16 §5): ΔP = { g(P′,S) | S ∈ cells ∧ g(P′,S) ⊐ g(P,S) }. That is, Δ holds exactly the cells (keys) whose value **strictly increased**, and **carries the full new value**. Flix explains why the full value matters: a filter function applied to an increment instead of the lub would break the "compactness requirement". Their example is `A(Odd), A(Even)`: the rule must re-fire on `A(⊤)`, not on `A(Even)`.
- **Bloom^L** (Conway et al. SoCC'12 §4.1): per lattice identifier keep a "total" value and a "delta" value (the lub of the last round's derivations). "If a statement only applies morphisms to lattice elements, the rewrite adjusts the statement to use the lattice's delta value rather than its total value." Non-morphism monotone functions (their example is `size` on `lset`) **cannot** be applied to deltas. Doing so computes ⊔ size(Δᵢ) instead of size(⊔Δᵢ).
- **DFIR/Hydroflow thesis** calls these "monotone tricky (MTT)" functions: "not differentially computable". They must be re-run reactively on complete values (Samuel, Hydroflow tech report §2–3).
- **Ascent implementation** (`ascent_macro/src/ascent_codegen.rs`, head-clause codegen):
  1. look up the key in the lattice full-index of `new`, then `delta`, then `total`;
  2. if the key is found, `Lattice::join_mut(&mut row.value, new_value)`; if that returns *changed*, re-insert the **row index** into the `new` indices;
  3. otherwise push a new row.
  The row is updated in place, so Δ readers see the current (possibly larger) value. This is sound for monotone rules. In parallel mode Ascent uses `RwLock` per row plus a striped `Mutex` array (`__{rel}_mutex`) for inserting new keys.
- **Datalog° (Abo Khamis, Ngo, Pichler, Suciu, Wang, PODS'22, arXiv 2105.14435 §6):** semi-naïve generalizes to *complete, distributive dioids* (⊕ idempotent) with a difference operator v ⊖ u. For min-plus APSP the delta is δ^(t)(X,Y) = (min_Z δ^(t−1)(X,Z) + E(Z,Y)) ⊖ T^(t)(X,Y), where ⊖ keeps the new value only if it is strictly better.

**Rule for our engine.** Always carrying the full current value in Δ (Flix/Ascent) is correct for every monotone rule. Using increments (Bloom^L) is an optimization, legal only when every function applied to the lattice value is a morphism. We need a static classification of each lattice use: morphism, monotone non-morphism, or non-monotone. Non-monotone uses must be stratified.

### 3.6 Self-computing relations (eqrel, trrel) and the "no fact skips Δ" law

Soufflé's `eqrel` stores an equivalence relation as a union-find: "quadratic worst-case speed-up and space improvement" (Nappa, Zhao, Subotić, Scholz PACT'19). It has three layers: an equivalence-relation interface, a *densification* layer mapping sparse ids to dense ints, and a wait-free union-find.

The subtle part is semi-naïve. Merging new pairs creates *implicit* pairs, e.g. current classes {a,b,c},{f,g} plus new (b,f) implies (a,f). So Δ must be *extended*: for each element e of `new` that already occurs in R^k, insert e's whole old class into Δ (PACT'19 Algorithm 1, amortized O(α(n)·n)). This deliberately over-approximates Δ, which is safe.

BYODS (Sahebolamri, Barrett, Moore, Micinski OOPSLA'23 §3) states the general law. For custom data structures with concretization γ, the τ/Δ concretizations must satisfy (1) γ_Δ ∪ γ_τ = γ and (2) γ_Δ ⊇ γ − γ(db_τ). In words: "facts [must not] skip the delta database". Their `RelIndexMerge::merge(new, delta, total)` trait is where a provider enforces it.

### 3.7 Alternatives to semi-naïve

- **Eager evaluation** (Bembenek, Greenberg, Chong, "Making Formulog Fast", arXiv 2408.14017 §4). Each derived tuple immediately spawns the rule applications it enables, as tasks on a work-stealing pool (LIFO, so close to DFS). There are no rounds and no global barriers. It sometimes makes redundant derivations. It wins when rule bodies call expensive externals (SMT): 5.2× (interpreter) and 7.6× (Soufflé codegen extension) over stock Soufflé on SMT-heavy benchmarks. It is relevant to ticks with tiny deltas, where per-iteration barriers dominate.
- **Datafun** (Arntzenius & Krishnaswami POPL'20) does semi-naïve for a higher-order language by *static differentiation*: φ/δ transformations produce derivatives f′ with `semifix(f, f′)`. This is useful background for rules that call user-defined monotone functions.

---

## 4. Soufflé in depth

### 4.1 Architecture and IR

Soufflé frames compilation as a chain of Futamura projections (CAV'16 §2.1, https://souffle-lang.github.io/pdf/cav16.pdf):

1. semi-naïve evaluation specialized to the rules gives a RAM program;
2. RAM specialized to C++ templates;
3. template instantiation by the C++ compiler.

AST-level optimizations: constant propagation, alias elimination, rule elimination and relation elimination (CC'16 §3).

**Original RAM grammar** (CC'16 §4.2, verbatim):
```
S → insert O
O → search R [where C] do O1
O → project (V1,…,Vk) into R1
S → merge R1 into R2 | swap R1 and R2 | purge R
S → S1 ; S2 | loop S1 endloop | exit C | par S1 || … || Sk endpar
```

**Current RAM node set** (Soufflé `src/ram/*.h`): `Scan`, `IndexScan`, `ParallelScan`, `ParallelIndexScan`, `IfExists`, `IndexIfExists` (+Parallel), `Aggregate`, `IndexAggregate` (+Parallel), `Filter`, `Break`, `Insert`, `GuardedInsert`, `Erase`, `UnpackRecord`, `PackRecord`, `ExistenceCheck`, `ProvenanceExistenceCheck`, `EmptinessCheck`, `Loop`, `Exit`, `Swap`, `Clear`, `MergeExtend` (eqrel Δ extension), `EstimateJoinSize` (profiling for the join optimizer), `Call`/`SubroutineArgument`/`SubroutineReturn` (provenance queries), `Parallel`, `Sequence`, `Query`, `IO`, `LogTimer`/`LogSize`.

**RAM transforms** (`src/ram/transform`), each a concrete optimization we should replicate:
- `ExpandFilter` (split conjunctions);
- `HoistConditions` ("to the most-outer/semantically-correct loop");
- `MakeIndex` (turn `FOR t IN A IF t.x=10 ∧ t.y=20` into an index range query);
- `IfConversion` ("IndexScan … whose tuples are not further used … rewritten to Filter/Existence Checks");
- `IfExistsConversion` (a scan whose variables are used only in the following filter becomes `IfExists`, which stops at the first match);
- `ReorderConditions`, `ReorderFilterBreak`, `EliminateDuplicates`, `CollapseFilters`, `HoistAggregate`;
- `Parallel` (turn the outermost scan into its parallel version);
- `TupleId` (renumbering).

### 4.2 Relation data structures

**Decision table from CC'16 §5.1** (by arity and number of indexes). Nullary relations are a flag. Arity 1–2 uses a trie. Arity 3–5 uses a B-tree. Arity ≥ 6 uses a "blocked list + indirect B-tree index", with indexes pointing into row storage. Current docs: B-tree is the default, "direct" if arity ≤ 6 and "indirect" otherwise; `brie` is for dense data of arity ≤ 2; `eqrel` is for binary equivalence relations (https://souffle-lang.github.io/relations). Every relation and index gets its own template instance with compile-time lexicographic comparators (`Comperator<2,0>` in CAV'16).

**Concurrent B-tree** (Jordan, Subotić, Zhao, Scholz PPoPP'19, https://souffle-lang.github.io/pdf/ppopp19.pdf):
- **Two-phase usage.** Semi-naïve guarantees that a relation is either only read or only inserted into during a parallel section. So only *inserts* are synchronized, reads are not, and **no delete is needed**.
- **Optimistic read-write locks.** These extend seqlocks. A read records the version (a "lease") and validates it afterwards. Writers upgrade and make the version odd. Traversal takes read leases top-down; write locks are taken **bottom-up** during splits, which is deadlock-free. The C++ memory-model recipe: acquire loads of the version, relaxed loads of the data, an acquire fence, then re-read the version.
- **Operation hints.** Each thread keeps the last leaf for lookups, inserts, lower_bound and upper_bound. Nodes are never freed or moved, so hints never dangle. Sorted insertion order makes most operations skip the descent. Hints give "up to 6× … for membership tests".
- **Other details:** iterative rather than recursive operations, a 3-way comparator, a specialized B-tree-into-B-tree merge.
- **Results:** up to 59× faster than industry-standard structures in micro-benchmarks; 3× overall Datalog speedup.

**Brie** (PMAM'19, https://souffle-lang.github.io/pdf/pmam19.pdf):
- Each level of the trie is a map int → sub-brie, implemented as a *blocked quadtree*. Inner nodes collapse 6 bits, i.e. 64-way fan-out (`next[(val >> level) & 0x3F]`). Leaves are machine-word **bitmaps**, so the last log2(64) levels collapse into bits.
- It grows at the root when a value is outside the covered prefix (`raiseLevel` with CAS on {prefix, level, root}).
- It is lock-free: CAS on child pointers, atomic OR on leaf bits.
- Results: up to 15× faster in micro-benchmarks, 4× faster than B-trees for points-to on OpenJDK, and about 2× less space. It is only good for **dense, low-arity** data.

**Soufflé datastructure headers** show more infrastructure: `BTreeDelete.h` (a deletion-capable B-tree, needed for subsumption), `ConcurrentFlyweight.h`, `ConcurrentInsertOnlyHashMap.h`, `SymbolTableImpl.h` (string interning), `RecordTableImpl.h` (record/ADT interning), `PiggyList.h`, `UnionFind.h`, `EquivalenceRelation.h`.

### 4.3 Automatic index selection: minimum chain cover (Subotić et al. VLDB'18)

(http://www.vldb.org/pvldb/vol12/p141-subotic.pdf)

- A **primitive search** σ_{x1=v1,…,xk=vk}(R) is abstracted as its **search** S = {x1,…,xk}.
- An **index** is a lexicographic order ℓ over all attributes. ℓ **covers** S iff S is exactly the set of the first |S| attributes of ℓ (a prefix set). Search predicates become *lex searches* with bounds padded by ⊥/⊤ in the unspecified attributes. The result is a contiguous interval, retrievable in O(|result|·log n).
- A **search chain** is S1 ⊂ S2 ⊂ … ⊂ Sk, which one index covers. The index is S1 ≺ (S2−S1) ≺ … ≺ (Sk−Sk−1) ≺ rest.
- **Minimum Index Selection Problem = Minimum Chain Cover** (Lemma 4). By Dilworth's theorem it is solvable via **maximum bipartite matching**:
  1. Build G = (U, V, E) with a copy of each search on both sides and an edge (S, S′) iff S ⊂ S′.
  2. Take a maximum matching M. It yields |S| − |M| chains: start from vertices with no incoming matched edge and follow the matched edges (Algorithm 1 `MinChainCover`).
  3. Complexity is O(|S|²·m) to build plus O(|S|^2.5) for the matching (Hopcroft–Karp).
- Example: {x},{x,y},{x,z},{x,y,z} gives 2 indexes, x≺y≺z and x≺z.
- Sam Arch's 2020 honours thesis extends this to inequalities (title only; not read).

This is directly implementable. Run it per relation over all searches from all rule versions, including delta versions.

### 4.4 Parallelism and its limits

Soufflé parallelizes by partitioning the outermost loop of each loop nest (OpenMP). Inserts go into concurrent structures (CC'16 §5.3). Observed limits:

- FlowLog: "modern Datalog systems such as Soufflé and Flan … only distribute the outermost for-loop … insufficient to saturate resources even for simple recursive queries"; Soufflé averages under 50% CPU on DOOP (VLDB'26 §5.3, §10.2).
- GPUlog: "when run at 32 threads on transitive closure, Soufflé … spends 77.8% of its time in serialized tuple deduplication/insertion" (Sun et al. ASPLOS'25, https://thomas.gilray.org/pdf/datalog-gpu.pdf).
- FlowLog SPLASH'26 demo (arXiv 2607.23971): on Polonius, a workload with many small iterations, "Soufflé, DDlog, and Ascent barely move past a single thread … FlowLog … reaching 15× on Polonius and 8× on DOOP".

### 4.5 The Soufflé Tree Interpreter (STI): de-specializing relations

From Hu's thesis:
- Data structures are templated on arity, index order and implementation. An *adapter* layer exposes virtual interfaces backed by **pre-instantiated templates for arity 0..30**. Factory `switch(arity)`; above the limit, "fatal('Size not support yet.')" (Listing 14).
- **Comparator virtualization by permutation encoding.** There are n! possible orders, so the interpreter instantiates only the natural order and stores each tuple *permuted*: it inserts φ(t) and decodes with φ⁻¹ on read (§5.1.4.2). *This is the right design for our index storage too: an index is a permuted copy of the columns, sorted naturally.*
- **Stream buffering.** Iterators pull 128 tuples at a time through the virtual interface to amortize virtual calls.
- **"Switch-based Shadow Tree".** A lightweight executable tree mirrors the RAM tree. Nodes are dispatched with a switch, with pre-resolved relation pointers, avoiding visitor double-dispatch. A stack-VM alternative (SVM) was slower. Super-instructions help when guided by profiles.
- **Results.** Thesis: 2.11–5.88× slower than compiled. PLDI'21 abstract: 1.32–5.67× slowdown, and "6.46× faster on average" when compile time is counted. FlowLog reports the Soufflé interpreter "in general 1.5× slower" than compiled.

### 4.6 Join ordering: profile-guided optimizer (Arch et al. LOPSTR'22)

(https://souffle-lang.github.io/pdf/lopstr2022.pdf)

- A profiling stage instruments the RAM with `EstimateJoinSize`. It collects only the join-size estimates reachable in a *sideways-information-passing (SIP) graph*.
- The estimate uses selectivity: project onto the join attributes and count distinct values. For recursive relations it is computed **per iteration** of the fixpoint loop, with Δ sizes.
- Selinger-style dynamic programming then picks cost-optimal left-deep orders under a *recursive rule cost model*.
- Result: geometric-mean speedup of 12.07× over untuned orders and 1.09× over hand-tuned ones, on DOOP, DDISASM and VPC.

### 4.7 Provenance: proof annotations (Zhao, Subotić, Scholz TOPLAS'20)

(https://souffle-lang.github.io/pdf/toplas20.pdf)

- Every tuple stores **(rule number, height)**, where height is the height of a *minimal-height* proof tree.
- **Provenance lattice.** (I1,h1) ⊑ (I2,h2) iff I1 ⊆ I2 and h1(t) ≥ h2(t) for t ∈ I1. Going "up" means more tuples and smaller heights. The consequence operator sets h(head) = max(h(body_i)) + 1 and keeps the smaller annotation.
- **Update semantics break plain semi-naïve.** Δ^{i+1} = (new − R^i) ∪ {t ∈ R^i | h^i(t) > h^{i+1}(t)}. A tuple whose height *decreases* re-enters Δ. This is implemented with `PROV NOT IN`, which allows insertion if the tuple is absent **or** present with a larger height.
- **B-tree change.** The *insert* index order excludes the annotation columns, so set semantics hold. The *retrieve* index order puts annotations **last**, so updates never reorder tuples.
- **Worst case** is quadratic extra work (Theorem, §3.2). The practical cost is **1.31× runtime and 1.76× memory** on Doop/DaCapo.
- **Proof reconstruction is lazy and top-down.** For tuple t with rule ρ(t), find body tuples t1..tn with `matches(t, …)` and h(ti) < h(t). Stop at the first solution. Each step is an indexed query compiled as a RAM *subroutine*. Negations and constraints are shown as leaves.
- **Non-existence** is explained interactively: the user picks a rule, and the system shows which body atoms hold or fail.

### 4.8 Other Soufflé language features relevant to us

- **Subsumptive clauses** `A <= B :- body.` "permits to delete more specific tuples by more general tuples". They are implemented with `SubsumeRejectNewNew` / `SubsumeDeleteCurrentCurrent` translation modes (ClauseTranslator.cpp) and need a delete-capable B-tree. This is Soufflé's route to lattice-like dominance.
- **`choice-domain`** (APLAS'21; not read).
- **`.plan`** per version.
- **Records/ADTs** interned in a record table.
- **Functors.**

---

## 5. Differential dataflow, DDlog, DBSP, FlowLog

### 5.1 Differential dataflow (McSherry, Murray, Isaacs, Isard CIDR'13)

(https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf)

- A **collection trace** A is a function from a *partially ordered* time domain to collections. Differences are defined by δA_t = A_t − Σ_{s<t} δA_s, so A_t = Σ_{s≤t} δA_s. Operators produce δB_t from δA_t and strictly-prior differences.
- **Time domain.** With times (epoch, iteration) under the product order, iteration and input change compose. δA₀₁ advances the iteration and δA₁₀ updates the input, and neither is used to derive the other. This lets incremental *and* iterative computation compose. This is exactly our (tick, fixpoint-iteration).
- **Data model.** Rows are **(data, time, diff)**. Join multiplies diffs. `iterate` uses nested timestamps (outer, i). `distinct` is the non-linear operator that clamps multiplicities (FlowLog §2.3 explains this well).

**Arrangements** (McSherry, Lattuada, Schwarzkopf, Roscoe VLDB'20, https://arxiv.org/pdf/1812.02639):
- An arrangement is an indexed, shared, multiversioned collection. A *trace* is "an append-only list of immutable batches of update triples". Each batch covers the times between a lower and an upper frontier.
- The default batch is sorted by data then time, with each field in its own column.
- A background merge keeps logarithmically many batches. Merges are **amortized**: "for each new batch, we perform work proportional to the batch size on each incomplete merge".
- Readers hold **trace handles** with a frontier. Advancing a frontier permits *logical compaction*, which coalesces times that no reader can distinguish.
- **Join** does "alternating seeks" between cursors, so its work is linear in the *smaller* side. Key-preserving operators (filter, concat, negate, enter/leave) wrap arrangements without re-indexing.
- Arrangements can be shared across operators and dataflows. FlowLog's subplan sharing automates this.

### 5.2 DDlog (Ryzhyk & Budiu, Datalog 2.0 2019)

(http://ceur-ws.org/Vol-2368/paper6.pdf)

**Language features:**
- typed relations: `input relation`, `output relation`, intermediate; an optional `primary key` for deletion by key;
- rich types (bit<N>, bigint, strings, tagged unions, generics, `Ref<'T>` with structural equality);
- functions, `match`, string interpolation;
- `FlatMap`, `Aggregate((keys), f(col))`;
- a FLWOR-style "imperative" rule syntax;
- extern functions; a module system.

**Execution:**
- Execution is *always* incremental: a transaction applies inserts and deletes and emits output changes.
- The compiler (Haskell) generates Rust over DD. It shares indexes, uses reference counting for large values, minimizes `distinct`, and reuses common rule prefixes.

**Evidence and status:**
- The paper's OVN controller is about 6000 lines of DDlog.
- FlowLog's measurements show DDlog using more than 20 GB where others use under 5 GB, with a "median of 6.3× more" memory than FlowLog (SPLASH'26 demo).
- Rust compilation time is "often >100s" (VLDB'26).
- A Datalog 2.0'22 study found DDlog's evaluation 2–3× slower than Soufflé/RecStep on its workloads (Fan, Mallireddy, Koutris, https://ceur-ws.org/Vol-3203/paper10.pdf).
- The GitHub repository is archived (read-only).

### 5.3 DBSP (Budiu, Chajed, McSherry, Ryzhyk, Tannen VLDB'23)

(https://arxiv.org/pdf/2203.16684)

DBSP is the cleanest theory for our cross-tick IVM:
- Streams are over abelian groups. The primitives are lifting ↑f and delay z⁻¹. I (integration) and D (differentiation) are inverses.
- **The incremental version of Q is Q^Δ = D ∘ Q ∘ I.**
- **Chain rule:** (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ.
- **Linear time-invariant operators are their own incremental versions:** Q^Δ = Q. This covers filter, map/project, union and negate.
- **Bilinear operators (equi-join):** (a×b)^Δ = a×b + z⁻¹(I(a))×b + a×z⁻¹(I(b)), i.e. Δ(a⋈b) = Δa⋈Δb + a⋈Δb + Δa⋈b, with a and b the accumulated values from before the change.
- **Z-sets** (functions A → ℤ with finite support) encode sets and bags. Set union is distinct(a+b) and difference is distinct(a−b).
- **Incremental distinct** uses H(i,d)[x] = −1 if i[x]>0 ∧ (i+d)[x]≤0; +1 if i[x]≤0 ∧ (i+d)[x]>0; 0 otherwise (Prop. 4.7). It needs only the integrated input i and the change d.
- **Recursion.** δ₀ introduces a stream and ∫ integrates a nested stream. Stratified Datalog with negation compiles to circuits with nested time. Semi-naïve falls out of incrementalizing the inner loop (§5–6).
- Feldera is the industrial successor (not studied here).

### 5.4 FlowLog (Zhao, Yu, Rao, Frisk, Fan, Koutris; VLDB'26 / arXiv 2511.00865)

**Per-rule relational IR.** Each rule is a tree of logical operators (Map, Filter, Join, …) that is lowered to DD. Recursion control (semi-naïve, dedup, incrementality) is kept out of the per-rule plan. The executor merges all IRs into one dataflow.

**Logic fusion:**
- `FlatMap` fuses Map and Filter.
- `Join-FlatMap` fuses a Join with the maps and filters that follow it, rendered as DD `join_core` with a closure. This "avoids materializing the full join output".

**Structural cost model (worst-case aware):**
- The cost of an operator is the **number of distinct variables** it touches. The cost of a plan is the max over its operators, which bounds worst-case intermediate size.
- The search space is all **rooted join spanning trees (JSTs)**: maximum spanning trees of the join graph weighted by the number of shared variables.
- Plans come from a post-order traversal: join each atom with its parent, then project away dead variables. Semijoins, antijoins and filters are pushed to the leaves first. Ties prefer bushier trees.
- Example: `reach(x) :- edge(x,y), edge(y,z), reach(z)` gets cost 2 by rooting at `edge(x,y)`, i.e. two semijoins. Rooting at `reach` costs 3.

**SIP (Yannakakis-style two-pass semijoin reduction).** This works on arbitrary, even cyclic, join graphs:
1. BFS from any atom, semijoin-reducing each atom by its visited neighbours;
2. repeat in reverse order.

This is implemented as rule rewriting. Verbatim for Galen r3 `p(x,z) :- c(y,w,z), p(x,w), p(x,y)`:
```
p1(x,y) :- c(y,_,_), p(x,y).
p2(x,w) :- c(_,w,_), p1(x,_), p(x,w).
p3(x,y) :- p1(x,y), p2(x,_).
c4(y,w,z) :- c(y,w,z), p2(_,w), p3(_,y).
r3'. p(x,z) :- c4(y,w,z), p3(x,y), p2(x,w).
```

**Subplan sharing.** Canonicalize each IR subtree by encoding variables by their positions within atoms (e.g. `(e.0, e.1)`), hash it, and replace duplicates with a pointer. Repeat to a fixpoint. This covers shared arrangements and common table expressions.

**Boolean specialization.** For batch (monotone) Datalog, diffs become zero-bit presence: join is AND, concat is OR. A `lift` casts to ℤ only where subtraction is needed (antijoin). More generally, **monoid diffs**: (bool, ∨) for batch, (ℤ, +) for incremental, (ℤ, MIN) for recursive `MIN` aggregation such as CC/SSSP.

**Results:**
- Fastest in 21/41 cases at 4 threads and 36/41 at 64 threads.
- "11.7× faster than Soufflé, 4.0× than RecStep, and 6.7× than DDlog" on Andersen (large).
- It loses on "programs having few but expensive iterations, such as Reach and CSPA". Soufflé is 1.6× faster on CSPA/httpd at 4 threads, but 3.5× slower at 64.
- Robustness: across 91 random join orders, FlowLog without optimizations and Soufflé each timed out or OOMed on about 25%. FlowLog with plan+SIP never did.
- Memory is about 3.5× lower than DDlog but 2–3× higher than Soufflé.
- The SPLASH'26 demo adds an incremental mode that retracts a DOOP fact "in milliseconds" and a per-operator profiler.

---

## 6. Incremental maintenance algorithms (for deletions and retractions across ticks)

### 6.1 DRed (Gupta, Mumick, Subrahmanian SIGMOD'93)
1. **Overdelete** everything that has a derivation using a deleted fact, transitively.
2. **Rederive** the overdeleted facts that still have a derivation from surviving facts.
3. **Insert** new facts semi-naïvely.

It needs no extra state but can overdelete huge chains (Motik et al. 2019 §1).

### 6.2 Counting and recursive counting (Motik, Nenov, Piro, Horrocks AIJ 2019)

(https://www.cs.ox.ac.uk/boris.motik/pubs/mnph19maintenance-revisited.pdf)

Plain counting (support counts) is unsound for recursion because cyclic support keeps counts positive. The fix is to keep a **trace**: for each stratum s and iteration i, a *multiset* N_i^s of facts derived in that iteration. A fact's multiplicity in iteration *i* counts only derivations from facts of earlier iterations. Updates then recompute the trace iteration by iteration. The paper also gives **optimized DRed** (no repeated derivations) and **FBF (Forward/Backward/Forward)**. FBF bounds overdeletion by *backward chaining*: it checks whether a fact still holds before deleting it.

### 6.3 Elastic incrementalization (Zhao, Raghothaman, Subotić, Scholz PPDP'21)

(https://psubotic.github.io/papers/PPDP.pdf)

- A lightweight state per tuple: **(iteration first derived, derivation count in that iteration)**.
- **Bootstrap** is a counting semi-naïve run that reproduces standard semi-naïve Δs and builds the state.
- **Update** maintains the state incrementally.
- The system switches to Bootstrap (full recompute) when an Update takes more than a fraction of the last Bootstrap's runtime. The premise: "recomputation can be cheaper than incrementalization for large updates".

### 6.4 Incremental LFTJ (Veldhuizen 2013, LogicBlox)

(https://arxiv.org/pdf/1303.5313)

The goal is maintenance cost proportional to the **edit distance between leapfrog triejoin traces**. It uses *sensitivity indices*: the intervals of the key space that an iterator's seeks depended on. Projections are handled with **support counts** η in the head (a reference count). Count aggregates use the same mechanism. "Short-circuit" rules (∃ projections) can skip counts.

### 6.5 DD/DBSP

§5.1 and §5.3. These are fully general, including nested recursion and non-monotone operators. The cost is persistent indexed state per operator, i.e. memory.

### 6.6 Bud's cross-tick invalidation/rescan (the Bloom precedent)

Bud (`lib/bud.rb`, `prepare_invalidation_scheme`) states the semantics as "All collections … are semantically required to erase any cached information at the start of a tick". It then solves once, at wiring time, "a just-in-time invalidation scheme that permits us to preserve data from one tick to the next, and to keep things in incremental mode unless there's a negation". The constraints:
1. A full scan of an element forces full scans downstream, transitively.
2. Invalidating an element's cache forces a rebuild and a full scan.
3. Invalidation requires upstream elements to rescan, "or to transitively pass the request on further upstream".

The results are `@default_invalidate`, `@default_rescan` and per-scanner `invalidate_set`/`rescan_set`, activated when a table has deletions at runtime. Scratch collections are always invalidated. The tick loop (`tick_internal`) runs each stratum as `until fixpoint { scanners.scan(first_iter); flush; tick_deltas }`.

**Takeaway.** Monotone state is kept across ticks and processed with deltas. Anything touched by negation, deletion or non-idempotent code falls back to a (scoped) rescan. We should do the same, with DBSP-style signed diffs in place of full rescans where profitable.

---

## 7. Join algorithms

### 7.1 Worst-case optimal joins

- **AGM bound.** |Q| ≤ Π_e |R_e|^{x_e} for any fractional edge cover x. For the triangle query with |R|=|S|=|T|=N this is N^{3/2}, while any pairwise join plan can take Ω(N²) (Ngo, Ré, Rudra, SIGMOD Record 2013, https://arxiv.org/pdf/1310.3314).
- **Generic Join:** "for a in ∩{Π_x(R_i) | R_i contains x}: compute Q[a/x]". Intersect by iterating the smallest set and probing the others. It is worst-case optimal for **any** variable order (Free Join §2, https://arxiv.org/pdf/2301.10841).
- **Leapfrog Triejoin** (Veldhuizen ICDT'14, https://arxiv.org/pdf/1210.0481v5):
  - **Linear iterator interface:** `key()`, `next()`, `seek(k)` (least key ≥ k), `atEnd()`, with next/seek in O(log N) and O(1+log(N/m)) amortized when m keys are visited in order.
  - **Trie iterators** add `open()`/`up()`.
  - **leapfrog-search:** keep iterators sorted by key. With x′ the max key, repeatedly `seek(x′)` on the iterator with the smallest key until all are equal.
  - It runs in O(Q* log N), where Q* is the fractional cover bound. It is used by LogicBlox and Nemo.
- **Treefrog leapjoin** (datafrog `treefrog.rs`) is a practical variant for extending a prefix tuple by one value. Each *leaper* implements `count(prefix)`, `propose(prefix, &mut vals)` and `intersect(prefix, &mut vals)`. Per prefix, the leaper with the minimum count proposes and the others intersect. `extend_anti` and `filter_*` leapers handle negation and filters. This gives **per-tuple adaptive** variable intersection. It improved Polonius from 6.985 s to 5.275 s (McSherry blog).

### 7.2 Free Join (Wang, Willsey, Suciu SIGMOD'23)

This is the unification we should adopt as our join IR.

- **Generalized Hash Trie (GHT).** "A tree where each leaf is a vector of tuples, and each internal node is a hash map whose keys are tuples". The interface is `iter()` and `get(key) -> Option<GHT>`. A hash table is a two-level GHT; a hash trie has single-attribute keys.
- **Free Join plan.** A list of *nodes* [φ1,…,φm]. Each node is a list of **subatoms** (atom restricted to a subset of its variables), and the subatoms of each atom across nodes partition its variables.
  - **Valid** iff in each node (a) no two subatoms share a relation, and (b) some subatom, the **cover**, contains all of the node's new variables (vs(φ_k) − avs(φ_k)).
  - **Execution:** iterate the cover and probe the other subatoms with the bound values. Recurse on success.
  - Examples: `[[R(x,a),S(x)],[S(b),T(x)],[T(c)]]` is the left-deep binary plan (R⋈S)⋈T. `[[R(x),S(x),T(x)],[R(a)],[S(b)],[T(c)]]` is Generic Join [x,a,b,c].
- **Binary → Free Join conversion.** Take any optimized binary plan and factor it. The result "runs as fast or faster".
- **COLT (Column-Oriented Lazy Trie).** Data stays columnar. A COLT node is either `Vec<row offsets>` or `HashMap<key, COLT>`. `force()` builds the hash map for **one level on demand**. Iterating a suffix vector reads base columns directly. The cover relation (the "left" table) is iterated directly **with no index build at all**. Subtries under pruned keys are never built.
- **Vectorized execution.** `iter_batch(batch_size)`, then probe each other trie for the whole batch, dropping failures, then recurse per survivor.
- **Results:** up to 19.36× faster than binary join and 31.6× faster than Generic Join on acyclic queries; up to 15.45× and 4.08× respectively on cyclic queries (JOB, LSQB).

### 7.3 Where WCOJ matters for Datalog

Cyclic recursive rules are common:
- CSPA `valueAlias(x,y) :- valueFlow(z,x), memoryAlias(z,w), valueFlow(w,y)`;
- Galen's triangle rules r3 and r6;
- Andersen's `pointsTo(y,w) :- load(y,x), pointsTo(x,z), pointsTo(z,w)`;
- DOOP's 8-way join (FlowLog Example 5.1).

Ascent attributes Datafrog's up-to-4× advantage on some Polonius inputs to Datafrog's use of "optimal multi-way join algorithms" (CC'22 §5.1). Most Dedalus protocol rules, however, are small, acyclic, key-lookup joins against a delta of a few messages. There, an index nested loop driven by the delta is optimal. That argues for Free Join, which picks per node.

---

## 8. Rust engines (prior art in our implementation language)

**Ascent** (Sahebolamri, Gilray, Micinski CC'22, https://thomas.gilray.org/pdf/seamless-deductive.pdf; https://github.com/s-arash/ascent)
- Procedural macros (`ascent!`, `ascent_run!`, `ascent_par!`) compile rules to a struct with one field per relation and per index, plus a `run()` that walks the SCC DAG.
- Rules become nested loops over **hash indexes** (key → row indices into a `Vec` of tuples). The emitted join iterates the *smaller* side first (`if r_ind_1_total.len() <= tc_ind_0_delta.len()`).
- Syntax, verbatim:
  ```rust
  ascent! {
     lattice shortest_path(i32, i32, Dual<u32>);
     relation edge(i32, i32, u32);
     shortest_path(x, y, Dual(*w)) <-- edge(x, y, w);
     shortest_path(x, z, Dual(w + l)) <--
        edge(x, y, w),
        shortest_path(y, z, ?Dual(l));
  }
  ```
- Features: `for` generators, `if`/`let` clauses, user-extensible aggregators (`agg p75 = (percentile(75))(x) in population(x)`), `Dual<T>`, generic programs.
- Performance: Polonius comparable to Datafrog, which is up to about 4× faster on some inputs. 56–170× faster than Flix on lattice shortest paths.
- **BYODS** (OOPSLA'23, https://kmicinski.com/assets/byods.pdf): `#[ds(provider)] relation …`. Providers implement macros (`rel_ind!`, `rel_ind_common!`, `codegen`) and traits `RelIndexRead` (`index_get`), `RelIndexReadAll` (`iter_all`, `len_estimate`), `RelIndexWrite` (`index_insert`), `RelIndexMerge` (`init/merge(new, delta, total)`), plus concurrent `CRelIndex*` variants over rayon. Shipped providers include `trrel_uf` (transitive relation via union-find over SCCs, about 10× faster), `eqrel`, `ind_share` (index sharing via bipartite matching, as in §4.3) and `lat` (parallel lattice).

**Crepe** (https://github.com/ekzhang/crepe): `crepe!{ @input struct Edge(i32,i32); @output struct Reachable(i32,i32); Reachable(x,y) <- Edge(x,y); … }`. Semi-naïve, stratified negation, automatic hash indices. Claims TC speed "comparable to compiled Souffle" (no numbers given).

**datafrog**: §3.4 and §7.1. A library, not a language. Single-threaded, sorted-vector relations. `Variable::from_join`, `from_antijoin` (static relations only), `from_leapjoin`, `Iteration::changed()`. Used by Polonius.

**egglog** (Zhang et al. PLDI'23, https://arxiv.org/pdf/2304.04332):
- A *functional* database: every table maps keys → one output, with `:merge` to resolve conflicts (min/max lattices, union for e-classes) and `:default`.
- *Rebuilding* canonicalizes after unions (congruence closure).
- Semi-naïve is expanded to m delta rules per rule.
- The query engine is Generic Join (relational e-matching). The new backend `core-relations` uses a **Free Join variant** (`free_join/execute.rs`), sharded hash tables, `FxHasher` and timestamp-sorted tables (§3.3).

**Nemo** (Ivliev et al., ICLP'23 https://arxiv.org/pdf/2308.15897, KR'24):
- Columnar tables, "hierarchically sorted" with run-length encoding with increments.
- Columns are typed, sorted by type, with dictionary-encoded strings.
- Tables are accessed as **tries** and joined with **leapfrog triejoin**. Projection and reordering go through row-based temp tables.
- Delta tables per rule application, with cached unions.
- Aims at syntax-independent performance: atom order and argument order don't matter.
- LUBM-01k (186.7 M inferred facts): 163.3 s vs VLog 199.4 s on a laptop.

**FlowLog** compiles Soufflé-syntax programs to Rust DD executables (§5.4).

**Hydro DFIR** (https://hydro.run): a Rust dataflow runtime compiled from a graph DSL. It has `'tick`/`'static` persistence on stateful operators, lattice types on edges (`SetUnionHashSet`, `Max`, `Min`, `MapUnion…`) and "tainted" (non-monotone) subgraphs. It is the compilation target for Dedalus in Chu et al. SIGMOD'24 (§13.3).

---

## 9. Columnar, vectorized, RDBMS-backed and GPU Datalog

- **VLog** (AAAI'16): column-oriented storage, one block per (step, rule), RLE, and dynamic optimizations that skip redundant rule applications (§3.3).
- **RecStep** (Fan et al. VLDB'19, https://arxiv.org/pdf/1812.03975) runs on QuickStep, a columnar parallel RDBMS. Its optimizations:
  1. **UIE** (unified IDB evaluation): all rules for an IDB are one `UNION ALL` query;
  2. **OOF** (optimize on the fly): re-analyze stats per iteration;
  3. **DSD** (dynamic set difference): choose between one-phase R_δ − R (build a hash table on R) and two-phase r ← R ∩ R_δ then R − r, using a cost model with α = C_build/C_probe, β = |R|/|R_δ| and μ estimated from the last iteration;
  4. **EOST**: the whole evaluation is one transaction;
  5. **FAST-DEDUP**: a global latch-free separate-chaining hash table on a *compact concatenated key* (e.g. two int32s packed into an int64);
  6. **PBME**: bit-matrix evaluation for dense TC/SG.
  Scaling flattens beyond 16 threads because of the shared hash table.
- **DuckDB/Umbra** (per FlowLog): vectorized hash joins win on *few, large* iterations (Reach on twitter). They cannot express mutual or non-linear recursion. Umbra OOMs on long-tail workloads.
- **GPUlog** (ASPLOS'25): the **Hash-Indexed Sorted Array (HISA)** is a dense row-major data array, plus a sorted index array (lexicographic on the join columns), plus an open-addressing linear-probing hash table from join key to the first position in the sorted array. It gives range queries, lock-free dedup and parallel iteration. Since Δ is deduplicated against full, merging Δ into full needs no dedup. Up to 45× faster than Soufflé on context-sensitive points-to (PostgreSQL). A 2026 follow-up does WCOJ on GPUs (arXiv 2604.20073, not read in depth).

**Takeaway.** Dense arrays with a sorted permutation index and a hash directory (HISA) is a very good *immutable-run* layout on CPU as well. It is essentially one run of our LSM index.

---

## 10. Adaptive and JIT planning

- **Carac** (Herlihy, Martres, Ailamaki, Odersky ICDE'24, https://arxiv.org/pdf/2312.04282): *Adaptive Metaprogramming*. Join reordering and code generation are deferred to runtime, when relation cardinalities (including Δ sizes per iteration) are known. Backends are Scala quotes, JVM bytecode, lambdas or an IR interpreter. The "compilation granularity" trades plan freshness against compile cost: per rule, per iteration, per stratum. Code generation is asynchronous with deoptimization. Unoptimized recursive queries improve by up to three orders of magnitude, and hand-optimized ones by 6×.
- **RecStep OOF**: re-optimize every iteration. Overhead is visible when the plan never changes (FlowLog §10).
- **Soufflé LOPSTR'22**: offline, profile-guided (§4.6).
- **FlowLog**: *robustness first*. A static structural plan plus SIP, and no statistics.

**Takeaway for us.** Dedalus workloads are long-running services with repeating tick shapes, so profile-guided *online* re-planning is attractive. Precompile a few alternative orders per rule version. Switch when the observed Δ/full ratios cross thresholds. Use structural planning plus SIP as the robust default.

---

## 11. Provenance capture for LDFI

**What Molly does** (source: `molly/src/main/scala/.../DedalusRewrites.scala`, `addProvenanceRules`):
- For every rule N, Molly adds a rule with head `<head>_prov<N>(head columns…, all other bound body variables…, time)`. This **records every distinct rule firing** (why-provenance, with all derivations) for all timesteps.
- `ProvenanceReader` then builds rule-goal graphs (`GoalNode`) from these tables, memoized per goal tuple, including "phony"/negative support.
- Execution is by the C4 engine over all times up to EOT. `model.tableAtTime(goal, eot)` implies history is retained per time.

**What Soufflé does** (§4.7): annotations with (rule, height), then lazy top-down reconstruction by indexed subproof search. It is cheap (1.31×), but yields *one minimal-height* proof per query step. **LDFI needs all derivations** (alternative supports are exactly what fault injection must defeat), so we generalize:

- **Tier A, off.** No overhead.
- **Tier B, annotations plus history.** Keep each fact's birth and death ticks (interval, not copies), plus rule id and height at first derivation. Reconstruct *all* derivations lazily: for goal t at tick τ and each rule whose head unifies with t, enumerate all body instantiations valid at the right ticks (τ for deductive, τ−1 for `@next`, any earlier tick for `@async` sends matched to receives). Use indexes built lazily (COLT-style), since head-bound lookups need indexes that forward evaluation never used. This costs nothing during evaluation beyond retaining history, which DD-style compaction frontiers make controllable.
- **Tier C, eager firing log (Molly-equivalent, engine-native).** In the join pipeline, *before* head dedup, append the full variable binding of each distinct firing to a per-rule columnar log stamped with the tick. Restrict it to rules in the **backward slice** of the goal and invariant relations (static program slicing).

Tier B has zero runtime cost but pays at query time. Tier C is fastest for repeated LDFI queries within one run. Both are needed. Molly runs thousands of short executions, so Tier C's cost is bounded by EOT × messages.

---

## 12. Techniques specifically for many small incremental ticks (synthesis)

These combine the above. Items marked **[ours]** are design inferences, not claims from a paper.

1. **Fixed per-tick overhead must be tiny.** Precompile the plan once, reuse buffers, don't allocate per tick. **Only schedule strata and rules whose input relations have a non-empty Δ** (a dirty bit per relation, propagated along the dependency graph). Bud evaluates all strata each tick; DFIR schedules subgraphs by readiness **[ours: scheduling by dirty inputs is the obvious generalization]**.
2. **Lifetime analysis ('tick vs 'static).** Classify relations as (a) static-monotone: persisted with no deletion path, or derived only from such; (b) static-nonmonotone: persisted with a `!del` in the persistence rule, or derived through negation or aggregation from changing inputs; (c) tick-local: derived from channel or periodic inputs and not persisted.
   - Tick-local data is cleared wholesale at the end of the tick. That costs O(|tick data|), which is the same as its creation cost, so there is no DRed.
   - Static-monotone data is continued semi-naïvely from new base facts.
   - Static-nonmonotone data is maintained with signed diffs (DBSP) or counting, with elastic fallback to recompute (PPDP'21).
3. **Deltas are driven by timestamps.** Stamp rows with (tick, iteration) (§3.3). A join of a tick-local Δ against a large persistent relation is an index nested loop driven by the Δ, with cost proportional to |Δ| × fan-out.
4. **Represent persistence as intervals, not copies [ours, following DD's (data,time,diff)].** `p(X)@next :- p(X), !del_p(X)` is recognized as "persist until deleted": a row keeps `birth`; `death` is set when `del_p` fires. Naive Dedalus semantics copies every persistent fact into every tick. That is O(|p|·T) work and space, and it is what C4-style evaluation materializes.
5. **Batch per tick.** One tick consumes all messages that have arrived. That amortizes fixed costs and makes Δs bigger (Hydroflow/DFIR `run_tick`). Chu et al. show Dedalus→Hydroflow reaching 50k Paxos commands/s per proposer (§13.3).
6. **Use DD's shape without DD's memory.** Immutable sorted batches with amortized geometric merging (datafrog/DD spine) are ideal for "large state, small appends". Don't keep a full multiversion history per operator unless provenance or LDFI asks for it. Compact to the current tick otherwise (logical compaction by frontier).
7. **Plan per Δ size.** When |Δ| is small, prefer tuple-at-a-time pipelined index nested loops. When |Δ| is large (bulk ingest, MapReduce stages), switch to vectorized Free Join batches and morsel-parallel scans. Carac and RecStep show that sizes vary wildly across iterations.
8. **Parallelism comes from nodes and scenarios, not inside the tick [ours, motivated by FlowLog SPLASH'26 and GPUlog evidence].** One thread per engine instance (node). Many instances per process for simulation. LDFI scenario exploration is embarrassingly parallel. Intra-tick parallelism is reserved for large Δs, and uses thread-local output buffers plus a parallel dedup merge (§14.6) rather than shared concurrent B-trees, whose dedup serializes (the 77.8% figure).
9. **Fork scenarios cheaply [ours].** Immutable batches behind `Arc` let an engine state be snapshotted in O(#runs). LDFI can then fork failure scenarios at the first tick where they diverge, instead of re-running from tick 0 as Molly does.

---

## 13. Benchmarks and published numbers

### 13.1 Standard programs (verbatim rules from RecStep VLDB'19 §6, FlowLog, and the Soufflé papers)

```
// TC
tc(x, y) :- arc(x, y).
tc(x, y) :- tc(x, z), arc(z, y).
// SG (same generation)
sg(x, y) :- arc(p, x), arc(p, y), x != y.
sg(x, y) :- arc(a, x), sg(a, b), arc(b, y).
// REACH
reach(y) :- id(y).
reach(y) :- reach(x), arc(x, y).
// CC (recursive MIN)
cc3(x, MIN(x)) :- arc(x, _).
cc3(y, MIN(z)) :- cc3(x, z), arc(x, y).
cc2(x, MIN(y)) :- cc3(x, y).
cc(x) :- cc2(_, x).
// SSSP (recursive MIN)
sssp2(y, MIN(0)) :- id(y).
sssp2(y, MIN(d1 + d2)) :- sssp2(x, d1), arc(x, y, d2).
sssp(x, MIN(d)) :- sssp2(x, d).
// Andersen
pointsTo(y, x) :- addressOf(y, x).
pointsTo(y, x) :- assign(y, z), pointsTo(z, x).
pointsTo(y, w) :- load(y, x), pointsTo(x, z), pointsTo(z, w).
pointsTo(z, w) :- store(y, x), pointsTo(y, z), pointsTo(x, w).
// CSPA (Graspan)
valueFlow(y, x) :- assign(y, x).
valueFlow(x, y) :- assign(x, z), memoryAlias(z, y).
valueFlow(x, y) :- valueFlow(x, z), valueFlow(z, y).
memoryAlias(x, w) :- dereference(y, x), valueAlias(y, z), dereference(z, w).
valueAlias(x, y) :- valueFlow(z, x), valueFlow(z, y).
valueAlias(x, y) :- valueFlow(z, x), memoryAlias(z, w), valueFlow(w, y).
valueFlow(x, x) :- assign(x, y).
valueFlow(x, x) :- assign(y, x).
memoryAlias(x, x) :- assign(y, x).
memoryAlias(x, x) :- assign(x, y).
// Galen (McSherry dynamic-datalog; FlowLog Ex. 6.1)
p(x,z) :- p(x,y), p(y,z).
p(x,z) :- p(y,w), u(w,r,z), q(x,r,y).
p(x,z) :- c(y,w,z), p(x,w), p(x,y).
q(x,r,z) :- p(x,y), q(y,r,z).
q(x,u,z) :- q(x,r,z), s(r,u).
q(x,e,o) :- q(x,y,z), r(y,u,e), q(z,u,o).
// Bipartite (FlowLog)
red(y) :- edge(x,y), blue(x).
blue(y) :- edge(x,y), red(x).
answer() :- red(x), blue(x).
```

Other suites:
- **CSDA** (null-dereference dataflow);
- **DOOP** (136 rules; DaCapo batik, biojava, eclipse, xalan, zxing);
- **DDISASM** (cvc5, z3);
- **Polonius** (Rust borrow checker; the `clap-rs` input);
- **Dyck-2 reachability** (CFPQ kernel/postgre);
- **CRDT** (Kleppmann text-editor CRDT, McSherry's dynamic-datalog);
- **LUBM** and ChaseBench (VLog/Nemo);
- the **OpenJDK7 points-to** programs (Soufflé CC'16/CAV'16).

**Datasets:**
- G*n*-*p* random graphs (GTgraph; p=0.001 by default; G5K … G80K);
- RMAT-*n* (n vertices, 10n edges);
- livejournal, orkut, arabic, twitter;
- httpd, linux, postgresql (Graspan).

### 13.2 Published numbers to benchmark against

**Soufflé CC'16, TC on a random graph (1,000 vertices, 10,000 edges), 8-core i7-5820K:**

| Tool | Time | Memory |
|---|---|---|
| hand-written C++ (STL) | 2.0 s | 91 MB |
| bddbddb | 6.5 s | 30 MB |
| µZ | 340 s | 1667 MB |
| SQLite semi-naïve | 12.2 s | 126.9 MB |
| Soufflé B-tree, sequential / parallel | 1.26 s / 0.42 s | 25.6 / 26.3 MB |
| Soufflé Trie, sequential / parallel | 0.38 s / 0.12 s | 3.5 / 4.5 MB |

**OpenJDK7 context-insensitive points-to** (about 840M tuples): Soufflé 35 s parallel and 1:15 sequential, vs bddbddb 30 min, SQLite 6:20 h, µZ DNF. CAV'16 adds CS points-to (6:44 h, 206 GB) and Security (14:45 h). Profile: inserts 45%, membership tests 35%, range queries 15%.

**FlowLog VLDB'26 Table 1** (seconds, 4 threads | 64 threads; 2× EPYC 7543, 256 GB; TO=900 s). Selected rows:

| Program / data | FlowLog | Soufflé | RecStep | DDlog |
|---|---|---|---|---|
| TC G10K-0.001 | 78.2 \| 8.5 | 112.7 \| 39.7 | 127.0 \| 69.5 | 209.2 \| 106.4 |
| TC G20K-0.001 | 542.8 \| 42.4 | 629.6 \| 249.2 | 703.7 \| 282.7 | TO \| 476.1 |
| SG G10K-0.001 | 177.6 \| 18.6 | 379.0 \| 41.3 | 427.5 \| 161.8 | 361.9 \| 122.0 |
| Reach livejournal | 11.3 \| 5.1 | 21.5 \| 19.1 | 21.3 \| 9.1 | 112.3 \| 104.1 |
| CC livejournal | 46.0 \| 9.1 | (recursive aggregate unsupported) | 90.0 \| 28.1 | 196.1 \| 116.2 |
| CSPA httpd | 112.6 \| 14.4 | 67.8 \| 50.9 | 382.4 \| 154.3 | 319.3 \| 290.3 |
| CSDA postgresql | 11.4 \| 4.9 | 125.1 \| 40.4 | 341.5 \| 206.3 | 73.9 \| 69.2 |
| Andersen large | 16.0 \| 5.7 | 187.8 \| 69.6 | 63.7 \| 17.4 | 107.4 \| 103.0 |
| Galen | 32.2 \| 8.7 | 59.3 \| 36.8 | 486.5 \| 667.9 | 111.6 \| 64.6 |
| CRDT | 248.3 \| 62.3 | 177.7 \| 230.2 | syntax error | TO \| 482.1 |
| Polonius | 215.4 \| 41.4 | 202.4 \| 337.9 | syntax error | 583.1 \| 526.4 |
| DOOP batik | 65.2 \| 22.9 | 651.1 \| 160.2 | syntax error | 151.4 \| 126.6 |
| DDISASM z3 | 106.0 \| 27.9 | 125.9 \| 109.9 | syntax error | 769.2 \| 510.6 |

The syntax-error and unsupported entries are as reported. DuckDB and Umbra run Reach twitter at 121.6 and 189.6 s (4 threads); both OOM on large TC and SG. Loading matters: Soufflé and DDlog spend 54.0 s and 66.7 s loading Andersen's 1.2 GB input single-threaded; FlowLog spends 4.6 s.

**dynamic-datalog** (McSherry, 1 core, laptop; https://github.com/frankmcsherry/dynamic-datalog):

| Benchmark | Soufflé compiled | DD |
|---|---|---|
| CRDT | 294.73 s (+10.15 s compile) | 3.44 s (+166.26 s compile) |
| DOOP | 111.76 s | 161.58 s |
| GALEN | 198.19 s | 123.54 s |

The Soufflé interpreter did not finish on CRDT and GALEN.

**Other published numbers:**
- **STI:** 1.32–5.67× slower than compiled Soufflé. **Provenance:** 1.31× time, 1.76× memory. **Brie:** 4× faster than B-tree on points-to. **B-tree:** 3× end-to-end.
- **Ascent/Polonius** (i7-8650U; CC'22 Table 1), Datafrog vs Ascent. Naive analysis: clap-rs 12.03 s vs 11.96 s; chess-search 17.0 s vs 55.7 s. Optimized analysis: clap-rs 4.49 s vs 4.54 s; chess-search 4.64 s vs 10.2 s. Ascent vs Flix shortest paths: 56–170×.
- **GPUlog:** up to 45× over Soufflé (CSPA PostgreSQL).
- **Nemo:** LUBM-01k 163.3 s, Galen EL 3.6 s, SNOMED CT 62.1 s (VLog OOM).

### 13.3 Protocol-level targets (Dedalus → Hydroflow, Chu et al. SIGMOD'24)

(https://hydro.run/papers/david-sigmod-2024.pdf)

Setup: GCP n2-standard-4 (4 vCPU, 16 GB, 10 Gbps), 16-byte commands, closed-loop clients, 0.22 ms ping.

| Configuration | Throughput |
|---|---|
| BaseVoting (1 leader + 3 participants) | **100k cmds/s** |
| ScalableVoting (26 machines) | 250k |
| Base2PC (1 coordinator + 3 participants, disk flushes) | **30k** |
| Scalable2PC (46 machines) | 160k |
| BasePaxos (2 proposers, 3 acceptors, 3 replicas; f=1) | **50k** |
| ScalablePaxos (29 machines) | 150k |
| Scala BasePaxos (Whittaker et al.) | 25k |
| Scala CompPaxos | 130k in their run |
| Dedalus CompPaxos | 160k |

These are the numbers our Paxos and 2PC must match or beat on equivalent hardware.

---

## 14. Recommended architecture for our engine

### 14.1 Values and interning
- Every column value is a `u64` word. Typed columns are known statically. Use `u32` packing where the type allows it: bool, small enums, interned ids.
- **Interning:**
  - a global concurrent string interner (Soufflé `SymbolTable`, Nemo dictionary);
  - **hash-consed records/ADTs/tuples** to ids (Soufflé `RecordTable` with `PackRecord`/`UnpackRecord`; DDlog `Ref<'T>` with structural equality);
  - location/node ids interned.
  Equality becomes word equality, and hashing and comparison become branch-free over fixed-arity word arrays.
- **Lattice values:** fixed-size lattices (max, min, bool, counters) live inline. Large lattices (sets, maps) sit behind an id plus a side store with in-place `join_mut`.

### 14.2 Tables
- **Row store:** append-only, fixed stride (arity words) in chunked arenas. It carries parallel stamp columns: `birth: (tick, iter)`, and optionally `death_tick` for relations with a deletion path. Rows are appended in stamp order, so Δ, old and full are row ranges (§3.3).
- **Primary dedup index:** an open-addressing hash table (hashbrown `HashTable` style, as egglog uses) from key columns to row id. The hash is FxHash-like over words, with the hash stored. For lattice relations the key is the non-lattice columns, and the value column is joined in place (Ascent).
- **Secondary indexes**, chosen per relation from the search patterns of all rule versions:
  - **Sorted permuted runs** (STI-style permutation encoding, so there is one comparator). They are organized as a geometric LSM (datafrog `changed()` / DD spine: merge while `last.len() <= 2*new.len()`). Each run is a HISA-like dense array plus an optional hash directory on the leading key. They support prefix, range, galloping merge-join and leapfrog.
  - **Hash indexes** (key → small vector of row ids) for equality-only lookups. They are built **lazily** per Δ range (COLT `force()`), so a tiny Δ that is only iterated never gets an index.
  - The index set is minimized by **minimum chain cover** (§4.3). Hash indexes can't share prefixes, so apply the chain cover to the sorted indexes only.
- **Specialized representations** behind a BYODS-like trait: `eqrel` (union-find with Δ extension), `trrel_uf`, dense bitsets/Brie for dense low-arity relations, and nullary flags. The trait must enforce the "no fact skips Δ" law (§3.6).

### 14.3 IR and joins
- **Front end:** Datalog/Dedalus AST, then an SCC/stratum graph, then a **per-rule logical IR** (FlowLog-style) with structural planning:
  - rooted JSTs with the "number of distinct variables" cost;
  - filter, semijoin and antijoin pushdown;
  - SIP two-pass semijoin rewriting for cyclic or expensive rules;
  - subplan sharing by canonical hashing;
  - user `.plan` override.
- **Lowering to Free Join plans** per semi-naïve version. Left-deep index nested loop is the default. Nodes cover several atoms (Generic Join / leapfrog intersection) where the join graph is cyclic. Treefrog-style `count/propose/intersect` gives per-prefix adaptivity.
- **Physical ops** (RAM-like): scan-range, index-probe, intersect, filter, `IfExists` (stop at first), insert-if-absent, lattice-join-insert, antijoin/`NOT IN`, aggregate, emit-async (outbox), emit-next (stage for t+1), record pack/unpack, provenance-capture. Apply Soufflé's RAM transforms (MakeIndex, IfConversion, IfExistsConversion, HoistConditions, EliminateDuplicates).

### 14.4 Plan execution: interpreter plus codegen sharing one kernel library
- Write kernels as generic Rust functions over `const ARITY`/key-shape parameters. Pre-instantiate them for arities 1..N, as STI does for arity 0..30. The **interpreter** dispatches to them from a compact tree ("switch-based shadow tree"), pulling batches of about 128 tuples per virtual step to amortize dispatch.
- **Rust codegen** (build-script or proc-macro, like Ascent and FlowLog) emits straight-line calls to the same kernels for production binaries. Budget: rustc compile times are tens of seconds (Soufflé is about 10–30 s; DDlog often more than 100 s), so the interpreter must stay within about 1.5–3× of compiled.
- **Online re-planning:** precompile alternative orders per rule version. Switch on observed Δ/full cardinalities per iteration or tick (Carac, RecStep OOF), with hysteresis.

### 14.5 Fixpoint and tick loop
```
tick(t):
  1. ingest inbox (async msgs delivered at t), clocks/periodics, external inputs → Δ stamped (t,0)
  2. apply staged @next facts from t-1; apply deletions (set death=t) → negative Δ
  3. for stratum in topo order where some input relation is dirty:
       monotone stratum: semi-naïve (Soufflé k-version scheme, §3.2) seeded by Δ ranges;
                          lattice relations use Flix Δ (strictly-increased keys, full value) (§3.5)
       non-monotone stratum (negation/aggregation over changed inputs, or negative Δ present):
            maintain via signed diffs (DBSP Q^Δ, incremental distinct H) or recursive counting;
            if |Δ| > θ·|state| → recompute the stratum (elastic, PPDP'21)
  4. evaluate @next rules → stage for t+1 (persist-until-deleted recognized: no copying)
  5. evaluate @async rules → outbox with delivery metadata
  6. clear tick-local relations (drop row ranges/indexes wholesale), advance compaction frontier
```

- **Semi-naïve details:**
  - use the Soufflé versions (Δ at j, full before, full∖Δ after);
  - head `NOT IN full` before insert;
  - nullary emptiness early exit;
  - eqrel Δ extension;
  - morphism-only increments;
  - full-value Δ for non-morphism monotone functions.
- **Stratification:** stratified negation and aggregation within a tick. Negation through `@next`/`@async` is temporally stratified. Monotone aggregates are expressed as lattices and may be recursive (CC, SSSP; FlowLog's monoid diffs).

### 14.6 Parallelism
- **Level 1:** one engine instance per Dedalus location, single-threaded tick loop, lowest latency.
- **Level 2:** many instances per process (simulation, LDFI). Scenario forking uses `Arc`-shared immutable runs.
- **Level 3:** inside a tick, only when |Δ| exceeds a threshold. Morsel-driven parallel scans of the Δ (Soufflé-style outer-loop partitioning, with work stealing). Each worker writes to a **thread-local output buffer**. At the end of the iteration: a parallel sort or hash-partition, dedup against full, and append. There are no shared concurrent B-trees on the hot path. Alternatively, DD-style key sharding with exchange for very large relations (FlowLog's scaling evidence).

### 14.7 Provenance
Implement Tiers A, B and C (§11). Tier B's `(rule, height)` annotations use Soufflé's update-aware Δ and "annotation columns last" in the retrieve index. Tier C's per-rule firing logs are sliced to the goal's backward dependencies. Both work with birth and death intervals so that queries can be answered as of any tick.

---

## 15. MUST-IMPLEMENT CHECKLIST

**Evaluation core**
1. **SCC stratification and topological scheduling:** precedence graph, then SCCs; non-recursive SCCs run once, recursive ones run a fixpoint loop. *(CC'16 §4.1)*
2. **Semi-naïve with full/Δ/new per recursive relation:** `new` gets only tuples `NOT IN full`; exit when all `new` are empty; `full ∪= new`; `Δ = new`. *(CC'16 Fig. 5)*
3. **k-version delta scheme:** version j reads Δ at recursive atom j, full at earlier atoms, full∖Δ (`NOT IN Δ`) at later atoms. Ascent's mirrored linear scheme is equally valid; never use the 2^k−1 expansion. *(Soufflé `ClauseTranslator.cpp`; Ascent `ascent_mir.rs versions_base`)*
4. **Nullary recursive head early stop:** skip the rule when the head relation is already non-empty. *(Soufflé `createCondition`)*
5. **Timestamped append-only tables:** Δ, old and full are binary-searchable row ranges of a stamp-sorted table; no Δ copies. *(egglog core-relations `fast_subset`; VLog blocks §3)*
6. **Lattice relations with functional dependency:** key columns map to a lattice value; insert = `join_mut`; a changed value re-enters Δ. *(Ascent CC'22 §3.1 and codegen; egglog `:merge`)*
7. **Flix Δ for lattices:** Δ = keys whose value strictly increased, carrying the full new value. *(Flix PLDI'16 §5)*
8. **Morphism-only increment optimization:** apply a rule to Δ increments only if all lattice functions in it are morphisms; otherwise use the full value. *(Bloom^L SoCC'12 §4.1; DFIR "MTT")*
9. **⊖-based semi-naïve for min/max dioids:** delta keeps a value only if it strictly improves. *(Datalog° PODS'22 §6)*
10. **Self-computing relations obey "no fact skips Δ":** custom representations implement a `merge(new, Δ, total)` that preserves γ_Δ ⊇ γ − γ(db_τ). *(BYODS OOPSLA'23 §3–4)*
11. **eqrel:** union-find equivalence relation with densification and Δ extension by old classes. *(PACT'19 §II–III, Alg. 1)*

**Storage and indexes**
12. **Value interning:** strings, records, ADTs and locations to fixed-width ids; `u64` word columns. *(Soufflé SymbolTable/RecordTable; DDlog `Ref`; Nemo dictionary)*
13. **Minimum index set via chain cover:** searches are attribute sets; chains come from maximum bipartite matching; each chain becomes a lexicographic index. *(VLDB'18 §5, Alg. 1–2)*
14. **Permutation-encoded indexes:** store φ(t) and compare naturally, so one comparator serves every index order. *(Hu thesis §5.1.4.2)*
15. **Geometric run merging for sorted indexes:** merge while `last.len() <= 2·new.len()`, giving logarithmically many runs; amortized merge work. *(datafrog `Variable::changed`; DD arrangements VLDB'20 §4)*
16. **Galloping merge-join and galloping dedup against stable runs.** *(datafrog `join.rs`)*
17. **Lazy per-level hash indexes (COLT):** build a hash level only on the first `get`; iterate the cover relation directly. *(Free Join §4.2)*
18. **Pluggable representations** behind read/readall/write/merge traits, with concurrent variants. *(BYODS Fig. 1)*
19. **Nullary relations as flags; dense low-arity relations as bitmap tries.** *(CC'16 §5.1; Brie PMAM'19)*

**Joins and planning**
20. **Free Join plans:** nodes of subatoms with a valid cover; iterate the cover, probe the rest; binary→Free Join conversion. *(Free Join §3)*
21. **Leapfrog/Generic Join intersection** for cyclic nodes: `seek`/`next`/`atEnd`, worst-case optimal. *(LFTJ ICDT'14 §3; Skew Strikes Back)*
22. **Treefrog `count/propose/intersect` leapers** for per-prefix adaptive extension, with anti and filter leapers. *(datafrog `treefrog.rs`)*
23. **Vectorized batch probing** for large Δs. *(Free Join §4.3)*
24. **Structural join-project planning:** rooted join spanning trees, cost = max distinct variables per operator, pushdown of semijoin/antijoin/filters. *(FlowLog §5)*
25. **SIP two-pass semijoin rewriting** for cyclic or expensive rules. *(FlowLog §6)*
26. **Subplan sharing by canonical subtree hashing.** *(FlowLog §7)*
27. **Operator fusion:** Join+Map+Filter as one pipelined operator; no materialization of intermediates in left-deep chains. *(FlowLog §4; Soufflé pipelined loop nests)*
28. **RAM-level rewrites:** MakeIndex, HoistConditions, IfConversion, IfExistsConversion (first-match stop), EliminateDuplicates. *(Soufflé `src/ram/transform`)*
29. **Per-version user `.plan`** plus online re-planning on observed Δ/full cardinalities. *(Soufflé `.plan`; LOPSTR'22; Carac ICDE'24)*
30. **Join-size estimation instrumentation:** distinct counts on join attributes, per iteration for recursive relations. *(LOPSTR'22 §4)*

**Incremental and tick machinery**
31. **Lifetime analysis:** classify each relation as tick-local, static-monotone or static-nonmonotone; drop tick-local data wholesale; continue static-monotone data semi-naïvely. *(DFIR `'tick`/`'static`; Bud invalidate/rescan)*
32. **Dirty-input scheduling:** evaluate only strata and rules reachable from relations with non-empty Δ this tick. *(Bud tick loop; DFIR — generalized, [ours])*
33. **Persist-until-deleted recognition:** `p@next :- p, !del_p` sets a birth/death interval with no per-tick copying. *([ours], modeled on DD (data,time,diff))*
34. **Signed-diff IVM for non-monotone strata:** Z-sets, Q^Δ = D∘Q∘I, bilinear join rule, incremental distinct H. *(DBSP §3–4)*
35. **Recursive counting or FBF** as an alternative for deletions under recursion. *(Motik et al. AIJ'19 §5, §7)*
36. **Elastic switch to recompute** when incremental work exceeds a fraction of the last full run. *(PPDP'21)*
37. **Boolean-specialized diffs on monotone paths, lifting to ℤ only at antijoins; monoid diffs for MIN/MAX aggregation.** *(FlowLog §8–9)*
38. **Logical compaction:** discard history older than the minimal reader frontier, or keep it within EOT for LDFI. *(Shared Arrangements §4)*

**Provenance**
39. **(rule, height) annotations** with update-aware Δ, insert index excluding annotations and retrieve index placing them last. *(TOPLAS'20 §3–4)*
40. **Lazy top-down subproof search** as compiled subroutines: `matches(t, …)` with h(t_i) < h(t); an enumerate-all mode for LDFI. *(TOPLAS'20 §3.3, extended)*
41. **Per-rule firing log** (all bound variables plus tick) restricted to the goal's backward slice. *(Molly `addProvenanceRules`)*
42. **Non-existence explanation** (failed subproof per chosen rule). *(TOPLAS'20 §3.4)*

**Execution modes**
43. **Interpreter over pre-instantiated kernels** (arity ≤ N) with batch pulls of about 128 tuples. *(STI; Hu thesis §4–5)*
44. **Rust codegen reusing the interpreter's kernels.** *(Soufflé synthesis; Ascent; FlowLog)*
45. **Intra-tick parallel mode for large Δs:** morsel-parallel outer loop, thread-local outputs, parallel dedup merge. *(Soufflé §5.3; GPUlog/RecStep dedup lessons)*
46. **Eager (work-stealing, DFS-ish) evaluation mode** for rules with expensive externals. *(Formulog OOPSLA'24 §4)*
47. **Subsumption (dominance deletion)** or lattice equivalents. *(Soufflé subsumptive clauses)*

---

## 16. TEST PROGRAMS (end-to-end, with expected behaviour)

**Correctness and semantics**
1. **Soufflé CAV'16 security example** (verbatim):
   ```
   .decl E(s:Node,d:Node) input
   .decl P(node:Node) input
   .decl I(node:Node) output
   I("s").
   I(y) :- I(x), E(x,y), !P(y).
   ```
   Inputs: E = {(s,l1),(l1,l2),(l2,s),(s,l3)}, with P = {l1} for the protect call. Expected: I contains s and l3, so the vulnerable call is reachable. It must not contain l1 or l2, since the only path to l2 goes through l1.
2. **Naive ≡ semi-naïve equivalence:** random TC/SG/Andersen/CSPA inputs, compared against a naive-evaluation oracle. Also count derivations: the k-version scheme must derive each (Δ,old) combination once.
3. **Lattice SSSP (Ascent, verbatim)** and **egglog path with `:merge (min old new)`**: edges (1,2,10), (2,3,10), (1,3,30) must give `path(1,3) = 20`.
4. **Non-morphism trap:** `size(lset)` threshold over a set built in several rounds. With the increment optimization wrongly enabled, the result differs from naive. The engine must match naive (Bloom^L §4.1).
5. **Flix compactness trap:** `A(Odd). B(Even). A(x) :- B(x). R(x) :- isMaybeZero(x), A(x).` must yield `R(⊤)` (Flix §5).
6. **eqrel Δ-extension (PACT'19 Fig. 4):** current classes {a,b,c},{f,g},{d,e}; new pairs (b,f),(g,c). A downstream rule joining on the relation must see (a,f) in the same fixpoint.
7. **CC and SSSP via MIN** (RecStep rules): results match a union-find or Dijkstra oracle.
8. **Bipartite (FlowLog):** `answer()` is non-empty iff the graph has an odd cycle.

**Incremental and tick behaviour**
9. **DDlog firewall reachability:** insert 12% more edges, then delete 3%. Outputs equal a full recompute, and incremental time is well below recompute time (Ryzhyk & Budiu Fig. 8).
10. **DOOP single-fact retraction** (FlowLog demo): millisecond-scale update. Output equals a recompute.
11. **Persistence and deletion across ticks:** `p(X)@next :- p(X), !del_p(X)`. After `del_p(a)` at tick t, `p(a)` is absent from t+1 on. Derived `q :- p` retracts `q(a)` at t+1. Storage is O(changes), not O(|p|·T).
12. **Tick-local clearing:** a relation derived from channel messages is empty at the next tick unless re-derived.
13. **Many-small-ticks micro-benchmark:** state of 10^6 persisted tuples, 1–100 messages per tick. Per-tick latency must stay flat as state grows, i.e. be O(|Δ|).

**Performance (compare with §13.2 numbers on similar hardware)**
14. TC and SG on G10K/G20K-0.001; Reach and CC on livejournal/orkut; Andersen; CSPA and CSDA on httpd/linux/postgresql; Galen; CRDT; Polonius; DOOP (batik, xalan…); DDISASM. Targets: at least Soufflé-compiled speed at 4 threads, and memory at most 2× Soufflé's.
15. OpenJDK7 CI points-to class workload: Soufflé 35 s parallel.

**Provenance and LDFI**
16. **Soufflé TOPLAS'20 points-to/alias example (Fig. 1):** `alias(a,b)` has height annotation 4 and rule `r4: alias(Var1,Var2) :- vpt(Var1,Obj), vpt(Var2,Obj)`. The one-level reconstruction must return `vpt(a,l1), vpt(b,l1)`, each with height < 4 (§3.3).
    **Update-semantics test** (modeled on their Fig. 10 scenario; the exact heights are in a figure I could not extract): build an input where a tuple is first derived through a long chain and, in a later iteration, through a shorter one. Its height must decrease, it must re-enter Δ, and every dependent annotation must be re-minimized.
17. **Molly-style LDFI end-to-end:** simple-deliv/retry-deliv/2PC programs over EOT ticks. The full rule-goal graph (all derivations) must match the output of Molly's `_prov` rewrite on the same program.

**Protocols (Dedalus programs on our runtime; compare with Chu et al. SIGMOD'24)**
18. Voting (1+3), 2PC with presumed abort (1+3, with disk flush), and Paxos (2 proposers, 3 acceptors, 3 replicas). Targets: at least 100k, 30k and 50k commands/s respectively on n2-standard-4-class machines. The decoupled/partitioned variants should scale to 250k, 160k and 150k.

---

## 17. References (URLs)

- Jordan, Scholz, Subotić. *Soufflé: On Synthesis of Program Analyzers.* CAV 2016. https://souffle-lang.github.io/pdf/cav16.pdf
- Scholz, Jordan, Subotić, Westmann. *On Fast Large-Scale Program Analysis in Datalog.* CC 2016. https://souffle-lang.github.io/pdf/cc.pdf
- Subotić, Jordan, Chang, Fekete, Scholz. *Automatic Index Selection for Large-Scale Datalog Computation.* PVLDB 12(2) 2018. http://www.vldb.org/pvldb/vol12/p141-subotic.pdf
- Jordan, Subotić, Zhao, Scholz. *A Specialized B-tree for Concurrent Datalog Evaluation.* PPoPP 2019. https://souffle-lang.github.io/pdf/ppopp19.pdf
- Jordan, Subotić, Zhao, Scholz. *Brie: A Specialized Trie for Concurrent Datalog.* PMAM 2019. https://souffle-lang.github.io/pdf/pmam19.pdf
- Nappa, Zhao, Subotić, Scholz. *Fast Parallel Equivalence Relations in a Datalog Compiler.* PACT 2019. https://souffle-lang.github.io/pdf/pact2019eqrel.pdf
- Zhao, Subotić, Scholz. *Debugging Large-scale Datalog: A Scalable Provenance Evaluation Strategy.* TOPLAS 42(2) 2020. https://souffle-lang.github.io/pdf/toplas20.pdf
- Hu, Zhao, Jordan, Scholz. *An Efficient Interpreter for Datalog by De-specializing Relations.* PLDI 2021. https://souffle-lang.github.io/pldi21.html ; Hu honours thesis https://souffle-lang.github.io/pdf/xiaowenthesis.pdf
- Arch, Hu, Zhao, Subotić, Scholz. *Building a Join Optimizer for Soufflé.* LOPSTR 2022. https://souffle-lang.github.io/pdf/lopstr2022.pdf
- Zhao, Raghothaman, Subotić, Scholz. *Towards Elastic Incrementalization for Datalog.* PPDP 2021. https://psubotic.github.io/papers/PPDP.pdf
- Soufflé source: https://github.com/souffle-lang/souffle (src/ram, src/ast2ram/seminaive/ClauseTranslator.cpp, src/include/souffle/datastructure); docs https://souffle-lang.github.io/relations , https://souffle-lang.github.io/subsumption
- McSherry, Murray, Isaacs, Isard. *Differential Dataflow.* CIDR 2013. https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf
- McSherry, Lattuada, Schwarzkopf, Roscoe. *Shared Arrangements.* PVLDB 13(10) 2020. https://arxiv.org/pdf/1812.02639
- Ryzhyk, Budiu. *Differential Datalog.* Datalog 2.0 2019. http://ceur-ws.org/Vol-2368/paper6.pdf ; repo (archived) https://github.com/vmware-archive/differential-datalog
- Budiu, Chajed, McSherry, Ryzhyk, Tannen. *DBSP.* PVLDB 2023. https://arxiv.org/pdf/2203.16684
- Zhao, Yu, Rao, Frisk, Fan, Koutris. *FlowLog: Efficient and Extensible Datalog via Incrementality.* PVLDB 19(3). https://arxiv.org/pdf/2511.00865 ; Yu, Zhao, Hou, Koutris, *FlowLog: Re-thinking Datalog for Fast and Extensible Static Analysis*, SPLASH Companion 2026. https://arxiv.org/pdf/2607.23971
- Fan, Zhu, Zhang, Albarghouthi, Koutris, Patel. *Scaling-Up In-Memory Datalog Processing (RecStep).* PVLDB 12(6) 2019. https://arxiv.org/pdf/1812.03975
- Fan, Mallireddy, Koutris. *Towards Better Understanding of the Performance and Design of Datalog Systems.* Datalog 2.0 2022. https://ceur-ws.org/Vol-3203/paper10.pdf
- Sahebolamri, Gilray, Micinski. *Seamless Deductive Inference via Macros (Ascent).* CC 2022. https://thomas.gilray.org/pdf/seamless-deductive.pdf ; https://github.com/s-arash/ascent
- Sahebolamri, Barrett, Moore, Micinski. *Bring Your Own Data Structures to Datalog.* OOPSLA 2023. https://kmicinski.com/assets/byods.pdf
- Crepe: https://github.com/ekzhang/crepe ; datafrog: https://github.com/rust-lang/datafrog ; McSherry blog (datafrog/treefrog) https://github.com/frankmcsherry/blog ; dynamic-datalog https://github.com/frankmcsherry/dynamic-datalog
- Zhang, Wang, Flatt, Cao, Zucker, Rosenthal, Tatlock, Willsey. *Better Together: Unifying Datalog and Equality Saturation (egglog).* PLDI 2023. https://arxiv.org/pdf/2304.04332 ; source https://github.com/egraphs-good/egglog (core-relations, egglog-bridge)
- Wang, Willsey, Suciu. *Free Join.* SIGMOD 2023. https://arxiv.org/pdf/2301.10841
- Veldhuizen. *Leapfrog Triejoin.* ICDT 2014. https://arxiv.org/pdf/1210.0481v5 ; *Incremental Maintenance for Leapfrog Triejoin.* 2013. https://arxiv.org/pdf/1303.5313
- Ngo, Ré, Rudra. *Skew Strikes Back.* SIGMOD Record 2013. https://arxiv.org/pdf/1310.3314
- Abo Khamis, Ngo, Pichler, Suciu, Wang. *Convergence of Datalog over (Pre-)Semirings.* PODS 2022. https://arxiv.org/pdf/2105.14435
- Madsen, Yee, Lhoták. *From Datalog to Flix.* PLDI 2016. https://plg.uwaterloo.ca/~olhotak/pubs/pldi16.pdf
- Conway, Marczak, Alvaro, Hellerstein, Maier. *Logic and Lattices for Distributed Programming (Bloom^L).* SoCC 2012 (local copy from the project's research corpus).
- Arntzenius, Krishnaswami. *Seminaïve Evaluation for a Higher-Order Functional Language.* POPL 2020. https://www.cl.cam.ac.uk/~nk480/seminaive-datafun.pdf
- Motik, Nenov, Piro, Horrocks. *Maintenance of Datalog Materialisations Revisited.* AIJ 2019. https://www.cs.ox.ac.uk/boris.motik/pubs/mnph19maintenance-revisited.pdf ; *Incremental Update of Datalog Materialisation: the Backward/Forward Algorithm.* AAAI 2015. https://www.cs.ox.ac.uk/people/ian.horrocks/Publications/download/2015/MNPH15b.pdf
- Urbani, Jacobs, Krötzsch. *Column-Oriented Datalog Materialization for Large Knowledge Graphs (VLog).* AAAI 2016. https://arxiv.org/pdf/1511.08915
- Ivliev et al. *Nemo: First Glimpse of a New Rule Engine.* ICLP 2023. https://arxiv.org/pdf/2308.15897 ; *Nemo: Your Friendly and Versatile Rule Reasoning Toolkit.* KR 2024. https://iccl.inf.tu-dresden.de/w/images/f/fb/KR-2024-CR.pdf
- Sun, Shovon, Gilray, Kumar, Micinski. *Optimizing Datalog for the GPU.* ASPLOS 2025. https://thomas.gilray.org/pdf/datalog-gpu.pdf
- Herlihy, Martres, Ailamaki, Odersky. *Adaptive Recursive Query Optimization (Carac).* ICDE 2024. https://arxiv.org/pdf/2312.04282
- Bembenek, Greenberg, Chong. *Making Formulog Fast.* OOPSLA 2024. https://arxiv.org/pdf/2408.14017
- Samuel, Cheung, Hellerstein. *Hydroflow: A Model and Runtime for Distributed Systems Programming.* Tech report 2021. https://hydro.run/papers/hydroflow-thesis.pdf ; DFIR operator docs https://hydro.run
- Chu et al. *Optimizing Distributed Protocols with Query Rewrites.* SIGMOD 2024. https://hydro.run/papers/david-sigmod-2024.pdf
- Alvaro, Rosen, Hellerstein. *Lineage-driven Fault Injection (Molly).* SIGMOD 2015; source https://github.com/palvaro/molly (DedalusRewrites.scala, derivations/ProvenanceReader.scala)
- Bud source: https://github.com/bloom-lang/bud (lib/bud.rb `prepare_invalidation_scheme`, `tick_internal`)
