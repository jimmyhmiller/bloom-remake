# Handoff: Blossom (bloom-remake)

Last updated: 2026-09-28, at the slice 1 gate.

## What this is

Blossom is a from-scratch Rust implementation of the Berkeley BOOM line of work (Overlog → Dedalus → Bloom →
Bloom^L → Blazes → Edelweiss → Molly → Hydro), built as its own statically typed language (`.bls`, CLI `blossom`),
with LDFI, bounded model checking and SMT verification, an interpreter and code generator, and flagship systems
(Raft, Paxos, BOOM-FS/MR, a lineage dataflow engine).

## How delivery works now: vertical slices

On 2026-09-28 the user chose to replace the remaining crate-by-crate milestones (M3–M15) with **vertical slices**
gated by end-to-end behaviour (docs/design/SLICES.md, normative for delivery order). LANGUAGE.md, ARCHITECTURE.md,
FEATURES.md and the golden corpus remain the specification. Each slice builds only what its demo exercises, but
builds it properly; everything else fails loudly with `Unimplemented` / BLS0908 / exit 7.

| Slice | What | State |
|---|---|---|
| M1, M2 | workspace, base, values, IR, parser, SAT/SMT, storage (the old milestones) | done |
| **S1** | **Molly parity: `.ded` programs through frontend → oracle → simulator → provenance → LDFI** | **gate: see below** |
| S2 | the Blossom language on the same core; `e02`/`e04` in Blossom under `sim` and `ldfi` | next |
| S3 | real processes: TCP, WAL recovery, `e01` KVS surviving `kill -9` | |
| S4 | Raft in Blossom, simulated, LDFI-checked, then as real processes | |
| S5+ | fast engine, lattices/analyses, verification (BMC/SMT), codegen, systems, operations | |

## Slice 1 results

- Every failure-free `.ded` case (19) matches its literature rows.
- **76 of the 77 `[backend.ldfi]` cases give the published verdict**; all 13 stated falsifier sets are exact; every
  BENCH-136 run count is at most the published one except ack-deliv 8/7/1 (2,741; published 673).
- The one open case, BENCH-133d (Flux 22/21/1), is excluded from the gate (SLICES.md): neither search here nor the
  reference checker decides it; its verdict rests on Flux's safety argument and moves to the verification slice.
- `tests/corpus/ldfi`: 95 backends ratcheted to `pass` (`cargo run -p xtask -- corpus --check --gate --area ldfi`).

Try it:

```sh
cargo build --release -p blossom-cli
B=./target/release/blossom
$B ldfi tests/corpus/ldfi/molly/BENCH-130a-simple-deliv-6-3-0/program.ded --eot 6 --eff 3 --nodes a,b,c
#   counterexample O(a,b,1) after 2 runs (the LDFI paper's Figure 12), with its lineage and message timeline
$B ldfi tests/corpus/ldfi/molly/BENCH-133b-paxos-synod-7-6-1/program.ded --eot 7 --eff 6 --crashes 1 --nodes a,b,c --stats
#   Paxos certified in 260 runs
$B sim tests/corpus/ldfi/molly/BENCH-130a-simple-deliv-6-3-0/program.ded --nodes a,b,c --ticks 3 --omit a:b:1 --messages
```

How LDFI decides (docs/plan/notes/S1.md has the full list of decisions and deviations):

1. the seeded lineage-driven search of ARCHITECTURE §8.5 (hypotheses extend the run's own faults), with
   speculative parallel workers that commit in sequential order, so results never depend on thread timing;
2. **tuple-level negative support** (the default): a negated read is falsified only if a matching tuple can appear
   (sound for supersets of the run's faults). It keeps every counterexample that needs negative support (3PC,
   Kafka) and turned Paxos 7/6/1 from >150,000 runs into 260;
3. **exhaustive certification** when the lineage-driven search spends 20,000 runs without a verdict: every
   admissible schedule, tick by tick, with equal states merged. Bully 10/9/1 and the larger Raft cases certify this
   way; the report says which search decided.

## Where things are

| What | Where |
|---|---|
| Decisions (user choices + defaults adopted) | `docs/DECISIONS.md` |
| Delivery order (slices) | `docs/design/SLICES.md` |
| Research reports and master feature spec | `docs/research/`, `docs/research/FEATURES.md` |
| Language reference, architecture, original build plan | `docs/design/LANGUAGE.md`, `ARCHITECTURE.md`, `PLAN.md` |
| Slice notes (what was built, deviations, follow-ups) | `docs/plan/notes/S1.md` |
| Golden corpus | `tests/corpus/` (`xtask corpus` runs the `.ded` oracle and ldfi backends) |
| Coding conventions | `docs/dev/CONVENTIONS.md` |
| Status board | pad `bloom-remake` |

## How to work on it

- One driver per slice's critical path; fan out only for genuinely wide work (corpus triage, stdlib modules).
- A slice gate is `scripts/ci.sh gate`, the slice's acceptance command (for S1: `xtask corpus --check --gate
  --area ldfi`), and an adversarial review of the slice diff; the slice then merges to `main` as `Slice N: …`.
- The old milestone workflow script and `scripts/milestone-gate.sh` belong to the M-milestones and are not used for
  slices.
- No stubs: unimplemented paths return `Unimplemented` / BLS0908 / exit 7 naming the feature and the slice.

## Things to watch

- Performance: exhaustive certification merges states in a `BTreeMap` of full instances; bully 10/9/1 takes about
  a minute and a half. Hash-consing states would speed it up.
- The `.ded` profile keeps Molly's conjunctive aggregate encoding (ARCHITECTURE §8.3), which does not account for new
  contributors appearing through upstream negation; both search modes share it.
- No license chosen yet: every package is `publish = false`.
