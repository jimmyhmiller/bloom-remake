# Decisions log

Normative decisions for bloom-remake. Later entries override earlier ones. `ODD-nn` refers to
`docs/research/FEATURES.md` §13; `CR-nn` refers to FEATURES.md §1 (all CR resolutions are adopted as written).

## User decisions (2026-09-26)

| Topic | Decision |
|---|---|
| Surface syntax (ODD-01) | **A new, modern syntax.** Not Bloom's collection-expression syntax and not raw Dedalus. It compiles to the Dedalus core IR. Dedalus/Molly `.ded` is also accepted as a compatibility frontend (P1) so the paper corpora run unchanged. |
| Execution backend (ODD-07) | **Interpreter + ahead-of-time Rust codegen** over one shared kernel/IR. Interpreter first. |
| Modern Hadoop successor (ODD-19) | **Lineage dataflow engine**: batch + streaming DAGs, hash-partitioned shuffle over channels, lineage/provenance-based recovery, watermarks as Blazes seals, on a replicated object store written in the language. Delivered via the ODD-19 (d) milestone path: M1a BOOM parity → M1b HOP parity → Tide (streaming) on the lakehouse. |
| Verification & dependencies (ODD-18) | **LDFI + bounded model checking + CALM/confluence certificates + SMT inductive invariants.** Well-established crates are fine for infrastructure (tokio, serde, rustls, SAT/SMT). The language, engine and analyses are ours. |
| Working mode | Fully autonomous; no questions. Commit per working milestone. Trait-based, clean, production-grade code. No silent stubs: unimplemented paths must fail loudly with a clear message. |

## Project decisions (made by Claude, 2026-09-27)

- **Name.** The language is **Blossom** (a modern Bloom). Source files use `.bls`. The CLI is `blossom`. Crates are
  prefixed `blossom-`.
- **Open design decisions.** Every other ODD in FEATURES.md §13 takes its recommended default, including the ⚑ ones
  (the user asked not to be consulted): ODD-02 (a)+(c), ODD-03 (a)+default, ODD-04 (c), ODD-05 (c), ODD-06 (c) with
  the R13 amendment, ODD-08 (b), ODD-09 (c), ODD-10 (c), ODD-11 (b), ODD-12 (Molly `.ded` P1, rest P2), ODD-13 (a)
  behind a storage trait, ODD-14 (TCP default, QUIC optional, schema-hashed field-numbered binary format),
  ODD-15 (c), ODD-16 (c), ODD-17 (b, P2), ODD-18 (incremental SAT for LDFI, Z3 for FOL, clingo optional for ASP),
  ODD-20 (b, P2), ODD-21 (c), ODD-22 (c), ODD-23 (bud-sandbox names, lattice implementations), ODD-24 (d),
  ODD-26 (a), ODD-27 (b), ODD-30 (a), ODD-31 (c), ODD-32 (c), ODD-33 (b), ODD-34 (b), ODD-38 (a), ODD-39 (a),
  ODD-50 (a), ODD-51 (b), ODD-52 (c).
- **SMT integration.** Z3 is driven over SMT-LIB2 through a solver-process trait (`z3 -in`), not native bindings.
  This keeps the verifier solver-agnostic (cvc5 works through the same trait) and keeps the build free of C++
  toolchain problems. Z3 4.x is installed at `/opt/homebrew/bin/z3`.
- **SAT for LDFI.** Behind a `SatSolver` trait. The default implementation is chosen in the architecture doc.
- **Priorities.** P0 first, then P1, then P2. Every feature in FEATURES.md is tracked in `docs/design/PLAN.md` with
  its milestone.

## Surface syntax (made by Claude, 2026-09-27)

- **The normative language reference is `docs/design/LANGUAGE.md`.** The example programs `examples/e01_kvs.bls` …
  `examples/e10_specs.bls` conform to it exactly and are parser, type-checker and end-to-end tests. The design record
  (four proposals, three judgments) is `docs/design/syntax/`.
- **Base: proposal B's reactive/choreographic skeleton, rebuilt on proposal A's typed foundations.** Programs are
  written as handlers (`on EVENT { … }` / `while CONDITION { … }`) whose statements start with a timing verb (`emit`,
  `next`, `send`, `delete`, `upsert`, `seal`), closed `view`s with inferred schemas, and multi-role modules with
  reopenable `at Role { }` sections. Declarations, types, generics, expressions and `#[attributes]` are Rust-shaped
  (A); every non-monotone operator carries a compiler-checked bang (A). Grafts from C (`where`, `bootstrap fresh`,
  input seals, relation parameters, `max!` vs `lmax{}`, `stable fn`, `prove … by induction`) and D (inferred views,
  `forall` over closed domains, `partition by`, keywords as field names, `expect confluent`). Rationale:
  LANGUAGE.md §1.1 and §22.
