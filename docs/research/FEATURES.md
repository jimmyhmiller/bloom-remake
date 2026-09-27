# bloom-remake: Master Feature Specification

This is the output of the research phase and the input to the design phase. It deduplicates and organizes
every feature named in research reports 01–10 and in the gap reports (`NN-gap-*.md`). It also fixes the semantics wherever the source papers
disagree (§1). The last section lists the design choices that are still open.

## 0. How to read this document

- **IDs** have the form `AREA-NNN`. The areas are LANG, SEM, ENG, DIST, ANA, TEST, VER, LIB, FLAG and BENCH. IDs
  are stable. Gaps in the numbering are left on purpose so later features can be inserted.
- **Priority:**
  - **P0** is core: without it the language, the runtime or a flagship system does not work.
  - **P1** is required for full coverage of the BOOM-lineage literature. The user asked for "full support for
    everything", so every P1 item is in scope.
  - **P2** is nice to have, a stretch goal, or beyond the literature.
- **Sources:** `Rnn` means `docs/research/nn-*.md`. The text in parentheses after it is the paper tag and section,
  written as the report writes it, or the report's own checklist ID (for example R03 `BB-26`, R06 `D24`, R02 `#12`).
  `[ours]` marks a design inference made in a report, not a claim taken from the literature.
- `CR-nn` is a conflict resolution in §1. `ODD-nn` is an open design decision in §13.

