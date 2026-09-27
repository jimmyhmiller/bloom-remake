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