- **Refinements of FEATURES.md.** None changes a CR-xx; each is a spelling or a clarification (LANGUAGE.md §22.3):
  - LANG-190: `next` means t+1 everywhere, including in `bootstrap`; the tick-0 effect of a bootstrap `<+` is written
    `emit`. "Imported modules bootstrap first" means one boot-tick fixpoint ordered by stratification. `bootstrap`
    runs in every incarnation; `bootstrap fresh` only when no durable state was recovered, and durable relations may
    be written only there.
  - CR-14: a direction-form channel (`: Src -> Dst`) declares its location by the direction instead of a named `@`
    column; the column form keeps one `@` column. Both normalize to the IR's first column.
  - CR-16: `fact` targets only `static` relations; initial mutable state goes in `bootstrap`.
  - LANG-208: `#` comments are kept, except when `#` is followed by `[`, `![` or a digit (attributes, field numbers).
  - Seals default to unanimity over the declared producer set, with explicit per-producer reads; the digest is
    compiler-maintained.
  - ANA-041's `Rep` annotation is `#[replicated]`; every compiler-generated IR relation is provenance-transparent;
    `#[deterministic]` asserts an output's determinism class.
  - Surface spellings of LANG items (`not`, `let`, `count!(x)`, `any { … }`, `view` for `temp`, `timer` for
    `periodic`, `translate c to/from N`, `majority(s, R)`, `a<..=b` ring intervals) are listed in LANGUAGE.md §22.3.

## Delivery plan (made by Claude, 2026-09-27)

The build follows `docs/design/PLAN.md` (15 milestones, 94 work packages; machine-readable copy
`docs/design/plan.json`). Its milestone ids M1–M15 replace the milestone names of ARCHITECTURE.md §14.2 everywhere,
including corpus `until` fields. Delivery-level refinements of ARCHITECTURE.md (PLAN.md §4 gives the detail; none
changes what is built):

- D1: the interface freeze is staged: M1 publishes `blossom-base` and the `blossom-value` type surface, M4.8 the
  engine boundary; every other crate's surface is published by the WP that implements it, before its consumers.
- D2: `MonoClass` and the other operation-class enums live in `blossom-value::class` and are re-exported from
  `blossom-ir::core::lattice`; `blossom-ir` does not depend on `blossom-lattice`.
- D4: cross-crate tests live in a `tests/integration` crate (`blossom-integration-tests`).
- D5: each work package runs in its own git worktree off the milestone base and is merged at the milestone gate.
- D8: the corpus runs through `cargo xtask corpus` (no `blossom corpus`); the CLI adds `admin`, `completions`, `lsp`.
- D9: corpus manifests carry a status, `unimplemented` list and `until` per backend (refines ARCH-28); schema v1 is
  PLAN.md §5.
- D10–D15: lazy per-module std loading; `BLOSSOM_REQUIRE_SOLVERS` takes a solver list and tools install into
  `.tools/`; `SimFs` takes crash fates from its caller; `FileDurability` and the `MigrationRunner` trait live in
  `blossom-store`; the planner falls back to construct expansions when an executor lacks a native; lattice delta
  shipping is node-side.
- D18–D20: `InputRow`/`OutputRow` live in `blossom-engine::abi`; no project license is chosen yet (every package is
  `publish = false`); the Raft core is the library module `std::consensus::raft`.
- Priority inversions are placed as PLAN.md §6 lists; FLAG-028 (CompPaxos, P2) is pulled into M9.2 because
  BENCH-177 (P1) requires it.

## M1 gate (made by Claude, 2026-09-27)

The M1 gate merged M1.1–M1.5; every change it made is listed in `docs/plan/notes/M1-gate.md`.

- **xxhash-rust is allowed under BSL-1.0** (for that crate only, in `deny.toml`). ARCHITECTURE §1.2 names the crate
  for blossom-value's xxh3 fingerprints but its license allowlist omits the crate's license; the named crate wins,
  as §1.2 already does for MPL-2.0 (M1.1 deviation; `docs/design/DEPENDENCIES.md`).
- **The frozen `blossom-base` surface is what M1.1 built**, including its recorded deviations from ARCHITECTURE:
  `Symbol` is an interned text handle, not a `define_idx!` id (ARCHITECTURE §2.1 amended), `Diagnostic::primary` is
  an `Option<Span>`, `CodeInfo` carries `also` and `origin`, CLI subcommands take `run(args, &Context)`, and
  `blossom_std_host::registry()` returns a `Result`. The gate added `graph::shortest_cycle_through_node` and
  `SourceError::LineOutOfRange` before the freeze; from now on `blossom-base` changes follow ARCHITECTURE §1.6.
