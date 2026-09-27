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