| Tag | Report |
|---|---|
| R01 | 01-overlog-p2-ndlog.md: Overlog, P2, NDlog, Evita Raced, JOL, C4 |
| R02 | 02-dedalus.md: Dedalus, its stable-model and operational semantics, confluence, CRON, Molly's dialect |
| R03 | 03-bloom-bud.md: Bloom, Bud, CALM in Bloom, bud-sandbox, BloomUnit |
| R04 | 04-bloom-lattices.md: Bloom^L, Anna, CRDTs, Katara, the Hydro `lattices` crate, Flo |
| R05 | 05-calm-blazes-edelweiss.md: CALM theory, Blazes, Edelweiss, I-confluence |
| R06 | 06-molly-ldfi.md: LDFI and Molly, Netflix, Nemo, provenance foundations |
| R07 | 07-boom-analytics-consensus.md: BOOM-FS, BOOM-MR, Overlog Paxos, Raft in declarative languages |
| R08 | 08-hydro.md: Hydroflow/DFIR, the Datalog front end, Hydro, Flo, the SIGMOD'24 rewrites |
| R09 | 09-fast-datalog-engines.md: Soufflé, DD/DBSP/FlowLog, Free Join, Rust engines, provenance tiers |
| R10 | 10-verification-modern-hadoop.md: the verification stack and the successor to Hadoop |
| R11/G1 | 11-gap-1.md: Dedalus^L, a formal semantics for Dedalus with lattice-typed columns. It covers the TPLP transition with ⊔, L-stratification, L-stable models (after Ross–Sagiv §5.5), whether TPLP Thm 4 and Dedalus+ confluence carry over, lattice encodings for ASP/clingo and FOL/EPR, and PosBool(X)⊗𝓛 provenance with exact threshold supports for LDFI |
| R12/G2 | 12-gap-2.md: nondeterministic and order-sensitive operators under persistence, replay and incremental maintenance. Sources: Saccà–Zaniolo PODS'90, Greco–Zaniolo (JICSLP'98, TPLP'01), Giannotti et al. DOOD'91 and JCSS'01, GSZ ICDT'95, LDL++, Soufflé choice-domain (APLAS'21), Bud `aggs.rb`/`group.rb`, the dfir-datalog lowering of `choose`/`index()`, DD/DBSP/Materialize/Feldera. Topics: seeded and sticky choice, numbering, ordered folds, record/replay, the exact oracle |
| R13/G3 | 13-gap-3.md: Power's algebraic line. Wrapping Rings in Lattices (PaPoC'24), Free Termination (ICDT'25), and Power's dissertation (EECS-2025-103), including OnceTree and Emmy. Also Dolan's undoable-CRDT theorem and Hydro's retries/idempotence typing. Topics: exactly-once wrappers for Z-set/group deltas, and finality analysis. |
| R14/G4 | 14-gap-4.md: MapReduce Online / HOP (NSDI'10, SIGMOD'10 demo, Condie's dissertation Ch. 10, HOP v0.2 source): pipelined shuffle, online aggregation, snapshot pipelining, continuous jobs, alert-based speculation |
| R15/G5 | 15-gap-5.md: production hardening. Security: SeNDlog, LBTrust, SecureBlox, Binder; etcd, CockroachDB, Flink, Timely and Hydro Deploy identity; SPIFFE; Biscuit. Upgrades: Ajmani/Liskov/Shrira, the SOSP'21 upgrade-failure study with DUPTester/DUPChecker, UpFuzz, Erlang/OTP, Protobuf/Avro evolution, etcd/CockroachDB/hashicorp-raft rolling upgrades, Hydro multi-version simulation |

**Product in one paragraph.** We are building a standalone, statically typed, Dedalus-semantics logic language with
Bloom-style collections and operators and first-class lattices. It comes with:

- a Rust compiler with two backends, an interpreter and code generation, over one fast incremental Datalog kernel;
- a distributed runtime;
- CALM, Blazes and Edelweiss analyses;
- a deterministic simulator;
- an LDFI engine ("Molly-2") with full provenance;
- bounded and unbounded verifiers;
- a standard library at bud-sandbox parity and beyond;
- flagship systems: full Raft, Multi-Paxos, 2PC/3PC, an Anna-style KVS, BOOM-FS, BOOM-MR, and a modern Hadoop successor.

---

## 1. Adopted semantics: cross-paper conflicts and how we resolve them

Each resolution below is normative, and later sections rely on it.

**CR-01 Core semantic model.**
- *Positions.*
  - Overlog/P2 used a "chain of fixpoints": one external event per fixpoint, ECA-style rules, deletes and inserts
    deferred to the end of the fixpoint (R01 §4.4).
  - JOL used a three-phase timestep with per-stratum queues (R01 §4.4; R07 §2.1).
  - Bud uses a three-phase tick over a batch of inputs (R03 §3.1).
  - Dedalus reifies time as data, with deductive, inductive and async rules. Its operational semantics provably
    coincides with its stable-model semantics (R02 §6, TPLP Thm 4).
- *Adopted.* Dedalus with the TPLP operational semantics is **the** semantics. Bloom's tick phases are its
  operational description. Every Overlog and Bloom construct is sugar compiled into it (ENG-001).
- *Why.* It is the only semantics that is formally specified and proven. Overlog's ambiguous update and aggregate
  semantics caused most of BOOM's bugs (R01 §9; R07 §8). Final P2 already made mutations visible only in the next
  fixpoint (seAtomicity, R01 §4.4).

**CR-02 Batch per tick vs. one event per fixpoint.**
- *Positions.* P2 admits exactly one external event per fixpoint (R01 §4.4). Bloom, Dedalus and DFIR ingest a whole
  batch per tick.
- *Adopted.* A tick ingests a batch. Programs that need one-at-a-time atomicity use the atomic-dequeue library idiom
  (LIB-061) or an ordered fold (LANG-110). Overlog's "at most one event per rule" restriction is dropped.
- *Why.* Batching is what the formal semantics and every modern runtime assume. The Symphony-style atomicity case
  can still be written declaratively (R01 §9 item 4, §10.2).

**CR-03 Set vs. bag semantics for events and messages.**
- *Positions.* P2 treats events as bags, so self-sent and remote pings behave differently (NR09 Fig. 2; R01 §8.8).
  Dedalus, TPLP and Bloom use sets per tick.
- *Adopted.* Every relation, including a delivered channel batch, is a set per tick. Programs that need multiplicity
  carry an explicit id or count field. In NR09 Fig. 2, every node increments exactly once (SEM-013).
- *Why.* TPLP uses "set sending semantics" (R02 §5.3). Sets remove the asymmetry between self-sends and remote sends.

**CR-04 Update visibility.**
- *Positions.* NR09 Fig. 1 is ambiguous: does a message carry the old or the new sequence number?
- *Adopted.* Every read in tick t sees the state at t. Mutations land at t+1. A message derived at t carries the
  pre-update value.
- *Why.* This follows directly from CR-01, and final P2 behaves this way.

**CR-05 Insert and delete of the same fact for the same next tick.**
- *Positions.*
  - Dedalus: the insertion rule re-derives the fact, so **insert wins** (R02 §3.3).
  - Bud: `BudTable#tick` applies deletions and then pending insertions, so insert wins as well (R03 §2.3).
  - Relational transducers: the conflicting pair is a no-op (R02 §10.1).
- *Adopted.* Insert wins.
- *Why.* Two of the three sources agree, it needs no special-casing, and `<+-` depends on it.

**CR-06 Deletion.**
- *Positions.* P2 `delete` removes the exact tuple at the end of the fixpoint. Bud `<-` removes the exact tuple at the
  next tick. Dedalus `p_neg` blocks persistence at the next tick.
- *Adopted.* Deletion matches the exact tuple and takes effect at t+1. Deleting by key is done with `<+-` or with an
  explicit key join.
- *Why.* The three positions are the same model.

**CR-07 Primary-key collisions.**
- *Positions.*
  - P2 and JOL: a new value for an existing key replaces the old one, an implicit delete plus insert. With two
    key-conflicting deltas the result is nondeterministic (NR09 `Update`; R01 §4.4).
  - Bud: two distinct tuples with the same key in the same tick raise `KeyConstraintError`. If every differing
    column is lattice-typed, the lattice columns merge instead (R03 §2.6).
- *Adopted.* Bud's rule applies (SEM-050). Intentional overwrite is written only as `<+-`, which is a next-tick upsert.
  Two different upserts to the same key for the same tick are an error. The Overlog frontend compiles key overwrite
  into `<+-`.
- *Why.* This gives a deterministic, explicit update semantics. The same-tick self-overwrites in I Do Declare only
  make sense under a next-state reading anyway (R07 §4.5).

**CR-08 Aggregation over an empty input.**
- *Positions.* In P2, a per-event aggregate emits one row per event even when the join is empty: count 0, min/max
  `null`. Narada R5/R6 and Chord l3/l4 depend on this (R01 §4.3). Dedalus, Bloom and SQL use GROUP BY: an empty group
  produces no row (R02 §14.20).
- *Adopted.* GROUP BY semantics. Aggregates with an explicit default, driven by an outer relation (LANG-106), recover
  the P2 idioms.
- *Why.* It matches the formal semantics and keeps the per-event behavior as an explicit, analyzable construct.

**CR-09 Aggregation in stratification.**
- *Positions.* Evita's checker tracks only `notin` (R01 §4.7). Bud treats `group` as non-monotone. Dedalus says nothing,
  so the analysis in R02 §3.4 treats it as negative.
- *Adopted.* A non-lattice aggregate is a negative edge. Monotone aggregation is written with lattices (thresholds,
  `lmax`, `lset.size`), which may be recursive (SEM-021).
- *Why.* This is Mumick–Shmueli stratification plus Bloom^L.

**CR-10 Scope of stratification.**
- *Positions.*
  - Bud: only `<=` edges are considered.
  - Dedalus: the deductive reduction must be stratified.
  - Dedalus_S forbids negation cycles through temporal edges too, so it is strictly more restrictive.
  - Old DFIR puts a barrier at every blocking input.
- *Adopted.* Temporal stratification of the deductive reduction is the acceptance rule (SEM-020). Dedalus+ and
  Dedalus_S are confluence **certificates** (ANA-025/026), not acceptance rules.
- *Why.* Real protocols such as Molly `2pc.ded` are temporally stratified but not statically stratified (R06 §3.4).

**CR-11 Recursion inside a tick.**
- *Positions.* Current DFIR rejects cycles within a tick (R08 §4.6). Dedalus and Bloom need recursion to fixpoint
  inside a tick.
- *Adopted.* In-tick recursion is supported, evaluated semi-naively (ENG-042).
- *Why.* Datalog requires it.

**CR-12 Semantics of async delivery.**
- *Positions.*
  - TR/DL11: arrival may be ⊤ (lost) and may even precede the send.
  - TPLP, MAR and CRON: delivery is causal, finitely many messages arrive per step, and every message is eventually
    delivered (fair).
  - Molly: synchronous delivery at t+1, or NEVER.
  - Bud: UDP best effort.
- *Adopted.* The normative semantics for correctness and confluence reasoning is TPLP fair causal delivery (SEM-040).
  Loss and crashes are **fault models**: they describe runtime reality and drive testing (SEM-043, TEST-020).
  Synchronous rounds are the LDFI mode. Non-causal replay is used only for recovery, and only where CRON Thm 6.1
  justifies it.
- *Why.* TPLP Thm 4 ties the semantics to fair runs. Putting loss inside the semantics would make "correct"
  meaningless without a fault model.

**CR-13 Time domain and origin.**
- *Positions.* Dedalus TR uses ℤ; TPLP uses ℕ from 0; Molly uses 1..EOT; Bud's `budtime` starts at 0.
- *Adopted.* Each node counts ticks in ℕ starting at 0. Tick 0 is bootstrap. Molly round k maps to our tick k, so
  Molly's `@1` facts land at the first tick after bootstrap. The time column exists only in the IR.
- *Why.* This reproduces the Molly corpus verbatim and keeps the formal ℕ domain.

**CR-14 Location-specifier syntax.**
- *Positions.* The papers differ:
  - SOSP05 annotates the predicate.
  - SIGMOD06 marks every address field with `@`.
  - CACM09 and later P2 mark one `@` field, in any position.
  - Dedalus uses `#` on the first column.
  - Molly makes the first column the location, implicitly.
  - Bloom puts `:@addr` on channels only.
  - Hydroflow writes the head as `rel@addr(...)`.
- *Adopted.* Local state implicitly lives at `self`, as in Bloom. Channels and messages carry exactly one declared
  `@` column, in any position. Async rule heads may also use the head-location form. The IR normalizes the location
  to the first column. Overlog-style located tables exist only in the Overlog frontend and compile to channels.
- *Why.* This is the least surprising form, and it enforces body locality structurally.

**CR-15 Rule bodies that span several locations.**
- *Positions.* Overlog allows them and localizes them (R01 §5.6). Dedalus forbids them. BOOM avoided them because they
  have no semantics under failure (R07 §8).
- *Adopted.* The IR forbids them. The surface language accepts them only as sugar with an explicit localization
  rewrite plus a lint (LANG-095, ODD-11).

**CR-16 EDB conventions.**
- *Positions.* In TR, DL11 and Molly, facts are timestamped events. In MAR, TPLP and CRON, facts hold at every step
  (R02 §14.4).
- *Adopted.* Both exist. `static` relations and unannotated program facts hold at every tick. `@k` facts and host
  inputs are events.

**CR-17 Soft state.**
- *Positions.* P2 has a lifetime and max size per table, expires lazily on access, and refreshes on re-derivation
  (R01 §2.3). JOL has no soft state; programs use a timer plus delete. Dedalus writes a TTL guard over `now()`.
- *Adopted.* The `soft table` sugar compiles to TTL-guarded persistence. Expiry is deterministic at tick boundaries
  against the tick's sampled `now` (SEM-060).
- *Why.* Lazy expiry makes deletion deltas depend on access timing (R01 §2.3 item 3).

**CR-18 Wall clock and randomness.**
- *Positions.* P2's `f_now()` is read at evaluation time, and BOOM-MR calls `currentTimeMillis()` inside rules. Bud's
  `bud_clock` is stable within a tick. Dedalus treats the clock as a foreign function.
- *Adopted.* `now` and `random` are sampled once per tick as inputs and recorded for replay (LANG-171, LANG-174).
  Rule bodies make no ambient impure calls.
- *Amended by R12/G2.* `random` and `rand(k̄)` are keyed PRF values of the seeds, node, incarnation and tick.
  Replay records seeds and incarnations, not individual draws (SEM-084, LANG-175, DIST-033). `now` is still
  sampled and recorded.

**CR-19 Timers.**
- *Positions.* P2 has `periodic(secs, count)`. JOL has physical and logical timers. Bud has best-effort `periodic`.
  Molly has the logical `timeout_svc`.
- *Adopted.* Timers are input relations and come in physical and logical flavors. Simulation uses a virtual clock
  (LANG-173, ODD-16).

**CR-20 Crash semantics.**
- *Positions.* The LDFI paper says a crashed node stops sending and stops internal transitions. The Molly code keeps a
  crashed node computing and only stops its sends, and the `crash` oracle is visible everywhere (R06 §3.4).
- *Adopted.* The paper's semantics: no rule firings and no sends from the crash tick on. The frozen state is visible
  only to spec rules. `crash` is a spec-only oracle (SEM-070, ANA-010).

**CR-21 LDFI EFF boundary.**
- *Positions.* The paper uses "≤ EFF" in one place and "< EFF" in another; the code uses strict `<`.
- *Adopted.* An omission is allowed iff 1 ≤ send tick < EFF.
- *Why.* It reproduces Molly's published verdicts.

**CR-22 LDFI leaves that are not clocks.**
- *Positions.* Algorithm 2 as printed returns `true`; Molly's Z3 implementation returns `false`.
- *Adopted.* `false`. An unfalsifiable premise contributes nothing to the OR (R06 §3.6).

**CR-23 What "morphism" means.**
- *Positions.* SoCC'12 requires f(⊥)=⊥ plus join preservation. Hydro requires join preservation only.
- *Adopted.* Join preservation. The engine always evaluates a morphism on the full value in the first round, so
  preserving ⊥ is irrelevant to correctness (R04 §3.4).

**CR-24 Lattice persistence.**
- *Positions.* In Bud, lattices always persist. Hydro offers both `'tick` and `'static`.
- *Adopted.* Persistent by default, with tick-scoped (`scratch`) lattices available (LANG-128). The analysis treats
  tick-scoped lattices like scratches.

**CR-25 Dominating and lexicographic pairs.**
- *Positions.* Anna and Katara present the dominating pair as a lattice. Hydro documents and tests that it is not
  associative (R04 §4.5).
- *Adopted.* A proper lexicographic pair with a chain-typed key (LANG-131), plus a separate antichain type for
  multi-value semantics (LANG-132). `DomPair` exists only as `unsafe` (LANG-136).

**CR-26 Replay of persistent state.** *(Amended by R12/G2.)*
- *Positions.*
  - DFIR `'static` joins replay their full output every tick.
  - In Dedalus, persisted facts exist at every tick, so derivations from them recur.
  - Bud caches results and invalidates the caches. Its `choose`/`choose_rand` state survives ticks that only insert,
    and is rebuilt in storage order after any deletion from the input table (R12 §3.7).
  - Hydro's Datalog `choose` over a persisted body keeps the first value it saw, forever. Its `index()` over a
    persisted body is arrival numbering (R12 §3.8).
  - Soufflé's choice-domain keeps the first tuple in evaluation order (R12 §3.6).
- *Adopted.* The **semantics** is Dedalus: derivations recur at every tick in which their body holds. The
  **implementation** maintains deltas (ENG-004). It must be **observationally identical** to naive per-tick
  re-evaluation of the program's Dedalus expansion (ENG-067). **No observable difference is permitted.** Four rules
  make this achievable:
  1. Every nondeterministic or order-sensitive operator is defined by a Dedalus expansion over the canonical order
     and a seeded PRF (SEM-083, SEM-084). Its output is a function of the tick's input set, its explicitly carried
     state and the seeds. It never depends on iteration, arrival, interning or cache order.
  2. `choose` and `index()` are per tick: the tick is in the FD's determinant (SEM-085). Carry-over exists only in
     the explicit `choose sticky` and `seq()` forms, whose state is part of their expansion (LANG-115, LANG-098).
     Persist-pullup rewrites never cross these operators (ENG-069).
  3. Async sends derived from persistent state are re-sent every tick (ODD-05). Suppression is allowed only where it
     is proven equivalent (DIST-007). Host callbacks declare whether they want full contents or deltas (LANG-185).
  4. Replay needs only inputs, seeds, incarnations and overrides. Choices and draws are recomputed, and per-tick
     digests detect divergence (TEST-010, TEST-011).
- *Why.*
  - Only functions can be maintained incrementally with retractions (DBSP; Feldera "assumes that all computations
    are deterministic").
  - DD stores its produced output "to avoid potential non-determinism" in user logic (Shared Arrangements §5.3).
  - Making a choice a function of the seed keeps `choose` in the language without giving up exact replay, exact
    oracles or comparable LDFI runs (R12 §2, §9).

**CR-27 Blazes ambiguities.**
- *Positions.* The papers differ on:
  - the label name (Diverge vs. Split);
  - the scope of `protected`;
  - seal consumption;
  - seal propagation through confluent paths;
  - a seal entering an incompatible OR path.
- *Adopted.* The R05 §4.4.1 resolutions are adopted verbatim, including rule (1′). `OR*`/`OW*` is never compatible.

**CR-28 Typing.**
- *Positions.* P2 and NDlog infer arity and types. JOL and C4 require typed schemas. Bud is dynamic and pads short
  tuples with nil. Molly infers INT/STRING/LOCATION.
- *Adopted.* Relation schemas are declared and statically typed, and types are inferred inside rules. An arity
  mismatch is a compile error. There is no nil padding; nullable columns use `Option`.

**CR-29 Coordination-free, confluent, consistent.**
- *Positions.* Ameloot's definition requires that a heartbeat-only run exist. Blazes and CALM define consistency as
  confluence. Consistency under fairness is strictly stronger (R10 A1(4)).
- *Adopted.* Certificates report "confluent" and "consistent under fair runs" separately (ANA-029, SEM-044). Ameloot's
  "message join" program is a regression test for this (BENCH-091).

**CR-30 Missing pre/post.**
- *Positions.* The LDFI paper says to fall back to all persistent relations. The code requires `pre`/`post` to exist.
- *Adopted.* Both are required, and a missing one is a clear error (TEST-022).

**CR-31 Negative support in LDFI.**
- *Positions.* Molly's flag name is inverted, and the feature is on by default.
- *Adopted.* Conservative negative support is on by default (TEST-025).

**CR-32 Pipelined map output: tentative-until-commit vs. sequence-numbered idempotent spills.**
- *Positions.*
  - The HOP papers (NSDI'10 §3.3, SIGMOD'10 demo §3.3) keep each map attempt's output "tentative" until the JobTracker
    says the attempt committed. A failed attempt's spills are discarded.
  - The NSDI talk ("Revised PFT design") and the HOP v0.2 code key the reducer's cursor by the *logical* task and
    accept a spill only if its id is the next one expected. Any attempt's copy counts. Failure events are a no-op in
    the code.
  - The revised design is sound only if map output is deterministic. The code also has a merged-range straddle hazard
    (R14/G4 §5.3).
- *Adopted.* Both, layered.
  - When the map is certified deterministic (ANA-037), identity-keyed exactly-once applies: set semantics over
    `(task, spill, idx)`, or disjoint cursor ranges for combined partials (DIST-011, FLAG-107).
  - Otherwise, per-attempt tentative partials released by a commit seal apply (FLAG-108).
  - Merge ranges always start at the cursor the destination acknowledged.

**CR-33 Are snapshots "monotonic"?**
- *Positions.* HOP TR09 §4.2 says "the snapshots produced by j1 are monotonic". NSDI'10 §4.2 says instead that the
  output of a reduce "is not 'monotonic'", and that later snapshots "are taken from a superset of the mapper output".
- *Adopted.* The snapshot's *input* only grows. Its *output* is a chain of lower bounds only for class-L reducers.
  Class-A and class-H snapshots are estimates (ANA-036).
  - Downstream consumers keep the snapshot with the highest progress through `LexPair<Max<progress>, Snapshot>`, which
    is monotone whatever the reducer class (FLAG-136).
  - Only the final snapshot is deterministic.

**CR-35 Who supplies idempotence: payload or channel.**
- *Positions.*
  - R04 and DIST-005..007 ship only lattice deltas, which are idempotent, so duplicates are harmless.
  - R09, R10 and ODD-06 introduce DBSP Z-set diffs but say nothing about sending them.
  - R14's LANG-112 and DIST-011 require exactly-once identities for non-idempotent shuffle partials only.
- *Adopted.* A payload is either a lattice value, which may travel at-least-once and be merged or relayed freely,
  or a group or ring value (Z-set, counter, sum, moment tuple), which travels **only** through an exactly-once
  wrapper (LANG-158, DIST-015..017). No channel is assumed to deduplicate, and no group type may be declared a
  lattice (LANG-142).
- *Why.* An idempotent abelian group is trivial, and so is an inflationary one (R13 §3.1; Wrapping Rings §3,
  Strawmen 1–2, App. B). Signed diffs therefore cannot be made safe by the lattice machinery alone.

**CR-36 Quiescence vs. termination.**
- *Positions.* SEM-010 defines quiescence and uses it to stop simulation and model checking. BloomUnit, Molly and
  model checkers also stop "when nothing changes".
- *Adopted.* Quiescence is observable only by a global observer (simulator, model checker). A running node never
  infers completion from silence. It declares an output final only when its state is a free-termination state for
  that output (SEM-016), which it establishes by ANA-120..122, including through seals.
- *Why.* A grow-only set replica cannot detect that it has everything (Free Termination §1.1). (Q, I) is
  coordination-free-correct iff I is a free-termination state (FT Thm 22).

**CR-40 Where security checks are enforced.**
- *Positions.*
  - SeNDlog's authenticated PSN discards a tuple whose signature fails, per tuple, in a `SigChecker` operator
    (R15/G5 §2.2).
  - SecureBlox processes each batch of facts from a remote node in one local ACID transaction. A violated
    runtime constraint, such as a bad signature or a missing `writeAccess`, rolls back the whole transaction,
    including the input tuples (R15/G5 §2.4).
  - LBTrust makes the entire evaluation fail through `fail()` constraints.
- *Adopted.* Authentication, identity binding, version and schema checks, channel ACLs and quotas are enforced
  **per message at ingress, before the tick** (DIST-062). A rejected message is dropped, counted and audited. It
  is an omission (SEM-090). Authorization that depends on data is ordinary rules over the `principal` column
  (LANG-244). Its denials are program outputs, not drops.
- *Why.*
  - A tick batch mixes senders (SEM-002), so a rollback would let one bad message suppress good ones.
  - A rollback would also need a second staging area in front of the durability barrier (SEM-072).
  - A drop is already a legal behavior under lossy delivery (SEM-043), so every safety argument, CALM
    certificate and LDFI verdict carries over unchanged.

**CR-41 Where version identity lives.**
- *Positions.*
  - Cassandra carried the peer version only in its connection handshake, and a gossip message that raced the
    handshake was misclassified (CASSANDRA-6678).
  - HDFS versions whole image files (`LayoutVersion`).
  - Hydro's multi-version simulator matches channels by name.
  - Ajmani's upgrade layer labels every call with the caller's version (R15/G5 §5.1–5.2, §5.6).
- *Adopted.* All of them, layered:
  - the post-TLS hello frame negotiates the program version window and per-channel schema ids;
  - **every frame** carries its channel schema id;
  - every WAL segment and checkpoint header carries the storage format version, the program version and
    per-relation schema hashes (DIST-080, DIST-081).
- *Why.* SOSP'21: "the version ID should be used in all messages". Recovery must be able to tell, without
  guessing, whether it can read what is on disk.

**CR-45 Resolving choice.**
- *Positions.*
  - Declaratively, choice may denote *any* stable (choice) model (Saccà–Zaniolo PODS'90; Greco–Zaniolo TPLP'01).
  - Bud keeps the first value pushed. Soufflé keeps the first in evaluation order. Hydro keeps the first in dataflow
    order, and is sticky over persisted bodies.
  - Bud's `choose_rand` is a reservoir sample over a process-global RNG.
  - Greedy choice (GZ01) takes the least cost and breaks ties arbitrarily.
- *Adopted.*
  - `choose` takes the least seeded priority `(PRF_σc(site, X̄, Ȳ), Ȳ)` among the tick's candidates. The choice seed
    σc is shared by all nodes (SEM-085, ODD-38).
  - `choose_least`/`choose_most` order by cost first (LANG-114).
  - `choose_rand` and `rand` add the node, the incarnation and the tick to the key.
  - Stickiness exists only in explicit forms (LANG-115, ODD-39).
- *Why.*
  - It selects a genuine choice model (GZ01 Thm 5.3).
  - It is deterministic and replayable from the seed, and independent of plan and iteration order.
  - Its incremental form is an argmin index (ENG-068).
  - All nodes that see the same candidates agree, as in rendezvous hashing.
  - A fault injection changes a choice only when it removes the chosen candidate (R12 §5.15).

**CR-46 Numbering and folding over persistent inputs.**
- *Positions.*
  - Hydro `enumerate::<'tick>` restarts at 0 each tick. `enumerate::<'static>` counts up in arrival order.
  - dfir-datalog numbers body rows, not distinct head tuples.
  - Bud says `each_with_index` is undefined by the language.
  - DD's `identifiers` hashes records.
  - Dedalus replay re-applies a carried fold to every row of a persistent input every tick.
- *Adopted.*
  - `index()` is a dense per-tick rank in `(key, canonical)` order, computed after the head is deduplicated
    (LANG-097).
  - `seq()` is stable arrival numbering with an explicit high-water-mark expansion (LANG-098).
  - `fold_ordered` is a per-tick pure left fold in the same order (LANG-110).
  - Carried folds over persistent inputs are linted (ANA-011).
- *Why.* Each is a function of its input set, plus explicit state for `seq`. There are two numbering needs: "rank
  within this tick's batch" (Paxos slots, FLAG-021) and "permanent id" (LIB-064). Neither is silently substituted for
  the other.

**CR-50 Normative semantics of lattice-valued relations (Dedalus^L).**
- *Positions.*
  - Bloom^L has no formal semantics. SoCC §3.6 and Conway's dissertation §3.2 (p. 24) leave it as future work.
    TPLP covers only set-valued Datalog¬, and its §6 lists aggregation as future work.
  - Ross–Sagiv define lattice-ordered least models for monotone aggregation. For them, two heads that differ only in
    the cost argument are an inconsistency (Def. 2.6), and they accept limit values, even transfinite ones (§6.2).
  - Flix merges each cell by lub, requires finite height, and has no negation.
- *Adopted.* Dedalus^L (R11/G1 §3, D1–D13), specified in SEM-100–SEM-109:
  - Every relation is lattice-valued under its key FD. A set relation is the case over 𝔹, and instances are
    ⊥-normal maps.
  - The TPLP transition is used unchanged, with ∪ replaced by the cellwise ⊔.
  - L-stratification is computed from the polarity of each occurrence.
  - Each stratum is evaluated to a finite Kleene fixpoint.
  - L-stable models are defined by the Ross–Sagiv §5.5 reduct, which reduces only exact literals.
- *Why.* It is the smallest extension of the proven TPLP semantics. It coincides with TPLP on every set-only program
  and with FLP answer sets on the monotone fragment.

**CR-51 Same-key derivations: merge or error.**
- *Positions.* Ross–Sagiv: conflicting cost values are an inconsistency (Defs 2.6, 2.10). Bloom^L and Bud
  `merge_to_buf`: lattice columns merge, and a conflict on a non-lattice payload raises an error.
- *Adopted.* Lattice values merge. SEM-050 still governs non-lattice payloads, and Ross–Sagiv's conflict-freedom
  check becomes a static analysis for them (ANA-140).

**CR-52 Identity and batching of lattice-valued messages.**
- *Positions.*
  - TPLP: one arrival per (sender, step, addressee, fact), its "set sending semantics".
  - Bud: the channel `pending` buffer key-normalizes and merges lattice columns before `flush`, and
    `receive_inbound` merges same-key arrivals within a tick.
  - BENCH-069: merging at the sender is equivalent to sending each value separately.
- *Adopted.* Merge at the sender. There is one message per (sender, send tick, addressee, relation, key), and it
  carries that tick's join. The delivered batch is the ⊔-normalization of the messages that arrive in that tick.
  Nothing merges across ticks without a persistent sink (SEM-104, SEM-105).
- *Why.* This applies FD normalization to `async_P(D)` exactly as to every other relation, and it matches Bud. It also
  removes runs in which parts of one tick's value arrive at different ticks. BENCH-069's equivalence holds only for
  join-morphism consumers (BENCH-303).

**CR-53 Lattice recursion that does not converge within a tick.**
- *Positions.* Ross–Sagiv take the least fixpoint in a complete lattice, possibly transfinite (the `halfsum` example,
  Ex. 5.1). Datafun's bounded `fix` clamps to the bound. Datalog° and Flix define only finite convergence.
- *Adopted.* A stratum that does not reach its fixpoint in finitely many steps has no meaning, and the engine raises a
  hard error (SEM-032, SEM-103). Limits exist only across ticks, in ultimate models (SEM-107).

**CR-54 Aggregate semantics used by the verifiers.**
- *Positions.*
  - FLP, and clingo's Ferraris/Abstract-Gringo semantics, which coincides with FLP on aggregates that are not under
    `not`.
  - Gelfond–Zhang's Alog, built on the vicious-circle principle.
  - They differ on recursion through monotone aggregates. `p(a). p(b) :- #count{X:p(X)} > 0.` has the answer set
    {p(a), p(b)} under FLP and no answer set under Alog (GZ Ex. 8).
- *Adopted.* FLP/Ferraris, which is clingo's default. Dedalus^L's least-fixpoint semantics agrees with it on the
  monotone fragment, as checked on clingo 5.8.2 (BENCH-313).

---

## 2. Language core (LANG)

### 2.1 Program structure and modules
- **LANG-001** `P0` **Unordered rule sets.** A program or module is a set of declarations and rules. The textual order of statements and blocks carries no meaning. — _R03 (CIDR11 §3.3; BB-01); R08 (NewDir §2.2)_
- **LANG-002** `P0` **Standalone language.** It has its own lexer, parser and type checker; it is not a DSL embedded in a host language. It compiles to the Dedalus IR (ENG-001). Every rule body can be analyzed, because rules contain no opaque host closures. — _R03 §9(1); R08 §12.2_
- **LANG-003** `P0` **Modules with typed interfaces.** `input` and `output` interface relations are the only points where modules connect, and the catalog records each interface's direction. — _R03 (BB-06, BB-07); R07 §8_
- **LANG-004** `P0` **Import creates instances.** `import M as a` creates an independent, namespaced instance. The same module can be imported again under a different alias. Nested members are reached with qualified names (`a.b.rel`). Reusing an alias is an error. — _R03 (BB-05)_
- **LANG-005** `P1` **Include/mixin and file include.** A module can include another module's state and rules flat, as Bloom's `include` does. `include "file"` is textual and resolves relative to the including file. — _R03 (BB-04); R02 #14; R06 A10_
- **LANG-006** `P1` **Protocol (abstract) modules.** A protocol module is an interface-only contract. It can have several implementations, and one is chosen when modules are composed. — _R03 (BB-06; CIDR11 §3.4)_
- **LANG-007** `P1` **Named rule blocks with override.** If an extending module defines a block with the same name as a base block, it replaces the base block. Duplicate block names within one module are an error. — _R03 (BB-03)_
- **LANG-008** `P1` **Interposition.** A module can import a component and route that component's interfaces through its own rules. BOOM used this for LATE, for inserting Paxos, and for metering. — _R03 §2.8; R07 §8_
- **LANG-009** `P1` **Location-typed (choreographic) modules.** One module can contain rules for several roles (process, cluster, external). The compiler projects out one program per location. — _R08 (R8, §6.1); R02 §4.2_
- **LANG-010** `P1` **Constants and deploy-time parameters.** Named constants, `#define`-style macros, and command-line or config substitution, e.g. `SUCCESSORS`, `REP_FACTOR`. — _R01 #32; R07 §3.6_
- **LANG-011** `P2` **Multi-program runtime.** One runtime hosts several independent programs, each with its own namespace. `public` rules can be seen across programs. — _R01 §3.3; R07 §8_

### 2.2 Types, values and schemas
- **LANG-020** `P0` **Typed relation declarations.** A relation declares key columns and value columns. By default every column is a key. An empty key means a singleton (a "register"). A schema can be reused with `like r`. — _R03 (BB-08); R01 #7; R03 §8_
- **LANG-021** `P0` **Type inference with located errors.** Types of rule variables, temps and literals are inferred by unification. An arity or type error lists every piece of conflicting evidence with its source position. — _R06 A11; R03 §8_
- **LANG-022** `P0` **Scalar types.** bool, i64/u64 and sized integers, f64 (not usable as a lattice Ord), string, bytes, unit, the `Node` address type, duration and timestamp. — _R01 §2.1; R06 A11; R04 §7.2_
- **LANG-023** `P0` **Compound values.** Tuples (with destructuring), lists, sets, maps, and records/ADTs with structural equality. Compound values are hash-consed. — _R01 #27; R08 §5.1; R09 §5.2, §14.1_
- **LANG-024** `P0` **A deterministic total order on every value.** Every type has one canonical total order. It is used for `<`, the min-node-id idiom, the ordered walk behind ∀, sorting, and the canonical order of a batch. — _R02 §14.21; R07 §10.3_
- **LANG-025** `P1` **Option types instead of implicit null.** A nullable column has type `Option<T>`. Short tuples are never padded, and an arity mismatch is a compile error (CR-28). Overlog's `null` maps to `None`. — _R03 (BB-42); R01 §8.5_
- **LANG-026** `P1` **Wide modular IDs and ring intervals.** N-bit modular IDs (160-bit for Chord) with the literal form `0x…I`, modular arithmetic and shifts. Ring-interval predicates such as `x in (a,b]` wrap around, so `(n,n]` covers the whole ring. — _R01 #28, #29_
- **LANG-027** `P1` **Opaque host values.** A column can hold a Rust type if the type is Eq, Hash, Ord and serializable. Its methods can be called only through declared pure UDFs. — _R07 #16; R01 #37_
- **LANG-028** `P1` **Blob columns and byte streams.** Large values such as chunks and snapshots are stored or streamed outside the tuple store, and tuples hold handles to them. — _R07 §3.4, §11.7_

### 2.3 Collection kinds (storage classes)
- **LANG-040** `P0` **`table`.** A persistent relation. It compiles to the frame rule `p@next :- p, notin del_p`. — _R03 (BB-11); R02 #12_
- **LANG-041** `P0` **`scratch`.** A tick-local relation, empty at the start of every tick. A `<+` into a scratch shows up only in the next tick. — _R03 (BB-12)_
- **LANG-042** `P0` **`channel`.** An asynchronous relation with exactly one `@` address column. Only async rules can derive into it. The receiving side is tick-local. The channel key is checked at the sender. — _R03 (BB-14, T13); R02 #5, #7_
- **LANG-043** `P0` **`input`/`output` interfaces.** Tick-local relations at a module or host boundary. — _R03 (BB-07); R08 (.input/.output)_
- **LANG-044** `P0` **`durable table`.** Persistent and write-ahead-logged. Its changes are committed atomically at the end of the tick, before that tick's messages are released (SEM-072). Columns have stable field numbers (LANG-261), and schema changes need a migration path (LANG-262, ANA-100). The WAL and checkpoints record each relation's schema hash (DIST-081). — _R07 #1–#2; R01 #38; R03 (BB-20); R15/G5 §6.2–6.7_
- **LANG-045** `P0` **`static` relations.** Base facts that hold at every tick, such as configuration, membership and topology. — _R02 #8, §14.4; R08 (.static)_
- **LANG-046** `P1` **`loopback` channel and `localtick`.** A loopback sends to self through the network path, so the message arrives in a later tick. `localtick` is a built-in way to request another tick. — _R03 (BB-15, BB-19)_
- **LANG-047** `P1` **`temp`.** An inline scratch defined by a single rule, with an inferred schema. Shadowing a declared name is an error. — _R03 (BB-13)_
- **LANG-048** `P1` **`soft table` (TTL state).** `soft table t(..) ttl <dur> max <n>`. Tuples expire after the TTL; re-deriving a tuple refreshes it; when the table is full the oldest tuple is evicted, ordered by (birth, canonical order). It compiles to TTL-guarded persistence (CR-17). — _R01 #6, #9–#11; R02 §3.3_
- **LANG-049** `P1` **`sealed` collection.** Writable only during bootstrap; any later write is an error. A whole-relation seal is emitted after bootstrap. — _R05 #36_
- **LANG-050** `P1` **`range` collection.** Every column is a key. One integer column is compressed into disjoint [lo,hi] buckets for each value of the other columns. Range collections are never reclaimed. — _R05 #34_
- **LANG-051** `P1` **Terminal and file sources.**
  - `stdio`: on the right-hand side it yields the stdin lines read since the last tick; `<~` into it writes to stdout at the end of the tick.
  - `file_reader(lineno → text)`.
  - `readonly`.

  Using any of these on the left-hand side where not allowed is an error. — _R03 (BB-17, BB-21)_
- **LANG-052** `P1` **`halt`.** Inserting into it stops the node at the end of the tick, and optionally the whole process. — _R03 (BB-18)_
- **LANG-053** `P1` **Materialization switch per view.** A derived view can be declared materialized (maintained incrementally) or recomputed. The choice does not change the program's meaning. — _R07 #20, §2.2_
- **LANG-054** `P2` **Host-backed collections.** A relation can be backed by an external store through a provider trait. — _R03 §2.2; R09 #18_

### 2.4 Rules and temporal operators
- **LANG-060** `P0` **Deductive rules (`:-` / `<=`).** The head holds on the same node in the same tick. These rules may be recursive. — _R02 #3; R03 (BB-23); R08 #27_
- **LANG-061** `P0` **Inductive rules (`@next` / `<+`).** The head holds on the same node at t+1. The rule is evaluated once, on the completed fixpoint. — _R02 #4, §14.7; R03 (BB-24)_
- **LANG-062** `P0` **Async rules (`@async` / `<~`).** The head is delivered to the `@` node at a later tick, which is chosen nondeterministically. Any head on a remote node must use an async rule. — _R02 #5, §14.9; R03 (BB-27)_
- **LANG-063** `P0` **Deferred delete (`<-`).** Removes the exact tuple from a persistent relation at t+1. — _R03 (BB-25); R02 #13; R01 #17_
- **LANG-064** `P0` **Deferred upsert (`<+-`).** At t+1, atomically deletes every tuple with the same key and inserts the new tuple. — _R03 (BB-26)_
- **LANG-065** `P0` **Explicit Dedalus persistence.** `p(X)@next :- p(X), notin del_p(X)` and `persist p` are accepted directly; `table` is sugar for them. — _R02 #12; R08 (.persist)_
- **LANG-066** `P0` **Operator × collection legality matrix, checked statically.** Forbidden combinations include:
  - `<=` or `<+` into a channel;
  - `<~` into a table or a lattice;
  - any insertion into periodic, readonly or file sources;
  - `<-` on a scratch or a lattice.

  — _R03 (BB-28); R04 C26_
- **LANG-067** `P0` **Host insertion is always deferred.** External code inserts through `<+` or async paths only. Inserting into the current tick from outside the tick is an error. — _R03 (BB-30)_
- **LANG-068** `P0` **Named rules.** Rule labels are used by provenance, tracing, coverage, `.plan` hints and override. — _R01 §3.1; R07 §2.2_
- **LANG-069** `P1` **Timestamped facts.** `p(c)@k` is an input event at tick k. A fact without `@k` is static (CR-16). — _R02 #8; R06 A4_
- **LANG-070** `P1` **Absolute-time body atoms.** In spec rules, `p(..)@k` matches p at tick k, e.g. `notin bcast(X,P)@1`. — _R06 A3; R10 #13_
- **LANG-071** `P1` **Delta pseudo-relations.** `r.inserted` and `r.deleted` are the facts that became present or absent at this tick boundary, like JOL's `#insert`/`#delete`. — _R01 #19; R08 §4.3_
- **LANG-072** `P2` **Entanglement.** The body's tick can be bound as a value (`q(A)@N`). This is gated behind an analyzer warning. It enables the Lamport `p_wait` idiom (ODD-17). — _R02 #16, §4.6_

### 2.5 Rule bodies
- **LANG-080** `P0` **Positional and named-field atoms.** Positional atoms support the `_` wildcard and repeated variables as equality. Named fields (`r(key: K)`, `a.x == b.y`) are also supported, because long positional joins are hard to read. — _R07 §8; R03 (BB-33); R08 #29_
- **LANG-081** `P0` **Constants in body atoms match correctly.** We do not reproduce P2's bug where they were ignored. — _R01 §3.1_
- **LANG-082** `P0` **Stratified negation.** `notin p(..)` and `!p(..)` are range-restricted: every negated variable must be bound by a positive atom. — _R02 #9; R01 #25; R03 (BB-35)_
- **LANG-083** `P0` **Anti-join forms.** Whole-tuple, key-pair, and key-pair plus a predicate. — _R03 (BB-35)_
- **LANG-084** `P0` **Expressions with standard precedence.** Comparisons, arithmetic, bit operations, `**`, boolean operators, the ternary, and string/list concatenation. Molly's right-nested parse is not reproduced. — _R01 #27; R06 A9_
- **LANG-085** `P0` **Let-binding.** `X := expr` binds a fresh variable. The planner orders body terms by which variables are available. Every head variable must be bound. — _R01 #26; R02 #9_
- **LANG-086** `P0` **Joins.** N-way equality joins, equivalence-class predicates, semi-joins, natural join and Cartesian product. — _R03 (BB-33)_
- **LANG-087** `P1` **Left outer join.** Unmatched rows are padded with `None`. It is non-monotone. — _R03 (BB-34)_
- **LANG-088** `P1` **Unnest and destructure.** `flat_map`, `*x` flattening, and tuple patterns inside atoms. — _R03 (BB-32); R08 #29_
- **LANG-089** `P1` **Disjunction and conditional sugar.** `;` alternatives and `if/else` value expressions compile to sets of rules. — _R01 §9 item 12_
- **LANG-090** `P1` **Membership tests are first-class.** `in`, `exists` and `is_empty` compile to visible joins and anti-joins; they never hide inside closures. — _R03 (BB-40); R07 §3.6_
- **LANG-091** `P1` **Indexed lookup and range scans.** `log[i]` and `log[i..j]` compile to index range queries. — _R07 #21_
- **LANG-092** `P1` **Generator relations with binding patterns.** `range(lo,hi,x)`, `less_than(x,n)`, and library functions modeled as infinite relations that may be used only with bound inputs. — _R08 #32; R02 §13.1_
- **LANG-093** `P1` **Order-sensitive operators.** `sort`, `enumerate`/`index()`, `topk`/`limit` and `percentile` always break ties by the canonical order of the whole tuple (LANG-118), so they are deterministic. The old "flagged as nondeterministic" becomes a lint, "ties broken by canonical order", decided by FD closure (ANA-038, condition D2). — _R03 (BB-39); R07 #8; R08 #30; R12/G2 §5.9–5.10, §6_
- **LANG-094** `P1` **Utility projections.** `keys`, `values`, `payloads` (drops the address column), `rename`, and schema accessors. — _R03 (BB-41)_
- **LANG-095** `P1` **Bodies that span several locations, as sugar.** They are accepted only through an explicit localization rewrite: either the chain rewrite over well-connected bodies, or NDlog Algorithm 2 for link-restricted rules. Each hop becomes an `@async`, and the compiler emits a lint (CR-15). — _R01 #4, #5, §5.6; R02 §4.8_
- **LANG-096** `P2` **NDlog link literals.** `#link` relations and a link-restricted mode that can be switched off for full-mesh networks. — _R01 #3_
- **LANG-097** `P1` **`index() [per (Ḡ)] [by (K̄)]`.** A dense 0-based rank within the tick.
  - The head is deduplicated first. (dfir-datalog numbers body rows instead.)
  - The order is K̄, then the canonical order of the whole tuple.
  - Over a persistent input it re-ranks every tick, and ANA-011 lints it.
  - Hydro frontend: per-tick `index()` maps here; `index()` over a persisted body maps to LANG-098.

  — _R12/G2 §5.9; R08 #30; R07 #8_
- **LANG-098** `P1` **`seq()`: stable numbering.**
  - Each distinct tuple gets the next number in the first tick it appears. Within that tick, numbers follow
    (K̄, canonical).
  - Numbers are never reused. `seq() release` frees a tuple's number when the tuple leaves.
  - It is defined by an explicit high-water-mark expansion.
  - Its state must be `durable` if the numbers escape the node (ANA-011).

  — _R12/G2 §5.9; LIB-064; dfir-datalog `enumerate::<'static>`_
- **LANG-099** `P2` **In-tick recursive greedy choice.** Prim, Dijkstra and spanning trees within one tick. This is
  GZ01's Algorithm 7.2: one candidate at a time, least priority first, deterministic under SEM-084's priority. Until
  it exists, these algorithms run across ticks or with `lmin` lattices. — _R12/G2 §5.4, §5.13; GZ01 §6–7; Soufflé
  choice-domain_

### 2.6 Aggregation, choice and folds
- **LANG-100** `P0` **Head aggregates with GROUP BY.** `count<*>`, `count<X>`, `count distinct`, `sum`, `min`, `max`, `avg`. The group is the set of non-aggregate head terms. Input is deduplicated first (set semantics). — _R01 #21; R02 #11; R03 (BB-36); R08 #30_
- **LANG-101** `P0` **Aggregates are non-monotone.** Unless an aggregate is written with lattices, it is stratified as non-monotone. An empty group produces no row (CR-08). — _R02 §3.4, §14.20_
- **LANG-102** `P1` **Collection aggregates.** `set`/`accum`/`collect_vec`/`mklist` and `accum_pair`. — _R03 (BB-36); R07 #6; R08 §5.1_
- **LANG-103** `P1` **Exemplary aggregates.** `argmin`, `argmax` and `argagg` return every tied exemplar tuple; also `bool_and` and `bool_or`. — _R03 (BB-36, BB-37)_
- **LANG-104** `P1` **Statistical aggregates.** `percentile<p,X>`, quantile sketches, and `topk`/`bottomk`/`limit`. — _R07 #6; R01 #21_
- **LANG-105** `P1` **User-defined aggregates.** Defined by init, transition and final functions, with declared properties: commutative, associative, idempotent, exemplary. — _R03 §2.4; R07 #16; R04 §7.7_
- **LANG-106** `P1` **Aggregates with an explicit default.** `count<*> default 0 per e(..)` emits one row for each driving tuple, even when its join is empty. This recovers Overlog's per-event aggregates (CR-08). — _R01 #22, §10(5)_
- **LANG-107** `P2` **An aggregate chooses the destination.** An aggregate may sit in the `@` column, e.g. `lookup(@min<BI>, ..)`. — _R01 #24_
- **LANG-108** `P0` **Nondeterministic choice.** `choose((X),(Y))` picks under a functional dependency.
  `choose(x)`, `argagg(:choose)` and `choose_rand(x)` are aggregates.
  - The choice is *per tick*: the node and the tick are always in the FD's determinant.
  - `choose` takes the least seeded priority, so it is seed-dependent. `choose_rand` redraws every tick, so it is
    schedule-dependent (SEM-085, SEM-087).
  - Both are defined by Dedalus expansions and maintained by ENG-068.
  - Outputs are labeled with their nondeterminism class, unless ANA-038 proves the choice forced or locally resolved.

  — _R02 #18; R03 (BB-36); R08 #30; R12/G2 §5.1, §5.3, §5.7_
- **LANG-109** `P1` **General fold.** `reduce(init, f)` returns a collection. — _R03 (BB-38)_
- **LANG-110** `P0` **Ordered fold over a canonical batch.** `fold_ordered(init, step, rows order by key)` supports
  sequential step functions, such as applying a log or an optional Raft step. It folds the pure `step` over the
  tick's *distinct* rows in (key, canonical) order, so its result depends only on the batch's set. There are two
  forms:
  - the aggregate form, where an empty group gives no row;
  - the carried-state form `state(S2)@next :- state(S), S2 = fold_ordered(S, …)`. It re-applies *every* input row
    each tick, which Dedalus demands. Over a persistent input it is linted (ANA-011).

  `reduce` and UDAs without declared commutativity and associativity are evaluated as `fold_ordered` by canonical
  order; TEST-015 checks the declarations. The incremental form is ENG-073. — _R07 #7, §11.1(5); R12/G2 §5.11_
- **LANG-111** `P1` **Quorum sugar.** `majority<N in Members>` means count > |Members|/2 at runtime. The verifier maps it to a quorum sort with an intersection axiom. — _R10 (V1, #30)_
- **LANG-112** `P1` **Decomposable aggregates and derived combiners.** The compiler derives a partial aggregate from each aggregate's declared properties. Senders apply it before a partitioned channel, and receivers apply it while merging.
  - A lattice merge gives `merge`.
  - A commutative monoid (sum, count) gives a partial fold.
  - Algebraic aggregates (avg, variance) give moment tuples `(n, Σx, Σx²)`.
  - Holistic aggregates (median, rank-based) get no combiner.
  - A user-defined aggregate (LANG-105) gets a combiner if it is declared associative and commutative.

  Non-idempotent partials must travel with exactly-once identities (DIST-011). Merging idempotent ones is always safe. — _R14/G4 §3.3, §10.8; HOP NSDI §3.1.3_
- **LANG-113** `P1` **Estimator aggregates for progressive outputs.**
  - `scale_by progress`: HOP's job-progress scale-up. It is always labeled biased.
  - `scale_by coverage(h)`: HOP's per-stratum "sample fraction". The estimate is value × W(h)/cov(h), where
    `cov = sum<w>` is a monotone lattice.
  - `ola_sum`, `ola_count` and `ola_avg<p>`: return `(estimate, lo, hi)` over block-level samples taken in a
    randomized block order. Two interval forms are available:
    - the large-sample CLT interval, ε = z_p·(T_{n,2})^{1/2}/n^{1/2}, scaled by N for sum and count;
    - the conservative Hoeffding interval, ε = (b−a)·((1/2n)·ln(2/(1−p)))^{1/2}.
  - Every interval states its assumptions: random order, and processing time independent of value. The latter is
    the PANSARE inspection-paradox caveat.

  — _R14/G4 §6.4–6.5, §10.6; OLA SIGMOD'97 §4_
- **LANG-114** `P1` **`choose_least((X̄),(C))` / `choose_most`.** GZ01's greedy choice.
  - Declaratively it is the FD X̄ → C. Operationally it takes the least (most) C.
  - Ties are broken by the seeded priority, then by canonical order.
  - At most one per rule. With several choose goals, the greedy order is (C, priority, W̄).
  - Unlike `argmin` (LANG-103), it returns exactly one exemplar.

  — _R12/G2 §5.4; GZ01 §5.2, Thm 5.3_
- **LANG-115** `P1` **Sticky choice (`choose sticky`, `choose_rand sticky`).** Keeps the previous tick's choice while
  it is still a candidate.
  - The choice is revoked at the first tick in which it is no longer a candidate, or in which an override picks
    something else. A fresh priority choice is then made in the same tick.
  - A group with no candidates forgets its choice.
  - The expansion carries a `held` relation with `@next`. It is non-durable unless declared `durable`.
  - It is monotone in time for insert-only candidates, but it depends on the schedule (SEM-087).
  - Protocol decisions that must be permanent (Raft votes, Paxos promises) are persisted explicitly, not through
    `sticky`.

  — _R12/G2 §5.5; GPZ01 §9; LDL++ Thm 2.2_
- **LANG-116** `P1` **Several choose goals per rule.** `choose((X̄1),(Ȳ1)), …, choose((X̄k),(Ȳk))`, for example
  bipartite matching or a successor chain. Candidates are scanned greedily in seeded-priority order, and each is
  accepted iff it is FD-consistent with those already accepted. The result is maximal, hence a choice model. It is
  maintained per conflict component (ENG-075). — _R12/G2 §5.2; GZ01 Ex 4.1–4.2, Alg 7.2_
- **LANG-117** `P1` **Relation-level resolution `key(k̄) resolve <policy>`.**
  - The policy is `choose`, `choose sticky`, `choose_rand`, `choose_least(c)`, `choose_most(c)`, or a lattice merge.
  - The candidates for a key are every tuple that would be present at t+1: persisted tuples, `<+` inserts and `<+-`
    upserts. The policy picks one, replacing the SEM-050 error.
  - `<+- resolve …` does the same for SEM-051.
  - This concretizes option (c) of ODD-02. `resolve choose sticky` is first-writer-wins; `choose_least(ts)` over a
    tie-free timestamp is LWW.
  - The relation may not be on a same-tick cycle.

  — _R12/G2 §5.12; Soufflé choice-domain (HU21)_
- **LANG-118** `P0` **Canonical order for every order-sensitive aggregate and output.** All of these order by
  (user key, then the canonical order of the whole tuple):
  - `sort`, `topk`/`limit`, and `percentile` (nearest rank);
  - `collect_vec`, `accum` and `mklist`, which return canonically sorted lists, never hash or insertion order;
  - host-callback, `stdio` and dump output.

  — _R12/G2 §5.10; SEM-088; dfir-datalog `collect_vec` uses an FxHashSet_

### 2.7 Lattices
- **LANG-120** `P0` **Lattice types.** A lattice type declares a bottom element ⊥ and an associative, commutative, idempotent merge, plus an optional top ⊤ and an optional order. Lattices can be 0-ary identifiers or the types of columns. — _R04 C25; R03 (BB-10)_
- **LANG-121** `P0` **Lattice columns under a functional dependency.** The key is every non-lattice column. Two derivations with the same key merge. A lattice column cannot be a key, a join key or a group key. It cannot be compared with `==` except through a declared monotone predicate. — _R04 C27, §10.1(2)_
- **LANG-122** `P0` **Lattice merge statements.** `<=` merges now and `<+` merges at the next tick. Both sides must have the same lattice type. `<-` is not allowed on lattices. — _R04 C26_
- **LANG-123** `P0` **Converting between collections and lattices.** A collection becomes a lattice by an implicit fold: each tuple is made into a singleton and the singletons are merged. A lattice becomes a collection through `when_true`, `to_collection` or a threshold. — _R04 C28, C29_
- **LANG-124** `P0` **Built-in lattices.**
  - `lbool` (Max<bool>), `lmax`, `lmin`.
  - `lset`, `lmap` (an entry whose value is ⊥ counts as absent), `lbag`, `lpset`.
  - `Pair`, `WithBot`, `WithTop`, `Conflict`, `Point`, `Unit`, `VecUnion`, `UnionFind`.

  — _R04 B9–B20; R08 #34_
- **LANG-125** `P0` **Monotonicity class of every operation.** For each argument, every lattice operation declares one class: morphism (M), bimorphism (BM), monotone (Mon), antitone (Anti) or non-monotone (NM). The table in R04 §2.4 is normative. *Amended (R13):* the class is relative to the lattice's own natural order, and the user-visible value of an encoded non-inflationary type is NM. For example, a PN-counter's `value` (P−N) is NM, while its `pos` and `neg` are Mon. A threshold on `value` is therefore not a threshold query, and it is never final (SEM-017). — _R04 A7, §2.4; R13 §8.4 (FT App. B)_
- **LANG-126** `P0` **Threshold operations.** `gt_eq`, `contains`, `key?`, `is_top`, `when_true`, and a generic `threshold(t1..tn)` whose thresholds must be pairwise incompatible. These are the only monotone ways to read a value out of a lattice. — _R04 §10.2, T21; R08 §7.1_
- **LANG-127** `P0` **`reveal`.** The explicit, non-monotone way to read a lattice's raw value. — _R04 C32_
- **LANG-128** `P0` **Persistent and tick-scoped lattices.** Lattices persist by default. A `scratch` lattice resets to ⊥ at every tick (CR-24). — _R04 C33_
- **LANG-129** `P0` **Typed ⊥.** `m.at(k)` on a missing key returns ⊥ of the value type. — _R04 C34_
- **LANG-130** `P0` **Vector clocks.** `MapUnion<Node, Max<u64>>`, with happens-before and concurrency tests. — _R04 B23_
- **LANG-131** `P0` **Lexicographic pair with a chain key.** A proper Lex lattice: when the keys are incomparable, the merge is (k⊔k′, ⊥). Built on it are `Ballot(round, node)` and last-writer-wins registers keyed by a tie-free `(ts, node)` timestamp. — _R04 B17; R07 #12, §4.3_
- **LANG-132** `P1` **Antichain / MV-register (`ldom`).** Keeps the ⟨version, value⟩ pairs that no other pair dominates. `version` is a morphism; `value` is non-monotone. — _R04 B19_
- **LANG-133** `P1` **Tombstone lattices.** Set and map union with pluggable tombstone sets (for example roaring bitmaps). — _R04 B21_
- **LANG-134** `P1` **Causal dot-store lattices.** `Causal<DotSet|DotFun|DotMap>`, whose causal context can be compressed to a version vector. — _R04 B22_
- **LANG-135** `P1` **User-defined lattices.** Defined either through a Rust `Merge`-style trait or through a restricted DSL of verified constructors. Each method is declared as a morphism, monotone or plain (ODD-09). — _R04 C31, G54; R03 §8_
- **LANG-136** `P1` **`DomPair` only as `unsafe`.** Its non-associativity is documented, and using it requires an explicit `unsafe` marker (CR-25). — _R04 B18, §4.5_
- **LANG-137** `P1` **Lattices inside messages.** A channel tuple can carry lattice values. When the receiver gets a tuple whose key it already has, it merges the two. *Clarified (R11/G1):* merging happens at the sender and within one tick's delivered batch (SEM-105). Across ticks it happens only through a persistent sink. — _R04 C30; R11/G1 §3.3_
- **LANG-138** `P1` **Weighted (Z-set) collections.** Collections with signed multiplicities, visible to users, for views that must support retraction (streaming in Tide).
  *Amended (R13): priority raised from P2 to P1*, because FLAG-130 (P1) and replicated IVM (LIB-093) depend on it.
  - There are two kinds. `zset<T>` has ℤ weights. `bag<T>` has ℕ weights and is insert-only, which ANA-030 must
    prove; a single possible negative weight turns a `bag` into a `zset`.
  - A `bag` is inflationary, so thresholds over it can be final. A `zset` is never final without a seal (SEM-017).
  - Views over them are declared as one of: set view (`distinct`, weight > 0); clamped multiset (max(w, 0)); raw
    weights.
  - Weights are checked i64. Overflow is a hard runtime error, never a wrap-around or saturation.
  - A `zset` or `bag` payload crosses nodes only through a wrapped channel (LANG-158, CR-35).

  — _R04 G56; R10 #41; R13 §6.5, §7.1_
- **LANG-139** `P1` **Progressive snapshots.** The form is `snapshot s of L at progress every Δ upto pmax | at (p1, p2, …) [mode committed_only | include_tentative] [estimate …]`.
  - It emits one `(point, actual_progress, class, attempt, value)` row for **every** progress point crossed. HOP
    emits only one per notification. The HOP defaults are Δ = 1.0 (off) and pmax = 0.9.
  - The progress is a threshold on a progress lattice: the mean over all producers of each producer's latest
    `Max<progress>`, with producers not yet heard from counting as 0.
  - It desugars to `reveal` gated by a threshold, and it is typed `nondet "progressive"` (LANG-204). This is Hydro's
    `snapshot(&tick, nondet!)`, and Flo shows why it cannot be deterministic.
  - It carries the class computed by ANA-036. For class L, snapshots are certified to form a chain of lower bounds of
    the final value.

  — _R14/G4 §6.1–6.2, §10.5; HOP NSDI §4.1; R04 §8; R08 §6.4_
- **LANG-142** `P1` **Group and ring types.** A declared or built-in `group` provides 0, + and neg; a `ring` also
  provides × and 1.
  - Built-ins: ℤ, ℤₙ, `zset<T>` (finite-support maps T→ℤ), and tuples and maps of these. The ℕ monoid of `bag` is a
    commutative monoid, not a group.
  - Each operator over a group-typed collection is classified linear, bilinear or non-linear. This is the DBSP
    analogue of LANG-125, and it drives ENG-062.
  - A group or ring type can **never** be declared a lattice. The law harness rejects `+` as a merge, because an
    idempotent or inflationary group is trivial.
  - Ring × is used only for local plan rewrites. Replication sees only the group.

  — _R13 §3.1, §3.5, §2; Wrapping Rings §3–4, App. B_

- **LANG-280** `P0` **Lookup and generator reads of lattice relations.** A generator `R(k̄; x)` ranges only over cells whose value is not ⊥. A lookup `x = R[k̄]`, with the keys bound elsewhere, returns the value, and ⊥ if the cell is absent. A 0-ary lattice relation (a Bloom^L identifier) can be read only by lookup. — _R11/G1 §3.1 N4–N5; SoCC §3.2.1_
- **LANG-281** `P0` **Numeric lattices have an adjoined ⊥.** `lmax<T>` and `lmin<T>` use ⊥ = −∞ and +∞ respectively, distinct from every value of T. That makes `size(∅) = 0` a real value (ODD-50). — _R11/G1 §3.1, T5; R04 §3.4_
- **LANG-282** `P1` **Keyed monotone sum.** `lmap<K, lmax<N≥0>>.sum_values` is monotone (though not a morphism) and supports thresholds. It expresses Ross–Sagiv/FLP company control. — _R11/G1 T12; Ross–Sagiv Ex. 2.7, Fig. 1; FLP Ex. 1.1_
- **LANG-283** `P1` **A default by joining with a constant.** `x ⊔ c` is the monotone way to give a lattice read a default, for example `vc.at(n) ⊔ 0`. There is no non-monotone `??` operator. — _R11/G1 T10 [ours]_
- **LANG-284** `P1` **Monotone reset idiom.** Lattice state is reset with `Lex<epoch, 𝓛>`: raising the epoch discards the old component. `<-` and `<+-` on lattices are compile errors. — _R11/G1 §3.3 D9; LANG-122, LANG-131_

### 2.8 Locations and distribution
- **LANG-150** `P0` **Location specifier.** Channels and messages have exactly one `@` column. Local state lives implicitly at `self` (CR-14). — _R01 #1; R02 #2; R03 (BB-14)_
- **LANG-151** `P0` **Body locality.** Every body atom in a protocol rule is local to a single node. A rule can derive a head on another node only by being async. — _R02 #6; R07 #11_
- **LANG-152** `P0` **Identity and membership built-ins.** `self` (Id), `members` (All), and the node-address type. — _R02 #17; R05 #10_
- **LANG-153** `P1` **Cluster locations.** An SPMD role. It provides member ids, the node's own id, broadcast, send-to-member, receive keyed by sender, and a membership stream. — _R08 #40_
- **LANG-154** `P1` **Declarative partitioning.** Each relation can declare a partition key and a hash or range routing function. Programs can call `owner(key)`. — _R07 #22; R05 #11; R10 #36_
- **LANG-155** `P1` **Channel fault models.** A channel declares its delivery guarantee:
  - reliable ordered prefix;
  - lossy, where a lost message is modeled as delayed forever;
  - lossy;
  - reliable but unordered.

  The declaration sets the stream properties of the receiving relation and drives the simulator. — _R08 #39, §6.5_
- **LANG-158** `P1` **Wrapped (exactly-once) channels for group payloads.** The form is
  `channel c(@dst, …) carries zset<T> via exactly_once(dots | cumulative | tree)`.
  - The compiler inserts the wrapper: `dots` is DIST-015, `cumulative` is DIST-016 and `tree` is DIST-017. The
    default is ODD-26.
  - The receiving side is a group-typed input to the local Z-set stratum. `unwrap` (ENG-070) turns wrapper Δ into
    Z-set Δ.
  - Sending a group-typed payload through a plain `<~` is a compile error (ANA-015).
  - Updates the sender makes to the channel in one tick are summed into one payload. A zero sum sends nothing.

  — _R13 §6, §7.2; Wrapping Rings §3; CR-35_

### 2.9 Time, timers and randomness
- **LANG-170** `P0` **Tick number.** `tick()` is a read-only built-in holding the local tick counter. A rule that reads it is marked time-dependent. — _R03 (BB-44)_
- **LANG-171** `P0` **Wall clock sampled once per tick.** `now()` has the same value for the whole tick, and the value is recorded for replay (CR-18). — _R03 (BB-44); R07 #10; R02 §9_
- **LANG-172** `P0` **Periodic timers.** `periodic p every <dur> [times n]` produces `(id, time)` events. There is also a fire-once-at-start form for initialization. — _R01 #13; R03 (BB-16)_
- **LANG-173** `P0` **Logical and physical timers.** A timer counts either ticks or wall-clock time, and one source program can use both. Under simulation, physical time maps to virtual time (ODD-16). — _R07 #9; R01 #14_
- **LANG-174** `P0` **Seeded randomness.** `random()` is one value per node per tick, defined as `rand(())`
  (LANG-175). It is a pure function of the seeds, node, incarnation and tick (SEM-084). Replay needs the seeds, not
  the draws. — _R07 #10; R03 §9(1); R12/G2 §5.8_
- **LANG-175** `P0` **Keyed randomness `rand(k̄)`.**
  - `rand(k̄) = PRF_{σ_node}("rand", incarnation, tick, fingerprint(k̄))`.
  - Helpers: `rand_float`, and `rand_range`, which is unbiased (retries use a counter extension).
  - The same key gives the same value within a node and tick.
  - It is time-varying (ENG-074). A value that must stay fixed is captured into state with `@next`. Uncaptured use
    over persistent inputs is linted (ANA-011).
  - Cryptographic randomness comes from an async service (LANG-184), not from `rand`.

  — _R12/G2 §5.8; Random123 (SC'11)_

### 2.10 Functions, UDFs and host interop
- **LANG-180** `P0` **Built-in function library.** Math, strings, hashing, list/set/map operations (head, tail, cons, contains, concat, size), `to_string`, and id generation. — _R01 #30_
- **LANG-181** `P0` **Pure UDFs.** Rust functions called from expressions. They must be deterministic, so each can be memoized per input per tick. Anything impure has to be an input relation or an async service. — _R03 (BB-44; §9(1)); R08 §2.2_
- **LANG-182** `P1` **Declared function properties.** UDFs can be annotated `monotone`, `morphism`, `injective`, `commutative`, `idempotent` or `pure`. Analyses use these: Blazes functional dependencies, semi-naive evaluation over lattices, and checks on which folds are legal. — _R05 #21; R04 §10.2; R03 §8_
- **LANG-183** `P1` **Table functions.** Host iterators that produce tuples. They can be used as relations, subject to binding patterns. — _R01 #37; R07 #16_
- **LANG-184** `P1` **Async services.** External calls whose results arrive as input in a later tick. This is the Dedalus rendezvous and JOL's `async`. — _R01 #39; R02 §3.3_
- **LANG-185** `P0` **Host API.**
  - Injection is always deferred (LANG-067).
  - The host can subscribe to each tick's contents of a relation, or only to its deltas.
  - `sync_do` and `async_do` run host code between ticks and then run a tick.
  - A single tick can be stepped with `tick()`.

  — _R03 (BB-47); R07 §3.4_
- **LANG-186** `P1` **Output-event handlers for bulk data.** Events derived by rules trigger host handlers, which move bytes outside the engine. This is the BOOM-FS data path. — _R07 #16, §3.4_

### 2.11 Bootstrap
- **LANG-190** `P0` **Facts and the bootstrap block.** Program facts and `bootstrap` rules are evaluated at tick 0. Imported modules bootstrap before the module that imports them. A `<+` in bootstrap takes effect at tick 0. — _R03 (BB-02, §2.1); R01 #31_

### 2.12 Assertions, specs and metaprogramming
- **LANG-200** `P0` **Violation relations.** Any tuple derived into `violation`, `die` or `fail` is an invariant violation. The action is configurable: abort the node, raise an alert, or log with provenance. Violations can also be shipped to a remote checker. — _R07 #19; R10 #10, #16; R03 (BB-65)_
- **LANG-201** `P0` **Spec rules.** `spec` rules may join across locations. They may read oracles (`crash`, `R_log`, `hb`) and use absolute-time atoms. They are evaluated over a global snapshot or trace, and they can never feed protocol relations. — _R06 A12, §3.2; R02 §14.22_
- **LANG-202** `P1` **Catalog relations.** The program's own rules, predicates, dependencies (with nm/in_body flags), strata, schemas and interfaces can be queried as relations. — _R03 (BB-22); R01 #40; R07 #17_
- **LANG-203** `P2` **Metaprogramming and hot install.** Rewrites can be written in the language itself over the catalog. Rules can be installed and uninstalled at runtime. Installation is an admin-plane operation only (DIST-066). No remote principal can ship rules through the data plane, unlike LBTrust's `active(R) <- says(_,me,R)`. A hot install that changes schemas follows SEM-094 and passes ANA-100. — _R07 #17–#18; R01 #40, §7; R15/G5 §2.3, §6.10_
- **LANG-204** `P1` **`nondet "reason"`.** Marks nondeterminism that the programmer accepts, with a mandatory justification. The annotation can be passed up through module interfaces, and both the analyzer and the simulator track it. — _R08 #38; R05 §6_
- **LANG-205** `P1` **Trusted coordination modules.** An annotation for coordination logic that has been verified by hand. The CALM analysis does not flag it again; the verifier checks the module's interface instead (VER-020). — _R03 (BB-58); R05 §3.1_
- **LANG-206** `P1` **Atomic regions.** Outputs are released only after the tick's state updates are visible to snapshot reads. This gives read-after-write within one node. — _R08 #41_
- **LANG-207** `P1` **Seals and punctuations.** `seal r on key = v [digest]` is a first-class statement. — _R05 §7(4), #37; R10 #37_
- **LANG-208** `P0` **Comments.** `//`, `/* */` and `#`. — _R02 #15_
- **LANG-212** `P1` **`final` outputs and finality built-ins.**
  - `output final r` is a compile error unless ANA-120 classifies r as POS-, NEG-, TOP-, THRESH-, FINITE- or
    SEALED-final. At runtime, emission is gated by ANA-121 and ANA-122.
  - Built-ins: `is_final(atom)` and `when_final(expr)`. Finality is itself monotone, so both are thresholds.
  - Host subscriptions (LANG-185) and output channels carry a per-tuple status: `provisional`, `final_present` or
    `final_absent`.
  - An output classified NEVER-FINAL (SEM-017) can only be subscribed to as provisional.

  — _R13 §9, §11; FT Def 3, Thm 22_

### 2.13 Compatibility frontends
- **LANG-220** `P1` **Molly `.ded` frontend.** Accepts Molly's dialect so the Molly corpus runs verbatim (CR-13): `@next`, `@async`, `@k`, `notin`, `count<X>`, `include`, with the first column as the location. — _R02 §12.1; R06 §3.2_
- **LANG-221** `P2` **Overlog/NDlog frontend.** Accepts `materialize`, `periodic`, `delete` rules, `f_` functions, 1-based keys and `@` location specifiers. Compiles them to the core semantics, with the differences in CR-01..08 documented. — _R01 §10(1)_
- **LANG-222** `P2` **Hydroflow `datalog!` frontend.** Accepts `:-`, `:+`, `:~`, `.persist`, `.input`, `.async` and `index()`, so the autocomp protocols run. — _R08 §5; R07 §9.3_
- **LANG-223** `P2` **Bloom collection-expression syntax.** Accepts `lhs <= rhs.pairs(..)`, `group`, `argagg` and `notin` for porting bud-sandbox code, unless this becomes the primary surface syntax (ODD-01). — _R03 §2_

### 2.14 Principals, sessions and authorization
Keywords in this subsection (`from`, `principal`, `accept from`) are provisional until ODD-01 is settled. Their semantics is normative.
- **LANG-240** `P0` **`Principal` type and node identity.** A `Principal` is an authenticated identity, such as a SPIFFE ID `spiffe://td/prog/role/n3`. It is distinct from `Node`, which is a routable location. The directory has rows `node(Node, Address, Principal, Role)`, and `principal_of(n)` and `role_of(n)` are built-ins over it. The directory is `static` for static membership and epoch-sealed for dynamic membership (DIST-041, DIST-042). — _R15/G5 §4.2; SecureBlox §5.1 `principal_node`; SPIFFE X.509-SVID_
- **LANG-241** `P0` **Implicit `sender` and `principal` columns on received messages.** A received channel tuple carries two runtime-populated columns: `sender`, which is a `Node` or a `Session`, and `principal`. They are bound with `m(@self, ..) from S` and `… principal P`. They are never part of the payload, so they cannot be forged. When no rule reads them they are projected away (SEM-091). They are SeNDlog's import predicate `S says m`, authenticated hop by hop by the transport (DIST-060). — _R15/G5 §4.5; SeNDlog ICDE'09 Def. 1_
- **LANG-242** `P0`/`P1` **Channel and module ACLs.**
  - `P0` **Inferred, default-deny ACL.** A channel accepts a frame only from roles that have an async rule with that channel in the head. The compiler computes this set by projecting the choreography (LANG-009). With it, a client cannot send `append_entries`, and nobody has to write an ACL.
  - `P1` **Explicit ACLs.** `accept from <role | external client | principal in R>` narrows the inferred ACL, or opens an external channel. `R` must be a unary `static` or `table` relation of the receiving node, read at the last committed tick.
  - `P1` **Module interface ACLs** (LANG-003). An importer may narrow a module's ACLs but not widen its peer channels.

  Enforcement happens at ingress (DIST-062). Consistency is checked by ANA-105. — _R15/G5 §4.6; SecureBlox §3.2 `writeAccess[T]`; LBTrust §4.1 `mayWrite`_
- **LANG-243** `P1` **External clients and sessions.** Clients are the `external` role and are not `Node`s.
  - `session_open(S, P, T)` and `session_closed(S, Reason)` are input events.
  - Ingress channels carry the implicit columns `session` and `principal`.
  - Replies go to `@S` on egress-only channels. A reply to a closed session is dropped and counted.
  - Raft client ids (FLAG-011) are checked against `principal` (ANA-106).
  - A follower that forwards a request carries an `on_behalf_of` principal, which the leader accepts only on channels whose ACL admits only cluster roles.

  — _R15/G5 §4.8; R08 §6.1 (`External`)_
- **LANG-244** `P1` **Rule-level authorization idiom.** Policies derive `authorized(P, Op, Obj)`: RBAC, Binder-style delegation, k-of-n thresholds (LIB-120–122). A missing authorization derives `authz_denied(Session, Op, Obj, Reason)`, which becomes an error reply and is counted in `authz_denied_total{rule}`. This is program logic, not an omission (CR-40). — _R15/G5 §4.7; Binder §3; LBTrust §4_
- **LANG-245** `P2` **`signed<T>` values.** `sign(T)` uses the local key through the host keystore. `verify(signed<T>) -> Option<(Principal, T)>` is a pure, memoized UDF. They give end-to-end authenticity for statements relayed through other nodes: SeNDlog's honesty constraint, a client-signed command inside a Raft entry, a CA-issued Chord `nodeIDCert`. A `signed<T>` is opaque bytes for schema evolution. — _R15/G5 §4.13; SeNDlog Def. 3; Binder §3_

### 2.15 Program versions, schema evolution and migrations
- **LANG-260** `P0` **Program version and schema lock.** A program declares `program X version N;`. A checked-in, canonical `schema.lock` holds, for every released version and for every channel, durable relation and wire type:
  - field numbers, names, types and defaults;
  - key columns;
  - lattice type identities;
  - reserved numbers;
  - canonical-form hashes;
  - the minimum supported version.

  `bloomc release` appends a version. CI fails when a schema changes without a version bump (TEST-108). — _R15/G5 §6.2; SOSP'21 Finding 8 (KAFKA-10173); CockroachDB MinSupported_
- **LANG-261** `P0` **Stable field numbers.** Every column of a channel, durable relation or interface, and every field of a type that reaches one, has a stable field number `#n`. The compiler auto-assigns numbers and records them in the lock.
  - Numbers are never reused, and removed numbers become `reserved`.
  - Fields added later carry defaults (`= v` or `Option`) and `since N`.
  - Stored or sent ADTs and enums must have an `unknown` fallback variant, which is kept as opaque bytes and re-encoded identically.
  - Variants are encoded by stable number, never by index.
  - The in-memory layout stays positional.

  — _R15/G5 §6.2–6.3; Protobuf proto3 "Updating A Message Type"; Avro schema resolution; SOSP'21 (HDFS-15624, HBASE-25238)_
- **LANG-262** `P0`/`P1`/`P2` **`migrate from N { … }` blocks.** Their rules read `old.r`, which is typed by the lock's version-N schema, and write durable relations of the current version.
  - They are deterministic and restartable. Temporal and async rules, `now()` and `random()` are forbidden, and pure UDFs are allowed.
  - They run at recovery (DIST-082), and at finalization for non-monotone migrations (ANA-103).
  - Several versions are processed one step at a time.
  - A key collision that the migration causes is a hard error naming both source tuples (SEM-050).
  - Priorities: `P0` for auto-synthesized migrations (defaults, projections, widening, renames) and explicit tuple-local blocks; `P1` for general monotone and non-monotone blocks; `P2` for `migrate from N down`.

  — _R15/G5 §6.4; Ajmani ECOOP'06 §5 (transform functions); Erlang `code_change/3`_
- **LANG-263** `P1` **Cross-version channel translation.** `emit c to N { old.c(..) <= c(..) }` and `accept c from N { c(..) <= old.c(..) }` are tuple-local rules: one channel atom, pure UDFs, no state (ANA-103). They run in the codec layer (DIST-087). A tuple that matches no `emit` rule is *disallowed*: it is dropped and counted as an omission (SEM-090). Mixed-version behavior that needs state is written as explicit dual channels gated by `cluster_version` (LIB-123). — _R15/G5 §6.5; Ajmani ECOOP'06 §3–4 (simulation objects, the disallow constraint)_
- **LANG-264** `P1` **`cluster_version` gates.** `cluster_version` is a built-in input of type `lmax<u32>`, sampled once per tick and recorded for replay. It is read only through thresholds, `cluster_version.at_least(V)`, so gate reads are monotone (SEM-092). A feature marked `since V` may be written or sent only under such a gate (ANA-102). `unsafe_ungated "reason"` overrides the check and is reported. — _R15/G5 §6.6; CockroachDB version gates; etcd cluster version_
- **LANG-265** `P1` **Semantic-change and deprecation annotations.** `semantics_changed since N` on a field forces a new field number. `deprecated since N` warns at every use. — _R15/G5 §6.3; SOSP'21 §4.1.2 (KAFKA-7403)_

---

## 3. Semantics (SEM)

### 3.1 Tick model
- **SEM-001** `P0` **Per-node logical clocks.** Each node counts ticks in a gap-free sequence starting at 0. There is no global time. Clocks on different nodes are related only through happens-before. — _R02 §14.13, §9; R03 §3.3_
- **SEM-002** `P0` **The tick transition.** This is exactly TPLP's local transition. Each tick runs these steps in order:
  1. Apply the deletions, then the insertions, that were staged for this tick. Clear the tick-local relations. Ingest the batch: delivered messages, timer events, host inputs, and the `now`/`random` samples.
  2. Compute the stratified deductive fixpoint.
  3. Evaluate the inductive rules, which stage facts for t+1, and the async rules, which fill the outbox.
  4. Commit durable state.
  5. Release the outbox.
  6. Run callbacks.

  — _R02 #27, §6.2; R03 (BB-45); R07 #1_
- **SEM-003** `P0` **Inductive and async rules read the completed fixpoint.** They may negate or aggregate any stratum. Each is evaluated exactly once per tick; they never recurse. — _R02 §14.6–7; R03 (BB-29)_
- **SEM-004** `P0` **Relations only grow within a tick.** Every mutation takes effect together at the next tick boundary. All atomicity rests on this. — _R03 §3.1; R01 #18_
- **SEM-005** `P0` **Deletion takes effect at the next tick.** A fact deleted at t still holds at t and is absent from t+1 on, unless something re-derives it. — _R02 §14.1; R03 (BB-25)_
- **SEM-006** `P0` **Insert wins.** If a fact is both deleted and inserted for t+1, it is present at t+1 (CR-05). — _R02 §3.3; R03 §2.3_
- **SEM-007** `P0` **Set semantics per tick.** Every relation is a set within a tick, including channel batches. There is one message per (sender, send tick, receiver, fact). Copies of the same fact sent at different ticks are distinct messages (CR-03). SEM-105 generalizes this to lattice-valued relations: a message is identified by (sender, send tick, receiver, relation, key) and carries the join. — _R02 #30, §14.10; R08 #31; R11/G1 §3.3_
- **SEM-008** `P0` **Facts are ephemeral by default.** A relation that is not persisted holds only at the tick where it was derived or delivered. — _R02 §14.3_
- **SEM-009** `P0` **Event-driven ticks.** A node ticks only on a message, a timer event, host input, or a staged state change. An idle stretch is observationally equivalent to a run of empty ticks, except through entanglement (ODD-04). — _R03 (BB-46); R08 #2; R02 §14.17–18_
- **SEM-010** `P0` **Quiescence.** A node is quiescent at t when its state is equivalent modulo time to its state at t−1 and it has no pending inputs. Simulation and model checking stop on quiescence. Ultimately periodic behavior is detected when a state repeats. *Amended (R13):* quiescence is a global-observer notion for the simulator and model checker only. A running node never infers completion or finality from it (CR-36, SEM-016). — _R02 #29; R10 V0(2); R13 §9.1_
- **SEM-011** `P1` **Time skipping.** When entanglement makes tick values observable, an idle node advances its tick counter by the number of ticks it skipped. — _R02 #29, §9_
- **SEM-012** `P0` **Bootstrap at tick 0.** Program facts, static relations and bootstrap rules are all visible at tick 0. — _R03 §2.1_
- **SEM-013** `P0` **Documented answers to NR09's ambiguities.** In Fig. 1, the update message carries the value from before the increment (CR-04). In Fig. 2, every node, including the node that sends to itself, increments exactly once (CR-03). — _R01 #43, T14–15_
- **SEM-016** `P1` **Final answers (free termination).**
  - *Definition.* A fact, or value, of output O at node n and tick t is **final** iff n's state at t is a
    free-termination state for "is this fact in O" (or "the value of O"). That means every state reachable under a
    fair continuation respecting the declared seals, schemas and fault model gives the same answer.
  - *Finality is monotone.* An FT state reaches only FT states with the same value, so finality bits are
    never-retracted facts and may travel on raw lattice channels.
  - *Emission rule.* A `final` emission that some continuation contradicts is a correctness bug (TEST-088).
  - *Consistency.* Finality is not agreement. Replicas are guaranteed to finalize equal values only when the state
    space is a join-semilattice or the updates commute (FT Prop 15, 33). Otherwise "final and consistent" also
    needs ANA-029.
  - *Key results.* Positive coordination-freeness holds iff the query is monotone, and negative iff it is antitone
    (FT Thm 24–25). A (query, input) pair is coordination-free-correct iff the input is an FT state (Thm 22).

  — _R13 §8–9; FT Def 3, Thm 22, 24, 25, Prop 15, 33_
- **SEM-017** `P1` **Inverse curse.** Suppose every reachable state of the relations an output depends on can return to an identity state. This covers Z-set inputs that can carry negative weights, deletable host inputs, `<-` or `<+-` driven by unsealed input, and PN-style values. Then no non-constant output over them is ever final, unless a seal closes those relations. This is normative: the compiler rejects `final` on such outputs (LANG-212, ANA-120), and Tide panes that accumulate and retract stay provisional until their watermark seal. Every undoable replicated type is a tuple of counters (Dolan), so no choice of representation escapes this. — _R13 §5, §8.4; FT Thm 18, Cor 20, App. B; Dolan PODC'20_

### 3.2 Stratification
- **SEM-020** `P0` **Temporal stratification is the acceptance rule.** Only the deductive reduction has to stratify. A cycle through negation, aggregation or a non-monotone lattice operation is allowed only if it passes through an `@next` or `@async` edge. Every accepted program has a unique perfect model per tick. — _R02 #19, Lemma 2; R03 (BB-52)_
- **SEM-021** `P0` **Negative edges.** These edges are negative:
  - the argument of a `notin`;
  - a non-lattice aggregate;
  - a deletion;
  - an outer join;
  - an order-sensitive operator;
  - a choice site: `choose` in any form, `choose_rand`, `seq` (SEM-086);
  - an antitone or non-monotone lattice operation;
  - `reveal`.

  Joins, projection and union are positive, as are lattice morphisms, bimorphisms, monotone functions and thresholds. — _R04 D41; R03 (BB-51); R05 #1–#2_
- **SEM-022** `P0` **Stratification algorithm.**
  1. Condense the same-tick dependency graph into strongly connected components.
  2. Reject any component that contains a negative edge, and report the cycle as a witness.
  3. A relation's stratum is the longest path to it, counting only negative edges.
  4. A rule goes in the highest stratum among its body relations, plus 1 if it crosses a negative edge.
  5. Temporal rules go in a final pseudo-stratum.

  — _R03 §4.3; R08 #9_
- **SEM-023** `P1` **Independence from the stratification chosen.** The result does not depend on which valid stratification is picked. — _R02 §5.1_

### 3.3 Lattice semantics
- **SEM-030** `P0` **One lattice value per key.** A persistent lattice only grows over time: l@t+1 ⊒ l@t. — _R04 §10.1_
- **SEM-031** `P0` **Recursion through monotone lattice operations.** Recursion through morphisms, bimorphisms and monotone functions is allowed within a stratum. The meaning is the least fixpoint in the lattice. — _R04 D41, T3_
- **SEM-032** `P0` **Termination.** Termination needs the ascending chain condition or data that converges. A configurable iteration bound is enforced as a hard error, never as silent truncation. — _R04 D42, §2.3_
- **SEM-033** `P0` **Morphism means join-preserving.** See CR-23. — _R04 §2.2, §3.4_
- **SEM-034** `P0` **⊥ normalization.** A map entry whose value is ⊥ counts as absent. Sending ⊥ is the same as sending nothing. SEM-101 gives the complete ⊥-normalization rules. — _R04 §10.1(6), §7.6; R11/G1 §3.1_
- **SEM-036** `P1` **Lattice-wrapper semantics for group-valued updates.**
  - A wrapped channel is a lattice W (grow-only dot set, or per-origin `Lex<seq, cumulative>`) together with a
    translation T from Δ(W) to G.
  - Normative invariant: replica state = base + Σ over applied dots of payload(dot). Each dot is applied exactly
    once, whatever the duplication, reordering, loss-with-resend or batching.
  - Consequence: replicas that have applied the same dots hold identical group state, and so identical views,
    because the group is commutative and associative.
  - Dots are globally unique: `(origin, incarnation, seq)`. Reusing a dot with a different payload is a hard
    `Conflict` error.

  — _R13 §6; Wrapping Rings §3.1–3.2_

### 3.4 Asynchrony
- **SEM-040** `P0` **Normative async semantics: fair causal delivery (CR-12).**
  - Messages in flight form a multiset.
  - Each message gets one arrival tick, strictly after its send in happens-before.
  - Only finitely many messages arrive at any one tick.
  - Every message is eventually delivered.
  - The receiver reads the batch delivered at a tick as a set.
  - A message addressed to an unknown node is dropped.

  — _R02 #30, §6_
- **SEM-041** `P0` **Self-sends arrive strictly later.** — _R02 §5.3_
- **SEM-042** `P0` **Communication is transparent.** A delivered fact cannot be told apart from a local fact of the same relation. — _R02 §6.2_
- **SEM-043** `P0` **Network modes.**
  - Fair async: the default for reasoning.
  - Lossy: a message may never arrive (⊤/NEVER).
  - Synchronous rounds: delivery at t+1, used for LDFI and BSP.
  - Non-causal replay: used only to recover positive, consistent components (CRON Thm 6.1).

  — _R02 #31, #35, §15.2(6)_
- **SEM-044** `P0` **Output and confluence.** The output of a run is its ultimate facts, meaning the facts of the declared output relations that eventually hold forever. A program is confluent if every input has exactly one ultimate model. A program is consistent if all fair runs agree; this is a stronger property and is reported separately (CR-29). — _R02 #32; R10 A1(4)_
- **SEM-045** `P1` **The `pure(P)` translation is part of the spec.** The rules for dynamic choice, causality and finiteness are published as the formal definition of async behavior. For small programs they double as an ASP test oracle. — _R02 #40; R10 A1.1_

### 3.5 Keys and conflicts
- **SEM-050** `P0` **Key constraint.** Two distinct tuples with the same key in one relation at one tick are a runtime error. That applies whether they come from rules, from `<+` arrivals or from a channel's send buffer. The exception is when every column that differs is lattice-typed; then the tuples merge (CR-07). — _R03 (BB-09, BB-10)_
- **SEM-051** `P0` **Conflicting upserts.** Two different `<+-` writes to the same key for the same tick are an error. A deterministic resolution has to be written out explicitly, with argmin, choose or a lattice. — _R03 (BB-26, T19)_
- **SEM-052** `P1` **Overlog key overwrite.** Compiled to an upsert at the next tick that emits both the deletion delta and the insertion delta. — _R01 #8_

### 3.6 Soft state
- **SEM-060** `P1` **Deterministic expiry.**
  - TTLs are checked at tick boundaries against that tick's `now`.
  - An expiry is a deletion delta.
  - Re-deriving an identical tuple resets its birth time; it does not count as an insertion.
  - When a table is at its maximum size, the oldest tuple is evicted first, ordered by (birth, canonical order).

  — _R01 #9–#11, §10(4)_
- **SEM-061** `P1` **Cascaded refresh.** Refreshing a soft body tuple also refreshes the soft heads derived from it. — _R01 §2.3(5), §5.5_

### 3.7 Failures and durability
- **SEM-070** `P0` **Crash-stop.** From the tick it crashes, a node fires no rules and sends nothing. Its final state is frozen, and only spec rules can see it (CR-20). — _R06 B16; R10 A3.4(1)_
- **SEM-071** `P0` **Crash-recovery.** When a node restarts, its non-durable relations are empty, its durable relations are reloaded, and it begins a fresh tick. Programs must tolerate losing the messages that were in flight. — _R10 A3.4(8); R07 §11.5_
- **SEM-072** `P0` **Durability barrier.** The messages derived at tick t are released only after t's durable deltas have been fsynced. Group commit is allowed. — _R07 #1, §2.1_
- **SEM-073** `P1` **Omissions and partitions.** A message is lost per (sender, receiver, send tick). A partition is modeled as a burst of such losses. — _R06 B15; R10 A3.4_
- **SEM-074** `P2` **Byzantine faults.** Out of scope for v1, and noted for BFT extensions. — _R06 §2.2; R07 §9.4_

### 3.8 Determinism
- **SEM-080** `P0` **Where nondeterminism comes from.** The only sources are:
  - when and in what batches messages are delivered;
  - timers and `now`;
  - host inputs;
  - crashes and restarts, including each incarnation's boot nonce;
  - the seeds (SEM-084), from which every `choose`, `choose_rand`, `rand` and `random` value is computed;
  - choice overrides, in simulation and replay only (TEST-012);
  - `nondet` operators.

  Every one of them can be injected and recorded. Choices and random draws are recomputed from the seeds, not
  recorded (R12/G2). — _R10 A2.7; R08 §6.3; R12/G2 §7_
- **SEM-081** `P0` **Each node is internally deterministic.** Given the same inputs, seeds, incarnation and
  overrides, a tick always produces the same result. Replay and LDFI completeness both depend on this. — _R06 §3.8;
  R08 §2.2; R12/G2 §4.1_
- **SEM-082** `P0` **Monotone programs are eventually consistent.** Once a monotone program quiesces, its distributed state equals the centralized result. This is tested. — _R01 #35; R05 §2_
- **SEM-083** `P0` **The operator determinism contract.** For every nondeterministic or order-sensitive operator,
  the output at a tick is a function of four things:
  - the operator's input *set* at that tick;
  - state that the operator's own Dedalus expansion carries with `@next`;
  - the seeds;
  - the overrides.

  The output never depends on iteration, hash, interning, arrival-within-tick, plan or cache order. Each operator is
  *defined* by a Dedalus expansion over the canonical order and the PRF. Naive evaluation of that expansion is the
  reference that ENG-067 compares against, exactly. — _R12/G2 §4.1; DBSP §2; Shared Arrangements §5.3_
- **SEM-084** `P0` **Seeds, sites and the PRF.**
  - The root seed ρ is the run seed in simulation, and the deployment seed in production (DIST-033).
  - The choice seed σc = PRF(ρ, "choose") is shared by all nodes. Each node also has its own node seed σn.
  - Each node has an incarnation: a durable restart counter plus a boot nonce.
  - Site ids are stable, derived from the module, the rule label and the operator's ordinal.
  - The PRF is fixed and versioned (recommendation: SipHash-1-3). It runs over cached canonical value fingerprints
    (ENG-032).
  - Collisions only create ties, and ties are broken by canonical order.

  — _R12/G2 §4.3; Random123 (SC'11); rendezvous hashing (ToN'98)_
- **SEM-085** `P0` **`choose` semantics.**
  - There is one choice per (node, tick, X̄): the candidate with the least `(PRF_σc(site, X̄, Ȳ), Ȳ)`. The result is
    maximal, so it is a choice model (GZ01 Thm 5.3).
  - A user-level FD that leaves out the tick is rejected. It would be Krishnamurthy–Naqvi static choice over all of
    time, which is non-causal.
  - `choose_rand` uses the node seed, with the incarnation and tick in the key.
  - Precedence: an override beats a sticky keep, which beats priority. An override that names a non-candidate is a
    hard error.

  — _R12/G2 §4.2, §5.1, §5.6–5.7; DL11 §5.3; SZ90 §6; GPSZ91 §4_
- **SEM-086** `P0` **Choice is stratified.** No choice site and no order-sensitive site may be on a same-tick recursive
  cycle. The error reports the cycle as a witness (ANA-002). Cycles through `@next` are fine. This is SZ90's condition
  (c), and GSZ95/LDL++'s "stratified modulo choice": each stratum is evaluated as a choice fixpoint over complete
  candidates. — _R12/G2 §5.13; SZ90 §6; GSZ95 §4; LDL++ §2_
- **SEM-087** `P1` **Nondeterminism classes.** Every relation and output gets one of three classes:
  - **deterministic**;
  - **seed-dependent**: stateless and multi-FD `choose`, and `choose_least` with possible ties;
  - **schedule-dependent**: `choose sticky`, `choose_rand`, `rand`/`random`, `seq`, `tick()` and `now()`.

  The classes feed ANA-029, ANA-039 and TEST-003. Stateless `choose` is non-monotone, like `min`. Sticky choice is
  monotone in time for insert-only candidates, but it is not confluent. — _R12/G2 §5.13; GPZ01 §9_
- **SEM-088** `P0` **Canonical order compares values, never intern ids.** Intern ids (ENG-020) are assigned in
  arrival order, so they may be used only for equality, hashing and joins. Every externally observable order is
  canonical: list aggregates, callbacks, `stdio`, dumps and traces. — _R12/G2 §4.4_

### 3.9 Security and versioning semantics
- **SEM-090** `P0` **Every ingress rejection is an omission.** A message rejected at ingress (DIST-062) never enters a tick. The reasons are handshake, expiry, identity, version window, schema/decode, disallowed translation, ACL or quota. Such a rejection is exactly a loss in the sense of SEM-073. Hence:
  - every execution under enforcement is an execution under lossy delivery with those messages lost;
  - every safety property that holds under lossy delivery is preserved;
  - CALM, Blazes and Edelweiss results and LDFI verdicts carry over (CR-40).

  Liveness additionally needs protocol messages to eventually stop being rejected. That is why rejections must be observable (DIST-063) and self-contradictory ACLs are rejected statically (ANA-105). — _R15/G5 §4.9_
- **SEM-091** `P0` **Meaning of the `sender` column.** For an async rule `m(@D, X̄)@async :- body` evaluated at S, the delivered tuple is `m(@D, X̄, S)`. The sender is already part of message identity (SEM-007). When no rule reads the column it is projected away, so identical facts from different senders merge as before. When a rule reads it, they stay distinct. `principal` = `principal_of(sender)` for peers, and the session's principal for clients. — _R15/G5 §4.5_
- **SEM-092** `P1` **Meaning of version gates.** `cluster_version` is an input sampled once per tick that only rises, because downgrade after finalization is forbidden. Threshold reads of it are monotone, so gates add no points of order. In a replicated-state-machine program an activation is a log entry. Every replica interprets entry k by the rules active at version(k), which keeps apply deterministic (SEM-081). — _R15/G5 §6.6; etcd `ClusterVersionSetRequest`_
- **SEM-093** `P1` **Meaning of an upgrade.** An upgrade of node n to program P′ is a crash (SEM-071) followed by recovery under P′. P′'s durable relations are initialized to M(checkpoint) by the migration M (LANG-262). Messages in flight are lost as with any crash. — _R15/G5 §6.9; Ajmani ECOOP'06 §2.1_
- **SEM-094** `P2` **Meaning of a hot install.** A hot install switches programs at a tick boundary t: program P runs ticks ≤ t and P′ runs ticks > t. State is carried through a migration run between ticks. Stratification and all ANA checks are re-run first, and the switch is atomic. In-flight messages are translated or dropped as in LANG-263. — _R15/G5 §6.10; R07 §2.3 (JOL); Erlang code replacement_

### 3.10 Dedalus^L: formal semantics of lattice-valued relations (R11/G1)
- **SEM-100** `P0` **Lattice-valued schema.** Every relation has key columns and exactly one value lattice. Set relations are the case over 𝔹, and several lattice columns form a product. — _R11/G1 D2; CR-50_
- **SEM-101** `P0` **⊥-normal instances.** An instance is a finite map from cells to non-⊥ canonical values, with order and join taken pointwise. Morphisms need not be strict. Rules N1–N6:
  - there are no ⊥ cells;
  - equality, hashing and message identity use canonical forms;
  - a ⊥-valued derivation, `<+` or send is a no-op: no message, no lineage leaf, no wake-up;
  - generators range over the support only;
  - lookups return ⊥ for absent cells;
  - presence is a threshold.

  — _R11/G1 D3; Ross–Sagiv §2.3.2; Flix §3.2; SEM-034_
- **SEM-102** `P0` **Occurrence polarity and L-stratification.** A lattice occurrence is monotone if every path from it to the head or to a guard has an even number of Anti operations and no NM operation. Otherwise it is exact: `reveal`, `==`, non-lattice aggregates and negation are exact. Exact occurrences create − edges, and only deductive rules must stratify. — _R11/G1 D4, D7; TPLP §3.2.2_
- **SEM-103** `P0` **Immediate consequence and stratum evaluation.** T_P(I) = I ⊔ ⨆ of all head contributions, with lattice variables bound to the exact cell values. Each stratum's result is the finite Kleene fixpoint above its input. If iteration does not converge, the result is undefined and the engine raises a hard error (CR-53). — _R11/G1 D6, D8; Flix §3.2; Datalog° Def. 3.1_
- **SEM-104** `P0` **Transition with ⊔.** The local transition is TPLP's with ∪ replaced by ⊔: I = st ⊔ (delivered batch), D = deduc(I), st′ = H ⊔ induc(D). A persistent lattice has an implicit identity inductive rule. A tick-scoped lattice holds only its `<+` contributions. — _R11/G1 D9–D10; TPLP §5.1.3; CR-24_
- **SEM-105** `P0` **Merge at the sender, merge within the delivered batch.** async_P(D) has one cell per (addressee, relation, key) per step (CR-52). Same-key arrivals in one step merge. Across steps nothing merges without a persistent sink. SEM-007 is the case over 𝔹. — _R11/G1 §3.3; Bud `pending_merge`, `receive_inbound`_
- **SEM-106** `P0` **Declarative semantics.** In `pure^L(P)`:
  - `snd_R` is a lattice relation;
  - the arrival choice is keyed by (x, s, y, t, k̄);
  - the arrival rule merges into the receiving relation.

  L-stable models use the Ross–Sagiv §5.5 reduct, which reduces only exact literals, and all their values must be finite. On set programs this reduces to Gelfond–Lifschitz; on monotone programs it reduces to FLP. — _R11/G1 D12–D13; TPLP §4.3–4.5; Ross–Sagiv §5.5; FLP Thm 3.5_
- **SEM-107** `P0` **Ultimate L-models.** ult(c) is the ideal of eventually-always lower bounds of a cell's history. It is compact if the history is eventually constant, and a limit if the history increases forever. On sets it equals MAR's ultimate facts. Confluence and consistency are stated over ultimate L-models and ultimate threshold facts. — _R11/G1 D11; MAR §2.2_
- **SEM-108** `P1` **Theorem 4^L proof obligation.** The operational and declarative semantics coincide under conditions C1–C6:
  - C1: the program is L-stratified;
  - C2: every in-tick fixpoint converges;
  - C3: each transition sends finitely many messages;
  - C4: lattice operations are total and deterministic;
  - C5: no entanglement;
  - C6: messages are merged at the sender.

  This must be proved on paper and in VER-044, and guarded by the BENCH-312 differential test. — _R11/G1 §4.1 [ours]_
- **SEM-109** `P1` **Delta shipping has a side condition.** A channel may carry deltas instead of full values only if every consumer is a join-morphism into persistent state. Otherwise the batching is observable. — _R11/G1 §3.3 [ours]; ODD-05_

---

## 4. Engine and performance (ENG)

### 4.1 IR and compilation
- **ENG-001** `P0` **Dedalus IR.** Every relation carries an explicit location and time. Every rule is tagged deductive, inductive or async, with an explicit delay term. All analyses, rewrites and backends work on this IR. — _R02 §15.2(1)_
- **ENG-002** `P0` **Logical plan per rule.** Each rule becomes a tree of logical operators, kept separate from recursion control. It is lowered to one physical plan per semi-naive version. — _R09 #24–#27, §5.4, §14.3_
- **ENG-003** `P0` **Persistence is storage.** Identity persistence rules (`p@next :- p [, notin del_p]`) are recognized and compiled to stored tables with birth/death intervals, never re-derived every tick. — _R02 #28; R08 (R4); R09 #33_
- **ENG-004** `P0` **Persist/delta incrementalization.** Rules over persisted relations are maintained at O(|Δ|) per tick, using the persist_pullup family of rewrites, e.g. Join(P,P) → P(Δ(Join)). The meaning is still Dedalus replay (CR-26). — _R08 #46, §10_
- **ENG-005** `P1` **Interpreter and codegen on one kernel library.** Generic kernels are pre-instantiated per arity. The interpreter runs a compact tree and pulls batches of about 128 tuples. The Rust codegen emits straight-line calls to the same kernels. Both have identical semantics, and the interpreter stays within 1.5–3× of compiled code (ODD-07). — _R09 #43–#44, §14.4; R08 (R2)_
- **ENG-006** `P1` **Shape of the compiled dataflow.**
  - The graph is split into in-out trees: pull fan-in → pivot → push fan-out.
  - Subgraphs run on a static topological schedule.
  - Per-tick buffers are arena-allocated; next-tick buffers are double-buffered.
  - Code is generated in phases: prologue, iterator, tick-end.

  — _R08 #22–#25_
- **ENG-007** `P1` **Plan and graph dumps.** Can print the rewritten program, strata, physical plans, and the dataflow graph (mermaid or dot). — _R08 #47; R03 (BB-63)_

### 4.2 Storage and indexes
- **ENG-020** `P0` **Interning.** Strings, records/ADTs and locations are interned to fixed-width ids. Columns are u64 words, packed to u32 where the type allows. — _R09 #12, §14.1_
- **ENG-021** `P0` **Stamped append-only tables.** Every row carries a (tick, iteration) stamp. The Δ, old and full sets are row ranges found by binary search, so Δ is never copied. — _R09 #5, §3.3_
- **ENG-022** `P0` **Primary dedup hash index.** Open addressing over the key columns. A lattice relation is keyed by its non-lattice columns. The lattice value is merged in place (`join_mut`), and the row re-enters Δ only if the value changed. — _R09 #6, §14.2_
- **ENG-023** `P0` **Minimal index selection.** Indexes are chosen by a chain cover, computed with maximum bipartite matching over every search in every rule version. — _R09 #13_
- **ENG-024** `P1` **Permutation-encoded sorted indexes.** Each index stores its columns permuted, so a single comparator serves every index. — _R09 #14_
- **ENG-025** `P1` **Geometric run merging.** Sorted runs are kept LSM-style and merged while the last run is at most twice the new one. Merge-joins and dedup use galloping search. — _R09 #15–#16_
- **ENG-026** `P1` **Lazy hash tries (COLT).** A hash level is built on the first probe, and the relation that covers the join is iterated without building any index. — _R09 #17_
- **ENG-027** `P1` **Pluggable representations.** A trait with read, read-all, write and merge(new, Δ, total) operations. The trait enforces that no fact skips Δ. Concurrent variants exist. — _R09 #10, #18_
- **ENG-028** `P1` **Specialized representations.** Nullary flags, dense bitmap tries (Brie), union-find for equivalence relations with Δ extension, and union-find for transitive relations. — _R09 #11, #19_
- **ENG-029** `P0` **Interval history.** Persistent facts carry birth and death ticks. History is kept back to a compaction frontier (EOT when running under LDFI). This serves both as-of queries and provenance. — _R09 §12(4), #38; R06 §10(3)_
- **ENG-030** `P1` **Cheap snapshots and forks.** Runs are immutable and shared with `Arc`, so snapshotting or forking a node's state costs O(number of runs). — _R09 §12(9); R07 §11.7_
- **ENG-031** `P1` **Storing lattice values.** Small lattices are stored inline. Large ones (sets, maps) are stored behind ids in a side store and merged in place. A singleton delta can be merged directly into a hash-set total, even though the two are different types. — _R09 §14.1; R04 A2_
- **ENG-032** `P0` **Canonical fingerprint per interned value.** A 64-bit hash of the value's canonical encoding
  (recommendation: xxh3-64), computed once at interning and cached next to the id. SEM-084's PRF runs over these
  fingerprints, so computing a priority costs one short PRF call. The encoding and the hash carry versions, which are
  recorded in traces. — _R12/G2 §4.3_

### 4.3 Fixpoint evaluation
- **ENG-040** `P0` **Scheduling by SCC.** The strongly connected components of the relation graph run in topological order. A non-recursive component runs once; a recursive one runs to a fixpoint. — _R09 #1_
- **ENG-041** `P0` **Semi-naive evaluation.**
  - Each recursive relation keeps three sets: full, Δ and new. A tuple enters new only if it is not already in full.
  - A rule with k recursive atoms gets k delta versions, never 2^k−1.
  - Evaluation stops when every new set is empty.

  — _R09 #2–#3_
- **ENG-042** `P0` **Recursion inside a tick.** A recursive component runs as a nested loop. The two halves of each symmetric join keep their state across iterations (CR-11). — _R08 (R3, #10)_
- **ENG-043** `P0` **Semi-naive evaluation over lattices.**
  - Δ holds the keys whose value strictly increased, each carrying its full new value (Flix).
  - A rule that uses only morphisms may run on increments.
  - A monotone but non-morphism function is re-evaluated on the cumulative value.
  - For a bimorphism, Δ = f(ΔA,B) ⊔ f(A,ΔB).

  — _R04 D35–D38; R09 #6–#8_
- **ENG-044** `P0` **Merge reports whether it changed anything.** Merges happen in place. The "changed" result decides when the fixpoint terminates and filters Δ. — _R04 A1, D39; R08 #33_
- **ENG-045** `P1` **Minimal deltas.** For set, map, pair, chain and Lex-over-chain lattices, the optimal Δ(a,b) is computed from the join decomposition (Atomize). Only the changed items are emitted, as in Hydro's `state[items]`. — _R04 A4–A5, D40_
- **ENG-046** `P1` **Stop at ⊤.** Once an operator's input reaches ⊤, the engine stops maintaining that operator's state. — _R04 D43_
- **ENG-047** `P0` **Iteration guard.** Exceeding the configured iteration bound is a hard error. The compiler warns statically about recursion through lattices that do not satisfy the ascending chain condition. — _R04 D42_
- **ENG-048** `P1` **Early exit for nullary heads.** — _R09 #4_
- **ENG-049** `P1` **Semi-naive over dioids.** For min/max-plus recursion, Δ is computed with ⊖ and keeps only strict improvements. — _R09 #9_
- **ENG-050** `P2` **Eager work-stealing mode.** Meant for rules that call expensive external functions. — _R09 #46_
- **ENG-051** `P2` **Subsumption clauses.** Rules that delete dominated tuples, as an alternative to lattices. — _R09 #47_

- **ENG-140** `P0` **Divergence detection per stratum.** ENG-047's iteration bound applies per stratum and per tick. Exceeding it aborts the tick, and the error names the offending cells and their last two values as a witness (T7-neg, T10b). — _R11/G1 §3.2 D8, T7, T10b_
- **ENG-141** `P1` **Base-point derivatives for monotone non-morphisms.** `size`, `sum_values` and user-defined Mon functions may supply a derivative f′(x, dx) with f(x ⊔ dx) = f(x) ⊔ f′(x, dx). Semi-naive evaluation can then avoid recomputing them from the total. — _Datafun POPL'20 §4.3, Lemma 4.1; R11/G1 §2.5_
- **ENG-142** `P1` **Termination classification.** Each recursive lattice stratum is reported as one of:
  - ACC;
  - p-stable (for example Trop+ `lmin` with non-negative `+`);
  - PreM inflation- or deflation-preserving;
  - "unknown", which produces a warning.

  — _Datalog° Thm 1.2; PreM Prop. 1; R11/G1 §3.2_

### 4.4 Incrementality across ticks
- **ENG-060** `P0` **Lifetime analysis.** Each relation is classified as one of:
  - tick-local: dropped wholesale at the end of the tick;
  - static-monotone: continued semi-naively from the new base facts;
  - static-non-monotone: maintained with signed diffs.

  — _R09 #31, §12(2)_
- **ENG-061** `P0` **Dirty-input scheduling.** A tick runs only the strata reachable from relations whose Δ is non-empty. The fixed cost per tick allocates nothing. — _R09 #32, §12(1)_
- **ENG-062** `P0` **Correct retraction.** Non-monotone strata are maintained with DBSP signed diffs: Z-sets, Q^Δ = D∘Q∘I, the bilinear join rule, and incremental distinct. Pipelined semi-naive with deletions is never used, because it can be unsound, duplicate work, and diverge. — _R09 #34; R01 §5.4; R10 A2.1_
- **ENG-063** `P1` **Recursive views under deletion.** Recursive counting, FBF or DRed, e.g. for `fqpath` when a directory is removed. — _R09 #35; R07 #20_
- **ENG-064** `P1` **Elastic recompute.** A stratum switches to full recomputation when incremental work exceeds a set fraction of its last full run. — _R09 #36_
- **ENG-065** `P1` **Specialized diffs.** Boolean presence on monotone paths, integer diffs only at antijoins, and monoid (MIN/MAX) diffs for recursive aggregates. — _R09 #37_
- **ENG-066** `P1` **Invalidate/rescan fallback.** Invalidation sets, computed statically, cover operators that sit downstream of impure or non-incremental code. — _R03 (BB-54); R09 §6.6_
- **ENG-067** `P0` **Naive-evaluation oracle.** Every incremental code path is tested differentially against naive
  re-evaluation.
  - For choice and order operators, the oracle evaluates their Dedalus expansions (SEM-083). Given the same trace,
    seeds and overrides, it must match the engine *exactly*, at every tick, on relation contents, outbox and choice
    log.
  - It is complemented by the choice-validity checker (TEST-013), equivalence modulo choices and seed sweeps
    (TEST-014), shuffle checks (TEST-015), incremental stress schedules, and priority-permutation enumeration
    (TEST-012).

  — _R10 A2.1; R09 T2; R12/G2 §8_
- **ENG-068** `P0` **Choice maintenance.**
  - Each site keeps a per-group ordered set of `(priority, Ȳ)` over candidates with positive support. The candidates
    are maintained as a Z-set (ENG-062).
  - Each site materializes its output. Retractions are computed against that materialized output, never by re-running
    the choice.
  - If lifetime analysis (ENG-060) proves the candidate relation never shrinks, only the group minimum is kept. This
    is GZ01's unique-key optimization, valid for a single FD only.
  - Sticky sites also keep a held map.
  - Cost: O(|Δ| log g).

  — _R12/G2 §5.1, §5.5; Shared Arrangements §5.3; GZ01 §7; DD `reduce`_
- **ENG-069** `P0` **No persist-pullup through non-distributive operators.** ENG-004's rewrites are valid only for
  operators that distribute over union and are time-invariant. They never cross `choose` in any form, `index`,
  `seq`, `sort`, `topk`, `fold_ordered` or any non-lattice aggregate. For example, `Choose(P(x)) ≠ P(Choose(Δx))`;
  the right-hand side is Hydro's first-seen sticky behavior. These operators are maintained as stateful operators
  over their full per-tick input. — _R12/G2 §4.5; DBSP Def 2.6_
- **ENG-070** `P1` **`unwrap` translation operator (lattice wrapper → Z-set Δ).**
  - Consumes the per-tick Δ of a wrapper lattice (ENG-043/044) and emits Σ of new payloads as the Z-set input Δ of
    a DBSP stratum. It is linear in Δ.
  - For the cumulative wrapper, the merge primitive must return `(old, new)` for each changed key, and unwrap emits
    `new − old`.
  - Also includes sender-side summation per dot, and checked i64 weight arithmetic where overflow is a hard error.
  - Edges from a Z-set stratum back into a monotone or lattice stratum are negative edges for SEM-021 and ANA-022.

  — _R13 §6.2–6.3, §7.2; Wrapping Rings Fig. 1_
- **ENG-071** `P1` **Finality maintenance.**
  - Per output, keep the certain model M⁻ incrementally. It only grows.
  - Keep the possible model M⁺ demand-driven over requested tuples. It only shrinks as seals arrive, and uses
    ENG-062 on a small demand set.
  - Aggregates carry interval bounds.
  - Finality bits form a monotone lattice.
  - Early exit: a recursive stratum whose only consumer is a POS-FINAL Boolean goal stops as soon as the goal
    enters M⁻. This generalizes ENG-048.

  — _R13 §9.4; FT §3.1 "Fixpoint Computation"_
- **ENG-072** `P1` **Order-statistic index.** Used for `index()`, `topk` and `percentile` over persistent inputs.
  - For `topk`, one insert or delete changes the output by at most two tuples.
  - For `index()`, a change at rank r re-emits every later rank. That cost is inherent, and ANA-011 lints it.
  - Tick-local inputs are simply sorted.

  — _R12/G2 §5.9–5.10; Materialize TopK; DD topk_
- **ENG-073** `P1` **Prefix-checkpointed `fold_ordered`.** Keeps checkpoints `(key_j, state_j)` every c rows. On a
  change, it resumes from the last checkpoint below the smallest affected key. Inputs appended in key order (logs)
  cost O(|Δ|) per tick. An insertion or deletion at the front costs O(n), bounded by ENG-064. — _R12/G2 §5.11_
- **ENG-074** `P0` **Time-varying sites.** A stratum that contains `choose_rand`, `rand`, `random` or `tick()` is
  dirty in every tick in which its input is non-empty, because its output changes with the tick. This is an
  exception to ENG-061, and it matches DBSP's time-variance: the clock is an input. — _R12/G2 §5.7–5.8; DBSP Def 2.6_
- **ENG-075** `P1` **Multi-FD choice by conflict component.** A conflict component is a union-find class of
  candidates that share some X̄i value. The greedy result of LANG-116 on one component does not depend on any other,
  so only the components touched by Δ are recomputed. — _R12/G2 §5.2; GZ01 §7.3–7.4_

### 4.5 Joins and planning
- **ENG-080** `P0` **Free Join plans.** One plan form that unifies left-deep index nested-loop joins and Generic Join. Binary plans are converted to it. — _R09 #20_
- **ENG-081** `P0` **Δ-driven index nested loops.** Joining a small Δ against a large persistent relation costs |Δ| × fanout. — _R09 §12(3)_
- **ENG-082** `P0` **Structural planning.** Plans are rooted join spanning trees, costed by the most distinct variables any operator touches. Filters, semijoins and antijoins are pushed down. — _R09 #24_
- **ENG-083** `P0` **Operator fusion.** Join, map and filter are pipelined together, with no materialized intermediate results. — _R09 #27_
- **ENG-084** `P1` **Worst-case-optimal join nodes.** Cyclic nodes use leapfrog or Generic Join intersection, with treefrog leapers for count, propose and intersect, including anti and filter leapers. — _R09 #21–#22_
- **ENG-085** `P1` **Vectorized batch probing.** Used when Δ is large; the plan is chosen by Δ size. — _R09 #23, §12(7)_
- **ENG-086** `P1` **Two-pass semijoin (SIP) rewriting.** — _R09 #25_
- **ENG-087** `P1` **Subplan sharing.** Common subplans are found by hashing a canonical form of each subtree, and arrangements are shared. — _R09 #26_
- **ENG-088** `P1` **RAM-level rewrites.** MakeIndex, HoistConditions, IfConversion, IfExistsConversion and EliminateDuplicates. — _R09 #28_
- **ENG-089** `P1` **Adaptive planning.** `.plan` hints per version, instrumentation of join sizes, and re-planning online, with hysteresis, when observed Δ/full sizes shift. — _R09 #29–#30_
- **ENG-090** `P1` **Fused join and aggregate.** The aggregate state lives in the join's hash table. — _R08 #18_
- **ENG-091** `P1` **Incremental aggregates.** Min and max survive retraction by exposing the next extreme in O(log n). Percentiles and quantile sketches, count, sum and avg are also maintained incrementally. — _R01 #23; R07 §7.4_
- **ENG-092** `P2` **Magic sets / demand transformation.** — _R01 #42_
- **ENG-093** `P2` **Aggregate selections.** — _R01 #42, §6_

### 4.6 Parallelism
- **ENG-100** `P0` **One engine per node.** Each engine is single-threaded and shares nothing. — _R09 §14.6; R08 (R1)_
- **ENG-101** `P0` **Many engines per process.** The simulator and LDFI run many nodes and many scenarios in parallel. — _R09 §12(8)_
- **ENG-102** `P1` **Parallelism within a tick, for large Δ only.** The outer loop runs morsel-parallel, each thread writes to its own output, and dedup runs as a parallel sort or partition. There is no shared B-tree on the hot path. — _R09 #45_
- **ENG-103** `P2` **Exchange sharded by key.** For very large relations. — _R09 §14.6_

### 4.7 Capturing provenance
- **ENG-110** `P0` **Provenance tiers, chosen per run.** Tier A is off, Tier B keeps annotations, Tier C keeps a firing log (ODD-08). — _R09 §11_
- **ENG-111** `P0` **Tier B.**
  - Each tuple is annotated with (rule, minimal height). Δ is update-aware: the insert index excludes the annotations and the retrieve index puts them last.
  - Interval history is kept.
  - Proofs are rebuilt lazily by top-down subproof search compiled into subroutines. An enumerate-all mode is available.

  — _R09 #39–#40; R06 §7.5_
- **ENG-112** `P0` **Tier C.** A per-rule log of firings: all bound variables, the tick, and the send and receive ticks of async rules. The log is limited to the backward slice of the goal relations. Aggregate rules are split into a bindings rule and an aggregate rule. — _R09 #41; R06 D24–D25_
- **ENG-113** `P1` **Negative dependencies.** Records which `notin` subgoals each firing read. — _R06 D26; R10 #20_
- **ENG-114** `P1` **Semiring annotations.** Pluggable semirings: B, N, N[X], Why(X), PosBool(X), Lin(X), and tropical/min-height. Cycles of unit rules are detected. — _R06 H44_
- **ENG-115** `P1` **Firing counters.** Counts of rule firings and tuples, used for profiling and coverage. — _R07 #18; R10 #16_

- **ENG-145** `P1` **Lattice provenance domain.** A lattice cell is annotated with guarded values Σ(φᵢ, vᵢ) ∈ PosBool(X) ⊗ 𝓛. The operations are:
  - sum;
  - absorption: (φ, v) + (ψ, w) = (ψ, w) when φ ⊨ ψ and v ⊑ w;
  - valuation: ⨆{vᵢ | ν ⊨ φᵢ}.

  — _Amsterdamer–Deutch–Tannen PODS'11 §2.3, Thm 3.12; R11/G1 §6.1_
- **ENG-146** `P1` **Propagation rules.**
  - Morphisms map over the pairs, adding (φ, f(⊥)) when the read is a lookup.
  - Bimorphisms are bilinear.
  - Persistence is the identity.
  - Replayed sends add alternatives.
  - Exact reads use negative support.
  - Monotone non-morphisms stored in cells use supports, or fall back to all-contributors (ODD-52).

  — _R11/G1 §6.2_
- **ENG-147** `P1` **Semimodule annotations for aggregates.** ENG-114's semirings extend to K ⊗ M for non-lattice aggregates, with comparison tokens for comparisons on aggregate values. — _Amsterdamer–Deutch–Tannen §2–4; R11/G1 §6.5_
- **ENG-148** `P2` **Bounded annotated fixpoints.** Annotated iteration has a bound. When the bound is exceeded it falls back to all-contributors supports, which remain genuine. — _R11/G1 §6.6 [ours]_
- **ENG-116** `P1` **Choice provenance.**
  - Tier C logs `(site, tick, X̄, chosen Ȳ, |candidates|, reason ∈ {priority, sticky, override})`.
  - Lineage for a chosen tuple has two modes. The default depends on every candidate in the group, as TEST-024 does
    for aggregates. The refined mode uses positive support on the chosen candidate plus negative support on
    lower-priority potential candidates (TEST-025, TEST-051).
  - `whynot` answers with one of: "not a candidate", "higher priority", "held since t₀", or "override".

  — _R12/G2 §5.15_

### 4.8 Miscellaneous
- **ENG-120** `P0` **State hashing modulo time.** A fingerprint of each node's state. It detects quiescence and periodicity, and it is the state hash for the model checker. — _R10 V0(2); R02 #29_
- **ENG-121** `P1` **Logical compaction.** History older than the minimum reader frontier is discarded. — _R09 #38_

---

## 5. Distribution runtime (DIST)

### 5.1 Networking
- **DIST-001** `P0` **Transport abstraction.** TCP, UDP, QUIC and in-process transports all sit behind channels. Whatever the transport, the language-level guarantee stays the same: messages are unordered and may be delayed or dropped (ODD-14). — _R03 §9(6); R08 §6.5_
- **DIST-002** `P0` **Per-tick outbox.** Outgoing messages are batched per destination and flushed after the durable commit. Delivered messages are queued as inputs to the receiver's next tick. — _R03 §3.2; R07 §2.1_
- **DIST-003** `P0` **Wire format.** A compact, schema-versioned binary encoding of each tuple. It names the destination relation and the module instance, and it can carry lattice values. Fields are keyed by stable field number (LANG-261), and each frame carries its channel schema id (DIST-080). — _R03 (BB-49); R15/G5 §6.2, §6.6_
- **DIST-004** `P0` **Addressing.** Logical node ids are resolved to transport addresses through a directory. A message to an unknown destination is dropped and logged. — _R03 (BB-49); R02 §14.11_
- **DIST-005** `P1` **Merge at the sender.** Outgoing lattice payloads are pre-merged per destination and key within each send epoch. — _R04 F50, §4.1_
- **DIST-006** `P1` **Ship deltas.** Lattice state goes over the network as deltas, never as cumulative sequences whose meaning depends on order. Anti-entropy exchanges either the full state or delta intervals. *Amended (R13):* every frame carries a delta-kind tag.
  - `LDelta` is a lattice delta: idempotent, and may be duplicated, merged at the sender or relayed.
  - `GDelta` and `GAck` are the dotted group deltas of DIST-015.
  - `GCum` and `GCumDiff` are the cumulative-per-origin frames of DIST-016.
  - `OTAgg` is the OnceTree edge aggregate of DIST-017.
  - A `ZBatch` payload is `(tuple, zigzag-varint i64 weight)` pairs in canonical order, with no zero weights.
  - A known key or dot arriving with a different payload is a hard `Conflict` error.

  — _R04 F49, F51; R13 §7.3_
- **DIST-007** `P1` **Deduplicate sends only when proven safe.** Repeated async sends derived from persistent state are suppressed only when ARM proves the receiver is idempotent (ANA-061). Otherwise the resend semantics is kept (ODD-05). — _R05 #29; R04 §10.4_
- **DIST-008** `P1` **Flow control.** Backpressure, fragmentation of large messages, and bounded queues. When a queue drops messages, the drops are counted and reported, never silent. — _R03 §9(6)_
- **DIST-009** `P1` **Channel filter hook.** A per-channel function that accepts, postpones or drops each message. It works in live deployments as well as in tests. — _R03 (BB-48)_
- **DIST-010** `P1` **Named, versioned channels.** Needed for rolling upgrades. Raised from P2. It is realized by stable field numbers (LANG-261), versioned framing (DIST-080) and codec translation (DIST-087). — _R08 §6.5; R15/G5 §5.6, §6.5_
- **DIST-011** `P1` **Batch-granular push channels with retained output.**
  - The sender cuts a partitioned channel into **deterministically numbered batches**: `(task, seq)`, where the cut
    points are a function of the input and the configuration, never of time. Each batch carries `progress` and `eof`
    metadata.
  - The sender pushes batches as they are produced and retains them durably until every consumer has committed. A
    consumer that did not exist when a batch was produced pulls it.
  - The number of concurrent push connections per consumer is bounded. The consumer pulls from the rest.
  - The receiver keeps a cursor per *logical* producer, not per attempt, and replies with the next id it expects.
    The sender merges ranges **per destination, starting at that destination's acknowledged cursor**, so a retry
    never straddles.
  - When the producer is closed with nothing left, it sends an empty eof sentinel batch.
  - Per-producer completion is a seal carrying the batch count, so it does not rely on ordered delivery.

  — _R14/G4 §3.2, §5.3, §10.2; HOP NSDI §3.1–3.3, talk "Revised PFT"_
- **DIST-012** `P1` **Adaptive send policy.** Evaluated per batch:
  `send ⇔ closing ∨ (stall < σ ∧ reduction × backlog ≥ 1)`.
  - `stall` is the fraction of destinations that answered RETRY in the last flush round. `reduction` is
    combined/raw bytes. HOP uses σ = 0.5.
  - A backlog is merged and combined (LANG-112) before it is sent.
  - A reverse-pressure rule switches the channel to the fully sorted, blocking layout when every consumer has stalled
    for k rounds, and switches back when stall < σ/2.
  - Changing the policy never changes output. It only changes batching.

  — _R14/G4 §3.3, §10.8; HOP NSDI §3.1.3, §6.2; HOP src `JOutputBuffer.spill()`_
- **DIST-015** `P1` **Dotted exactly-once transport for group payloads (wrapper W2).** This is the general form;
  DIST-011's `(task, seq)` batches are its shuffle specialization.
  - *Dots.* A dot is `(origin, incarnation, seq)`. `incarnation` increments on every restart unless `seq` is
    durable. `seq` is consumed only by non-zero payloads.
  - *Receiver.* The receiver keeps a causal context: per (origin, incarnation), a contiguous max plus exception
    intervals, as in LANG-134. A payload is applied iff its dot is new; the "changed" bit of the merge is the gate.
  - *Sender.* The sender keeps `outbuf[dot]` **durably**, committed in the same tick as the local apply
    (DIST-020), until every destination in the current membership epoch has acknowledged it. A timer (LANG-172)
    resends anything unacked.
  - *Acks.* Acks are cumulative causal-context summaries, so losing one is harmless.
  - *Joining replicas.* A replica that joins receives a snapshot plus its causal context, or a replay of `outbuf`.
  - *Relaying.* Relays forward `(dot, payload)` unchanged.

  — _R13 §6.2, §7.3; Wrapping Rings §3.2 (+ crash/incarnation rules [ours])_
- **DIST-016** `P1` **Cumulative-per-origin wrapper (W3).**
  - *Lattice.* `Map<(origin, incarnation), Lex<seq, cumulative G>>`. The receiver applies `C′ − C` when an entry
    advances; this uses the group inverse.
  - *Transport.* It needs no acks and tolerates loss, duplication, reordering and multi-hop gossip. An optional
    delta interval `(k→k′, C′−C)` is applied only by a receiver at exactly k; others fetch the full `GCum`.
  - *Without inverses.* For a commutative monoid it degrades to recomputing Σ over origins (the Node-ID-Map /
    PN-counter trick).
  - *Compaction.* Folding causally stable prefixes into a shared base is P2.

  — _R13 §6.3; Power diss. §4.2.1; FT App. B_
- **DIST-017** `P2` **OnceTree transport (W4)** for commutative-monoid aggregates on a spanning tree.
  - *State.* Per neighbor, `(clock, agg)` merged by max clock.
  - *Messages.* Neighbor i is sent `(ts, local + Σ_{j≠i} state_j)`. The query is `local + Σ_j state_j`.
  - *Topology changes.* Join as a leaf; move up or down per Power diss. Algorithm 2; leave temporarily (become a
    leaf); leave permanently (the parent absorbs the node's local value); reset on fault (rebuild the tree and
    report again only after one full flooding round).
  - *Cost and guarantees.* O(degree) metadata and O(diameter) latency. It gives SEC but not causal consistency.

  — _R13 §4, §6.4; Power diss. ch. 4 Alg. 1–2_

### 5.2 Durability
- **DIST-020** `P0` **Write-ahead log.** Each tick's durable deltas are appended and fsynced before the outbox is released. Commits may be grouped across ticks. — _R07 #1–#2, §11.5_
- **DIST-021** `P0` **Recovery.** Load the latest checkpoint, replay the WAL, and resume at a fresh tick. — _R07 §11.5_
- **DIST-022** `P1` **Checkpoints.** Durable state is snapshotted copy-on-write while ticks keep running, and the WAL is then truncated. — _R07 §11.7_
- **DIST-023** `P1` **Relation snapshots as of a tick.** Used for Raft snapshots and time-travel queries. — _R07 §11.7; R10 C3_
- **DIST-024** `P1` **Efficient range deletion.** Needed for log truncation and for garbage-collecting a prefix below a watermark. — _R07 #4_
- **DIST-025** `P2` **Non-causal replay of the log.** Speeds up recovery of positive, consistent components; CRON justifies it. — _R02 #35_

### 5.3 Clocks and timers
- **DIST-030** `P0` **Timer wheel.** Feeds `periodic` and timer inputs, and triggers ticks. — _R03 §3.3_
- **DIST-031** `P0` **Clock abstraction.** The real clock in deployment, a virtual clock in simulation. `now` is sampled once per tick. — _R07 #9–#10_
- **DIST-032** `P0` **Randomness abstraction.** All protocol randomness comes from SEM-084's keyed PRF. The root seed
  is the run seed in simulation and the deployment seed in deployment (DIST-033). Individual draws are not recorded;
  seeds and incarnations are. OS entropy is used only for the deployment seed, boot nonces and async cryptographic
  services. — _R07 #10; R12/G2 §4.3, §5.8_
- **DIST-033** `P0` **Seed and incarnation management.** The deployment seed is drawn once, at deployment creation,
  and stored in config. Each node keeps a durable restart counter and draws a boot nonce per start, which is written
  to the WAL header and the trace. A node that rejoins with an empty disk gets a fresh nonce, so its random sequences
  never repeat. — _R12/G2 §4.3; FLAG-025_

### 5.4 Process and deployment
- **DIST-040** `P0` **Node runtime.** Provides `run_tick`, `run_available`, `run`, `pause` and `stop`. Ships both as an embeddable Rust API and as a standalone binary (ODD-22). — _R08 #2; R03 (BB-47)_
- **DIST-041** `P0` **Static membership configuration.** Nodes and roles are given as static relations. — _R03 (BB-70)_
- **DIST-042** `P1` **Dynamic membership.** Membership changes by epoch and is driven by consensus. It is exposed as relations, and each epoch is sealed. — _R05 §5.7; R07 #31_
- **DIST-043** `P1` **Deployment launcher.** Starts N nodes of a projected program locally or on a cluster, from a deployment spec. — _R08 §6.6_
- **DIST-044** `P1` **Observability.**
  - Metrics: tick latency, Δ sizes, message rates, queue depths.
  - Structured traces of each tick.
  - Firing counters for each rule.
  - Ingress rejection, authorization and certificate metrics (DIST-063).

  — _R03 (BB-61); R07 §6; R15/G5 §4.11_
- **DIST-045** `P1` **Distributed provenance.** Provenance by reference: `prov` and `ruleExec` relations keyed by hashed tuple and rule ids (ExSPAN). — _R06 H45_
- **DIST-046** `P1` **Failure-detector outputs.** Membership and suspicion streams, exposed as relations. — _R08 #40_
- **DIST-047** `P2` **Maelstrom/Jepsen adapters.** — _R08 §6.6, T25_

### 5.5 Security
- **DIST-060** `P0` **Mutual TLS on every network transport.** TLS 1.3 with mutual authentication (rustls) on TCP and QUIC. Production mode refuses to start with a plaintext listener. Development plaintext needs an explicit flag and sets `security_mode{mode="insecure"}`. There are two listeners, each with its own trust root:
  - the **peer listener** needs a certificate from the peer CA and serves only peer channels;
  - the **client listener** needs a client certificate or a token and serves only external-ingress channels.

  The in-process and simulator transports carry identity by construction. — _R15/G5 §4.3; etcd `--peer-client-cert-auth`; Flink internal mTLS; CockroachDB node certificates_
- **DIST-061** `P0` **Identity binding.** The principal is taken from exactly one SPIFFE URI SAN, or from the CN against an allow-list if so configured (ODD-31). A hello frame claiming node N is accepted only if the connection's principal equals `principal_of(N)`. Nothing is ever keyed on a self-declared id; Timely's u64 worker index written in plaintext is the anti-pattern. — _R15/G5 §3.4, §4.3–4.4; SPIFFE X.509-SVID_
- **DIST-062** `P0` **Ingress admission pipeline.** Per connection, then per frame, in order:
  1. TLS handshake;
  2. identity binding;
  3. version window (DIST-084);
  4. decode and schema id (DIST-080, DIST-087);
  5. channel ACL (LANG-242);
  6. quota;
  7. enqueue for the next tick with `sender` and `principal` attached.

  Any failure drops the frame before the tick (SEM-090, CR-40). No frame can crash the receiver. Loopback and self-sends skip steps 1–3. The ACL decision is a pure function shared with the simulator. — _R15/G5 §4.4; SeNDlog `SigChecker`; SOSP'21 (CASSANDRA-4195)_
- **DIST-063** `P0` **Rejection metrics and audit.**
  - `net_rejected_total{reason, channel, peer_role}`, with reason ∈ {handshake, expired, bad_identity, unknown_principal, principal_node_mismatch, version_unsupported, schema_mismatch, decode, disallowed, acl, rate_limit, session_closed};
  - `tls_handshake_failures_total`, `authz_denied_total{rule}`;
  - gauges `cert_expiry_seconds` and `sessions_active`;
  - a structured, rate-limited audit log that also records certificate rotations and admin actions.

  — _R15/G5 §4.11_
- **DIST-064** `P1` **Certificate lifecycle.**
  - Certificates rotate without a restart. Existing connections are recycled.
  - Certificates are short-lived, which is Binder's first remedy for revocation. Expiry is enforced, against the virtual clock under simulation.
  - CRL/OCSP support is `P2`.
  - ACL relations change only at tick boundaries and are sealed per epoch, like DIST-042, because revocation is non-monotone.

  — _R15/G5 §4.3, §4.6; CockroachDB certificate rotation; Binder §6_
- **DIST-065** `P1` **Client listener and sessions.**
  - Sessions (LANG-243), authenticated by client certificate or token.
  - Per-principal token-bucket quotas and a limit on concurrent sessions.
  - A `REJECTED(reason)` frame to clients, which is outside program semantics. Peers get none.

  — _R15/G5 §4.8; etcd client-cert-auth; Flink REST_
- **DIST-066** `P1` **Authorization of the admin plane.** The admin role, over mTLS, is required for the control API (DIST-040), the network REPL (TEST-090), rule install (LANG-203), upgrade control (DIST-085) and provenance or trace export (DIST-069). There is no data-plane path to install rules. — _R15/G5 §4.12; LBTrust `says1` (rejected)_
- **DIST-067** `P2` **Attenuable Datalog bearer tokens (Biscuit-style).** Token facts enter as tick-local inputs, and token checks are rules. — _R15/G5 §3.7; Biscuit specification_
- **DIST-068** `P2` **Encryption at rest** for the WAL, checkpoints and blobs (LANG-028). — _R15/G5 §4.1_
- **DIST-069** `P1` **Access control on provenance and traces.** Exporting provenance (DIST-045), traces and Tier-C firing logs is an admin operation, with redaction of payload columns. — _R15/G5 §4.10, §7(11); SeNDlog ICDE'09 §VI (provenance leakage)_

### 5.6 Versioning and upgrades
- **DIST-080** `P0` **Versioned wire protocol.** The post-TLS hello frame carries `program_id`, `program_version`, the supported version window and every channel's schema id and canonical hash. **Every frame** then carries its channel schema id, a small id interned at the handshake. This refines DIST-003 and ODD-14's "schema-hashed" format. — _R15/G5 §4.3, §6.6; CR-41; SOSP'21 (CASSANDRA-6678); Avro fingerprints_
- **DIST-081** `P0` **WAL and checkpoint headers carry schemas.** Each segment and checkpoint header has `magic`, `storage_format_version`, `program_id`, `program_version`, `finalized_cluster_version`, `catalog_digest`, and a table of `(relation_id, name, schema_hash, field layout)`. Records refer to `relation_id`. On recovery:
  - if every hash is current, load;
  - if a migration path exists, migrate (DIST-082);
  - otherwise **refuse to start** with an error that lists the unknown relations and hashes and the supported versions. Nothing is ever skipped or truncated.

  Snapshots (FLAG-010) use the same format. — _R15/G5 §6.7; etcd storage version; SOSP'21 (HDFS-1936 `LayoutVersion`)_
- **DIST-082** `P0` **Migration at recovery.** The migration (LANG-262) runs over the decoded old checkpoint and WAL. Its result is written as a new checkpoint in one atomic switch, and the old one is kept until finalization. It is restartable, because the output is a deterministic function of the input and the switch is atomic. It makes no remote calls. Before finalization, migrations change only the in-memory representation, and the on-disk format stays old (DIST-086). — _R15/G5 §6.4; Ajmani ECOOP'06 §5_
- **DIST-083** `P1` **Cluster-version protocol.**
  - Each node durably records `understood_version` when it starts.
  - A controller computes the minimum over a membership epoch, re-reading it until it is stable (the fence).
  - The controller proposes `activate_version(v)` when min ≥ v and v is within the operator's finalization target. The controller is the leader in Raft or Paxos programs.
  - In replicated-state-machine programs (FLAG-014) activation is a log entry, so every replica switches at the same index.
  - Other programs receive the value over a runtime channel, and "the initiator decides" what is supported.

  — _R15/G5 §6.6; etcd `decideClusterVersion` and `ClusterVersionSetRequest` (InternalRaftRequest field 1300); CockroachDB long-running migrations RFC and fence versions_
- **DIST-084** `P1` **Version window.** A cluster admits at most two program versions, {cluster_version, cluster_version+1}. A peer outside the window is rejected with `version_unsupported`. A node further behind is upgraded offline through chained migrations. — _R15/G5 §6.6; Ajmani ECOOP'06 §4.2; Erlang "current and old"; CockroachDB MinSupported_
- **DIST-085** `P1` **Rolling-upgrade orchestrator.**
  - It consumes the release manifest, including the role order from ANA-101.
  - It upgrades one node at a time, with the leader last, after leadership transfer by TimeoutNow (FLAG-013).
  - It waits on health gates: the node is a member and has caught up.
  - It raises failure-detector suspicion timeouts for the node being restarted.
  - It supports pause, abort and rollback before finalization.
  - Finalization is automatic or manual. It proposes activation (DIST-083), runs non-monotone migrations as barrier jobs, and bumps the storage version at the next checkpoint.

  — _R15/G5 §6.9; Ajmani ECOOP'06 §6 (scheduling functions); etcd; CockroachDB finalization; SOSP'21 (HDFS-11856)_
- **DIST-086** `P1` **No format change before finalization.** Until finalization the WAL, checkpoints and snapshots are written in the old storage format. Every write of a `since V` relation or field is gated (ANA-102), so a node can roll back to the old binary. After finalization, downgrade needs `migrate … down` (`P2`), and is otherwise refused with a clear error. — _R15/G5 §6.7; etcd 3.6 downgrade support; CockroachDB rollback before finalization; SOSP'21 (CASSANDRA-15794)_
- **DIST-087** `P1` **Codec translation.**
  - Frames written with an older schema in the window are decoded with defaults and `unknown` variants, and with `accept` blocks where needed.
  - Tuples sent to an older receiver go through its `emit` block (LANG-263). A tuple no `emit` rule matches is dropped as `disallowed` and counted (SEM-090).
  - An unknown field or variant never causes a crash.

  — _R15/G5 §6.5; Ajmani ECOOP'06 §3.2 (the disallow constraint); SOSP'21 §4.1.1_

---

## 6. Analyses (ANA)

### 6.1 Well-formedness
- **ANA-001** `P0` **Range restriction.** Every head, negated and comparison variable must be bound by a positive atom. A violation is an error. — _R02 #20, §14.25_
- **ANA-002** `P0` **Temporal stratification, with a cycle witness.** Implements SEM-020 to SEM-022. — _R02 #19; R10 #1_
- **ANA-003** `P1` **Temporal safety.** Computes the least fixpoint of instantaneous predicates and checks rule kinds 1–3. Warns about possible oscillation or unbounded counters, e.g. arithmetic in heads or entanglement. — _R02 #21; R10 #2_
- **ANA-004** `P0` **Location checks.**
  - Body locality.
  - Address type safety.
  - A head on another node requires an async rule.
  - Sugar that will be localized must be well-connected.

  — _R01 #41; R02 #6_
- **ANA-005** `P0` **Type and lattice checks.**
  - Schemas and arity.
  - Lattice columns are never keys.
  - Merges are between values of the same lattice type.
  - The operator legality matrix (LANG-066) holds.

  — _R06 A11; R04 C27; R03 (BB-28)_
- **ANA-006** `P1` **Soft-state lints.** A head's TTL must be at least the TTL of every soft body atom. Archival rules (hard head, soft body) get a warning. — _R01 #41, §4.2_
- **ANA-007** `P1` **Key-conflict lint.** Warns when two rules could write the same key with different values in one tick. — _R03 §9(3); R07 #3_
- **ANA-008** `P1` **Underspecification.** Reports input interfaces nobody reads, output interfaces nobody writes, and rules that can never fire. — _R03 (BB-07); R05 §3.1_
- **ANA-009** `P1` **Purity check.** Flags impure calls inside rule bodies. — _R03 (BB-44)_
- **ANA-010** `P1` **Oracle containment.** Protocol rules may not read the spec-only relations `crash`, `R_log` and `hb` (CR-20). — _R06 §3.4(5)_
- **ANA-011** `P1` **Lints for choice, order and randomness.** Each of these is an error under `--strict`:
  - `index`, `sort` or `topk` over a persistent input;
  - a carried `fold_ordered` over a persistent input;
  - `choose_rand`, or `rand`/`random` whose value is not captured in state, over a persistent input;
  - a `choose` into a keyed relation with X̄ ⊄ key;
  - `seq` numbers that reach an async head or an output without `durable`;
  - an attempt to write a user FD without the tick, which is only expressible under entanglement.

  — _R12/G2 §5_
- **ANA-015** `P1` **Retries × idempotence typing.** This extends ANA-030. A consumer that is not idempotent is rejected over an at-least-once source unless the source is a wrapped channel (LANG-158) or the consumer's idempotence is *proved* (TEST-087). A consumer counts as not idempotent if it is a group-typed fold (sum, count, Z-set integration) or a UDF or aggregate lacking a *proved* idempotence property. Proofs that are merely *tested* do not count. The rule is the same as Hydro's `ValidIdempotenceFor<ExactlyOnce>` being implemented only for `NotProved`, where `unique()` upgrades a stream to `ExactlyOnce`. Commutativity over unordered sources has the same form. A group-typed payload on a plain channel is a compile error. — _R13 §7.2, §10; hydro_lang `properties/mod.rs`_

### 6.2 Monotonicity and CALM
- **ANA-020** `P0` **Lattice-aware monotonicity classification.** Every operator is classified using the classes in R04 §2.4. Threshold tests count as monotone. — _R05 #1–#2; R04 E44_
- **ANA-021** `P1` **Polarity analysis.** Each argument gets a polarity: +, −, 0 or ±. A path is monotone if and only if it has an even number of − edges and no ± edge. — _R04 §2.2 [ours]_
- **ANA-022** `P0` **Points-of-order report.**
  - The dependency graph marks temporal, async and non-monotone edges.
  - Temporal clusters are found.
  - Every non-monotone edge, and every edge that touches a cluster, is reported with its source span.

  — _R05 #3; R03 (BB-55)_
- **ANA-023** `P0` **Path labels.** Each path is labeled Bot, A, N or D, where A followed by N gives D. Each sink takes the disjunction of its paths. The report suggests placing coordination at the last async edge before the first D. — _R05 #4, #6; R03 (BB-56)_
- **ANA-024** `P0` **Guarded asynchrony.** When two channel streams meet, and at least one of them has not passed through persistent state or a lattice, the result is divergent. — _R05 #5; R03 (BB-57); R02 §14.16_
- **ANA-025** `P1` **Dedalus+ certificate.** A semipositive program with guarded asynchrony is certified confluent. — _R05 #9; R02 #23_
- **ANA-026** `P1` **Dedalus_S certificate.** A program with no negation cycle through any edge, plus guarded asynchrony, is certified confluent under the coordination rewrite. — _R02 #24_
- **ANA-027** `P1` **Membership dependence.** A negation or aggregate whose completeness depends on `members` or `self` is reported as coordination. — _R05 #10_
- **ANA-028** `P1` **Final outputs.** A monotone threshold output may be emitted as final as soon as it holds. A non-monotone output never may. *Amended (R13):* "never" is too strong, and "final" is now defined by SEM-016.
  - A monotone output is final on derivation (FT Thm 24/25).
  - An antitone output is final on refutation.
  - A non-monotone output may still be final *for a particular input* when ANA-121's bounds decide it (FT Thm 22).
  - An output over invertible state is never final without a seal (SEM-017).
  - The classification is ANA-120, the runtime test ANA-121, and the finite-state case ANA-122.

  — _R05 #12; R13 §9_
- **ANA-029** `P1` **Per-output determinism certificate.** Each output is reported as exactly one of: confluent; confluent given seals S; coordinated at X by protocol Y; or nondeterministic by design (CR-29). — _R10 #7_
- **ANA-030** `P1` **Stream-property inference.** Each relation gets three properties:
  - boundedness: bounded, unbounded, or monotone singleton;
  - order: total or none;
  - retries: exactly-once or at-least-once.

  A fold over unordered or duplicated input needs a proof of commutativity or idempotence. Strengthening a property needs `nondet` (ODD-10). — _R08 #37; R04 E47_
- **ANA-031** `P1` **Eager execution and streaming progress.** An operator may block only on bounded input. An unbounded lattice can be read only through a threshold. `reveal` or to-sequence on an unbounded input is rejected. — _R04 E48_
- **ANA-032** `P1` **Taint propagation for diagnostics.** — _R04 E45; R08 §3_
- **ANA-033** `P1` **CRDT query classification.** A query is either safe to answer locally (monotone or threshold) or needs a quorum. — _R04 E46_
- **ANA-034** `P2` **Guess/guarantee taint.** Tracks "memories, guesses and apologies". — _R03 (BB-59)_
- **ANA-035** `P2` **Policy-aware negation.** Given the partitioning, operators that are domain-distinct-monotone are certified coordination-free. — _R05 #11; R02 §10.2_
- **ANA-036** `P1` **Early-emission class of every reducer or aggregate output.** One class is assigned to each output, relative to its completion seal:
  - **T, threshold-final.** The output is a monotone threshold over a lattice. It is emitted as final early, per key,
    and never retracted (ANA-028).
  - **L, progressive lower bound.** A lattice morphism or monotone function. Snapshots satisfy `snap ⊑ final` and
    form a chain.
  - **A, algebraic estimate.** Snapshots are estimates, with an interval only when the block order is randomized.
    Downstream consumers need Z-set retraction or recomputation.
  - **H, holistic.** Snapshots are nondet estimates.
  - **W, windowed.** The inner class applies per window, and the window's seal makes it final.

  Classes T and L over non-idempotent lattices require exactly-once inputs (DIST-011). Including tentative inputs
  keeps T or L only under ANA-037. Otherwise the class drops to A or H. The class is reported on the determinism
  certificate (ANA-029). — _R14/G4 §10.4; R05 #12; HOP NSDI §3.2, §4.2_
- **ANA-037** `P1` **Map determinism and resumability certificate.** A map or producer is certified in one of three ways:
  - **deterministic:** its output batches are a function of (input split, code, config). Rules are deterministic by
    construction; host UDFs must be declared `pure` (LANG-182).
  - **per-record stateless:** it has no state that crosses records, other than combining that restarts at batch
    boundaries.
  - **neither.**

  The certificate decides what is allowed:
  - attempt-agnostic batch dedup and speculation (FLAG-107, FLAG-110) need *deterministic*;
  - prefix seals from input offsets, with resume at an offset (FLAG-109), need *per-record stateless* as well;
  - keeping snapshot classes when tentative data is included (ANA-036) needs *deterministic*;
  - otherwise the tentative scheme (FLAG-108) is used.

  — _R14/G4 §10.2–10.3, §10.9; HOP NSDI §3.3; CR-32_

- **ANA-140** `P1` **Static key-conflict freedom.** Proves the FDs on non-lattice keys, using cost-respecting rules plus containment mappings or integrity constraints. Where it cannot, SEM-050 stays a runtime check. — _Ross–Sagiv Defs 2.7–2.10, Lemma 2.3; CR-51_
- **ANA-141** `P1` **Dedalus+^L certificate.** Extends ANA-025. A program is certified when it is:
  - semipositive^L: exact occurrences read only EDB relations; and
  - guarded-asynchronous^L: every async head is persistent, or it is ephemeral and every consumer (1) has no other non-EDB atom, (2) is a join-morphism, (3) has no exact occurrence of it, and (4) heads a persistent or recursively guarded relation.

  A threshold that is not join-prime, applied to an ephemeral lattice, fails the check (BENCH-302). A certified program is confluent, with ult(c) = ⨆ Kₙ over the flattened program. — _R11/G1 §4.2 [ours]; MAR §3.2_
- **ANA-142** `P1` **Sealed exact reads (Dedalus_S^L).** Extends ANA-026. An exact read (`reveal`, Anti, `==`) of a cell that depends on async input is certified only when that cell's seal guards it. — _R11/G1 §4.3; MAR §4.2; Conway thesis §3.7_
- **ANA-143** `P1` **"Confluent but not certified" is its own verdict.** An exact read of a cell that becomes constant (T11) must not be reported as diffluent. — _R11/G1 §4.2, T11; BENCH-084_
- **ANA-038** `P1` **Determinism analysis for choice and order.** Deciding whether a query is deterministic is
  undecidable in general (GSZ95), so the analysis uses sound PTIME sufficient conditions based on FD closure over
  F(rule). F(rule) comes from keys, singletons, equalities, pure and injective UDFs, lattice FDs, aggregate and
  choose outputs, and channel keys, reusing ANA-080. The conditions are:
  - **D1:** Ȳ ⊆ closure(X̄). The choice is forced, and the report says "redundant choose".
  - **D2:** closure(K̄) covers the input, so the order is tie-free. Otherwise the lint is "ties broken by canonical
    order".
  - **D3:** every path from `chosen` to an output first drops all Ȳ-derived columns or only tests `chosen(X̄, _)`.
    The output is then seed-independent. Checked by column taint (ANA-032).
  - **D4:** consumers that see only π_X̄ of `chosen`.
  - **D5:** a `choose_least` whose cost column is unique per group.
  - **D6:** commutativity claims, proven by VER-014 or tested by TEST-015.

  — _R12/G2 §6; GSZ95 §3_
- **ANA-039** `P1` **Propagating nondeterminism classes.**
  - The SEM-087 classes propagate to outputs.
  - ANA-029 gains a "deterministic given seed" certificate.
  - Schedule-dependent sites, such as sticky choice, are points of order for ANA-042.
  - TEST-003 must permute deliveries across tick boundaries whenever they feed schedule-dependent or seed-dependent
    non-monotone consumers.

  — _R12/G2 §5.13_

### 6.3 Blazes and coordination synthesis
- **ANA-040** `P1` **White-box component annotations.** Each path is labeled CR, CW, OR_gate or OW_gate:
  - C (confluent) if the path has no non-monotone operator;
  - W (writes state) if it reaches persistent state;
  - the gate is the group-by columns or the antijoin's theta columns, traced back to inputs;
  - `*` means unknown.

  — _R05 #13, #22_
- **ANA-041** `P1` **Stream annotations.** Seal_key and Rep. The default is Async. — _R05 #14_
- **ANA-042** `P1` **Blazes label propagation.**
  - Labels: NDRead, Taint, Seal, Async, Run, Inst, Diverge.
  - Inference rules 1–4, plus the patch rule 1′.
  - Reconciliation, and collapsing each SCC to one node.
  - The resolutions in CR-27.

  — _R05 #15–#19, §4.4.1_
- **ANA-043** `P1` **Injective-FD chase.** Decides `compatible(gate, key)` by following lineage through identity projections, renames, join equalities and declared injective UDFs. — _R05 #20–#21_
- **ANA-044** `P2` **Grey-box annotation files** for components the analysis cannot see into. — _R05 #23_
- **ANA-045** `P1` **Sink report.** Gives each output's label, and every coordination point inserted, with the reason. — _R05 #26_
- **ANA-046** `P1` **Coordination synthesis.** The preferred mechanism is sealing: each producer sends seals with a digest, the producer set votes unanimously, and the vote is skipped when there is only one producer. The fallback is ordering through our consensus service. — _R05 #24–#25_
- **ANA-047** `P1` **Coordination rewrite (Marczak).**
  - Negated atoms get `p_done` guards.
  - Async rules get acknowledgements.
  - ∀ is computed by a walk in `<` order.
  - Async-recursive SCCs use two-round voting led by the minimum node id.
  - Debug builds assert the sealing invariant.

  — _R02 #26; R05 #7–#8_
- **ANA-048** `P2` **I-confluence checker.** Looks up (invariant × operation) pairs in a table. — _R05 #40_
- **ANA-049** `P2` **Complete-CALM interface check.** A coordinated module's exposed outcome spec must be monotone under the refinement order it declares. — _R05 #41_

### 6.4 Edelweiss garbage collection
- **ANA-060** `P1` **Sublanguage check and persistence inference.** — _R05 #27–#28_
- **ANA-061** `P1` **ARM rewrite.**
  - An ack channel keyed on the channel's key columns.
  - An `_approx` range at the sender.
  - Every sender rule gets `notin approx`.
  - A channel is eligible only if every receiver rule persists what it receives without `<-`.

  — _R05 #29_
- **ANA-062** `P1` **DR+ rewrite.** Runs the full safety analysis. Conditions from different rules combine with AND; conditions within a chained `notin` combine with OR. — _R05 #30–#31_
- **ANA-063** `P1` **Join reclamation.**
  - joinbuf and missing buffers.
  - Seals, over the whole relation or per column.
  - Shortcuts for key-covering joins and semijoins.
  - Pulling a `notin` up out of a join.

  — _R05 #32_
- **ANA-064** `P1` **DR− rewrite.** Adds an X_keys range, suppresses duplicates, and reclaims Y only after the matching X tuple is gone. — _R05 #33_
- **ANA-065** `P1` **Seal inference.** Epoch punctuations for dynamic membership, and seals implied by `flat_map`. *Amended (R13):* every inferred or declared seal makes its relation or partition CLOSED for ANA-121. This is FT §5.2's `All()` construction: a sealed partition's future transitions are self-loops. The sink report states which outputs a seal makes final, and what coordination the seal cost (ANA-046). — _R05 #37–#38; R13 §9.4, §9.6_
- **ANA-066** `P0` **GC safety.** Every rewrite preserves semantics: if in doubt it leaks rather than loses data. Rewrites are tested differentially, and when a relation cannot be reclaimed the tool explains why. — _R05 #39, §5.11_
- **ANA-067** `P2` **Lattice GC.** Tombstone compaction and the GLB across replicas. — _R05 §5.11; R04 §3.10_

### 6.5 Optimization analyses
- **ANA-080** `P1` **FD inference.** Functional dependencies come from keys, rule lineage and declared injective functions. Blazes, partitioning and Edelweiss all use them. — _R05 §7(7); R07 §9.4_
- **ANA-081** `P1` **Decoupling rewrites, gated by their preconditions.** Mutually independent, monotonic, functional and asymmetric decoupling are P1; state-machine decoupling is P2. — _R07 #23; R08 #44_
- **ANA-082** `P1` **Partitioning rewrites.** Co-hashing (checked for parallel-disjoint correctness), FD/CD-based partitioning, and partial partitioning with sealing. — _R08 #45; R07 §9.4_
- **ANA-083** `P1` **Interlock protection.** State that interlocks, such as Raft's vote and log, and atomic regions are never decoupled or partitioned. — _R07 #30; R08 §9.3_
- **ANA-084** `P2` **Rewrite to replicate by consensus.** Turns a component into one replicated through a consensus log, automatically. — _R07 §4.1–4.2_
- **ANA-085** `P2` **Placement by profile-driven ILP** (as in hydro-optimize). — _R08 (R11), §9.3_

### 6.6 Security and version-compatibility analyses
- **ANA-100** `P0` **Compatibility checker across two versions (`bloomc compat`).** It compares the program against `schema.lock` for the previous version, or for every supported version with `--transitive`. For each channel and durable relation it outputs identical, FULL, R-new-only, R-old-only or breaking. It flags:
  - reused field numbers or changed field types;
  - fields added without a default;
  - variants added to types that lack `unknown`;
  - changes to key columns;
  - changes to a lattice type or merge definition;
  - `semantics_changed` fields;
  - missing migrations (LANG-262) or translations (LANG-263).

  It emits a report and JSON for CI (TEST-108). The rules table is R15/G5 §6.3. — _R15/G5 §6.3, §6.8; DUPChecker (SOSP'21 §6.2); Protobuf; Avro; Confluent compatibility types_
- **ANA-101** `P1` **Rollout order.** Take each channel's compatibility direction and its sender and receiver roles from the choreography. Build a precedence graph over roles: receivers go first for R-new-only channels, and senders go last for ungated R-old-only channels. Report a total order, "any order", or a cycle together with a suggested gate. The result goes into the release manifest for DIST-085. — _R15/G5 §6.8 step 6; Confluent BACKWARD/FORWARD order; Ajmani ECOOP'06 §6_
- **ANA-102** `P1` **Gated-write check.** A rule that writes a durable relation, column or variant marked `since V`, or sends a channel or non-default field marked `since V`, with V the current version, must have its body dominated by `cluster_version.at_least(V)`, directly or through a gated upstream relation. Otherwise it is an error, unless overridden by `unsafe_ungated "reason"`. — _R15/G5 §6.6; CockroachDB version-gate discipline; SOSP'21 (CASSANDRA-15794)_
- **ANA-103** `P1` **Migration classification.** Each migration, and each translation, is one of:
  - **tuple-local:** one `old.*` atom per rule; distributes over union; coordination-free;
  - **monotone:** keeps `old.*` materialized during the mixed window; coordination-free;
  - **non-monotone:** runs as a barrier at finalization.

  Translation blocks must be tuple-local. The report says whether the rollout as a whole is coordination-free. — _R15/G5 §6.4.2; R05 (CALM); ANA-020_
- **ANA-104** `P1` **Lattice migrations must be morphisms.** A migration f: L_old → L_new of a lattice column must be declared `morphism` (LANG-182). The declaration is checked by the law harness on the pair (TEST-083) and by SMT where supported (VER-014). Only then do replicas converge whatever the order of upgrades and deliveries. Otherwise the migration is classified non-monotone and needs the barrier. — _R15/G5 §6.4.3; CR-23_
- **ANA-105** `P0` **Channel ACL inference and consistency.** The analysis computes `senders(c)` for every channel (LANG-242). An explicit ACL that excludes a role in `senders(c)` is an error, because the program would silently drop its own messages. An external channel without an explicit ACL is a warning. — _R15/G5 §4.6_
- **ANA-106** `P1` **Sender-binding lint.** It warns when a payload column is used as an identity without being equated with `sender` or `principal`. This covers `Node`-typed payload columns used for comparison, quorum counting (`majority<>`, LANG-111), reply addresses or state keys, such as a Raft `leader_id` or a vote's `voter`, and client ids that are not checked against `principal`. — _R15/G5 §4.5, §7(5)_

### 6.7 Finality (free termination)
These implement SEM-016 and SEM-017. For general FO or Datalog¬ queries over open relational state, deciding
free termination is undecidable (R13 §9.2, by reduction from Trakhtenbrot). The analyses are therefore sound but
incomplete, except for finite-state components, which are decided exactly.
- **ANA-120** `P1` **Static finality classification.** Each output, or output column pattern, gets one or more
  classes, each reported with its evidence:
  - **POS-FINAL:** monotone over inflationary inputs (FT Thm 24/25, Prop 13).
  - **NEG-FINAL:** antitone, e.g. ∀, or `notin` over an inflationary relation (Prop 10, Ex 11).
  - **TOP-FINAL:** a lattice output reaches a maximal element or ⊤ (Prop 9/10).
  - **THRESH-FINAL:** `threshold(t1..tn)` with pairwise incompatible tᵢ. Prop 14/15 show incompatibility is what
    distinct final values need, which matches LANG-126.
  - **MIXED:** decided per tuple by ANA-121.
  - **FINITE:** decided by ANA-122.
  - **SEALED(S):** final after seals S (FT §5.2).
  - **NEVER-FINAL:** the inverse curse (SEM-017).

  R14's early-emission classes (ANA-036) are the reducer-level instance: T ⊆ POS/THRESH, L is progressive and not
  final, A and H are estimates, and W = SEALED. `output final` (LANG-212) requires a class other than MIXED
  without a runtime gate, and other than NEVER-FINAL. — _R13 §9.3, §11.1; FT §4–5_
- **ANA-121** `P1` **Runtime bound ("Kleene") finality test.**
  - *Relation classes.* INFL (content only grows), CLOSED (sealed, static, or `sealed` after bootstrap) and OPEN.
  - *Base bounds.* L(R) = R if INFL or CLOSED, else ∅. Up(R) = R if CLOSED, else ⊤.
  - *Two models.*
    - M⁻: positive atoms over L; `notin A` only if A ∉ M⁺.
    - M⁺: positive atoms over Up, computed demand-driven; `notin A` if A ∉ M⁻.
    - By induction over strata, M⁻(O) ⊆ O(J) ⊆ M⁺(O) for every reachable J.
  - *Verdict.* A tuple is final-present iff it is in M⁻, and final-absent iff it is not in M⁺.
  - *Aggregates* get interval bounds. A threshold is final when its interval is decided. This generalizes R14's
    FLAG-138.
  - *Coverage.* It decides R(c)∧¬S(c), FT §5.1's mixed query, |R|>k, ∀-refutation, and sealed aggregates.
  - *Maintenance.* ENG-071 maintains it incrementally.

  — _R13 §9.4 [ours], justified by FT Thm 22_
- **ANA-122** `P1` **Exact free termination for finite-state components.**
  - *Graph.* Build the per-key abstract transition graph of a `finite` component. The component is declared, or
    inferred from finite-domain types with a state cap; exceeding the cap is a hard error. Edges come from every
    input label the schemas allow. Over-approximating edges is sound.
  - *Algorithm.* Run FT Prop 26's linear-time algorithm:
    1. condense into SCCs;
    2. drop SCCs where Q is not constant, together with everything that reaches them;
    3. mark FT in reverse topological order: a sink SCC, or an SCC whose successors are all FT with the same value.
  - *Output.* Emit an FT table and the collapsed automaton (Prop 30/31), so the runtime check is O(1): "is this
    state's collapsed node self-looping?"
  - *Abstraction.* A homomorphic abstraction h, a congruence with Q = Q′∘h, is sound: h(s) FT ⇒ s FT.
  - *Uses.* 2PC and Paxos decision states, Raft commit, phase and timer automata, and DFA-encoded specs.

  — _R13 §9.5; FT Prop 26, Cor 27, Prop 29–31_

---

## 7. Testing and debugging (TEST)

### 7.1 Deterministic simulation
- **TEST-001** `P0` **Deterministic simulator.** It runs every node of the same compiled program in one process. One seed drives:
  - message delay, order, batching, duplication and loss;
  - crashes and restarts;
  - timers;
  - `choose` and `random`.

  Replay is exact. — _R10 #14; R08 #43_
- **TEST-002** `P0` **Exploration modes.** Random fuzzing, bounded exhaustive search, and fully scripted runs. A failing schedule is saved as a reproducer and shrunk by delta debugging over its faults and schedule. — _R08 #43; R10 A3.3_
- **TEST-003** `P0` **Schedules pruned by CALM.** Only deliveries that reach a non-monotone consumer after an async edge are permuted. All other deliveries use one canonical order. — _R03 (BB-67); R10 #26_
- **TEST-004** `P1` **Explorer for the operational semantics.** Explores which node runs, which subset of messages is delivered, and heartbeats, for small programs. — _R02 #33_
- **TEST-005** `P1` **Confluence tester.** Compares the ultimate models reached under different schedules. When they differ, it reports a pair of schedules as the witness. — _R02 #32_
- **TEST-006** `P0` **Synchronous-round mode.** A message arrives at t+1 unless it is faulted. — _R02 #34; R06 B14_
- **TEST-007** `P1` **Stochastic delivery at quiescence.** When the system goes quiet, a random subset of buffered messages is delivered, and a guaranteed number is dropped (as in BloomUnit). — _R03 (BB-67)_
- **TEST-008** `P1` **History checkers.** Checks linearizability (WGL/Porcupine style) and transactional anomalies (Elle style) over invoke/ok histories. — _R10 #15_
- **TEST-009** `P1` **Statistical harness for progressive outputs.** Runs one job under N fixed seeds of block order and schedule.
  - Per snapshot point, it reports mean relative error, per-key relative error, and the empirical coverage of every
    declared interval.
  - It checks that class-L snapshots are chains and lower bounds.
  - It checks that class-T early outputs appear in the final output.
  - Faults can be triggered at batch granularity: kill an attempt after its k-th batch, after its first snapshot, or
    between writing a batch and sending it.

  — _R14/G4 §14 (T1–T3, T9)_
- **TEST-010** `P0` **Trace format for record and replay.**
  - The versioned header records: the program digest (normalized IR including site ids), the compiler, PRF and
    encoding versions, the root seed, the mode, the nodes, the constants, and the recording level.
  - Events: SchedStep, TickBegin (incarnation, `now`, trigger), Deliver (sender, send tick, fact), Drop, TimerFire,
    HostInput, ChoiceOverride, PriorityTable (test-only), Crash, Restart, TickEnd, ChoiceLog and Send.
  - TickEnd carries state, outbox and choice digests.
  - Levels: Minimal (inputs only, enough to replay), Digests (the default in simulation and CI), and Full (for LDFI
    and the viewer).
  - Choices and random draws are recomputed, never recorded as inputs.

  — _R12/G2 §7.2_
- **TEST-011** `P0` **Replay with divergence detection.** A mismatch in the program digest, the PRF version or the
  encoding version is a hard error; there is no best-effort mode. Inputs, seeds and overrides are re-fed in
  SchedStep order, and digests are compared at every TickEnd. The first mismatch stops the replay with a report
  naming the node, tick, relation and site. — _R12/G2 §7.3_
- **TEST-012** `P1` **Choice exploration.**
  - Choices can be forced with `__choice(site, X̄, Ȳ)` overrides, or a test-only `PriorityTable` can replace the
    priority.
  - Exhaustive mode enumerates the priority permutations of small groups.
  - Overrides are recorded, and are rejected in production unless explicitly allowed.
  - An override that names a non-candidate is a hard error.

  — _R12/G2 §5.6; Hydro sim hooks (issue #1875)_
- **TEST-013** `P0` **Choice-validity checker.** It is independent of both the engine and the oracle. At every site
  and tick it checks:
  - chosen ⊆ candidates, and every FD holds;
  - maximality;
  - priority minimality;
  - the sticky rule;
  - that overrides were followed;
  - `index` density and order;
  - `seq` monotonicity and no reuse.

  — _R12/G2 §8 B_
- **TEST-014** `P1` **Equivalence modulo choices, and seed sweeps.**
  - External or alternative implementations include Bud, Hydro and Soufflé (through LANG-222/223), or a second
    backend. For each, extract its per-site choices, validate them with TEST-013, and replay our oracle with them
    as overrides. Everything else must then match exactly.
  - A diagnostic reports which of our modes, per-tick or sticky, explains the sequence, and under which fixed
    priority.
  - Seed sweeps assert that outputs ANA-038 certifies as seed-independent are identical across seeds, and that the
    spec rules hold for every seed.

  — _R12/G2 §8 C–D_
- **TEST-015** `P1` **Shuffle checks for algebraic claims.** The oracle evaluates every UDA or `reduce` declared
  commutative/associative in k random orders. A mismatch is a hard error that names the aggregate. This is Hydro's
  "commutativity proofs are hooked, not trusted". — _R12/G2 §8 E; Hydro sim-hooks §4.5_

### 7.2 LDFI ("Molly-2")
- **TEST-020** `P0` **Failure spec.** ⟨EOT, EFF, maxCrashes⟩ plus the list of nodes. An omission may occur only at a send tick with 1 ≤ t < EFF (CR-21), and EFF < EOT. — _R06 B13_
- **TEST-021** `P0` **Fault mask at the network layer.** A virtual `clock(from,to,t)` relation is answered from a FaultSchedule, never materialized as facts. One omission drops everything from `from` to `to` at send tick t. Self-sends are never dropped. Crashes follow SEM-070. — _R06 §10(1–2), B15_
- **TEST-022** `P0` **Outcome oracle.** `pre` and `post` have the same schema and are evaluated at EOT. A run fails if some `post` tuple from the failure-free run is missing from its `post` while still present in its `pre`. A run where `pre` does not hold is vacuous and passes. A missing `pre` or `post` is an error (CR-30). — _R06 C21–C22_
- **TEST-023** `P0` **Complete lineage.** For every `pre`/`post` tuple, a rule/goal graph with every alternative firing. It comes with the run's message log: relation, from, to, send tick, and receive tick or LOST. — _R06 D23, D27_
- **TEST-024** `P0` **Lineage for aggregates and quorums.** By default an aggregate depends on all of its contributors. A threshold or quorum test is treated as k-of-n: falsifying it needs at least n−k+1 contributors lost. *Refined (R11/G1):* k counts elements, not messages, and an element can have alternative contributors (TEST-141). Other thresholds use exact supports (TEST-140). — _R06 D25, §10(8); R11/G1 §6.3_
- **TEST-025** `P0` **Negative support.** Off, conservative (the default, CR-31), or surrogate, with an optional odd-negation parity filter. — _R06 D26_
- **TEST-026** `P0` **Hazard encoding.**
  - A goal is the AND of its firings; a firing is the OR of its premises.
  - A message leaf is O(f,to,t) (when t<EFF) OR K(f,t).
  - A leaf that is not a message is `false` (CR-22).
  - The formula is memoized over the DAG and encoded with Tseitin or as a hitting-set problem, never by naive CNF expansion.

  — _R06 E28, §10(4)_
- **TEST-027** `P0` **Crash encoding.** K(n,t) implies K(n,t+1), and ΣK ≤ maxCrashes. Crash variables exist only for senders in the lineage, starting at each sender's first send. — _R06 E29, §10(5)_
- **TEST-028** `P0` **Enumerating minimal falsifiers.** Incremental SAT loop: find a model, shrink it greedily, block it. There is one problem per goal tuple, and the results are unioned. Omissions already covered by a crash are dropped. — _R06 E31–E32; R10 #21_
- **TEST-029** `P0` **Forward/backward loop.** Start with the failure-free run. Each new hypothesis is a seeded superset of an earlier one. An explored set avoids repeats. The loop can stop at the first counterexample or find all, and it produces a lazy stream of runs. — _R06 F34, E30; R10 #22_
- **TEST-030** `P1` **Single-shot mode for monotone goals.** One failure-free run plus one hitting-set enumeration is enough. — _R10 #23_
- **TEST-031** `P1` **Prune vacuous hypotheses before running them.** — _R06 E33_
- **TEST-032** `P1` **Symmetry reduction.** Considers node permutations that fix the program's EDB and do not appear as literals in any rule. — _R06 F35; R10 #24_
- **TEST-033** `P1` **Resume from checkpoints.** A hypothesis restarts from the checkpoint taken at its earliest fault tick. Hypotheses run in parallel. — _R06 §10(6); R09 §12(9)_
- **TEST-034** `P1` **Parameter sweep.** Start with EFF=0 and raise EOT until runs are non-vacuous, then raise EFF, and so on, within a wall-clock bound. Report either the minimal failing parameters or the maximal certified ones. — _R06 B19_
- **TEST-035** `P1` **Run-count estimator.** grossEstimate, computed with bignums. — _R06 B20_
- **TEST-036** `P1` **Baselines.** Random fault injection, and an ablation that uses only causal lineage. — _R06 F36–F37_
- **TEST-037** `P1` **Extensions.** Crash-recovery faults (durable relations survive) and bounded reordering at points of order. Both are labeled experimental. — _R10 A3.4(8), A2.4_
- **TEST-038** `P2` **Merge lineage across runs.** — _R06 §4, §10(9)_
- **TEST-039** `P1` **LDFI as a CI regression gate.** — _R06 §5_
- **TEST-040** `P1` **LDFI with choices.**
  - Hypothesis runs reuse the failure-free run's seeds with no overrides, so choices are recomputed.
  - SEM-085's priority keeps a choice unchanged unless the fault removes the chosen candidate.
  - The Full trace's ChoiceLog feeds lineage (ENG-116).
  - The report states which choices differ from the failure-free run, and why.

  — _R12/G2 §5.15, §7.3_

- **TEST-140** `P0` **Exact threshold supports.** Take a distributive lattice and a threshold t with a finite decomposition ⇓t. The support is ⋀_{a∈⇓t} ⋁_{i: a ⊑ vᵢ} φᵢ, so the threshold is falsified exactly when some atom of t loses all its contributors. `lmax ≥ c`, `contains`, `key?`, `when_true` and lmap sub-thresholds use this form. — _R11/G1 §6.3 [ours]_
- **TEST-141** `P0` **Cardinality thresholds count elements (refines TEST-024).** `size ≥ k`, `majority<>` and key counts count elements, not messages. An element survives if any of its contributors survives. Falsification is Σ_e s_e ≤ k−1, encoded with Tseitin variables and a cardinality constraint. Weighted sums use pseudo-Boolean constraints. — _R11/G1 §6.3; R06 §10(8)_
- **TEST-142** `P0` **Only genuine supports.** Every claimed support must be sufficient on its own; this keeps LDFI complete (LDFI App. B). Exact supports are preferred. All-contributors is the sound fallback for non-distributive lattices and for non-morphisms stored in cells. — _R11/G1 §6.4_
- **TEST-143** `P1` **Exact reads in lineage.** Anti, NM and `reveal` reads of lattice cells get conservative negative support (TEST-025). Every contributor and potential contributor counts as a cause. — _R11/G1 §6.2_

### 7.3 Provenance queries and debugging
- **TEST-050** `P0` **Explain queries.**
  - `explain t` shows the minimal-height proof, limited by a depth.
  - `explain all t` shows every derivation.
  - `whynot t` lets the user pick a rule and bindings, then shows which atoms fail.

  — _R06 G42; R09 #42_
- **TEST-051** `P1` **Why-not provenance over time intervals.** NEXIST, NAPPEAR, NDERIVE, NRECEIVE, NSEND and NARRIVE, built top-down on demand. — _R06 G43_
- **TEST-052** `P1` **Nemo graph algebra and repair.**
  - Operators: ∩, ∪, −, prop, normalize (which collapses `@next` chains), leaves, roots and reachable.
  - Strategies: DiffC, SkelDiffC, and DepsA correction suggestions.
  - Hazard windows.

  Implemented natively. — _R06 G41; R10 #25_
- **TEST-053** `P2` **Why-across-time provenance.** — _R06 §6.2_

- **TEST-145** `P1` **Explain and why-not for lattice cells.**
  - `explain c ⊒ t` returns a minimal support.
  - `explain value(c)` returns one witness per atom of ⇓value.
  - `whynot c ⊒ t` lists the missing atoms, or the k − |value| missing elements, each with its NDERIVE, NRECEIVE or NARRIVE reason.

  — _R11/G1 §6.5; TEST-051_

### 7.4 Tracing and visualization
- **TEST-060** `P0` **Space-time diagram for each run.** — _R06 G38_
- **TEST-061** `P0` **Lineage graph rendering.** — _R06 G39_
- **TEST-062** `P1` **Run reports.** Per-run JSON (fault set, model, messages, provenance) plus an HTML index. — _R06 G40_
- **TEST-063** `P1` **Static dataflow plot** using the budplot legend, colored by label. — _R03 (BB-60)_
- **TEST-064** `P1` **Replay viewer.** Shows a snapshot of every relation at each tick, and a timeline across nodes. — _R03 (BB-61)_
- **TEST-065** `P1` **Watch taps.** Per relation: insert, delete, refresh, receive, send, timer, join and projection events. — _R01 #36_
- **TEST-066** `P1` **Tracing and coverage.** Firings per rule, rules that never fired, and messages per protocol step. — _R07 #18, T20_

### 7.5 Specs and inputs
- **TEST-080** `P0` **Trace relations.** Every relation automatically gets `R_log(loc, …, tick)`, and there is a happens-before relation `hb(n1,t1,n2,t2)`. — _R03 (BB-64); R10 #12_
- **TEST-081** `P0` **Spec programs.** A spec uses no temporal operators and has one `fail`/`violation` output. It is evaluated once, over the trace. — _R03 (BB-65)_
- **TEST-082** `P1` **Input generation from constraints.** Default constraints come from schemas and keys; users add exclusion and inclusion constraints. A relational model finder with symmetry breaking generates the inputs, each carrying a time and a location. — _R03 (BB-66)_
- **TEST-083** `P0` **Lattice law harness.** Checks:
  - associativity, commutativity and idempotence;
  - ⊥ is the identity;
  - the order agrees with the merge;
  - partial-order laws;
  - is_bot and is_top;
  - atomize;
  - morphism and bimorphism claims.

  Inputs are generated randomly and shrunk. The known-bad DomPair case is included. — _R04 A8; R10 #8_
- **TEST-084** `P1` **Implementation equivalence.** Runs two implementations of one interface under the same inputs and schedules. — _R03 §7_
- **TEST-087** `P1` **Algebraic-property verification of UDFs and user algebra, with no false positives.** This covers
  UDFs, user-defined aggregates, user lattices, and user groups or rings (LANG-105, LANG-135, LANG-142, LANG-182).
  - *Tools.* Bounded model checking (Kani-style) or SMT proofs for bounded types, and fuzzing for unbounded types.
  - *Status.* Each claimed property gets one of three statuses:
    - **proved** enables optimizations and the ANA-015 upgrade;
    - **tested** passed fuzzing only; it gives a warning and can be relied on only under `nondet`;
    - **refuted** has a counterexample and is an error.
  - *Law set.* Extends TEST-083 with group laws (inverse, identity), ring laws (distributivity), and claims of
    linearity and bilinearity.
  - *Commutativity obligations* are shape-specific, as in Hydro PR #3198: for maps, the output multiset and the
    captured state must both be order-independent.
  - *Reference rates.* Emmy proves associativity, commutativity and idempotence for 83–99% of real
    `.fold`/`.reduce` closures. It times out on multiplication and on loop-based LCM.

  — _R13 §10; Power diss. ch. 6; hydro PR #3198_
- **TEST-088** `P1` **Finality soundness oracle.** In simulation, every emission marked `final_present` or
  `final_absent` is checked against the run's ultimate model, across explored schedules and crashes. Two replicas
  must never finalize different values for a key that is certified confluent. A simulated run may stop early once
  every oracle relation (`pre`, `post`, spec outputs) is final, and the early stop is recorded in the run report. —
  _R13 §9.1, §12 T15; FT Def 3, Prop 15_

### 7.6 Developer tools
- **TEST-090** `P1` **REPL.** `/tick n`, `/run`, `/stop`, `/lsrules`, `/rmrule`, `/lscollections`, `/dump`, and breakpoints. — _R03 (BB-62)_
- **TEST-091** `P0` **Diagnostics with source spans.** A stratification cycle is shown as a path. A type error lists its evidence sites. Locality violations are pointed out. — _R06 A11; R03 §4.3_
- **TEST-092** `P2` **Editor integration (LSP).** Diagnostics, hover types and points-of-order overlays. — _[ours]_

### 7.7 Upgrade and security testing
- **TEST-100** `P1` **Mixed-version deterministic simulation.** One simulation loads several compiled versions of a program and assigns versions to nodes. Channels are matched by name and schema id and translated by the runtime's own codec layer (DIST-087). `upgrade(node, v)`, `finalize` and `rollback(node)` are events: crash-restart under the new program, with migration. They are scheduling choices that fuzz and bounded-exhaustive exploration (TEST-002) interleave with deliveries and faults. This generalizes Hydro's `FlowBuilder::next_version` and `VersionedNetworkFork`. — _R15/G5 §5.6, §6.11; Hydro `sim_multi_version_*` tests_
- **TEST-101** `P1` **Upgrade scenario generator.** Runs, for each consecutive version pair and for gap-2 pairs with `--transitive`:
  - full-stop upgrade;
  - rolling upgrade under load;
  - new-version nodes joining an old cluster;
  - rollback before finalization;
  - attempted downgrade after finalization, which must be refused cleanly;
  - a crash in the middle of an upgrade.

  It uses 3 nodes by default and reuses the program's simulation tests and stress drivers as workloads. — _R15/G5 §6.11; SOSP'21 DUPTester, Findings 9–12_
- **TEST-102** `P1` **Differential oracle across versions.** The same seed and workload run on an old-only cluster and on the upgraded cluster. Client-visible outputs must match after normalizing `nondet` (LANG-204) and `semantics_changed` fields. — _R15/G5 §6.11; UpFuzz NSDI'26 §7.3_
- **TEST-103** `P1` **Crashing a migration.** A crash is injected at every durable write of a migration (LANG-262). The recovered state must equal one uninterrupted run. — _R15/G5 §6.11; Ajmani ECOOP'06 §5 ("a TF must be restartable")_
- **TEST-104** `P1` **Security faults in the simulator.** Three fault kinds, logged as `REJECTED(reason)` and never as `LOST`:
  - `reject(from,to,channel,t)`;
  - `cert_expired(n,t)`, which isolates n in both directions while n keeps computing, unlike a crash;
  - `acl_misconfig(channel, role)`.

  They use the same pure ACL function as the live runtime, and certificate validity is checked against virtual time. `whynot` (TEST-051) explains rejections. — _R15/G5 §4.10_
- **TEST-105** `P1` **LDFI with rejections.** Rejections are omissions (SEM-090), so the existing O(f,to,t) variables cover them. `--explain-omissions-as=auth` labels counterexamples accordingly. An `Iso(n,t)` isolation macro-fault, with `Iso(n,t) ⇒ Iso(n,t+1)`, is `P2`. — _R15/G5 §4.10_
- **TEST-106** `P1` **Golden storage fixtures.** Checkpoints, WAL segments and snapshots produced by every released version are committed as fixtures. Every new binary must load them, or refuse them with the documented error (DIST-081). — _R15/G5 §6.11; SOSP'21 §5.3 (HDFS pre-built images)_
- **TEST-107** `P1` **Security integration tests on a real network.** They run the real TLS stack and cover:
  - handshake failures;
  - CN and SAN binding and `principal_node_mismatch`;
  - rotation under load;
  - expiry;
  - listener separation, e.g. a client certificate on the peer listener.

  — _R15/G5 §4.3–4.4_
- **TEST-108** `P0` **Compatibility gate in CI.** CI fails when a schema changes without a version bump or lock update, and on any ANA-100, ANA-102 or ANA-105 error. — _R15/G5 §6.2; SOSP'21 (KAFKA-10173)_

---

## 8. Verification (VER)

- **VER-001** `P0` **One property language.**
  - `violation` denial constraints.
  - `pre`/`post` outcomes.
  - Trace queries over `R_log` and `hb`.
  - A block describing the failure model.
  - Bounded liveness: `eventually(post) within k after EFF`.
  - Past-time properties written with persistence.

  — _R10 (V1)_
- **VER-002** `P1` **Explicit-state model checker for bounded async runs.** Searches depth-first over (global state, multiset of in-flight messages). It reduces the space with CALM-based partial-order reduction, DPOR, symmetry, and state hashing modulo time. — _R10 A3.5_
- **VER-003** `P1` **Bounded encoding into ASP.** The STABLE transformation with bounded time: each arrival falls in (s, s+Δ] or is lost. Causality uses `before`, finiteness is enforced, and properties become integrity constraints (clingo). — _R10 #27_
- **VER-004** `P2` **Bounded encoding into SAT.** Kodkod style, with completion and loop formulas. — _R10 A3.5(b)_
- **VER-005** `P1` **Bound certificates.** Each result states the model (async or sync), the number of nodes, the number of ticks, the delay window, and the failure budget. — _R10 A3.5_
- **VER-006** `P1` **First-order transition system.**
  - Relations are the state, and the network is a grow-only set.
  - One action is a node tick over an arbitrary delivered subset; there are also environment inputs and crash.
  - Positive in-tick recursion is over-approximated only after a transitive polarity check.

  — _R10 #28, A3.6_
- **VER-007** `P1` **EPR fragment check.** Builds the quantifier-alternation graph and reports any cycle together with the formula fragments that cause it. Supports a semi-bounded mode. — _R10 #29_
- **VER-008** `P1` **Axiom library.**
  - A total order for `ordered` types.
  - A quorum sort with the intersection axiom, behind `majority<>`.
  - `max` via EPR equation (3).
  - FD axioms for lattice columns.

  — _R10 #30_
- **VER-009** `P1` **Auto-derived projection relations.** Relations in the style of `joined_round` and `left_round` break ∀∃ cycles. — _R10 #31_
- **VER-010** `P1` **Checking inductiveness.** The three verification conditions are discharged with Z3 or CVC5. A counterexample to induction is drawn as a graph of nodes and messages. — _R10 #32_
- **VER-011** `P1` **Regular invariants from syntax.** Send, receive and monotonicity invariants are generated automatically. — _R10 #33_
- **VER-012** `P2` **Invariant inference (DuoAI style).** Candidate invariants are filtered by running them as batch denial queries on our engine. — _R10 #34_
- **VER-013** `P2` **Prove synchronously, then lift.** Invariants proven on the synchronous semantics are lifted to the async one. — _R10 A3.6_
- **VER-014** `P1` **Proofs of lattice and function laws.** SMT proves the laws for supported fragments. For the rest, property-based testing is used and the result is reported as "tested, not proven". — _R10 #8; R04 G54_
- **VER-015** `P1` **Confluence certificates.** Combines the ANA certificates with bounded confluence testing. — _R10 A2.5_
- **VER-016** `P1` **Verifying rewrites.** Preconditions are checked, and the rewritten program is compared with the original: same outputs at the same timestamps. — _R10 A2.9, #9_
- **VER-017** `P2` **Exporters.** TLA+ (TLC/Apalache), Ivy/mypyvy, and Lean 4 inductive predicates. — _R10 #35_
- **VER-018** `P2` **Checking refinement of a sequential spec, and synthesizing CRDTs (Katara).** — _R04 G55; R10 A2.5_
- **VER-019** `P2` **Mechanized semantics in Lean.** A research stretch goal. — _R10 A3.7_
- **VER-020** `P1` **Interface checks for trusted modules.** A module marked trusted (LANG-205) must pass its declared interface spec under TEST-001 and TEST-020. — _R03 (BB-58); R05 #41_
- **VER-021** `P2` **Unbounded liveness.** Out of scope for v1. — _R10 A1(5)_
- **VER-025** `P2` **Mechanized algebra proofs.** Machine-checked proofs of:
  - the SEM-036 wrapper invariant: exactly-once application under duplication, reordering and batching, for W2
    and W3;
  - soundness of ANA-121's M⁻/M⁺ bounds for stratified programs;
  - correctness of the ANA-122 SCC algorithm (FT Prop 26).

  — _R13 §6.2, §9.4–9.5_

- **VER-040** `P1` **ASP semantics pin.** The VER-003 backend uses clingo's default aggregate semantics (Ferraris/FLP) and never Alog/GZ. BENCH-313 is its oracle. — _CR-54; FLP §5; Abstract Gringo §4.7; GZ Ex. 8_
- **VER-041** `P1` **ASP lattice encodings.**
  - Cells use one of two encodings:
    - E-atoms, the down-set of join-irreducibles: `ge` with closure for `lmax`, `mem` for `lset`, keyed nesting for `lmap`, and a per-node `ge` for vector clocks;
    - E-contrib, a contribution set read by `#max`, `#min`, `#count` or `#sum`, used for `Lex` and for large ranges.
  - Monotone thresholds become atoms or monotone aggregates. Exact reads go in higher strata.
  - Async delivery is a keyed arrival choice plus an optional omission choice.

  — _R11/G1 §5.1; R10 A3.5(a)_
- **VER-042** `P1` **FOL/EPR lattice axioms.** A lattice cell is `ge(k̄, v)` with total-order and down-closure axioms. Merge is disjunction and a threshold is an atom. Exact reads use eq. (3) of Paxos Made EPR. Every lattice cell gets an automatic monotonicity invariant. `size ≥ k` is outside EPR except through the quorum sort (VER-008). — _R11/G1 §5.2; EPR §5.1 eq. (3)_
- **VER-043** `P1` **Over-approximating in-tick recursion by pre-models.** Any pre-model (a state closed under the rules) may stand in for in-tick lattice recursion. This is sound for upward-closed safety properties once VER-006's polarity check passes. — _Ross–Sagiv Prop. 3.2–3.3; R11/G1 §5.2_
- **VER-044** `P2` **Mechanize Dedalus^L.** Extends VER-019 to D1–D13, Theorem 4^L and Theorem 1^L. — _R11/G1 §4_

---

## 9. Standard library of protocols (LIB)

The library covers bud-sandbox (R03 §6) and the idioms from I Do Declare (R07 §2.4), and goes beyond both. Protocols are written as abstract protocol modules (LANG-006) with one or more implementations.

### 9.1 Delivery and dissemination
- **LIB-001** `P0` **Delivery interface.** `pipe_in` / `pipe_sent` / `pipe_out`. — _R03 §6.1_
- **LIB-002** `P0` **Best-effort delivery.** — _R03 (BB-68)_
- **LIB-003** `P0` **Reliable delivery.** Messages are buffered and retransmitted periodically until acked (at-least-once). An exactly-once variant adds receiver-side dedup. — _R03 (BB-68)_
- **LIB-004** `P1` **Fault-injecting delivery.** Demonic delivery drops a set percentage of messages. Dastardly delivery reorders and delays them. — _R03 (BB-68)_
- **LIB-005** `P1` **FIFO delivery.** Per-sender sequence numbers plus a reorder buffer. — _R03 T31_
- **LIB-006** `P1` **Causal delivery.** The Schiper–Eggli–Sandoz algorithm on vector-clock lattices. — _R03 (BB-68); R04 §3.9_
- **LIB-007** `P0` **Multicast.** Best-effort and reliable multicast, with `mcast_done` and garbage collection. — _R03 (BB-69)_
- **LIB-008** `P1` **Reliable broadcast family.** Dedalus, classic, ack, redundant, causal and epoch variants. — _R02 §4.7; R05 §5.9; R06 §12.1_
- **LIB-009** `P1` **Gossip and anti-entropy.** Lattice deltas with delta intervals. — _R04 F51, §5.3_

### 9.2 Membership, failure detection, timers
- **LIB-020** `P0` **Static membership.** — _R03 (BB-70)_
- **LIB-021** `P0` **Heartbeats and failure detector.** Tracks `last_heartbeat`, expires stale entries, and reports suspicion. — _R03 (BB-70); R07 §3.6_
- **LIB-022** `P0` **Timers.** One-shot progress timers, a logical timeout service, tick counters, and doubling backoff. — _R03 (BB-70); R06 §12.2; R07 §4.3_
- **LIB-023** `P1` **Dynamic membership by epoch.** — _R05 §5.7_
- **LIB-024** `P1` **Leader election.** Min-id/bully, Kirsch–Amir views, and a ballot-based stable leader. — _R03 §6.6; R07 #27, §9.5_
- **LIB-025** `P1` **Leases with bounded clock drift.** — _R07 §11.6_

### 9.3 Voting and commit
- **LIB-040** `P0` **Voting.** Unanimous and majority masters. The agent's decision can be overridden. — _R03 (BB-71)_
- **LIB-041** `P0` **Two-phase commit.** Log, then act on the next tick, so the log is fsynced before the send. Uses presumed abort, aborts on timeout, and disseminates the outcome. — _R07 #25; R08 §5.4_
- **LIB-042** `P1` **2PC-CTP and 3PC.** — _R06 §12.2_
- **LIB-043** `P1` **Lock manager (2PL).** — _R03 (BB-71)_
- **LIB-044** `P0` **Quorum collection.** `collect_quorum(min, max)` emits each key once. — _R08 T15; R07 §9.5_
- **LIB-045** `P0` **Coordination idioms.** Roll call, barrier, choice, sequence and timeout. — _R07 #15_

### 9.4 Ordering and identity
- **LIB-060** `P0` **Unique ids and nonces.** Gap-free ids of the form node‖seq. — _R03 (BB-73); R05 #35_
- **LIB-061** `P0` **Serializer / atomic dequeue.** — _R07 #14; R03 (BB-73)_
- **LIB-062** `P0` **Priority and FIFO queues.** Ties are broken deterministically. — _R03 (BB-73); R02 §3.3_
- **LIB-063** `P0` **Counters and sequences.** — _R03 (BB-73)_
- **LIB-064** `P0` **Deterministic id assignment.** Assignment by sorting, with a persistent high-water mark. — _R03 (BB-73)_
- **LIB-065** `P0` **Lamport and vector clocks.** — _R03 (BB-73); R04 B23_
- **LIB-066** `P0` **Sealing multi-message replies.** A count is shipped with the reply, and a generic `seal<r>` desugaring handles the rest. — _R07 #13, §9.4_

### 9.5 Key-value stores and CRDTs
- **LIB-080** `P0` **KVS.** A common interface with basic, durable and replicated implementations. — _R03 (BB-72)_
- **LIB-081** `P0` **Lattice KVS.** An `lmap` replica with anti-entropy and a quorum client (W acks, R merged reads, read repair). — _R04 T12, §3.9_
- **LIB-082** `P1` **Multi-version KVS.** Causal, monotonic-read, read-your-writes and monotonic-write views. — _R03 (BB-72)_
- **LIB-083** `P1` **MVCC.** Snapshot isolation, abort on write-write conflict, and version GC. — _R03 (BB-72)_
- **LIB-084** `P1` **Dynamo-style versioned KVS.** Built on `ldom` plus vector clocks. — _R04 T13_
- **LIB-085** `P1` **Library of consistency levels.** Causal, RC, RU, item-cut, MR/MW/RYW/WFR and PRAM, each built from a lattice composition plus a client proxy. The default is LWW. — _R04 F52, §4.2_
- **LIB-086** `P0` **CRDT library.**
  - Counters: G-Counter, PN-Counter.
  - Sets: G-Set, 2P-Set, OR/AW-Set.
  - Registers: LWW-Reg, MV-Reg.
  - Flags: EW and DW flags.
  - `Owned<Node, L>`, a pattern for sub-state with a single writer.

  An RGA sequence is P2. — _R04 B24, §6.4(3); R08 T22_
- **LIB-087** `P1` **Lattice GC protocols.** Deletes carry timestamps and are reclaimed once they fall below the minimum clock heard from every replica. Tombstones are compacted. — _R04 F53, §4.1_
- **LIB-088** `P1` **Atomic registers.** Multi-register writes and snapshot reads. — _R05 §5.9_
- **LIB-089** `P1` **Causal KVS (COPS-style).** — _R05 §5.9_
- **LIB-093** `P1` **Replicated Z-set collections and views.**
  - The in-language executable spec of wrapper W2: a sender sequence number, durable outbuf, a causal-context
    lattice, `seen.inserted` as the fresh-dot Δ, and cumulative acks. The runtime's built-in (LANG-158) is
    differential-tested against it.
  - "Causal multiset semantics": deletes are negative weights, an optional client guard drops a delete whose
    causally observable count is ≤ 0, and views are clamped. Concurrent deletes can reach −1, and this is
    documented.
  - The same interface is offered with 2P-Set (tombstone) and OR-Set (observed-remove) deletion (ODD-27).
  - Replicated DBSP views are maintained over wrapped inputs.

  — _R13 §3.6, §6.5–6.6; Wrapping Rings §3, §5; Dolan PODC'20_
- **LIB-094** `P2` **OnceTree aggregates.** Counters, sums, and averages (as (sum, count)) over DIST-017, for
  like-counters, metrics and Tide global aggregates at large replica counts. — _R13 §4; Power diss. ch. 4_

### 9.6 Applications and examples
- **LIB-100** `P1` **Shopping carts.** Destructive, disorderly, replicated, and lattice-monotone with a manifest. — _R03 (BB-74)_
- **LIB-101** `P1` **State-machine module.** — _R03 §6.9_
- **LIB-102** `P1` **Chord DHT.** — _R03 (BB-75); R01 §8.5_
- **LIB-103** `P1` **Routing.**
  - Shortest path with aggregate selection.
  - Distance vector with split horizon and poison reverse.
  - DSR.
  - Link-state flooding.
  - Policy path-vector.

  — _R01 §8.3_
- **LIB-104** `P1` **Ping/pong and link liveness from soft state.** — _R01 §8.2_
- **LIB-105** `P1` **Distributed transitive closure, deadlock detection, and coordinated GC.** — _R05 T1–T3_
- **LIB-106** `P2` **Other examples.** Narada, rings, Symphony, MI cache coherence, chat, and the Bleet Twitter clone. — _R01 §8.4; R03 §6.9_
- **LIB-107** `P1` **Request/response rendezvous.** — _R02 §3.3_
- **LIB-108** `P2` **Log and metric collection pipeline (Chukwa-style).** — _R07 §6_

### 9.7 Security and upgrade idioms
- **LIB-120** `P1` **Authorization policy library.** RBAC roles, per-key-prefix ownership, admin sets, and the `authorized`/`authz_denied` pattern of LANG-244 with standard error replies. — _R15/G5 §4.7; etcd RBAC_
- **LIB-121** `P2` **Delegation.** Speaks-for restricted to a predicate, with depth and width limits. — _R15/G5 §2.3; LBTrust §4.2 (`delegates`, rules `dd0`–`dd4`)_
- **LIB-122** `P2` **Threshold (k-of-n) trust.** An aggregate over principals who assert a fact. — _R15/G5 §2.3; LBTrust §4.2.2 (rules `wd0`–`wd2`, from D1LP)_
- **LIB-123** `P1` **Idioms for version-gated features.**
  - Dual channels: old and new formats coexist, and rules choose one by `cluster_version`.
  - Expand/contract: read the new format, then write it, then drop the old one.
  - Command types for replicated state machines that are gated by an activation log entry.

  — _R15/G5 §6.5–6.6; hashicorp/raft ProtocolVersion ("understand" vs "speak"); CockroachDB version gates_

---

## 10. Flagship systems (FLAG)

### 10.1 Raft (the full feature set of Ongaro's dissertation)
- **FLAG-001** `P0` **Roles and terms.** One channel carries both RequestVote and AppendEntries. A node that sees a higher term steps down. Within a tick, term is handled first and stale messages are rejected. — _R07 §12.1(1,8), §11.1_
- **FLAG-002** `P0` **Elections.** Timeouts are randomized from a seed, and a candidate votes for itself. A majority wins. Heartbeats suppress new elections. A node casts at most one vote per term, choosing deterministically among requests that arrive in the same tick. — _R07 §12.1(2), §11.1(3)_
- **FLAG-003** `P0` **Election restriction.** Compares last term, then last index. — _R07 §12.1(3)_
- **FLAG-004** `P0` **Log matching.** A follower truncates only on a real conflict. The leader only appends. An assertion checks that no committed entry is ever truncated. — _R07 §12.1(4), §10.3_
- **FLAG-005** `P0` **Commit and apply.**
  - The leader counts replicas only for entries from its current term.
  - A follower's commit index is min(leaderCommit, index of the last new entry).
  - Committed entries are applied in order, exactly once, via an ordered fold.

  — _R07 §12.1(5–7), §10.1_
- **FLAG-006** `P0` **One component holds all Raft state.** The vote decision and the log decision are made in the same component, in the same tick, under a canonical order. They are never decoupled (ODD-15). — _R07 #30, §11.1_
- **FLAG-007** `P0` **Durability.** Term, vote and log are persisted before any reply. Restart recovery is supported, and a node never votes twice in a term. — _R07 #29, §11.5_
- **FLAG-008** `P0` **No-op when a leader starts.** — _R07 §11.3_
- **FLAG-009** `P1` **Membership changes.**
  - Single-server changes, including the 2015 fix.
  - Learners and catch-up.
  - Removing the leader.
  - A guard against disruptive servers.
  - Joint consensus.

  — _R07 #31, §11.8_
- **FLAG-010** `P1` **Snapshots and chunked InstallSnapshot.** — _R07 #32, §11.7_
- **FLAG-011** `P1` **Client semantics.**
  - Redirect to the leader.
  - Sessions with exactly-once execution and deterministic expiry.
  - RegisterClient.
  - A leader steps down if it has not heard a heartbeat response from a majority.

  — _R07 #33, §11.6_
- **FLAG-012** `P1` **Linearizable reads.** ReadIndex (including reads served by followers) and lease reads. — _R07 #33_
- **FLAG-013** `P1` **Extensions.**
  - PreVote.
  - TimeoutNow.
  - Fast log backtracking.
  - Batching and pipelining.
  - Parallel fsync on the leader.

  — _R07 #34_
- **FLAG-014** `P1` **Replicated-state-machine API.** Any component can be replicated through the Raft log. — _R07 #36, §4.2_
- **FLAG-015** `P1` **Linearizable KV on Raft.** — _R07 T14_
- **FLAG-016** `P1` **Rolling upgrades of Raft.**
  - `activate_version(v)` is a log entry, and apply switches semantics at that index.
  - New entry and command types are gated (ANA-102), so no replica ever meets a committed entry it cannot decode.
  - The leader is upgraded last, after TimeoutNow.
  - Snapshots stay readable by the old version until finalization (DIST-086).
  - Client sessions survive upgrades.
  - Must pass BENCH-220 and BENCH-221.

  — _R15/G5 §6.9, §7(1–2); etcd `ClusterVersionSetRequest`_

### 10.2 Multi-Paxos
- **FLAG-020** `P0` **Acceptor.**
  - Ballots are a lattice.
  - Accepted pvalues are stored per slot, durably.
  - A p1b reply carries the log and a sealed size.
  - Replies are Ok, or Err carrying the max ballot.

  — _R07 §12.2(1–3), §9.3_
- **FLAG-021** `P0` **Leader recovery.** For each slot, take the value with the highest ballot. Skip slots already known to be committed. Fill holes with no-ops, then resume at max+1. Slots are assigned with `index()`. — _R07 §12.2(4)_
- **FLAG-022** `P0` **Phase 2 and preemption.** — _R07 §12.2(5)_
- **FLAG-023** `P0` **Stable leadership.** Heartbeats, expiry, and staggered re-election. — _R07 §12.2(6)_
- **FLAG-024** `P0` **Replicas and GC.** Replicas apply commands in slot order with exactly-once semantics, and checkpoints let acceptors GC their logs below a watermark. — _R07 §12.2(7, 9)_
- **FLAG-025** `P1` **Catch-up, reconfiguration (s+WINDOW), leases, epochs, and rejoining after disk corruption.** — _R07 §12.2(8, 10–13)_
- **FLAG-026** `P1` **Kirsch–Amir election module.** — _R07 #27_
- **FLAG-027** `P1` **Flexible/grid quorums, thriftiness, batching, and flow control.** — _R07 §12.2(14)_
- **FLAG-028** `P2` **Compartmentalized/scalable Paxos.** — _R07 T7; R08 §9_

### 10.3 Commit protocols
- **FLAG-040** `P0` **2PC over Raft-replicated participants.** Gives atomicity across shards, for example an atomic rename across partitions. — _R07 §5, T18_
- **FLAG-041** `P1` **3PC and 2PC-CTP as LDFI demonstrators.** — _R06 §12.2_
- **FLAG-042** `P1` **Scalable 2PC and voting via rewrites.** — _R07 T3_

### 10.4 KVS (Anna-style)
- **FLAG-060** `P0` **Coordination-free per-core actors.** Each actor owns a private MapLattice. Replicas exchange state by epoch gossip, merge at the sender, and serve GETs locally. — _R04 §4.1_
- **FLAG-061** `P1` **Consistency levels by lattice composition.** — _R04 §4.2; R10 #44_
- **FLAG-062** `P1` **Deletes, with reclamation by minimum heard vector clock.** — _R04 §4.1_
- **FLAG-063** `P2` **Stateful functions on the KVS (Cloudburst style).** — _R10 B2_

### 10.5 BOOM-FS
- **FLAG-080** `P0` **Metadata as relations.** `file`, `fqpath` (recursive and incrementally maintained, including deletions), `fchunk`, `datanode` and `hb_chunk`. The metadata RPCs each have error rules. — _R07 #35, §3.2–3.3_
- **FLAG-081** `P0` **DataNode heartbeats.** Heartbeats expire, and chunk reports are sent as deltas and acked. — _R07 §3.4, §3.6_
- **FLAG-082** `P0` **Re-replication.** — _R07 §3.4, §3.6_
- **FLAG-083** `P0` **Data path outside the engine.** Chunked append and read, and pipelined replication. — _R07 §3.1_
- **FLAG-084** `P1` **High-availability metadata via Raft.** — _R07 #36_
- **FLAG-085** `P1` **Metadata partitioned by hash(fqpath).** Rename across partitions uses 2PC. — _R07 #37_
- **FLAG-086** `P1` **Client library.** Retries, monotone chunk ids, and leases. — _R07 §3.2, §3.6; R10 C1_
- **FLAG-087** `P2` **Permissions and an HDFS-compatible shim.** — _R07 §3.5_

### 10.6 BOOM-MR
- **FLAG-100** `P0` **Scheduler state as relations.** `job`, `task`, `taskAttempt` and `taskTracker`. A table function generates tasks. Scheduling policies are rule modules that can be swapped. — _R07 #38, §7.1_
- **FLAG-101** `P0` **FCFS with Hadoop's default speculation.** — _R07 §7.2_
- **FLAG-102** `P0` **LATE.** Enforces SpeculativeCap and the SlowNode/SlowTask 25th-percentile thresholds, and ranks tasks by estimated time left. — _R07 §7.2_
- **FLAG-103** `P0` **MapReduce data plane.** Map, a shuffle over partitioned channels with per-mapper seals, reduce, and an idempotent commit. — _R10 C1_
- **FLAG-104** `P1` **Locality/delay scheduling, fair share, and a highly available scheduler.** — _R07 §7.4; R10 C1(5)_

HOP parity. MapReduce Online is the BOOM group's own successor to Hadoop, and it is delivered as ODD-19 milestone M1b.
- **FLAG-105** `P1` **Pipelined shuffle.**
  - Mappers push sorted, combined spill runs over DIST-011 as they are produced.
  - The map function runs in a separate thread from the sender, so a network stall never blocks map progress.
  - Reducers merge received runs periodically during the shuffle.
  - Blocking mode (Hadoop) is a send-policy setting, not a separate code path.

  — _R14/G4 §3; HOP NSDI §3.1_
- **FLAG-106** `P1` **Adaptive flow control and combining.** DIST-012 applied to the shuffle, using the combiner that LANG-112 derives from the reduce aggregate. HOP's σ = 0.5 and k-spill heuristic are the defaults, plus the reverse-pressure rule. — _R14/G4 §3.3, §10.8; HOP NSDI §3.1.3, §6.2_
- **FLAG-107** `P0` **Exactly-once pipelined map output.**
  - Spill boundaries are deterministic, and each spill has the id `(task, seq)`. Each record's identity is
    `(task, seq, idx)`.
  - The reducer's input is a set over those identities. For combined partials, it accepts only disjoint ranges from
    a cursor per logical task.
  - Retries and speculative attempts are therefore idempotent.
  - Per-mapper completion is a seal carrying the spill count.

  Required by FLAG-103 whenever the shuffle is pipelined (CR-32). — _R14/G4 §5.2–5.3, §10.2; HOP talk "Revised PFT"; HOP src_
- **FLAG-108** `P1` **Tentative map output.** Used when ANA-037 does not certify the map as deterministic.
  - The reducer keeps partials keyed by `(mapper, attempt)`, and merges only within one attempt.
  - Merging across mappers happens only after the JobTracker's commit seal for `(mapper, attempt)`.
  - Failed attempts are ignored, and then reclaimed by DR+.
  - Snapshots offer a `committed_only` mode and an `include_tentative` mode.

  — _R14/G4 §5.1, §10.3; HOP NSDI §3.3_
- **FLAG-109** `P1` **Map progress checkpoints as prefix seals.** A mapper periodically announces `(offset x ↦ spill s)`, which seals its spills below s regardless of attempt. Reducers merge that prefix across mappers, and a retry resumes reading at x. Gated on ANA-037 (per-record stateless). — _R14/G4 §5.1, §10.9; HOP NSDI §3.3 (proposed, never built)_
- **FLAG-110** `P1` **Recovery and speculation under pipelining.**
  - Map output is retained until every reducer commits. A reducer retry re-fetches everything, and lost map output is
    recomputed through lineage (FLAG-123).
  - Speculative map attempts interleave by spill id.
  - Reduce outputs and snapshots commit first-wins (put-if-absent), recording the attempt id (FLAG-124).
  - LATE and the default speculation use the pipelined progress model: reduce progress is 0.75 × shuffle + 0.25 ×
    (reduce+commit).

  — _R14/G4 §5.4, §10.9; HOP NSDI §3.3, §6.1, §8_
- **FLAG-111** `P1` **Pipeline-aware scheduling.**
  - Jobs can be submitted as a chain, recorded in a dependency table, and upstream jobs get slot preference.
  - Jobs have a type: `batch | pipelined | online | continuous`.
  - For online and continuous jobs, rules h1–h3 (Condie thesis Fig. 10.10) ensure all reduces are RUNNING before any
    map is scheduled. The count must be written `count<T> default 0` (LANG-106), or h2 never fires.
  - Admission control makes an online or continuous job's reduces fit the cluster's reduce slots.
  - Under slot pressure for online aggregation, reducers are favored over mappers.

  — _R14/G4 §4; Condie thesis §10.4.1, Ch. 11_
- **FLAG-112** `P1` **Reflective monitoring and alert-based speculation.**
  - A continuous monitoring job runs one map per worker reading OS counters, and reducers per rack. It feeds the
    relations `machineStat`, `processStat`, `taskStat`, `jobStat` and `alert` (rules ts1, ts2, js1, js2, a1).
  - Speculation policy s1/s2: back up a map with a critical alert under 10 s old, a high estimated completion time,
    exactly one attempt so far, a free slot, and a local split.
  - The policy is a swappable module next to FCFS and LATE. `currentTimeMillis()` becomes `now()`.

  — _R14/G4 §7.3; Condie thesis §10.5_

### 10.7 Modern Hadoop successor
R10 recommends C2 "Tide" on C3 "Lattice Lakehouse", delivered through the C1 "BOOM-2" milestones. See ODD-19. R14/G4 adds milestone M1b, "HOP parity" (FLAG-105..112, FLAG-135..137). Tide's triggers and accumulation modes (M3) generalize HOP's snapshots.
- **FLAG-120** `P1` **M1: FS2.** Metadata from BOOM-FS, plus leases and an `lmax` heartbeat, on a Raft log. Followers serve snapshot reads. — _R10 C1(1)_
- **FLAG-121** `P1` **Stage planner.** Separates narrow dependencies (co-partitioned, pipelined) from wide ones (shuffled). — _R10 #36_
- **FLAG-122** `P1` **Shuffle by seals.** Lattice reducers start merging before the reducer is ready. Non-idempotent aggregates use partials keyed by mapper. Threshold reads can return early. The shuffle is pipelined by default (FLAG-105, FLAG-107; ODD-24). Seals carry a spill-count digest, and early outputs follow the ANA-036 classes. — _R10 C1(4), #37; R14/G4 §10.2–10.4_
- **FLAG-123** `P1` **Recovery from lineage.** `derived_from` is taken from provenance. Lost partitions are recomputed recursively, and wide dependencies are checkpointed where the cost model says so. — _R10 #38_
- **FLAG-124** `P1` **Deterministic speculation, with an idempotent commit keyed by attempt.** — _R10 C1(5,7)_
- **FLAG-125** `P1` **M2: Lattice Lakehouse log.**
  - Add/remove as a 2P-set, plus metadata, protocol and `txn(appId, version)`.
  - A single coordinated claim of each version, through Raft or put-if-absent.
  - A conflict rule, checkpoints, and time travel.

  — _R10 #43, C3_
- **FLAG-126** `P1` **Metadata cache as a lattice KVS with gossip.** — _R10 C3(5)_
- **FLAG-127** `P1` **Maintenance through commit.** Compaction, vacuum and overwrite all go through the commit. Orphan files are never visible. — _R10 C3(7)_
- **FLAG-128** `P2` **Stateless executors that scale to zero.** — _R10 C3(6)_
- **FLAG-129** `P1` **M3: Tide streaming.**
  - Watermark and frontier lattices, with triggers on thresholds.
  - AssignWindows and MergeWindows.
  - Accumulation modes, including retraction.

  *Amended (R13):* a pane over an insert-only (`bag`) stream is POS-FINAL for thresholds, and SEALED at the
  watermark for its value. A pane that accumulates and retracts is Z-set valued, so it stays **provisional** until
  its watermark seal (SEM-017). Late data for a window that has already been emitted final goes to a declared
  corrections stream that is never final, or it is dropped. It never silently changes a final pane (TEST-088).
  — _R10 #39; R13 §9.6_
- **FLAG-130** `P1` **Weighted collections and incremental views.** Z-sets, DBSP, and shared arrangements. *Amended (R13):* views are replicated only through wrapped inputs (LANG-158, DIST-015..017, LIB-093). They converge (SEM-036), but they are never final without seals (SEM-017). — _R10 #41; R13 §7.1_
- **FLAG-131** `P1` **Durable input log.** A partitioned log replicated with Raft. — _R10 C2(7)_
- **FLAG-132** `P1` **Fault tolerance minimized by CALM.** Deterministic operators are replayed. Only the nondeterministic events at points of order are logged causally. Optional aligned barrier snapshots, and sinks that are idempotent or transactional. — _R10 #42_
- **FLAG-133** `P2` **M4: Progress tracking.** Partially ordered timestamps and a pointstamp/frontier protocol, written in the language and verified. — _R10 #40_
- **FLAG-134** `P2` **SQL frontend subset.** For TPC-H, TPC-DS and Nexmark. — _R10 B3_
- **FLAG-135** `P1` **Online aggregation over MapReduce and Tide.**
  - Reducers publish progressive snapshots (LANG-139), each labeled with its class (ANA-036).
  - Publication is atomic per `(job, reducer, point)`. A completeness threshold `count<R> published ≥ numReducers`
    fires when every reducer has published a point.
  - Published snapshots are stored durably so that they can be recovered.
  - Estimators come from LANG-113.
  - An optional seeded **random block order** is enforced by the scheduler, with per-block timing statistics recorded
    for later correction of the inspection paradox.

  — _R14/G4 §6, §10.5–10.6; HOP NSDI §4.1, §4.3; PANSARE §2_
- **FLAG-136** `P1` **Snapshot pipelining between jobs.**
  - The downstream job keeps `MapLattice<UpstreamPartition, LexPair<Max<progress>, Snapshot>>` (LANG-131). This
    subsumes HOP's three failure cases: a stale snapshot is dominated, and the final (eof) snapshot makes the result
    deterministic.
  - Monotone downstream jobs consume deltas incrementally.
  - Class A/H upstreams, or non-monotone downstreams, receive Z-set differences through FLAG-130.
  - Recomputation from scratch is used only for opaque host reducers.
  - Class-T outputs pipeline into the next job as final tuples.

  — _R14/G4 §6.3, §10.7; HOP NSDI §4.2; CR-33_
- **FLAG-137** `P1` **Continuous jobs (HOP mode) mapped onto Tide.**
  - Map sources are unbounded, and a `flush` operation exists. Flushed batches carry nondet identity.
  - Reduce triggers can fire every Δ of processing time, every n rows, or on a logical field. Processing time is
    nondet; event-time windows with watermark seals (FLAG-129) are the deterministic default.
  - HOP's tumbling-discard mode is available for compatibility.
  - The map-side ring buffer is reclaimed by consumer acks (ARM, ANA-061), and only after **all** consumers,
    including speculative ones, have acked.
  - Reducer state is checkpointed automatically, together with upstream cursors, through ABS (FLAG-132).

  — _R14/G4 §7.1, §10.10; HOP NSDI §5.1; HOP src `ReduceTask.stream()`_
- **FLAG-138** `P2` **Bound-based early finalization.**
  - Each key keeps an interval knowledge lattice `[lo, hi]`: lo is the class-L snapshot, and hi is lo plus the
    remaining input mass times the maximum contribution.
  - Rank outputs such as top-K and argmax become final before `ready` once the bounds separate. Such a test is a
    threshold in the knowledge order.

  — _R14/G4 §10.4 [ours]_
- **FLAG-141** `P1` **Job and stage completion by free termination.**
  - Bounded sources emit a whole-relation (or per-split) seal after their last record.
  - A stage or job is **complete** exactly when every one of its output relations is final (SEM-016), as checked
    locally by ANA-121 over the seals received. No separate "job done" coordination round is needed beyond the
    seals themselves.
  - POS-FINAL and THRESH-FINAL outputs, such as `hot(w) :- n >= 1000`, finish early, before any seal.
  - An output whose inputs may retract (Z-set corrections) is NEVER-FINAL until its seals arrive.
  - Nodes may `halt` (LANG-052) only when all of their outputs are final and they hold no relay obligations:
    unacked `outbuf` entries, or OnceTree edges.

  — _R13 §9.6, §12 T16; FT §3.1, §5.2_

### 10.8 Other demonstrators
- **FLAG-150** `P1` **Replicated log with ISR (Kafka 0.8), reproducing its durability bug under LDFI.** — _R06 §12.3_
- **FLAG-151** `P2` **Chain replication, primary/backup (Elastic), and Flux, as LDFI showcases.** — _R06 §12.4, §12.7_
- **FLAG-152** `P2` **BFT (PBFT).** Out of scope for v1. — _R07 §9.4_

---

## 11. Benchmarks and test corpus (BENCH)

Every program is ported to our syntax, or run through LANG-220/221/222, and **keeps its published expected result**. Unless marked otherwise, each item runs in CI through the conformance runner (BENCH-000).

- **BENCH-000** `P0` **Conformance and performance harness.** A declarative test manifest listing, for each program: its inputs, the network mode, its expected outputs or verdicts, and its performance targets. — _[ours]; R03 §11; R06 §12_

### 11.1 Core semantics
- **BENCH-001** `P0` **DL11 Ex. 3 (persistence and deletion).** Expected:
  - `p(1,2)` holds at ticks 101..300 and is absent from 301 on.
  - `p(1,3)` holds from 102 on.
  - The program is quiescent from 302 on.

  — _R02 T1_
- **BENCH-002** `P0` **DL11 Ex. 4.** Must be accepted: it is temporally stratifiable but not syntactically stratifiable. — _R02 T2_
- **BENCH-003** `P0` **Unsafe rule `p(A,B) :- q(A)`.** Must be rejected. — _R02 T3_
- **BENCH-004** `P0` **flip_flop.** Accepted with a temporal-safety warning. It has period 2 and an empty ultimate model. — _R02 T4_
- **BENCH-005** `P1` **Toggle/announce.** `state` alternates between 0 and 1. — _R02 T5_
- **BENCH-006** `P0` **Sequence.** `seq` increments exactly at the ticks that have events. — _R02 T6_
- **BENCH-007** `P0` **DL11 priority queue.**
  - Per-user queue: at tick 124 the output is (bob,bash,200), (eve,ls,1) and (alice,ssh,204). At 125 it is (bob,ssh,205).
  - Global FIFO: eve at 124, bob/bash at 125, alice at 126, bob/ssh at 127.

  — _R02 T7_
- **BENCH-008** `P0` **Counter with request/response.** The counter increments once per tick that has a request, and the requester receives the value before the increment. — _R02 T9_
- **BENCH-009** `P1` **Soft-state TTL with a mocked `now`.** — _R02 T10; R01 T8_
- **BENCH-010** `P0` **timeout_svc.** `timer_svc(H,I,3)@k` produces `timeout(H,I)` at k+2. — _R02 T11_
- **BENCH-011** `P0` **Bud test_simple_deduction.** After tick 2:
  - `scrtch == [[c,d,5,6]]`
  - `scrtch2` is empty
  - `tbl == {(c,d,5,6),(z,y,9,8)}`
  - `the_keys == [[c,d],[z,y]]`

  — _R03 T3_
- **BENCH-012** `P0` **Key conflict.** `DupKeyBud` raises. `EmptyPk` raises on a second distinct tuple but accepts re-inserting the same tuple. — _R03 T4_
- **BENCH-013** `P0` **Exact-match delete.** Deleting `[5,11]` never removes `[5,10]`. `[5,10]` itself is removed 2 ticks after it is inserted into `del_buf`. — _R03 T5_
- **BENCH-014** `P0` **Upsert.** `joe` is `[[1,'a']]` at tick 1 and `[[1,'b']]` at tick 2. The same holds for `<-+`. — _R03 T6_
- **BENCH-015** `P0` **Stratification.** The unstratifiable `glass` rule is rejected; its `<+` variant is accepted. The toggle program is accepted with `@next` and rejected without it. — _R03 T7; R05 T6_
- **BENCH-016** `P0` **Illegal writes rejected.** A `<=` from outside a tick, and any write into `periodic`, are rejected. — _R03 T8_
- **BENCH-017** `P1` **Halt.** The node stops at the tick where key 2 appears. — _R03 T10_
- **BENCH-018** `P0` **P2 seAtomicity.** `count` is 0 within the same fixpoint. `tCounter` counts 0..10 across ticks. — _R01 T13_
- **BENCH-019** `P0` **NR09 Figs. 1 and 2.** Fig. 1: `update` carries the sequence number from before the increment. Fig. 2: every node, including node1, increments exactly once. — _R01 T14–15_
- **BENCH-020** `P0` **Bud paths.rb.** After one tick, `shortest` holds exactly the 10 listed tuples. Adding `(e,f,1)` adds exactly 5 more, with no key conflict. — _R03 T1_
- **BENCH-021** `P0` **All-paths in REBL.** Exactly 14 path tuples, including `(a,b,b,1)`, `(a,b,b,4)` and `(a,e,b,7)`. — _R03 T2_
- **BENCH-022** `P0` **SIGMOD06 shortest path.** Final costs: a→b 2, a→c 1, a→d 3, e→a 1, e→c 2, e→b 3, e→d 4. — _R01 T2_
- **BENCH-023** `P1` **Incremental and bursty updates (THESIS Fig. 5.7, 3.9).** After quiescence the result equals evaluation from scratch, both with FIFO links and with reordering. — _R01 T3_
- **BENCH-024** `P1` **Distance vector with split horizon and poison reverse.** No count-to-infinity. DV and DSR produce equal `bestPath`. — _R01 T4–5_
- **BENCH-025** `P2` **Magic-sets shortest path.** Same answers, with no more tuples sent at any node. — _R01 T6_
- **BENCH-026** `P1` **Localization equivalence.** Chord sb1–sb3 and SP2 give the same results before and after the rewrite. — _R01 T20_
- **BENCH-027** `P1` **Metaprogram stratification over the catalog.** `p :- notin p` is placed in stratum ∞ and rejected. — _R01 T21_
- **BENCH-028** `P1` **Soft-state lifetime lint.** The lint fires, and the program oscillates at runtime. — _R01 T19_
- **BENCH-029** `P0` **Soufflé security example.** `I = {s, l3}`. — _R09 T1_
- **BENCH-030** `P0` **Naive vs. semi-naive.** On random TC, SG, Andersen and CSPA inputs, both give the same result. Each (Δ, old) combination is derived exactly once. — _R09 T2_
- **BENCH-031** `P0` **Lattice SSSP.** `path(1,3) = 20`. — _R09 T3_
- **BENCH-032** `P0` **Non-morphism trap (`size` of an lset).** Must equal the naive result. — _R09 T4; R04 T8_
- **BENCH-033** `P1` **Flix compactness trap.** Produces `R(⊤)`. — _R09 T5_
- **BENCH-034** `P1` **eqrel Δ-extension.** `(a,f)` is visible within the same fixpoint. — _R09 T6_
- **BENCH-035** `P0` **CC and SSSP via MIN.** Match a union-find or Dijkstra oracle. For the bipartite check, `answer()` holds if and only if the graph has an odd cycle. — _R09 T7–8_
- **BENCH-036** `P0` **Tick behavior.**
  - Persistence and deletion across ticks use storage O(changes).
  - Tick-local relations are cleared each tick.
  - With 10^6 persisted tuples and 1–100 messages per tick, the time per tick stays flat.

  — _R09 T11–13_
- **BENCH-037** `P1` **Incremental maintenance.** DDlog firewall: insert 12% more, then delete 3%, and the result equals a full recompute. DOOP: retracting a single fact takes milliseconds. — _R09 T9–10_
- **BENCH-038** `P1` **DFIR tests.**
  - Reachability gives {0..4}; unreachability gives {11,12}.
  - Flip-flop.
  - Dedup across ticks: `1 2 3 4`, then `5 6`.
  - Join lifetimes.

  All are adapted to the semantics in CR-26. — _R08 T1–7_
- **BENCH-039** `P1` **Snapshot tests from the Datalog frontend.** Including `test_persist_uniqueness`. — _R08 T8_
- **BENCH-040** `P0` **Per-tick vs sticky `choose` over a persisted table.** In canonical-priority mode:
  - `pick` is m, m, c, c, p, —, m;
  - `spick` is m, m, m, c, p, —, m;
  - Bud's behavior, derived from its source, is m, m, m, p, p, —, m.

  Under every priority permutation and ≥100 seeds, the engine equals the oracle and the validity checker passes. —
  _R12/G2 §12 T1_
- **BENCH-041** `P0` **Retracting a chosen vs a non-chosen candidate, and key constraints.**
  - Deleting a non-chosen candidate leaves the choice unchanged.
  - A per-tick keyed view fed by `choose` never raises SEM-050.
  - A second rule writing the same key raises SEM-050. With `resolve choose`, it resolves to the least-priority
    value instead.

  — _R12/G2 §12 T2_
- **BENCH-042** `P0` **`index()` slot assignment across ticks (FLAG-021).** The batches are {x,a,m}, {}, {b} and
  {z,c}.
  - Slots: a0 m1 x2, then b3, then c4 z5.
  - `rank` over the persistent relation re-ranks every tick (a0 b1 m2 x3 at t3), and ANA-011 lints it.
  - `seq` gives a0 m1 x2 b3 c4 z5.
  - A duplicate payload within one tick takes one slot.

  — _R12/G2 §12 T3_
- **BENCH-043** `P0` **`choose_rand` under a fixed seed.**
  - With ρ = 42, the engine and the oracle both reproduce the golden values, and two runs are identical.
  - The sticky variant stays constant until its value is deleted.
  - Over 10,000 seeds, each of the four values is chosen as r1 between 2300 and 2700 times, and
    P(r_t = r_{t+1}) ∈ [0.23, 0.27].
  - Two nodes agree about 1/4 of the time for `choose_rand`, and always for `choose`.
  - After a restart, `rand` values change and `choose` values do not.

  — _R12/G2 §12 T4_
- **BENCH-044** `P0` **Raft vote among same-tick RequestVotes.** Three RequestVotes for term 5 arrive in one tick:
  one from a candidate whose log is behind, two from up-to-date candidates.
  - Exactly one grant is made. `min<C>` gives c2; `choose` gives c2 in canonical mode.
  - Retransmissions are re-granted only to the grantee.
  - A higher term produces a new vote, and a stale term is rejected.
  - After a crash, the durable `voted_for` prevents a second vote.
  - For every seed and schedule, each term has at most one grantee.

  — _R12/G2 §12 T5; FLAG-002, FLAG-006, FLAG-007_
- **BENCH-045** `P0` **`fold_ordered` over a growing log**, with step 2s+x.
  - `sm` is 0, 5, 21, 21, 52.
  - The aggregate `total` is 8, 21, 21, 52, then 276 after an insertion at key 0, which forces a full refold.
  - The carried fold over a persistent input, `bad`, is 0, 8, 149, 2405, 77012. ANA-011 lints it.
  - ENG-073 costs O(|Δ|) on appends.
  - A false commutativity declaration fails TEST-015.

  — _R12/G2 §12 T6_
- **BENCH-046** `P1` **Multi-FD choice (bipartite matching).** With g = {(1,a),(1,b),(2,a)}, canonical priority gives
  {(1,a)}. Every priority permutation gives either {(1,a)} or {(1,b),(2,a)}, and both are maximal. Incremental
  updates recompute only the affected conflict component. — _R12/G2 §12 T7; GZ01 Ex 4.2_
- **BENCH-047** `P0` **Choice stratification.** A same-tick recursive `choose` is rejected with a cycle witness. The
  one-layer-per-tick BFS tree over r→a, r→b, a→c, b→c, c→d gives {(a,r),(b,r),(c,a),(d,c)} in canonical mode. Under
  any seed, c's parent is a or b. — _R12/G2 §12 T8; SZ90 §6_
- **BENCH-048** `P1` **Ties in `topk`, `sort` and `percentile`.**
  - top2 = {u1,u3}.
  - sort = [u1,u3,u2,u4].
  - The median is 7.
  - Deleting u1 changes the output by exactly −u1 +u2.
  - ANA-038 emits the tie lint.

  — _R12/G2 §12 T9_
- **BENCH-049** `P0` **Replay and oracle harness.**
  - A trace replays with every digest matching.
  - A tampered override stops replay with `ChoiceOverrideNotCandidate`.
  - Renaming a rule makes replay refuse to start.
  - A mutant engine that uses first-seen instead of priority is caught by TEST-013 and by exact equality.
  - Fed Bud's choices, TEST-014 passes, and the "which mode explains it" diagnostic matches R12/G2 §12 T10.

  — _R12/G2 §12 T10_

### 11.2 Lattices
- **BENCH-050** `P0` **QuorumVoteL.** Exactly one result, emitted once there are 5 or more distinct voters, and no points of order. The Fig. 2 version must be flagged. — _R04 T1_
- **BENCH-051** `P0` **Bud tc_lattice max suite.** SimpleMax, MaxOfMax, EmbedMax, EmptyMaxMerge (m1 stays 5), MaxConstructorImplicit and MaxOverChannel. — _R04 T2_
- **BENCH-052** `P0` **ShortestPathsL.** The acyclic and cyclic inputs each produce exactly the `min_cost` sets listed in R04 T3, with everything in stratum 0. — _R04 T3_
- **BENCH-053** `P0` **lmin over a negative cycle.** Either the iteration bound fires as a hard error or the program is rejected statically. — _R04 T4_
- **BENCH-054** `P1` **MaxCapacityPaths and the AllPathsL variants.** — _R04 T5–6_
- **BENCH-055** `P1` **Semi-naive equivalence on transitive closure over DAGs.** — _R04 T7_
- **BENCH-056** `P1` **Map, set, bag and sum tests.** — _R04 T9–10_
- **BENCH-057** `P0` **Key semantics with embedded lattices.** Lattice columns merge; a non-lattice conflict is an error; a lattice used as a key is a type error. — _R04 T11_
- **BENCH-058** `P1` **KVS replica convergence under fuzzing, and the quorum client.** — _R04 T12_
- **BENCH-059** `P1` **Versioned KVS scenarios with ldom.** — _R04 T13_
- **BENCH-060** `P1` **Monotone cart.** Every complete replica produces an identical summary, bad inputs raise errors, and at most one response is sent per distinct summary. — _R04 T14_
- **BENCH-061** `P1` **Causal delivery and vc_scenario.** pipe_out delivers a, then b, and never d or e. — _R04 T15; R03 T21, T27_
- **BENCH-062** `P0` **Anna lattice laws.**
  - LWW with a (ts, node) key converges.
  - A bare `>=` fails commutativity.
  - DomPair with a set key fails associativity on x=({1},p), y=({2},q), z=({1,2},r).
  - Lex with a chain key passes.

  — _R04 T16_
- **BENCH-063** `P1` **CRDT catalog.** Property tests plus convergence fuzzing. — _R04 T17_
- **BENCH-064** `P1` **Query classification ("Keep CALM").** — _R04 T18_
- **BENCH-065** `P2` **Katara 2P-set agrees with a sequential set.** — _R04 T19_
- **BENCH-066** `P1` **Hydro lattice ops.** The Cartesian bimorphism gives 6 pairs. `state[items]` emits only what changed. `lattice_reduce` gives Max(5). — _R04 T20_
- **BENCH-067** `P1` **Flo thresholds.** `thresh(7)` fires exactly once, however the input is batched. `reveal` on an unbounded input is rejected, and so are thresholds that are not incompatible. — _R04 T21_
- **BENCH-068** `P0` **Stratification negatives.** `reveal`, `lt_eq`, and `ldom.value` inside recursion are all rejected. — _R04 T22_
- **BENCH-069** `P1` **Merging at the sender is equivalent to sending individually.** — _R04 T23_
- **BENCH-070** `P0` **Lattice persistence.**
  - A persistent lattice keeps its value.
  - A tick-scoped lattice resets.
  - `<+` takes effect at t+1.
  - BootstrapNoRules.

  — _R04 T24_
- **BENCH-073** `P0` **Replicated count with retractions over a duplicating channel (W2).**
  - *Setup.* Replicas A, B, C. Chaos: each message is duplicated 1–4×, reordered, and 30% of first sends are
    dropped, with resends.
  - *Operations.* A: +x at t1, +x at t2, −y at t5. B: −x at t1, then +z and −z in the same tick. C: +y at t1.
  - *Expected.*
    - Everywhere: x=1, y absent, z never sent.
    - Payload applications equal the number of distinct dots: 5 per remote replica.
    - B may transiently hold raw x=−1, displayed clamped as 0.
    - `output final` on the count view is rejected as NEVER-FINAL.
  - *Negative variant.* A raw weight column on a plain channel is a compile error (ANA-015). Forced through with
    `nondet`, the simulator finds x≠1.

  — _R13 §12 T1_
- **BENCH-074** `P1` **Cumulative wrapper (W3) over lossy gossip on a line A—B—C.** No acks, 50% loss,
  duplication, anti-entropy every 5 ticks. The final state is the same as BENCH-073, and C learns A's updates only
  through B. A mutant that applies C′ instead of C′−C must be caught diverging. — _R13 §12 T2_
- **BENCH-075** `P1` **Replicated DBSP view.** The view is `V(a,c) :- R(a,b), S(b,c), notin D(a)` over
  W2-replicated Z-set inputs, under chaos. After quiescence every replica equals naive centralized evaluation
  (ENG-067). V is classified NEVER-FINAL. — _R13 §12 T12_
- **BENCH-076** `P1` **Deletion-semantics corpus.**
  - Concurrent guarded deletes of a count-1 item give −1 (clamped view 0).
  - A guarded delete-then-insert gives 1; the unguarded version gives 0.
  - 2P-Set: a deleted element is final-absent and cannot be re-inserted.
  - OR-Set: Dolan Fig. 1a and 1b differ.
  - PN-Set: add, remove, remove reaches −1.

  — _R13 §12 T3–T4; Wrapping Rings §5; Dolan §1_
- **BENCH-077** `P2` **OnceTree counter.** 15-node binary tree, ±1 updates at leaves, chaos on every edge.
  Converges to the exact sum, keeps degree+1 entries per node, and survives move-up/down, join, leave and reset. —
  _R13 §12 T13; Power diss. §4.4–4.5_
- **BENCH-078** `P1` **Dot reuse after a crash (negative).** An origin without a durable seq and without an
  incarnation bump makes receivers silently drop its post-restart updates, and the test must detect the
  divergence. With the incarnation bump nothing is lost. W3 with a reused seq and a different cumulative raises a
  `Conflict` error. — _R13 §12 T14_
- **BENCH-079** `P1` **Finite-state free termination (ANA-122).**
  - FT Fig. 1:
    - (a) only the post-a sink is FT;
    - (b) the accept and reject sinks are FT, with different values;
    - (c) there are no FT states;
    - (d) only the accepting sink of the doesn't-start-with-b branch is FT.
  - 2PC per-transaction automaton: `committed` and `aborted` are FT.
  - Running product mod 2: `even` is FT via the homomorphic abstraction.

  — _R13 §12 T11; FT Ex 4, Prop 26_

### 11.3 Asynchrony, confluence and analysis oracles
- **BENCH-080** `P1` **DL11 reliable broadcast.** — _R02 T12_
- **BENCH-081** `P2` **Lamport `p_wait`.** Requires entanglement. — _R02 T13_
- **BENCH-082** `P0` **Marriage ceremony.** There are two ultimate models and the program is flagged diffluent. The Dedalus+ version has exactly one ultimate model and is certified. — _R02 T14–15; R05 T5_
- **BENCH-083** `P0` **Distributed GC.** Without coordination, some schedule collects garbage wrongly. After the coordination rewrite, every schedule is correct. — _R02 T16; R05 T3_
- **BENCH-084** `P1` **Max-element dequeue.** Confluent, but not certified as Dedalus+. — _R02 T17_
- **BENCH-085** `P1` **Concurrent arrival.** Diffluent. — _R02 T18_
- **BENCH-086** `P1` **Async singleton.** Ultimate models are {{}, {p}}. With persistence the ultimate model is {p}. — _R02 T19_
- **BENCH-087** `P1` **TPLP Figs. 1–3.** Covered, random ordering, and 2PC. — _R02 T20–22_
- **BENCH-088** `P1` **TPLP Figs. 4–5.** Non-causality and infinite grouping. — _R02 T23–24_
- **BENCH-089** `P1` **CRON Figs. 1, 3, 4 and 5.** The positive but inconsistent program in Fig. 5 must not be certified. — _R02 T25–28_
- **BENCH-090** `P1` **CALM oracles.**
  - Deadlock detection is certified monotone.
  - The emptiness query needs coordination.
  - Complement of TC is covered, including a negative test: a path of length 1 but not 2.
  - Threshold queries return a final result early.
  - Policy-aware `R\S` is P2.

  — _R05 T1–10_
- **BENCH-091** `P1` **Ameloot "message join".** It is confluent but not consistent under fairness, and the certificate must say so (CR-29). — _R10 T21_
- **BENCH-092** `P0` **Bud tc_labeling.**
  - TestNM: `{"response"=>"D"}`, with paths `{i1:A, i2:D}`.
  - TestGroup: D. TestMono: A. TestDeletion: its paths are D. TestNestMod: D.
  - BugButt and HalfGuard are unguarded; FullGuard is guarded.

  — _R03 T29; R05 T11_
- **BENCH-093** `P0` **CIDR'11 golden analyses.**
  - BasicKVS forms the cluster {kvstate, prev}.
  - DisorderlyCart has points of order only at the checkout join and at `accum`.
  - DestructiveCart has a point of order on every action.
  - ReplicatedKVS reports underspecification until multicast and membership are mixed in.

  — _R03 T28, T30; R05 T12_
- **BENCH-094** `P1` **Blazes.**
  - Storm wordcount: Run with no seal; Async with Seal_batch.
  - Ad reporting: THRESH is Async, POOR is Diverge, CAMPAIGN+Seal_campaign is Async, WINDOW+Seal_window is Async, and CAMPAIGN+Seal_window is Diverge.
  - Also: FD chase units, the antijoin subscript, and cycle collapse.

  — _R05 T13–17_
- **BENCH-095** `P1` **Edelweiss.** All ten artifact programs, plus every unit test in bud-gc `tc_gc.rb`.
  - Under random schedules, observable outputs are identical and storage plateaus.
  - The negative variants leak, as expected.
  - Range compression: `[1-4],[8]` becomes `[1-8]` after 5–7 are inserted.

  — _R05 T18–27_
- **BENCH-096** `P2` **I-confluence table pairs.** — _R05 T28_
- **BENCH-097** `P0` **Free-termination oracle suite (ANA-120/121).**
  - (a) Reachability `reach() :- P(s,t)` is final-true the moment P(s,t) is derived, before the fixpoint, with
    early exit. On a graph with no s→t path it stays provisional until `seal Edge`, then is final-false.
    `count<*> Edge > 10` with 7 edges stays provisional until the seal.
  - (b) `R(c), notin S(c)` is final-false once S(c) arrives. The FT §5.1 query `q(x)` gives (20) final-present and
    (5) final-absent from the inputs R(20), S(5), T(5).
  - (c) `forall R(x): x>0` is final-false at the first violation.
  - (d) `pn.value >= 10` is never final; `pn.pos >= 10` is final.
  - (e) 2P-Set: final-absent only.
  - (f) Sealed tally: `tally >= 3` is final early, and the exact count is final at the last seal.

  — _R13 §12 T5–T10; FT §1.1, §3.1, §4.1, §5.1, App. B_
- **BENCH-098** `P1` **Finality soundness under fuzzing (TEST-088).** 10⁴ schedules with crashes over BENCH-097.
  No final emission is contradicted by the ultimate model, and replicas never finalize different values. — _R13 §12
  T15_
- **BENCH-099** `P1` **Batch completion by seals and thresholds (FLAG-141).** A word count over 4 splits:
  - `hot(w)` (n ≥ 1000) is final early;
  - `counts` and `count<w>` become final at the 4th split seal;
  - the stage is complete exactly then;
  - different interleavings give identical finals;
  - the retracting variant reclassifies `hot` as NEVER-FINAL, and a `final` annotation on it is a compile error.

  — _R13 §12 T16_

### 11.4 Networking and protocol corpus (bud-sandbox, Overlog)
- **BENCH-100** `P0` **TickleCount.** `loopback_done == [[5]]` and `mcast_done == [[5]]`. There are exactly 2 strata, and the edge `mcast←loop_chan` is monotone. — _R03 T11_
- **BENCH-101** `P0` **Ring of 10.** The count reaches 39. Node i ends with `last_cnt == 30+i`. — _R03 T12_
- **BENCH-102** `P0` **Channel key conflict at the sender.** Also checks that `payloads` strips the address. — _R03 T13_
- **BENCH-103** `P1` **Channel filter.** With a drop filter, exactly `[[dst,3]]` is delivered. A batching filter delivers all 12 messages in a single tick. — _R03 T14_
- **BENCH-104** `P0` **Reliable delivery.**
  - All 4 messages are delivered and acked, after which `buf` is empty.
  - Nothing is ever delivered to an unreachable node.
  - Delivery still succeeds when combined with 50% demonic drops.

  — _R03 T15_
- **BENCH-105** `P0` **Multicast.** One `mcast_done`, and exactly one `pipe_out` at each other member. — _R03 T16_
- **BENCH-106** `P0` **Voting.** The exact `vote_cnt` and `vote_status` tuples. The majority variant decides as soon as more than half have voted. — _R03 T17_
- **BENCH-107** `P0` **2PC.**
  - A transaction starts in prepare. One Y vote leaves it in prepare; two Y votes commit; any N aborts.
  - If still in prepare after more than 10 ticks, it aborts.
  - Every peer learns the outcome.

  — _R03 T18; R01 T16; R07 T1_
- **BENCH-108** `P0` **KVS workloads.**
  - Sequential puts leave "bak".
  - With replication, both replicas hold "bak".
  - A delete empties the store.
  - The durable store survives a restart. The upstream version of this test is disabled; it must pass for real here.
  - Four puts in one tick raise a KeyConstraint error.

  — _R03 T19_
- **BENCH-109** `P0` **Carts.**
  - simple_workload gives `[["beer",13],["diapers",1]]`.
  - The multi-session case gives the listed results.
  - The monotone cart gives `[[5,1],[10,3]]` in stratum 0, and bad inputs raise errors.

  — _R03 T20_
- **BENCH-110** `P0` **Serializer and assigners.**
  - Dequeuing 1234 gives `[1234,1,'foo']`; dequeuing 2345 gives `[2345,2,'bar']`.
  - SortAssign gives 0..99 in sorted order; the persistent variant gives contiguous ranges.
  - GroupNonce gives t*3+1.

  — _R03 T22_
- **BENCH-111** `P0` **Lamport clock.** foo gets 0, bar gets 1; after receiving a message stamped 20, the next stamp is 22. — _R03 T23_
- **BENCH-112** `P1` **MVCC and MV-KVS suites.** — _R03 T24–25_
- **BENCH-113** `P1` **Other sandbox suites.** State machine, heartbeat, membership, Chord (the exact tuples from tc_chord), leader election, MI cache coherence, and BFS. — _R03 T26_
- **BENCH-114** `P1` **BloomUnit specs.**
  - The FIFO spec must fail under Dastardly delivery and must never fail under ordered delivery.
  - For CartSpec, exploring schedules must find a checkout that overtakes an action. The fix using a manifest, or using the monotone cart, passes every schedule.

  — _R03 T31–32; R10 T23_
- **BENCH-115** `P1` **Overlog corpus.**
  - Ping-pong: exactly 20 pings and 20 pongs.
  - Soft-state ping-pong: the link expires about 10 s after the last refresh.
  - Narada: a dead neighbor is removed within about 21 s, and the count-0 idiom works.
  - Static ring: looking up key 289383 returns localhost:33333.
  - Dynamic and robust rings.
  - Chord with 3 nodes: `bestSucc` forms a correct chain.

  — _R01 T7–12_
- **BENCH-116** `P2` **Chord at scale.**
  - A lookup takes about 0.5·log2 N hops.
  - On a 500-node static ring, at least 96% of lookups finish within 6 s.
  - Under churn with sessions of at least 64 minutes, at least 97% of lookups are consistent.
  - Symphony's degree bound holds under concurrent requests.

  — _R01 T12, T18_

### 11.5 LDFI corpus (verdicts must match Molly exactly)
- **BENCH-130** `P0` **Delivery family.** Nodes a,b,c. Each configuration is written EOT/EFF/Crashes; CE means a counterexample must be found.
  - `simplog`: CE at 6/3/0 and 4/2/0.
  - `rdlog`: no CE at 6/3/0 or 25/23/0; CE at 6/3/1 and 4/2/1.
  - `classic_rb`: CE at 6/3/0 and 5/3/0; no CE at 6/0/2.
  - `replog`: no CE at 6/3/0, 6/3/1, 8/6/1, or 11/10. At 6/3/1, the failure-free lineage alone must certify it.
  - `ack_rb`: no CE at 6/3/1, 8/6/1, or 8/7.

  — _R06 §12.1; R10 T1–5_
- **BENCH-131** `P0` **Commit protocols.** Nodes a,b,C,d; configurations as in BENCH-130.
  - `2pc`: no CE at 7/3/0; CE at 6/3/1, 6/0/1, 6/0/2 and 5/0/1.
  - `2pc` with the optimist spec: CE.
  - `2pc_timeout`: no CE with the optimist spec; CE with the strict spec.
  - `2pc_ctp`: CE at 6/0/1, 6/0/2 and 8/0/1.
  - `3pc`: no CE at 8/0/1 or 8/0/2; CE at 9/7/1 (agreement is violated).

  — _R06 §12.2; R10 T6–8_
- **BENCH-132** `P0` **Kafka ISR.** Nodes a,b,c,C,Z. CE at 7/4/1, no CE at 7/4/0, CE at 6/4/1. — _R06 §12.3; R10 T9_
- **BENCH-133** `P1` **Protocols expected to pass.** No CE for paxos_synod at 8/3/1 and 7/6, for bully-le at 10/9, or for flux at 22/21. — _R06 §12.4; R10 T10–12_
- **BENCH-134** `P1` **Nemo case studies.**
  - `pb_asynchronous` has a CE; after the `ack_log` repair it has none.
  - CA-2083, ZK-1270, MR-2995, CA-2434 and MR-3858 each have a CE.
  - The suggested repair for ZK-1270 is `success(L) :- sent_flag(L), ack(F)`.

  — _R06 §12.5_
- **BENCH-135** `P0` **Molly unit tests.**
  - grossEstimate returns 1, 16, 4, 96, 16, 4 and 121, plus a non-zero bignum.
  - Solver normalization.
  - The rewrite forms.
  - Provenance counts: 2 firings, 2 derivations, 3 and 1 contributors, and `vote_cnt` 2.
  - Derivation trees: 4.
  - The symmetry cases.
  - Netflix toy example: the only minimal falsifiers are {RepA,RepB} and {Bcast1,Bcast2}.
  - The LDFI §4.3 formula.
  - PODS'07 N[X] values, e.g. `q(d,e)=2r²+rs`, and the bag values 8, 10, 10, 55 and 7.
  - Soufflé heights: Θ(k²) updates, and `vpt(b,l1)` goes from 7 to 3.

  — _R06 §12.6_
- **BENCH-136** `P1` **Number of executions.** Must be at most the published counts:
  - bug-finding runs: simple 2, retry 3, classic 5, 2pc 2, ctp 3, 3pc 55, Kafka 38;
  - certification runs: redun 11, ack 673, paxos 173, bully 2, flux 187.

  — _R06 §3.9_
- **BENCH-137** `P1` **Molly raft.ded and negative_support_test.** Neither `two leaders` nor `disagree` is ever derived. Bugs seeded into the program are found. — _R10 T13–14_

### 11.6 Verification
- **BENCH-150** `P1` **Paxos Made EPR.**
  - Verifies equations (4)–(15).
  - Before the rewrite, the program is reported as outside EPR, with a cycle in the QA graph.
  - With equation (15) omitted, the Fig. 6 counterexample to induction is reproduced.

  — _R10 T15_
- **BENCH-151** `P2` **Inference and bounded model checking stretch goals.**
  - On simplified consensus, invariants (1)–(4) are inferred.
  - For ring leader election, BMC finds a trace when "IDs are unique" is omitted.
  - Multi-Paxos, Flexible Paxos and Stoppable Paxos.

  — _R10 T16–18_

### 11.7 Consensus and flagship systems
- **BENCH-170** `P0` **Raft core suite.** All 13 Hydro Raft tests plus the Molly `raft_assert` invariants:
  - `even_cluster_simultaneous_candidates_exactly_one_leader_per_term`
  - `heartbeats_converge_leader_views`
  - `vote_and_ack_decisions_interlock`
  - `leader_steps_down_on_higher_term_reply`
  - `stale_log_candidate_is_refused`
  - `leader_replicates_and_commits_requests`
  - `non_leader_redirects_requests`
  - `leader_without_quorum_commits_nothing`
  - `new_leader_overwrites_conflicting_uncommitted_entries`
  - `previous_term_entries_commit_only_transitively`
  - `composed_raft_elects_replicates_and_suppresses`
  - `concurrent_elections_never_fork_the_committed_log`
  - `fully_concurrent_run_never_forks_the_committed_log`

  — _R07 T9_
- **BENCH-171** `P0` **Raft paper scenarios.** None of the anomalies in Fig. 3.7, 4.2, 4.6 and 4.7, or in §6.3 and §6.4, occurs. — _R07 T10_
- **BENCH-172** `P1` **The 2015 membership bug.** It is found when the fix is removed, and absent when the fix is in. — _R07 T11_
- **BENCH-173** `P0` **Raft restart.**
  - No node votes twice in a term.
  - No committed entry is lost.
  - Crashing between fsync and send never releases a message before the state it depends on is durable.

  — _R07 T12_
- **BENCH-174** `P1` **Raft snapshots, and linearizable KV checked with Porcupine.** — _R07 T13–14_
- **BENCH-175** `P1` **Negative tests from Bud Raft.** noeleo's missing durability barrier is flagged. whitewater's uncapped commit leads LDFI to find a State Machine Safety violation. — _R07 T15_
- **BENCH-176** `P0` **Multi-Paxos.**
  - autocomp MultiPaxos: each slot commits once, every replica gets the same payload, and killing the leader triggers reconciliation.
  - NetDB'09 Overlog Paxos: `lt("fail")` is never derived, and `lt("succeed")` appears after 200 decrees.
  - Synod: `disagree` is never derived.

  — _R07 T4–6_
- **BENCH-177** `P1` **CompPaxos/ScalablePaxos, and Hydro Paxos + kv_replica GC below the checkpoint.** — _R07 T7–8_
- **BENCH-178** `P1` **Dedalus 2PC and voting (autocomp).** Includes GC of AllVotes and AllAcks. Rewritten programs produce the same client histories. — _R08 T9–12_
- **BENCH-179** `P1` **Hydro examples.**
  - `collect_quorum`; read-after-write with `atomic`; the six cart variants; the COVID tracker.
  - Chat with history (persist⋈persist delta); Flo nested iteration.
  - Deadlock detector, RGA, replicated KVS, Lamport and vector clocks.
  - Maelstrom broadcast.

  — _R08 T14–25_
- **BENCH-180** `P1` **BFS / BOOM-FS.**
  - tc_bfs semantics: removing a non-empty directory fails.
  - tc_e2e_bfs: the MD5 of `/usr/share/dict/words` round-trips.
  - Heartbeats expire.
  - Killing a datanode triggers re-replication.

  — _R07 T16_
- **BENCH-181** `P1` **BOOM-FS HA.** No metadata is lost when the primary fails, and replication costs almost nothing without failures. The reference numbers are 101.89 s vs. 102.70 s, and 148.47 s when the primary fails. — _R07 T17_
- **BENCH-182** `P1` **Partitioned NameNode.** Re-running an interrupted mkdir converges, and rename across partitions is atomic via 2PC. — _R07 T18_
- **BENCH-183** `P1` **BOOM-MR: FCFS vs. LATE with stragglers.** LATE keeps speculation at or below 10% of slots and never speculates on a slow node. Its reduce CDF has a shorter tail. — _R07 T19_
- **BENCH-184** `P1` **Monitoring metaprograms.**
  - Trace rewriting of Paxos.
  - A coverage report of rules that never fired.
  - The number of messages per decree matches the spec.

  — _R07 T20_
- **BENCH-185** `P1` **Modern Hadoop.**
  - Batch: WordCount, Grep, a small TeraSort and PageRank give the same result as a single-node reference, even with workers killed and speculation on.
  - Streaming: session windows with late data under each accumulation mode; Nexmark Q1–Q8; exactly-once output after a crash mid-epoch.
  - Recursive views (CC, TC) maintained incrementally under inserts and deletes equal recomputation.
  - Lakehouse:
    - concurrent appends all commit;
    - when an append and a delete conflict, exactly one aborts;
    - a crash between writing data and committing never becomes visible;
    - replaying `txn` produces no duplicates;
    - time travel works during compaction.

  — _R10 T24–28_
- **BENCH-186** `P1` **Online-aggregation WordCount whose estimates converge.**
  - *Setup:* 64 blocks × 10,000 Zipf(1.1) words over a 5,000-word vocabulary (640,000 words in total), 64 maps,
    8 reducers, snapshots every 0.1 up to 0.9, and 20 fixed seeds for a random block order.
  - *Expected:*
    - The final output equals the single-node reference exactly.
    - Every snapshot satisfies `snap_p(w) ≤ final(w)`, and within one reducer incarnation `snap_p(w)` never decreases
      in p.
    - The job-progress estimate of the grand total is within 0.5% of 640,000 at every p.
    - Over the top-100 words, the mean relative error of `snap/progress`, averaged across seeds, is non-increasing in
      p, and 0 at the final output.
    - The 95% CLT intervals cover the truth at least 90% of the time, over 20 seeds × 100 words × 9 points.
    - Killing map attempt m7 after its 2nd spill changes none of the above, in both snapshot modes.
    - Negative control: with identity dedup disabled, the simulator finds a run with `final(w) > reference(w)`.
    - `count` snapshots are labeled L, `count ≥ 1000` outputs are T, and `avg` outputs are A.
  - *Part (b), sample fraction vs. job progress:*
    - *Setup:* 24 hours × 4 identical blocks per hour, processed in hour-major file order, with fraction-of-hour
      weights.
    - *Expected:* the coverage-scaled estimate equals the final value **exactly** for every group whose hour has
      coverage > 0. At p = 0.25, the job-progress estimate is exactly 4× the final value for groups whose hour is
      fully seen, and 0 for groups whose hour is unseen. Its mean relative error is ≥ 0.5.
    - This reproduces HOP's result: job-progress error up to about 0.7, sample-fraction error about 0.03.

  — _R14/G4 §14 T1–T2; HOP NSDI §4.3.1_
- **BENCH-187** `P1` **Pipelined two-job chain, WordCount → Top-100, stays exact under failures.**
  - *Setup:* j1's reducers emit a local top-100. j2's single reducer merges the lists, breaking ties by word
    ascending. The input is BENCH-186's corpus, and snapshots are pipelined every 0.1.
  - *Fault schedules:*
    - (a) kill a j1 map attempt after 2 of its 5 spills;
    - (b) (a) plus a speculative attempt that runs ahead;
    - (c) kill a j2 map after it has consumed snapshot 0.3;
    - (d) kill a j1 reducer after it has published 0.5;
    - (e) (c) and (d) together.
  - *Expected:*
    - In every schedule, j2's final output equals the blocking two-job pipeline row for row.
    - The sum of j1's final counts is 640,000.
    - The progress values j2 applies are strictly increasing for each upstream partition.
    - In (c), the first snapshot applied after the restart is at ≥ 0.3.
    - In (e), the 0.5 snapshot is recovered from the durable store first.
    - With dedup off, (b) diverges from the reference.
    - Molly-2 finds no counterexample to "final = batch" with dedup on, within the bound of one crash plus
      omissions, and does find one with dedup off.

  — _R14/G4 §14 T3; HOP NSDI §4.2–4.3, TR09 §4.3_
- **BENCH-188** `P1` **Continuous windowed jobs.**
  - *Monitoring scenario (HOP §5.3):*
    - *Setup:* 7 hosts sampling every 100 ms of virtual time. Host h3 steps from a load of 10 to 60 at t = 10 s.
      Each host's 20 s average is compared with the 120 s mean and standard deviation of the other hosts, at 2σ,
      with an alert after 10 tentative alerts. Triggers run every 100 ms.
    - *Expected:* exactly one alert, for h3, at the tick computed by the single-node reference evaluator on the same
      event-time trace. Under processing-time triggers the output is labeled nondet, and only the host set {h3} is
      compared.
  - *Streaming WordCount:*
    - *Setup:* tumbling 10 s windows and sliding 30 s/10 s windows. A mapper is killed mid-window and restarted from
      its ring buffer.
    - *Expected:* every window equals the batch count over the same events. Map-side retention stays within at most
      2 windows' worth of unacknowledged spills. No spill is reclaimed before every consumer, including a speculative
      one, has acked it.

  — _R14/G4 §14 T4; HOP NSDI §5.1–5.3_
- **BENCH-189** `P1` **Pipelining vs. blocking (ratios only).**
  - *Setup:* WordCount over 10 GB of uniform random words, 20 maps, 10 workers with 2 map + 2 reduce slots each,
    and R ∈ {1, 5, 20} reducers. Also 100 GB, 240 maps and 60 reducers, on 20 workers with 4 map + 3 reduce slots
    each.
  - *Expected:*
    - Pipelined ≤ 0.85 × blocking for R = 5 and R = 20 (HOP: 0.823 and 0.803).
    - Adaptive ≤ 1.05 × blocking for R = 1 (HOP: about 1.17; the reverse-pressure rule must fix this).
    - Pipelined ≤ 0.80 × blocking at 100 GB (HOP: 0.75).
    - Output is byte-identical in every mode.

  — _R14/G4 §8, §14 T5; HOP NSDI §6_
- **BENCH-190** `P1` **Alert-based speculation.**
  - *Setup:* WordCount with 20 maps on 20 workers. m5 stalls after 60 s of virtual time by sleeping 1 s per record.
  - *Expected:*
    - Exactly one backup is launched, for m5, and none for any other task.
    - The backup launches in ≤ 0.5 × the time that Hadoop's default policy (FLAG-101) takes on the same trace. The
      Condie thesis reports "half the time".
    - The output is exact.

  — _R14/G4 §7.3, §14 T8_
- **BENCH-191** `P1` **Adaptive send-policy unit tests.**
  - *Scenarios:*
    - (a) no combiner and no stalls: one file per spill;
    - (b) combiner reduction 0.25: spills are sent in merged groups of 4;
    - (c) stall 0.6: sending is held until stall < 0.5, then the backlog goes out as one merged file per
      destination, starting at that destination's cursor;
    - (d) closing with an empty buffer: exactly one eof sentinel, with progress 1.0;
    - (e) HOP's straddled merge ranges across attempts: no hang, no loss, no duplication.
  - *Expected:* reducer output is identical to blocking mode in every scenario.

  — _R14/G4 §3.3, §5.3, §14 T6_
- **BENCH-192** `P1` **Online and continuous scheduling rules.**
  - *Setup:* 6 reduce slots.
  - *Expected:*
    - An ONLINE job with 8 reduces is held by admission control.
    - An ONLINE job with 6 reduces schedules no maps until all 6 reduces are RUNNING. `count … default 0` makes h2
      fire.
    - A concurrent batch job schedules maps immediately.
    - In a job chain, the upstream job gets slot preference.
    - Every trace matches its golden file.

  — _R14/G4 §4, §14 T7; Condie thesis Fig. 10.10_

### 11.8 Performance targets
- **BENCH-200** `P0` **Batch Datalog suite.**
  - Programs: TC, SG, Reach, CC, SSSP, Andersen, CSPA, CSDA, Galen, Bipartite, CRDT, Polonius, DOOP and DDISASM.
  - Datasets: G5K–G80K, RMAT, livejournal, orkut, twitter, and httpd/linux/postgresql.
  - Targets: at least as fast as compiled Soufflé at 4 threads, with at most 2× Soufflé's memory. The reference numbers are FlowLog VLDB'26 Table 1 and Soufflé CC'16 (TC on a random graph with 1k vertices and 10k edges: 0.38 s with the trie, sequential).

  — _R09 §13, T14_
- **BENCH-201** `P1` **OpenJDK7 context-insensitive points-to.** Soufflé takes 35 s in parallel; we must at least match it. — _R09 T15_
- **BENCH-202** `P0` **Protocol throughput.** Measured on machines of the n2-standard-4 class:
  - Base protocols: Voting ≥100k, 2PC with disk flush ≥30k, and Paxos (2 proposers, 3 acceptors, 3 replicas) ≥50k commands/s.
  - Rewritten protocols: ≥250k, ≥160k and ≥150k respectively.
  - CompPaxos: about 160k.

  — _R09 §13.3; R08 §9.3_
- **BENCH-203** `P1` **Overhead.** Tier-B provenance costs about 1.3× time and 1.8× memory at most. The interpreter is within 1.5–3× of compiled code. — _R09 §4.7, §14.4_
- **BENCH-204** `P1` **Blazes sanity check.** Sealing beats ordering, by about 1.8× at 5 workers and 3× at 20. — _R05 T13_
- **BENCH-205** `P2` **Data plane.** Sort and shuffle compared with smallpond (GraySort 3.66 TiB/min) and Exoshuffle (CloudSort $0.97/TB) at matched scale. — _R10 B2_

### 11.9 Production hardening: upgrades and security
Unless marked *live*, each scenario runs in the deterministic simulator (TEST-001, TEST-100).
- **BENCH-220** `P1` **Rolling upgrade of a 3-node Raft KV loses no committed entry.** The cluster goes from v6 to v7. v7 adds a defaulted column to the KV table, a `prevote` field and a gated `Cas` command. The run has continuous client load, random omissions and one crash, and explores ≥ 1000 seeds. The upgrade goes followers first, then the leader after TimeoutNow, then automatic finalization. Expected:
  - no committed entry is lost or altered;
  - every replica decodes and applies every committed entry;
  - `Cas` fails with "feature not active" before activation and succeeds after;
  - `activate_version(7)` is applied at the same log index on every replica;
  - the client history is linearizable (TEST-008);
  - client outputs equal those of a v6-only reference run, apart from `Cas` responses (TEST-102).

  — _R15/G5 §6.9, §9_
- **BENCH-221** `P1` **Rollback and downgrade.**
  - Before finalization, one node restarted on v6 rejoins with no error.
  - After finalization, a v6 binary on v7 storage refuses to start. Its error names the relation, the storage version and the supported range. No data file is modified.

  — _R15/G5 §6.7, §9; etcd; CockroachDB finalization_
- **BENCH-222** `P0` **A durable table gains a column with a migration rule.** v1 has `kv(key, val) key(key)`. v2 has `kv(key, val, ver: u64 = 0)` and `migrate from 1 { kv(K,V,0) <= old.kv(K,V); }`. Expected:
  - after restart every row has `ver = 0`;
  - the checkpoint's schema hash changes only after the migration commits;
  - a crash at every migration write gives the same final state (TEST-103);
  - without the block, the change is accepted as an auto-migration;
  - making `ver` a key column without a migration is rejected as breaking (ANA-100).

  — _R15/G5 §6.4, §9_
- **BENCH-223** `P1` **A non-monotone migration waits for finalization.** A v2 migration splits `log` into `live` and `tomb` using `notin`. ANA-103 classifies it non-monotone. The orchestrator runs it only as a finalization barrier, and applying it earlier in a mixed-version run is rejected. — _R15/G5 §6.4.2_
- **BENCH-224** `P0` **Corpus for the compatibility checker.** Expected verdicts:
  1. a required field added without a default (the HBASE-25238 analogue) → error;
  2. a field number reused → error;
  3. a variant added to a type without `unknown` → error for R-old;
  4. a variant inserted mid-enum with stable numbers (the HDFS-15624 analogue) → OK;
  5. a key column added → breaking;
  6. `lmax<u32>` → `lset<u32>` without a migration → breaking;
  7. `lmax<u32>` → `lmax<u64>` with a declared morphism → coordination-free;
  8. a format change without a version bump (the KAFKA-10173 analogue) → CI error;
  9. an ungated send of a `since 7` channel → ANA-102 error;
  10. a cycle in the role order → reported, with a gating suggestion.

  — _R15/G5 §6.3, §9; SOSP'21_
- **BENCH-225** `P1` **Semantic incompatibility across versions (Hydro's `sim_multi_version_gossip`).** v1 routes to the last sorted member and v2 to the first, with one member per version. Exhaustive search must find the read-after-write violation (`[Response { value: 0 }]`). With identical routing (`sim_multi_version_extra_unrelated_location`) no violation is found. — _R15/G5 §5.6; Hydro `hydro_test/src/distributed/versioning.rs`_
- **BENCH-226** `P1` **A write before finalization is caught (the CASSANDRA-15794 analogue).** Without an override, ANA-102 rejects the ungated write. With `unsafe_ungated`, v2 writes before finalization and then the node rolls back to v1. v1 must refuse to start with "unknown relation schema hash in WAL segment k". It must not crash and must not skip the record silently. — _R15/G5 §6.7; SOSP'21 §6.1.4_
- **BENCH-227** `P0` **An unauthenticated peer's `<~` is rejected** (*live and simulated*). A process without a valid certificate connects to a 3-node Raft KV and sends `append_entries(term = 999)`. Expected:
  - the handshake fails;
  - `net_rejected_total{reason="handshake"}` and `tls_handshake_failures_total` increase;
  - no tick runs on the frame, the term is unchanged and the leader is undisturbed;
  - an audit record is written;
  - the simulated variant's trace shows `REJECTED(handshake)`.

  — _R15/G5 §4.4, §9_
- **BENCH-228** `P0` **An authenticated but unauthorized sender is rejected.** A valid *client* certificate sends `append_entries`, and a peer of another program sends to the peer listener. Expected:
  - the rejection reasons are `acl` and `unknown_principal`, with metrics labeled by channel and `peer_role`;
  - the Raft invariants (BENCH-170) hold;
  - `whynot` on the non-delivery names the ACL.

  — _R15/G5 §4.6, §4.10_
- **BENCH-229** `P0` **Identity spoofing is detected.**
  - A node that claims another's `node_id` is rejected with `principal_node_mismatch`.
  - A Raft variant that trusts a payload `leader_id`, and a vote counter that counts a payload `voter`, trigger ANA-106. In simulation the payload-`voter` version lets one node vote several times, while the `from S` version does not.

  — _R15/G5 §4.5, §7(5)_
- **BENCH-230** `P1` **Certificate rotation and expiry** (*live*; the expiry part is also simulated in virtual time).
  - Rotation under load, without restarts, gives only reconnects, and throughput recovers within 2 s [ours].
  - When n3's certificate expires, n3 is isolated: `cert_expiry_seconds` goes below 0 and `reason="expired"` rejections grow. The majority keeps committing.
  - After renewal n3 rejoins and catches up.

  — _R15/G5 §4.3, §4.10_
- **BENCH-231** `P1` **Rule-level authorization.** In a KV with per-principal prefixes, alice writing `bob/x` gets `denied`, `authz_denied_total` increments and nothing changes. `alice/x` succeeds. — _R15/G5 §4.7_
- **BENCH-232** `P1` **LDFI verdicts are unchanged by ACL rejections.** For `2pc`, `rdlog` and `ack_rb` (BENCH-130/131), enabling ACL-rejection faults gives the same verdicts and minimal falsifiers as omission-only LDFI. — _R15/G5 §4.9–4.10_
- **BENCH-233** `P1` **The version window is enforced.** A v8 node joining a cluster at cluster version 6 is rejected with `version_unsupported`, and so is a v5 node after v6 is finalized. — _R15/G5 §6.6_
- **BENCH-234** `P1` **Sweep of upgrade scenarios.** The six TEST-101 scenarios pass for every consecutive version pair of the flagship Raft KV, Multi-Paxos and BOOM-FS test programs on 3 nodes, and the TEST-106 fixtures load. — _R15/G5 §6.11; SOSP'21 Findings 9–12_
- **BENCH-235** `P1` **Cost of security.** BENCH-202's protocols with mTLS lose at most 15% throughput against plaintext at equal batch sizes [ours]. For comparison, SecureBlox's per-message RSA signatures raised fixpoint latency from about 15 s to 25 s on 36 nodes. — _R15/G5 §2.4, §9; SecureBlox SIGMOD'10 §8_

### 11.10 Lattice semantics and lattice provenance corpus (R11/G1)

All runs use synchronous mode with tick 0 as bootstrap. LDFI parameters are written EOT/EFF/Crashes. Every value was
checked with the R11/G1 reference simulator. Items marked (clingo) were also checked with the ASP encoding.

- **BENCH-300** `P0` **Quorum vote over `lset` (T1).** k = 3 of 5 voters, at 4/2/0.
  - `decided` holds at c from tick 2.
  - There are exactly 10 minimal falsifiers: the 3-subsets of {O(vᵢ,c,1)}. (clingo finds 16 counterexample runs.)
  - With 1 crash among the voters there are 40 minimal falsifiers, each of size 3.
  - The first hypothesis round consists of exactly the 10 falsifiers.

  — _R11/G1 T1_
- **BENCH-301** `P0` **Max over a lossy channel (T2).** Senders hold 3, 7 and 7.
  - Post `best ≥ 7`: one minimal falsifier, {O(b,s,1), O(d,s,1)}.
  - Post `best ≥ 3`: one minimal falsifier, losing all three sends.
  - Retry variant: no CE at 4/3/0, and one falsifier of size 4 at 3/3/0.

  — _R11/G1 T2_
- **BENCH-302** `P0` **A lattice over `@async`, merged at the receiver (T3).**
  - In a synchronous run the batch merges to {x↦3, y↦5}.
  - With arrivals at ticks 2 and 3, `seen_both_eph` never holds and `seen_both` holds from tick 3.
  - The program is diffluent in `seen_both_eph`, which the analyzer must flag, and confluent in `store` and `seen_both`.
  - LDFI falsifiers are {O(a,r,1)} and {O(b,r,1)}.

  — _R11/G1 T3_
- **BENCH-303** `P0` **Merge at the sender (T4).** Two same-key derivations in one tick produce exactly one message, {k↦{1,2}}. `both_eph` is ultimately true under every schedule. The falsifier is {O(a,r,1)}. — _R11/G1 T4_
- **BENCH-304** `P0` **⊥-normalization (T5).** There is no "a" cell, `has_a` is false, `cnt` = 0, and exactly one message is sent. — _R11/G1 T5_
- **BENCH-305** `P0` **Antitone read across strata (T6).** `small` holds at ticks 0–2 and not at ticks 3–4. The cyclic variant is rejected, with the cycle reported as the witness. — _R11/G1 T6_
- **BENCH-306** `P0` **Convergence within a tick, and the hard error (T7).** The all-pairs `lmin` values match R11/G1 T7. The negative-cycle variant raises the iteration-bound error. — _R11/G1 T7_
- **BENCH-307** `P1` **A diverging ultimate value (T8).** c = t at tick t, `ge5` holds from tick 5, the ultimate value is non-compact, and the program is never quiescent. — _R11/G1 T8_
- **BENCH-308** `P0` **Lattice marriage ceremony (T9).** There are exactly two ultimate models. (clingo: the 9 schedules split 3 + 6.) `exactly_one` is diffluent; `at_least_one` and `both` are certified. — _R11/G1 T9_
- **BENCH-309** `P1` **Vector clocks (T10).**
  - b holds {a↦1} at tick 2 and {a↦1, b↦1} at tick 3.
  - hb(a@1, b@3) holds and hb(a@1, b@2) does not.
  - T10b, which increments deductively in the same tick, is a hard error.

  — _R11/G1 T10_
- **BENCH-310** `P1` **LWW register as `Lex` (T11).** It converges to ((1,r2),"y") under every delivery order and is confluent but not certified. The falsifiers are {O(r1,r3,1), O(r2,r3,1)} for `seen_v11` and {O(r2,r3,1)} for `val_is_y`. — _R11/G1 T11_
- **BENCH-311** `P1` **Company control (T12).** The simulator and clingo agree on all three inputs:
  - the FLP input yields no controls;
  - the Ross–Sagiv §5.6 input yields {c(b,c), c(c,b)};
  - the chain input yields {c(a,b), c(a,c)}.

  — _R11/G1 T12; FLP Ex. 2.13; Ross–Sagiv §5.6_
- **BENCH-312** `P1` **Key presence in a replicated `lmap` (T13), plus the differential test.**
  - At 4/3/0 there is one falsifier; at 4/4/0 there are the four listed falsifiers.
  - For every corpus program, bounded ASP model enumeration and exhaustive simulator schedules must produce the same ultimate models. This is the SEM-108 guard.

  — _R11/G1 T13, §4.1_
- **BENCH-313** `P0` **FLP-vs-GZ oracle (T14).** `p(a). p(b) :- #count{X:p(X)} > 0.` and its Dedalus^L form both yield {a, b}. The verifier backend must agree. — _R11/G1 T14; GZ Ex. 8_

---

## 12. Deliberately not adopted

Everything below appears in the sources and is rejected on purpose. The design phase should not reintroduce any of it by accident.

- **P2's one-external-event-per-fixpoint scheduling, and "at most one event per rule".** Replaced by batch ticks plus library serializers (CR-02).
- **Bags for events, and asymmetric self-sends.** See CR-03.
- **Lazy TTL expiry on access.** See CR-17.
- **A lattice whose merge is Z-set (or any group) addition.** It is impossible: an idempotent or inflationary group
  is trivial, and a ring with an idempotent × is Boolean. Signed diffs travel only through exactly-once wrappers
  (CR-35; R13 §3.1).
- **Treating a threshold on an encoded non-inflationary value, such as a PN-counter's `value`, as monotone or
  final.** Only thresholds in the state's natural order qualify (LANG-125 amendment; FT App. B).
- **Inferring completion from quiescence or silence at runtime.** See CR-36.
- **Pipelined semi-naive evaluation with deletions.** It is unsound, duplicates derivations, and can diverge (R01 §5.4; ENG-062).
- **Ambient `f_now()`, `currentTimeMillis()` or `rand` inside rules.** See CR-18.
- **Bud's dynamic typing, nil padding and arbitrary Ruby blocks inside rules.** See CR-28 and LANG-002.
- **The `=` statement from CIDR'11**, which Bud itself removed (R03 §2.3).
- **Leftovers from P2 and Evita.**
  - Remote functions (`f_now@Y()`).
  - SecLog (`says`) and the wireless broadcast rewrite. SecLog is **replaced** by:
    - mTLS with identity bound by the runtime (DIST-060–062);
    - the transport-authenticated `sender`/`principal` columns, which are SeNDlog's import predicates (LANG-240, LANG-241);
    - inferred and explicit channel ACLs (LANG-242);
    - rule-level policies (LANG-244);
    - opt-in `signed<T>` for statements relayed end to end (LANG-245) (R15/G5 §4.13).
  - The Click-style element runtime.
  - Optimizer stages written in Overlog. Our optimizer is native; metaprogramming stays as LANG-203.
- **Tokyo Cabinet and ZooKeeper `store` backends** (R03 §2.2).
- **Molly's crash semantics.** In Molly a crashed node keeps computing, and `crash` is visible to protocol rules (CR-20).
- **Naive CNF distribution, and "enumerate every model, then filter for minimal"** (R06 §3.11).
- **DFIR's rejection of cycles inside a tick** (CR-11).
- **`DomPair` as a safe lattice, and Anna's `>=` LWW merge**, which is not commutative on ties (CR-25; R04 §4.3).
- **Treating a Blazes `OR*` as compatible with any seal** (CR-27).
- **Hydro's "no new language" non-goal.** Our primary surface is the language itself (LANG-002).
- **Leftovers from HOP** (R14/G4).
  - Eager per-record pipelining, the "naive design". It defeats combiners and moves sorting to the reducer.
  - Discovering snapshots by polling an HDFS directory for a file count. Replaced by FLAG-135's completeness
    threshold.
  - Reducer state that crosses windows kept in `static` host objects stamped with the reducer's wall clock.
    Replaced by declared state and event time (FLAG-137).
  - Garbage-collecting stream spills while assuming no speculation (CR-32).
  - Ignoring failure events and trusting any attempt's output when the map is not certified deterministic (CR-32).
  - Treating the job-progress scale-up as an accuracy estimate. It is always labeled biased (LANG-113).
- **Leftovers from the security and upgrade literature** (R15/G5).
  - LBTrust's shipping of rules between principals (`active(R) <- says(_,me,R)`). It is remote code execution by design. Rule install is admin-only (DIST-066).
  - SecureBlox's rollback of a whole batch transaction on a constraint violation (CR-40).
  - Per-tuple RSA/HMAC signatures as the default authentication (ODD-34).
  - Peer identity that the peer declares itself (Timely's plaintext u64 worker index) or that comes from deployment wiring alone (Hydro Deploy) (DIST-061).
  - Plaintext transports by default (ODD-30).
  - Crashing on an unknown field, variant or schema; encoding enums by index; adding required fields without defaults (DIST-087, LANG-261; SOSP'21).
  - Writing a new storage format before finalization (DIST-086; CASSANDRA-15794).

- **Leftovers from the aggregation-semantics literature** (R11/G1).
  - Ross–Sagiv's rule that two heads with the same key and different cost values are an inconsistency. Lattice columns merge instead (CR-51).
  - Transfinite or limit semantics for recursion within a tick (Ross–Sagiv §6.2), and Datafun's clamping `fix x ≤ e` (CR-53).
  - Gelfond–Zhang (Alog) aggregate semantics in the verifiers (CR-54).
  - Flix's requirement that transfer functions be strict. ⊥-normalization makes it unnecessary (SEM-101).

---

## 13. OPEN DESIGN DECISIONS

These are choices the research leaves open. Each one lists the options and a **recommended default**. The design phase adopts the default unless someone overrides it. Decisions marked **⚑** change the user-visible product in a material way. Put those to the user with the question tool before the design is frozen.

**ODD-01 ⚑ Surface syntax.**
- *Options:*
  - (a) Dedalus/Datalog rule syntax (`head :- body`, `@next`, `@async`);
  - (b) Bloom collection-expression syntax (`lhs <= rhs.pairs(...)`);
  - (c) a hybrid: Datalog atoms and bodies with Bloom's merge operators as rule arrows (`<=`, `<+`, `<-`, `<+-`, `<~`), named-field atoms, and typed `state` declarations.
- *Recommendation:* **(c).** Bodies stay fully analyzable with no hidden closures, while programmers still get Bloom's operator vocabulary and readable joins. Dedalus/Molly, Overlog and Bloom are available as frontends (LANG-220–223).

**ODD-02 Same-tick key conflicts.**
- *Options:*
  - (a) a hard error, as in Bud;
  - (b) nondeterministic replacement, as in Overlog;
  - (c) a per-relation declared resolution (lattice column, `choose`, min or max).
- *Recommendation:* **(a) by default, with (c) as an explicit opt-in.** Deterministic behavior comes first (CR-07).
  Option (c) is specified as LANG-117 (R12/G2).

**ODD-03 Aggregates over empty groups.**
- *Options:* (a) produce no row, as in GROUP BY; (b) produce a default row.
- *Recommendation:* **(a), plus the explicit `default` form** (LANG-106, CR-08).

**ODD-04 What triggers a tick.**
- *Options:*
  - (a) purely event-driven, lazy ticks, as in Bud and DFIR;
  - (b) a fixed heartbeat rate;
  - (c) lazy ticks plus an optional per-node heartbeat timer.
- *Recommendation:* **(c).** Retry idioms use explicit timers, as Bloom's ReliableDelivery does. The equivalence of idle periods to empty ticks is documented (SEM-009).

**ODD-05 Resending async messages derived from persistent state.**
- *Options:*
  - (a) the literal Dedalus semantics: re-send every tick in which the body holds;
  - (b) send once per new derivation;
  - (c) literal semantics, with sender-side dedup only where ARM proves the receiver idempotent.
- *Recommendation:* **(c).** Molly's retry protocols (`rdlog`, `ack_rb`) depend on the literal semantics (DIST-007).

**ODD-06 Architecture for incremental evaluation.**
- *Options:*
  - (a) Soufflé-style semi-naive evaluation over stamped tables everywhere, with recompute for non-monotone strata;
  - (b) Differential-dataflow-style (data, time, diff) everywhere;
  - (c) a hybrid: stamped semi-naive for monotone and lattice strata, DBSP signed diffs for non-monotone strata, and elastic fallback to recompute. Recursive views with deletion use recursive counting or FBF.
- *Recommendation:* **(c)** (ENG-060–065). Z-sets remain internal except in Tide (LANG-138). The final choice between FBF and DBSP nested time is settled by BENCH-037.
- *Amendment from R13/G3.* Keep (c), and add a hard boundary rule (CR-35).
  - Z-set diffs never cross an async edge raw. They go through a wrapped channel (LANG-158, DIST-015..017). The
    wrapper is a lattice stratum, and `unwrap` (ENG-070) feeds its Δ into the DBSP stratum.
  - Edges from Z-set strata back into monotone or lattice strata are negative edges.
  - Replicated IVM converges (SEM-036) but is never final without seals (SEM-017). Z-sets are therefore not
    "internal only" anymore: LIB-093 exposes replicated Z-set collections, and LANG-138 is P1.

**ODD-07 Execution backend and its order of delivery.**
- *Options:* (a) interpreter only; (b) codegen only (proc-macro or build script); (c) a shared kernel library, delivering the interpreter first and codegen second.
- *Recommendation:* **(c).** The simulator, LDFI and the REPL need the interpreter. Codegen is how we reach the throughput targets.

**ODD-08 Default provenance tier.**
- *Options:* (a) always Tier B; (b) off in production, Tier C sliced under simulation and LDFI, Tier B available per deployment.
- *Recommendation:* **(b).**

**ODD-09 How users define lattices.**
- *Options:*
  - (a) a Rust trait implementation, unverified;
  - (b) only a restricted DSL of verified constructors;
  - (c) both. Trait implementations must pass the law harness (TEST-083), and the result is labeled "tested", not "proven".
- *Recommendation:* **(c).**

**ODD-10 ⚑ How strictly CALM is enforced.**
- *Options:*
  - (a) an advisory report after the fact, as in Bud and Blazes;
  - (b) a type-and-effect system in which every unannotated nondeterminism is an error, as in Hydro;
  - (c) inferred stream properties plus warnings, with errors under `--strict`, and with the standard library required to build under `--strict`.
- *Recommendation:* **(c).**

**ODD-11 Rule bodies that span locations.**
- *Options:* (a) forbid them entirely; (b) accept them as sugar with explicit localization and a lint.
- *Recommendation:* **(b)**, as a P1 feature. The IR stays single-location (CR-15).

**ODD-12 Scope of compatibility frontends.**
- *Options:* each of Molly `.ded`, Overlog, Hydroflow `datalog!` and Bloom-expression syntax can be built or skipped.
- *Recommendation:* **Molly `.ded` at P1**, because the LDFI oracles need it. The rest at **P2**.

**ODD-13 Storage and durability engine.**
- *Options:*
  - (a) our own WAL, in-memory tables and copy-on-write checkpoints;
  - (b) an embedded KV store (RocksDB, redb, …);
  - (c) our own LSM.
- *Recommendation:* **(a), behind a pluggable storage trait.** Tick-level group commit and birth/death intervals do not map well onto a generic KV store. Whichever backend is chosen, WAL segments and checkpoints carry schema headers (DIST-081), and the storage format changes only at finalization (DIST-086).

**ODD-14 Default transport and serialization.**
- *Options:* TCP, QUIC or UDP; serde-bincode/postcard, Cap'n Proto, or a custom format.
- *Recommendation:* TCP by default, QUIC optional. A schema-hashed, serde-based compact binary format. The language-level semantics stays unordered and lossy regardless (DIST-001). Both transports run inside mutual TLS 1.3 (DIST-060, ODD-30). The format is keyed by stable field numbers (LANG-261) and carries per-frame schema ids (DIST-080). Plain serde-bincode is positional, so it cannot evolve schemas by itself.

**ODD-15 ⚑ How Raft is formulated.**
- *Options:*
  - (a) pure rules, with per-tick serialization rules (term first, at most one vote per tick);
  - (b) a sanctioned `fold_ordered` step function inside the program, like Hydro's `raft_step`;
  - (c) both.
- *Recommendation:* **(c).** (a) is the flagship: it is the point of the project. (b) is a reference implementation for differential testing. Both must pass BENCH-170–175.

**ODD-16 Physical timers under LDFI and simulation.**
- *Options:*
  - (a) modules checked with LDFI must use logical timers only;
  - (b) physical timers are mapped to rounds automatically, using a configured duration per round;
  - (c) (b) by default, with a per-module override.
- *Recommendation:* **(c).**

**ODD-17 Entanglement.**
- *Options:* (a) unsupported; (b) supported, gated behind analyzer warnings, and excluded from the confluence classes and from VER-006.
- *Recommendation:* **(b), at P2.** It is needed for the Lamport `p_wait` idiom and for tick-valued time travel.

**ODD-18 Solver stack for verification, and build order.**
- *Solver options:* for LDFI, an incremental SAT solver (CaDiCaL/Kissat via FFI) or Z3 pseudo-Boolean; Z3 or CVC5 for VER-006–010; clingo for VER-003.
- *Build-order options:* V0 → V2 → V3 → V4 → V5.
- *Recommendation:* incremental SAT for LDFI, Z3 for FOL, clingo for ASP. Build in the order analyses → simulator → LDFI → bounded model checking → EPR proofs.

**ODD-19 ⚑ Which modern Hadoop successor.**
- *Options:*
  - (a) C1 "BOOM-2": lineage-recoverable batch analytics plus FS2;
  - (b) C2 "Tide": incremental, streaming-first dataflow;
  - (c) C3 "Lattice Lakehouse";
  - (d) Tide on the Lakehouse, delivered through BOOM-2 milestones M1 to M4.
- *Recommendation:* **(d)**, following R10 B4. This decides a large share of the project's scope.
- *Amendment from R14/G4 (HOP).* Keep (d), but split M1 and deliver the BOOM group's own successor to Hadoop before
  Tide:
  - **M1a, "BOOM parity"** (unchanged): FS2, the BOOM-MR scheduler (FCFS and LATE), blocking MapReduce, and Raft for
    metadata.
  - **M1b, "HOP parity"**, tested by BENCH-186..192:
    - pipelined shuffle with exactly-once spills (FLAG-105..107);
    - tentative output and prefix seals (FLAG-108/109);
    - speculation under pipelining (FLAG-110);
    - scheduling rules h1–h3 and job chains (FLAG-111);
    - online-aggregation snapshots with classes and estimators (FLAG-135);
    - snapshot pipelining between jobs (FLAG-136);
    - HOP-mode continuous jobs (FLAG-137);
    - reflective monitoring with alert-based speculation (FLAG-112).

  After M1b:
  - Shuffle by seals (FLAG-122) is pipelined by default, and blocking becomes a send policy (ODD-24).
  - M3 Tide **generalizes** M1b:
    - a snapshot becomes a progress trigger in accumulating mode;
    - HOP's "downstream recomputes from scratch" becomes accumulating-and-retracting mode;
    - processing-time windows become event-time windows with watermark seals, and processing time survives only as
      a labeled nondet mode;
    - HOP's reducer checkpoints become ABS;
    - ring-buffer GC becomes retention of the durable input log by consumer frontier.
  - M4's first verification target moves into M1b: "a published snapshot's progress never exceeds the true committed
    fraction", checked by LDFI and bounded model checking. The per-reducer progress lattice is the simplest instance
    of M4's frontier.

  M1b is the cheapest milestone that shows all three CALM output classes in one system: final-early thresholds,
  lower-bound snapshots, and retractable estimates.

**ODD-20 SQL frontend.**
- *Options:* (a) none; (b) a subset compiled to the IR, for the lakehouse, Nexmark and TPC workloads.
- *Recommendation:* **(b), at P2**, and only after M2 (FLAG-134).

**ODD-21 Membership model.**
- *Options:* (a) static only; (b) dynamic from the start; (c) static in the core, with dynamic membership as an epoch-based library on Raft.
- *Recommendation:* **(c).**

**ODD-22 How the system is packaged.**
- *Options:* (a) a standalone compiler and runtime binary; (b) a Rust embedding crate; (c) both.
- *Recommendation:* **(c).** Host embedding (LANG-185) is needed for the data paths in BOOM-FS and BOOM-MR.

**ODD-23 How the standard library's API is shaped.**
- *Options:* (a) mirror bud-sandbox's interface names; (b) redesign around lattices.
- *Recommendation:* **Keep the protocol names and interfaces from bud-sandbox**, so the tests in R03 port one-to-one, and write the implementations on lattices wherever that removes points of order.

**ODD-24 Default shuffle mode.**
- *Options:*
  - (a) blocking (Hadoop): materialize, then pull;
  - (b) always pipelined (HOP's eager design);
  - (c) HOP's adaptive policy: stall fraction plus combiner effectiveness;
  - (d) (c) plus the CALM class of the consuming reducer (ANA-036) plus a reverse-pressure fallback to the blocking
    layout.
- *Recommendation:* **(d)** (DIST-012).
  - HOP measured −17.7% to −25% completion time when reducers are under-utilized, but +17% with a single
    bottlenecked reducer. Its adaptive rule could not move work back to the mappers.
  - Class T/L consumers gain early final or bounded results from pipelining. Class A/H consumers with effective
    combiners gain from batching.
  - Every mode must give byte-identical output (BENCH-189, BENCH-191).

**ODD-26 Default exactly-once wrapper for group-valued channels.**
- *Options:*
  - (a) W2, dotted deltas with cumulative acks and resend (DIST-015);
  - (b) W3, cumulative per origin (DIST-016);
  - (c) W4, OnceTree (DIST-017);
  - (d) W1, a unique-ID set. This is only an oracle: its metadata grows with every update ever made.
- *Recommendation:* **(a) by default.** Its metadata is O(origins + exceptions), and bandwidth is proportional to
  the update. Use (b) for counter-like, small-support state that is gossiped over lossy or multi-hop topologies,
  since it needs no acks. Use (c) as a P2 opt-in for commutative-monoid aggregates read at very many replicas (the
  dissertation measured about 0.5 MB/node, flat, against a PN-counter's linear growth). Keep (d) as the
  differential-test oracle. The choice never changes results (SEM-036). — _R13 §6, §4.5_

**ODD-27 Deletion semantics for replicated collections.**
- *Options:*
  - (a) a Z-set with negative weights and clamped views, with no guard;
  - (b) (a) plus the client-side causal guard, which drops a delete when the observable count is ≤ 0;
  - (c) 2P-Set tombstones (delete wins forever, one insert at most);
  - (d) OR-Set (observed-remove), with a declared add-wins or remove-wins rule for concurrent operations.
- *Recommendation:* **(b) as the default for `zset` collections replicated with LIB-093**, with (c) and (d)
  selectable per collection.
  - Concurrent deletes can still reach −1, and this is documented and tested (BENCH-076).
  - Dolan proves that no undoable set avoids such states.
  - Only (c) gives any finality: an element that has been deleted is final-absent. — _R13 §3.6, §5; Wrapping
    Rings §5; FT App. B_

**ODD-30 ⚑ Default security posture.**
- *Options:*
  - (a) secure by default: production mode requires mTLS, and plaintext needs an explicit development flag that
    is reported in metrics;
  - (b) TLS off unless enabled, as in the research systems (Hydro Deploy, Timely).
- *Recommendation:* **(a)** (DIST-060). The user asked for a production-ready system. Every compared production
  system (etcd, CockroachDB, Flink) authenticates peers with certificates (R15/G5 §3).

**ODD-31 Principal format.**
- *Options:* (a) a SPIFFE URI SAN only; (b) the certificate CN only, as in etcd and CockroachDB; (c) SPIFFE, with
  the CN on an allow-list as a configurable fallback.
- *Recommendation:* **(c)** (DIST-061).

**ODD-32 ⚑ Upgrade mechanism.**
- *Options:*
  - (a) restart-based rolling upgrades with migrations;
  - (b) hot rule install only (JOL/Evita style);
  - (c) (a) at P1, with hot install (SEM-094) at P2 through the same compatibility checker.
- *Recommendation:* **(c).** Restart-based upgrades match what production systems do and what SOSP'21 studied.
  The tick boundary makes hot install semantically simple, but it needs indexes and delta state rebuilt in place.

**ODD-33 Default channel ACL.**
- *Options:* (a) open to any authenticated peer, as in CockroachDB and Flink internally; (b) default-deny, inferred
  from the choreography, with explicit ACLs to narrow or open.
- *Recommendation:* **(b)** (LANG-242, ANA-105). The compiler already knows which roles send on each channel.

**ODD-34 Per-tuple signatures (SeNDlog/SecureBlox `says`).**
- *Options:* (a) never; (b) the opt-in value type `signed<T>`, at P2; (c) a per-channel "signed" mode that signs
  every tuple.
- *Recommendation:* **(b)** (LANG-245). mTLS already authenticates each hop. SecureBlox measured per-tuple RSA
  raising fixpoint latency from about 15 s to 25 s on 36 nodes. End-to-end authenticity is needed only for relayed
  statements.

**ODD-38 Seed scope for `choose`.**
- *Options:*
  - (a) one choice seed shared by all nodes, derived from the deployment or run seed;
  - (b) a per-node choice seed.
- *Recommendation:* **(a)** (SEM-084).
  - Nodes that see the same candidates make the same choice, as in rendezvous hashing. That is useful for routing
    and for agreement without coordination.
  - A program that wants per-node variation puts `self` in the FD's left side.
  - `choose_rand` and `rand` are always per node.

**ODD-39 ⚑ Default persistence of `choose` over persistent inputs.**
- *Options:*
  - (a) per tick, with a seeded priority. The choice changes only when the group's minimum changes, and stickiness
    is opt-in (`choose sticky`).
  - (b) sticky by default: keep the previous choice while it is still a candidate.
  - (c) first-seen, as in Bud, Hydro and Soufflé.
- *Recommendation:* **(a)** (CR-45, LANG-115).
  - It is the Dedalus reading: the tick is in the determinant.
  - Its incremental form is a pure argmin, and it is confluent given the seed.
  - (b) would make outputs depend on the schedule by default.
  - (c) depends on iteration order and cannot be replayed exactly.

**ODD-50 ⚑ The bottom of numeric lattices.**
- *Options:* (a) an adjoined ⊥ of −∞ or +∞, as in Bud; (b) `T::MIN` or `Default`, as in Hydro's `Max<T>`.
- *Recommendation:* **(a)** (LANG-281). Under (b), `size(∅) = 0` equals ⊥, so a count of zero cannot be told apart
  from "no information" and is removed by ⊥-normalization (BENCH-304).

**ODD-51 Guarded asynchrony for ephemeral channels.**
- *Options:* (a) certify only async heads that persist, which is MAR's syntactic rule; (b) also certify ephemeral
  heads whose consumers are join-morphisms into persistent state (ANA-141 case (b)).
- *Recommendation:* **(b).** QuorumVoteL and most bud-sandbox programs read ephemeral channels into persistent
  lattices, and (a) would refuse to certify them.

**ODD-52 How precise provenance is for monotone non-morphisms stored in cells.**
- *Options:* (a) exact supports, which are exponential in the number of contributions; (b) all-contributors, which is
  sound but coarse; (c) exact up to a size bound, then (b).
- *Recommendation:* **(c)** (ENG-146, TEST-142).