- **One `[expect_analysis]` vocabulary for the corpus** (`tests/corpus/README.md`, "Expectation vocabularies"),
  validated by `tests/corpus/tools/check_manifests.py` and, from M5.2, by `xtask corpus --lint`. Finality classes
  use ARCHITECTURE §7.2's names (`POS` … `NEVER`); `confluent` is ConfluenceStatus in CR-29's (Ameloot's) sense;
  `certificates` lists the Dedalus-family certificates (ANA-025/026/141/142) exactly; `points_of_order` is a table
  whose `complete` flag makes its lists exact; `calm_labels.paths` is not exhaustive. The M1.2 and M1.3 cases that
  used other shapes were converted without changing what they assert.
- **Diagnostic comparison.** The `compile` backend compares every frontend diagnostic, warnings included, with
  `[[expect_diag]]` exactly (PLAN §5.2's "no diagnostics expected when empty"); the `analysis` backend requires
  every listed diagnostic and makes the codes of the ANA features a case lists exhaustive; runtime backends compare
  no diagnostics and fail on any hard error not in `[[expect_error]]`.
- **Molly's unit tests are represented by their properties.** The corpus programs for Molly's `ProvenanceSuite` and
  `negative_support_test` had transcribed Molly's test inputs, which PLAN §8 (M1.5) forbids for an unlicensed
  repository; the gate replaced them with programs written for the corpus (BENCH-135f–m, BENCH-137i), checked with
  the reference checker.
- **Bug items in WP notes name their owner crate first** and are append-only once collected;
  `scripts/collect-notes.sh` refuses a notes file that breaks this instead of dropping its bugs
  (`docs/plan/notes/README.md`).

## Delivery by vertical slices (user decision, 2026-09-28)

After the M2 gate no Blossom program ran, and under the milestone plan none would until M5–M6. Asked through the
question tool, the user chose:

- **Deliver by vertical slices** instead of the remaining crate-by-crate milestones M3–M15. `docs/design/SLICES.md`
  is now normative for delivery order; LANGUAGE.md, ARCHITECTURE.md, FEATURES.md and the corpus stay the
  specification.
- **Slice 1 is Molly parity**: the `.ded` frontend, the oracle, a synchronous-round simulator, provenance and LDFI,
  gated on the Molly corpus verdicts (BENCH-130–134, BENCH-137).

## Slice 1 amendments (made by Claude, 2026-09-28; details in docs/plan/notes/S1.md)

- `xtask/layers.toml`: `blossom-front` → `blossom-artifact` (the `.ded` frontend produces a `DedArtifact`) and
  `blossom-sim` → `blossom-oracle` (until the engine exists the oracle is the simulator's evaluator).
- `blossom-ir` (additive): `ConstructKind::DedRelation`, Tier C `FiringRecord`/`PosRead`/`NegRead` in `obs`, and
  `AggFunc::Count` may target any integer column (Molly's `count<X>` is an `i64`).
- LDFI decides by the seeded lineage-driven search with tuple-level negative support, and hands over to exhaustive
  certification when it spends its run budget or its lineage was incomplete; the report names the deciding search.
- The S1 gate excludes BENCH-133d (Flux 22/21/1), which no search here or in the reference checker decides.

## Slice 2 decisions (made by Claude, 2026-09-29; details in docs/plan/notes/S2.md)

- **Blossom specs and CR-20.** LDFI judges `.bls` programs under CR-20 (a crashed node is frozen from its crash tick,
  ARCHITECTURE §8.1); Molly's view (crashed nodes keep receiving) stays the `.ded` profile. `examples/e10_specs.bls`
  ported Molly's `deliv_assert` verbatim, whose `missing_log` counts a crashed neighbor that never received the
  message; under CR-20 that makes `AckRbFaults` fail with `{C(B,2)}`. Its `DelivAssert` now excludes crashed
  neighbors (`not crashed(a)`), which gives Molly's verdicts (SimpleLog fails with `{O(A,B,1)}`, AckRb holds).
- **Frozen crashes in LDFI.** Firings get an `Alive` premise (a crash stops them), a crashed node's ticks are frozen
  copies (`Support::Frozen`), and tuple-level negative support adds *frozen appearance*: a tuple a node held before
  tick `c` stays if it crashes at `c`. Relation-level support under the frozen view is `Unimplemented`.
- **`round` is required** in a spec's `faults` exactly when the target observes time (physical timers or `now()`);
  ODD-16 names no default duration.
- **Deployments.** A single-location program's only role is `Node` (LANGUAGE §7.7), so a manifest may assign it.
  `.ded` programs without `--nodes` take their nodes from their location constants (tests/corpus/README.md).
- **Sim artifact generalization.** `blossom-artifact::ded` became `::sim` (`SimArtifact`, `LogicalRel`, …) with a
  `Profile` (Molly or Blossom), and `blossom-sim::ded` became `::spec` (`SpecSim`); LDFI runs both frontends'
  programs through them.
- **IR amendments (additive):** `IrBuilder::set_persistence` and `set_construct_kind` (expansions whose spec names
  relations declared inside the construct), `ConstructKind::Members` (a role's `R$members`), `Node<R>` assignable
  where `Node` is expected (LANGUAGE §5.3), and `count` over a tuple (LANGUAGE §10.1's `count<(S, L, P)>`).
- **Code registry:** `BLS0106` may also be constructed by `blossom-front` (a clause against the relation's kind is a
  semantic check; ARCHITECTURE §13.1 gives the parser the syntactic part of BLS0100–0110).
