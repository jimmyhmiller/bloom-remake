# Blossom Delivery Plan

Status: **normative for the build**, 2026-09-27. It turns `docs/design/ARCHITECTURE.md` into **94 work packages
(WPs) in 15 milestones** for waves of parallel coding agents. `docs/design/plan.json` is the machine-readable copy;
both files are generated from one source and must stay identical in content. When they disagree, plan.json wins and
the disagreement is a bug.

Inputs, in order of precedence: `docs/DECISIONS.md`; FEATURES.md §1 (CR-xx); `docs/design/LANGUAGE.md`;
`docs/design/ARCHITECTURE.md`; the rest of FEATURES.md and the research reports. This plan changes none of them. The
places where it refines how ARCHITECTURE is *delivered* (not what is built) are listed in §4, and each has a line in
DECISIONS.md.

Contents: §1 strategy · §2 agent protocol · §3 milestone gates · §4 plan decisions · §5 corpus manifest schema ·
§6 priority inversions · §7 milestones at a glance · §8 work packages · §9 FEATURES id coverage · §10 P2 placement.

---

## 1. Strategy

**Order.** Foundation first, then an executable semantics, then the fast path, then distribution, then assurance,
then systems, then scale, hardening and P2:

| Phase | Milestones | What becomes true |
|---|---|---|
| Foundation | M1–M3 | The workspace builds; values, IR, lattices, kernel storage, schemas, the parser and the `.ded` frontend exist; the golden corpus is written from the literature. |
| Semantics | M4–M5 | The naive oracle is the executable definition; P0 analyses stratify; the planner produces physical plans; the corpus runner runs `.ded` and core `.bls` programs on the oracle. **First programs run at the M5 gate.** |
| Fast path | M6–M7 | The interpreter matches the oracle tick by tick under plan perturbation; the whole surface language lowers; natives and provenance capture land. |
| Distribution | M5, M7, M8 | The sans-IO node, durable recovery, the deterministic simulator over SimFs, the production runtime, mTLS. |
| Assurance | M8–M10 | LDFI reaches Molly verdict parity (M8), BMC and law proofs (M9), SMT/EPR and ASP (M10). |
| Library and systems | M8–M13 | P0 std (M8), P1 std (M9–M10); Raft, Paxos, Anna (M9); Raft P1, commit, BOOM-FS, ISR log (M10); BOOM-MR and the lakehouse (M11); HOP and BOOM-2 (M12); Tide (M13). |
| Scale and hardening | M10–M14 | WCOJ/SIP/alternatives (M10), incremental recursion and parallelism (M11), benchmarks (M11–M12), security P1 (M11), upgrades (M12), release engineering and docs (M12–M13), the P0/P1 audit (M14). |
| P2 | M15 | Editor support, compatibility frontends, P2 language, engine, analysis, verification and systems items. |

**Critical path.** M1.1 → M2.2 (IR) → M3.2 (fixtures) / M3.3 (kernel storage) → M4.1 (oracle), M4.2 (strata),
M4.3 (kernel execution) → M5.1 (planner) → M6.1 (interpreter) → M7.1 (natives, Tier C) + M7.2 (simulator) → M8.1
(LDFI) → M9.1 (Raft) → M10.x → M11.x → M12.x → M13.1 (Tide) → M14.1. Everything else is scheduled around it so every
wave is as full as the dependency graph allows.

**Mapping to ARCHITECTURE §14.2.** ARCHITECTURE names its milestones M0, M1a and M1–M8; this plan's M1–M15 replace
them (§4 D16). Corpus manifests, `until` fields and gates use **this plan's** ids.

| ARCHITECTURE | This plan |
|---|---|
| M0 skeleton | M1 (workspace, base, value surface), M4.8 (engine surface); other surfaces are published by the WP that implements them (§4 D1) |
| M1a foundation | M2–M3 |
| M1 (B + C) | M5 (runner, lowering core), M6 (full lowering, corpus triage) |
| M2 (A + B + C + H) | M6.1, M6.2, M7.1, M7.3 |
| M3 (D + E + M2) | M5.4, M5.5, M7.2, M8.2 |
| M4 (Molly parity) | M8.1 |
| M5 (runtime, codegen) | M7.4, M8.5, M8.7 |
| M6 (verification, P1 analyses) | M5.6, M6.6, M7.5, M9.5, M10.4 |
| M7 (flagship systems) | M9–M13 |
| M8 (engine and platform P1) | M8.6, M10.6, M11.x, M12.3 |

**Sizing.** Every WP is sized for one agent in one long session (about 2k–8k lines of code plus tests). The largest
(M2.2, M5.1, M6.1, M7.2, M10.4) are at the upper bound and have the most precise specs. Corpus WPs are measured in
cases, not lines.

**Risks specific to this plan and their containment.**
- *The corpus is written before the compiler exists (M1).* Programs are checked by the parser (M2.3), the type checker
  (M4.5) and two triage WPs (M6.7, M8.8) that may fix cases but never weaken expectations.
- *Parallel agents find bugs in code nobody owns this milestone.* §2.7: small blocking fixes are allowed under a rule;
  everything else goes to `docs/plan/BUGS.md` and is picked up by the next owner.
- *A gate fails.* §3: the orchestrator runs a gate-fix agent before the next wave; statuses are never loosened to
  pass a gate.
- *External tools are missing* (cvc5, clingo, Soufflé, cargo-deny). Every installer writes to the repository-local,
  git-ignored `.tools/`; nothing is installed globally; a missing tool is a failure where the WP's acceptance needs
  it, and never a silent pass.

---

## 2. Agent protocol (normative for every WP)

### 2.1 Starting a WP
1. Work in your own git worktree: `git worktree add .worktrees/<WP-id> -b wp/<WP-id> <milestone-base>` (the
   orchestrator may create it for you). The milestone base is the commit that closed the previous milestone's gate.
2. Read, in this order: this section; your WP in §8 (or plan.json); every section of ARCHITECTURE.md, LANGUAGE.md and
   FEATURES.md your spec cites; `docs/dev/CONVENTIONS.md` (from M1.1); the entries of `docs/plan/BUGS.md` that name a
   crate you own (fix them first, or defer each with a written reason in your notes).
3. Use a private target directory if several agents share a machine: `export CARGO_TARGET_DIR=target/<WP-id>`.

### 2.2 Ownership
- You may create, modify or delete **only** paths matched by your `owns` list (`!` entries are exclusions), plus your
  notes file `docs/plan/notes/<WP-id>.md`. Within a milestone, `owns` lists are pairwise disjoint (checked when the
  plan is generated).
- Owning a crate's module files but not the crate (`crates/X/src/m/**`) means you may not edit `crates/X/Cargo.toml`
  or `crates/X/src/lib.rs`; the module is already declared (§4 D6).

### 2.3 Interfaces between milestones
- You consume only work that is in your milestone base: earlier milestones. Never wait for, read or depend on a
  sibling WP of your own milestone.
- Public APIs published by earlier milestones change **additively only**. A breaking change needs you to own every
  consumer, or it is a plan change (DECISIONS.md line + plan update) made at a gate.
- The frozen crates of ARCHITECTURE §1.6 (`blossom-base`, `-value`, `-ir`, `-schema`, `-artifact`, `-trace`) follow
  its procedure once their implementing milestone has closed: an ARCHITECTURE amendment, a DECISIONS.md line and an
  `API_VERSION` bump in the same commit.

### 2.4 Shared files
- **Root `Cargo.toml`**: edited only by M1.1 and by gate-fix agents. Add an external dependency to *your crate's*
  `Cargo.toml`: use `dep.workspace = true` when the workspace declares it, otherwise an explicit version. Record every
  new external crate (name, version, license, reason) under `## New dependencies` in your notes; the gate copies it to
  `docs/design/DEPENDENCIES.md`.
- **`Cargo.lock`**: never resolve conflicts by hand; the gate regenerates it (§3).
- **Dispatch files** frozen after M1: `crates/blossom-cli/src/main.rs`, `xtask/src/main.rs`,
  `crates/blossom-std-host/src/lib.rs`, the module declarations M1.1 created. Later WPs own the per-command /
  per-module files.
- **CI**: `scripts/ci.sh` runs every fragment in `scripts/ci.d/`; add a fragment only if your `owns` lists it.
- **Corpus**: a case directory is owned by exactly one WP per milestone (see `owns`). Status flips of cases you do not
  own happen mechanically at the gate (§3).

### 2.5 Code rules (ARCHITECTURE §12, the user's rules)
- **No silent stubs.** A path that is not implemented returns `Unimplemented` via `unimplemented_feature!("ID",
  "…")` (or a BLS0908 diagnostic at compile time, or CLI exit code 7), naming the FEATURES id and, where known, the WP
  that will implement it. Never return a plausible default, never `todo!()`, never panic in library code.
- `thiserror` error enums per crate; large errors boxed; codes only from `blossom_base::codes` and only in the owning
  crate; `bug!` for violated internal invariants.
- Determinism: no `HashMap`/`HashSet`/`RandomState`, no ambient time or randomness outside the exempt modules;
  `DetMap`/`DetSet`; canonical order at every observable boundary.
- Mark implementing sites with `// FEATURE: <ID>` comments (the coverage tool counts them).
- `unsafe` only where ARCHITECTURE allows it (kernel `rows`, `chunk`, `prefetch`), with `// SAFETY:` arguments.

### 2.6 Tests and acceptance
- Every acceptance command of your WP must pass from the repository root, in your worktree, before you finish.
- `scripts/require-tests.sh <crate> <substring>…` fails if no test name contains a listed substring: name your
  tests so the required substrings match (for example `validator_v3_rejects_remote_body_atom`).
- Unit tests live next to the code; each crate has one integration binary `tests/it/main.rs` (plus the
  `harness = false` binaries a spec names, such as `smt_suite`, `corpus`, `molly_parity`); cross-crate tests live
  in `tests/integration/tests/<prefix>_*.rs` files owned by prefix (§4 D4).
- Property tests use generators from lower crates (feature `arbitrary`), never from `blossom-testkit`.
- **Tests must survive later milestones.** Never commit a test asserting that something a *later* WP implements is
  unimplemented (it would break that WP's gate). Assert "works, or fails only with `Unimplemented`/BLS0908 naming
  feature X" instead, or pin the unimplemented case with a test-only input that stays unimplemented (for example a
  std source override without the module). Tests inside a crate you own are yours to update when you implement them.
- Snapshot tests use `insta`; review every new snapshot for correctness rather than accepting it.
- Performance numbers are recorded with machine details and never gate on shared machines (ARCHITECTURE §4.14).

### 2.7 Notes, bugs and deviations
- Finish by writing `docs/plan/notes/<WP-id>.md` with: what was built; every deviation from the spec and why; `## Bugs`
  (problems found in code you do not own, each with a minimal reproducer and the owning crate); `## New dependencies`;
  `## Follow-ups` (anything deferred, with the FEATURES id and the reason).
- **Blocking bugs in code you do not own**: if a bug in a path owned by *no* WP of your milestone blocks your
  acceptance, you may make the minimal fix, list it under `## Out-of-scope fixes` in your notes, and add a regression
  test. If the path is owned by a sibling WP, do not touch it: record the bug and work around it in your own code
  only if the workaround is correct without the fix.
- A spec that is wrong or contradicts ARCHITECTURE/LANGUAGE/FEATURES: follow the higher-precedence document, record the
  contradiction in your notes, and continue.

### 2.8 Environment
- Toolchain: rustc/cargo 1.96.0 (pinned); nightly only for Miri and fuzzing.
- `z3` is at `/opt/homebrew/bin/z3`. cvc5 and clingo come from `scripts/install-solvers.sh` (M2.5) into `.tools/`;
  cargo-deny, cargo-hack and cargo-nextest from `scripts/install-dev-tools.sh` (M1.1) into `.tools/`; benchmark
  baselines from `scripts/install-baselines.sh` (M11.4). Set `BLOSSOM_REQUIRE_SOLVERS` as §4 D11 describes.
- Network access is available for fetching crates, solvers and the Molly sources.

---

## 3. Milestone gates

A milestone closes when every WP's acceptance passed in its worktree and the merged result is green. The orchestrator
runs `scripts/milestone-gate.sh Mk` (created by M1.1), which:

1. merges every `wp/<id>` branch of the milestone into the integration branch (paths are disjoint, so only
   `Cargo.lock` can conflict: the script takes the base version and regenerates it with `cargo metadata` +
   `cargo check --workspace --all-targets`);
2. appends new entries from `docs/plan/notes/*.md` to `docs/plan/BUGS.md` and `docs/design/DEPENDENCIES.md`
   (`scripts/collect-notes.sh`);
3. runs `scripts/ci.sh gate` (fmt, clippy `-D warnings`, `xtask check-layers`/`check-sans-io`/`check-codes`, the whole
   test suite, and every fragment present: corpus, differential, determinism, codegen, crashcheck, compat, LDFI parity);
4. from M5 on, runs `cargo xtask corpus --ratchet --milestone Mk` (mechanical status updates only, §5.3) and then
   `cargo xtask corpus --check --gate` (every case whose `until` ≤ Mk passes on that backend);
5. from M5 on, runs `cargo xtask coverage` and commits `docs/plan/coverage.md`;
6. writes the next milestone id into `docs/plan/MILESTONE` and commits "Mk: <title>" (DECISIONS.md working mode:
   commit per working milestone).

If any step fails, the orchestrator runs a **gate-fix agent** with the failure report. Nothing else runs in parallel
with it, so it may modify any path; it records every change in `docs/plan/notes/Mk-gate.md`. It may not loosen a
corpus expectation, extend an `until`, or delete a test; a problem that cannot be fixed within the gate is recorded as
a plan change (DECISIONS.md) that moves the work to a named later WP.

---

## 4. Plan decisions

Each is a delivery-level refinement of ARCHITECTURE (what is built does not change) and is recorded in DECISIONS.md.

- **D1 Staged interface freeze.** ARCHITECTURE §1.6 asks for an M0 skeleton with every public item of Appendix B.
  Here M1.1 creates every crate as a compiling placeholder and publishes the `blossom-base` API and the
  `blossom-value` type surface; every other crate's public surface is published by the WP that implements it, which
  always precedes its consumers, and M4.8 publishes the engine boundary types early so the node (M5.5) can be built
  before the engine. Worktree-based waves (D5) make an earlier freeze unnecessary.
- **D2 Monotonicity classes live in `blossom-value::class`.** `MonoClass`, `LatOpKind`, `HeightClass`, `LawStatus`,
  `Claim` and `ProofStatus` are defined in `blossom-value::class` and re-exported from `blossom-ir::core::lattice`
  (so ARCHITECTURE's paths resolve) and from `blossom-lattice`. `blossom-ir` does not depend on `blossom-lattice`, so
  the IR (M2) and the lattice library (M3) are built independently.
- **D3 The engine boundary is published first** (M4.8): `TickHeader`, `TickOutputRef`, `DurableImage`, `TickError`,
  `EngineError`, `EngineConfig`, `PlanExecutor`, `ExecutorFactory`, `abi` handles and `InputRow`/`OutputRow`, and an
  uninhabited `Engine` with the §4.7 signatures (no instance can exist until M6.1, so nothing can observe a default).
- **D4 A `tests/integration` crate** (`blossom-integration-tests`) holds cross-crate tests (engine ⇄ oracle, planner +
  engine, simulator + driver), because ARCHITECTURE's layer rules forbid those dev-dependencies inside the crates.
  WPs own its test files by prefix.
- **D5 One worktree per WP, merged at the gate.** A WP sees only its milestone base, so "consume only earlier
  milestones" is enforced by construction and concurrent edits never break a sibling's build.
- **D6 Module files are pre-declared where a crate is shared within a milestone** (syntax, front, std-host, CLI,
  xtask by M1.1; front's advanced-lowering, spec and lock entry points by M5.3; testkit's backend modules by M5.2;
  the planner's `natives/` registry by M5.1). Each placeholder returns `Unimplemented`/BLS0908 with its feature id.
- **D7 CLI and xtask dispatch are frozen after M1.1**; each subcommand lives in its own file owned by the implementing
  WP; unimplemented subcommands exit 7 naming their feature and WP.
- **D8 CLI inventory.** The corpus is run with `cargo xtask corpus`, not `blossom corpus` (the CLI must not depend
  on the test kit). Three commands are added to ARCHITECTURE §12.5's inventory: `admin` (M11.5), `completions` (M12.4)
  and `lsp` (M15.1).
- **D9 Per-backend corpus statuses** (refines ARCH-28; §5.3): each backend of a case has its own `status`,
  `unimplemented` list and `until`, because backends arrive in different milestones (oracle M5, interpreter M7,
  simulator M7, LDFI and codegen M8, BMC M9, SMT/ASP M10).
- **D10 The standard library is loaded lazily per module** (`blossom-std-src`), so a broken `std/x.bls` never affects a
  program that does not import `std::x`.
- **D11 Solver requirements.** `BLOSSOM_REQUIRE_SOLVERS` is `1`/`all` or a comma list (`z3,cvc5,clingo`); tests
  needing an unlisted, absent solver report "not run: …" and never pass; solvers are found via `$BLOSSOM_<SOLVER>`,
  then `PATH`, then `.tools/bin`.
- **D12 `SimFs` draws no randomness.** Its crash API asks the caller for each unsynced write's fate; the simulator
  supplies PRF-driven fates. This keeps `blossom-store` free of the simulator's decision streams.
- **D13 `FileDurability<F: Vfs>` and the `MigrationRunner` trait live in `blossom-store`**; the runtime implements the
  runner with a single-tick engine (recovery needs an engine; the store must not depend on one).
- **D14 Native operators degrade to their expansions.** The planner selects a native only when the target executor's
  capability set contains it and otherwise plans the expansion (always correct). Capability refusals (BLS0908) remain
  for features without an expansion. `PlanTarget::Any` skips the capability check for dumps.
- **D15 Lattice delta shipping is node-side** (M11.3): the node keeps per-(destination, channel, key) last-sent values
  and encodes `LDelta` frames; the engine's outbox keeps full values.
- **D16 Milestone ids.** This plan's M1–M15 replace ARCHITECTURE §14.2's names everywhere (§1 table).
- **D17 Corpus scope.** Golden cases written in M1 are self-contained programs (no `std::` imports); cases that test
  the standard library are written by the std WPs; security, TLS and multi-version scenarios that a manifest cannot
  express are integration tests named `bench_<id>_*`.
- **D18 `InputRow`/`OutputRow` live in `blossom-engine::abi`**, so generated typed bindings name only the ABI
  (ARCHITECTURE §4.7's rule for generated code).
- **D19 No project license is chosen**; every package is `publish = false` until the user decides.
- **D20 The Raft core is a library module**, `std::consensus::raft`, implementing the `std::consensus::Consensus`
  protocol that coordination synthesis (ANA-046) and dynamic membership (LIB-023) use; `systems/raft` is the flagship
  system built on it.

---

## 5. Golden corpus manifest schema v1 (normative)

ARCHITECTURE §11.4 gives the layout; this section fixes the manifest. `tests/corpus/tools/check_manifests.py` (shipped
with this plan, stdlib Python) validates it until M5.2 replaces it with `cargo xtask corpus --lint`.

### 5.1 Layout and fields

```
tests/corpus/<area>/<ID>[<letter>]-<slug>/      area ∈ core, lattices, lprov, async, net, verify, ldfi, upgrade,
    manifest.toml                                security, protocols, examples, std/<std-area>, frontends
    program.bls | program.ded                    (or several files named in `programs`)
    spec.bls                                     optional
    expected/                                    optional per-tick dumps (only with expected_from = "blessed")
```

```toml
schema   = 1                                   # required
id       = "BENCH-001"                         # required: the FEATURES id this case belongs to (BENCH-…, LIB-…)
title    = "DL11 Ex. 3: persistence and deletion"
priority = "P0"                                # required: the id's priority in FEATURES.md
source   = "R02 §3.2 (DL11 Ex. 3)"             # required: where the expected results come from
features = ["SEM-005", "SEM-006", "LANG-040"]  # required: FEATURES ids the case exercises (drives `until`)
program  = "program.bls"                       # or programs = { v1 = "v1.bls", v2 = "v2.bls" }
spec     = "spec.bls"                          # optional
include  = ["shared.bls"]                      # optional extra sources
derived  = false                               # true when re-derived from a paper instead of vendored verbatim
expected_from = "literature"                   # or "blessed" (xtask bless; only for large outputs, see M5.2)
notes    = "…"

[deploy]                                       # default: one node "n1" of the program's only role
nodes  = [{ name = "a", role = "Node" }, { name = "b", role = "Node" }]
params = { N = 3 }
seed   = 7

[run]
ticks = 20                                     # rounds (sync harness) or max ticks per node (sim)
stop  = "quiescent"                            # or "ticks"
seeds = 16                                     # sim backend: every expectation must hold in every seed
replay_check = false                           # run twice and replay; per-tick digests must match (BENCH-049)
swarm = false                                  # sim backend: add swarm faults on top of [[fault]]

[[input]]                                      # host input rows delivered in the node's tick `tick`
node = "a"
tick = 1
rel  = "ins"
rows = [[1, 2], [3, 4]]

[[fault]]                                      # scripted faults for the sync harness and the simulator
kind = "crash"                                 # crash | restart {node, tick}; omit {from, to, send_tick};
node = "b"                                     # partition {from = [..], to = [..], ticks = "3..=5"};
tick = 3                                       # reject {from, to, send_tick, reason}

[backend.oracle]                               # one table per backend to run (§5.2)
status = "unimplemented"                       # pass | unimplemented | known-failure
unimplemented = ["LANG-048"]                   # required iff unimplemented: the feature ids it may fail with
until = "M6"                                   # required unless pass
# issue = "BUGS.md#12"                         # required iff known-failure

[[expect]]                                     # one of four shapes:
node = "a"                                     #  {node, rel, row, holds and/or absent}: tick ranges "3..=5", "3..", "..=5", "3"
rel  = "p"                                     #  {node, rel, tick, rows}: exact contents at a tick (set equality)
row  = [1, 2]                                  #  {node, rel, final = true, rows}: contents at the end of the run
holds = "3..=5"                                #  {quiescent_from}: every node quiescent from that tick on
absent = "6.."

[[expect_send]]                                # from, to, channel, row; optional tick (send tick) and count
[[expect_error]]                               # code = "BLSRnnn", node, tick: a runtime hard error
[[expect_diag]]                                # code = "BLSnnnn", optional line, severity: a compile diagnostic
[expect_analysis]                              # points_of_order, strata, certificates, calm_labels, finality,
                                               # blazes, reclaimable, confluent, deterministic, fair_consistency
[expect_ldfi]                                  # eot, eff, crashes, nodes, verdict (counterexample |
                                               # no_counterexample | program_error), crash_view (molly | frozen),
                                               # runs_max, falsifiers = [["O(a,b,2)", "C(c,3)"], …]
[expect_verify]                                # check = bmc | smt | asp | sim, result = holds | fails, bounds = {…}
[perf]                                         # informational targets read by the benchmark harness
```

**Values of `[expect_analysis]` and diagnostic comparison.** Fixed at the M1 gate (DECISIONS.md) in
`tests/corpus/README.md`, "Expectation vocabularies", and validated by the manifest lint.

**Ticks.** Node-local ticks; tick 0 is the boot tick (CR-13). In the sync harness every live node ticks in every
round, so a round number equals every node's tick; messages sent in round t are delivered in round t + 1
(self-sends included). Inputs at tick k are delivered in tick k.

**Row values** are decoded by the target column's type: TOML integers → integer, `Duration` and `Instant` columns (ns)
and `Mod`; floats → `f64`; strings → `String`, a node name for `Node`, UTF-8 for `Bytes`; booleans; arrays → tuples,
`Vec`, `Set`, and `Map` as `[k, v]` pairs; inline tables: `{ some = v }`, `{ none = true }`, `{ variant = "Name",
fields = [...] }`, `{ bytes_hex = "…" }`, and `{ blossom = "<constant expression>" }` as the escape hatch (lattice
values such as `lset{1, 2}`, durations such as `5s`). A lattice column compares by its revealed value.

### 5.2 Backends

| Backend | Harness | Unimplemented as | Floor | Implemented by |
|---|---|---|---|---|
| `compile` | frontend only; `[[expect_diag]]` (no diagnostics expected when empty) | — | M5 | M5.2 |
| `analysis` | analyses over the IR; `[expect_analysis]` | the ANA id of an unsupported key | M5 (P0 keys), M7 (P1 keys) | M5.2, M7.3 |
| `oracle` | sync rounds on the oracle | — | M5 | M5.2 |
| `interp` | sync rounds on the interpreter (node + engine) | SEM-002 | M7 | M7.3 |
| `sim` | seeded asynchronous simulation on the engine, `seeds` runs | TEST-001 | M7 | M7.2, M8.2 |
| `ldfi` | Molly-2 | TEST-029 | M8 | M8.1 |
| `codegen` | tests/codegen-corpus (interp ⇄ codegen ⇄ oracle per tick) | ENG-005 | M8 | M8.5 |
| `bmc` | bounded model checking | VER-002 | M9 | M9.5 |
| `smt` | inductive-invariant proof | VER-010 | M10 | M10.4 |
| `asp` | bounded ASP encoding | VER-003 | M10 | M10.4 |

### 5.3 The status ratchet (per backend)
- `pass` must pass.
- `unimplemented` must fail with `Unimplemented` (or BLS0908) whose feature is in `unimplemented`; an unexpected
  success or any other failure fails with "stale status: update the manifest".
- `known-failure` requires `issue`.
- `cargo xtask corpus --check` fails any case whose `until` is **before** the current milestone
  (`docs/plan/MILESTONE`) and does not pass; `--check --gate` (the gate only) also fails `until` = current.
- The gate's mechanical updater may only: flip `unimplemented`/`known-failure` → `pass` when the backend passes;
  refresh the `unimplemented` list to the feature(s) actually reported. It never loosens a `pass`, never extends an
  `until`, and never edits expectations.
- Nothing is ever skipped.

### 5.4 Computing `until`
For a case and backend: `until = max(floor(backend), max{ M(f) : f ∈ features, f not a BENCH id })`, where `M(f)`
is the milestone of the **first** WP listing `f` in plan.json (its primary implementer, §9). A case whose features
include a P2 id belongs to M15. Corpus authors compute it from plan.json; the corpus runner re-checks it.

---

## 6. Priority inversions and forward pulls

FEATURES orders work by priority; a few P0/P1 items depend on higher-numbered work. They are placed with what they
need and recorded here.

| Item | Priority | Placed in | Why |
|---|---|---|---|
| SEM-106 declarative semantics (`pure^L(P)`) | P0 | M10.4 | Its executable form is the ASP encoding (VER-003, P1). The operational semantics it must agree with is the oracle (M4.1), and SEMANTICS.md is written with the encoder. |
| BENCH-200 batch gate, BENCH-202 protocol throughput | P0 | M11.4, M12.6 | Measurable only on the complete engine (BENCH-200's full target needs ENG-102, P1) and on the rewritten protocols (P1); ARCHITECTURE §0.2 already records the BENCH-200 inversion. |
| FLAG-107 exactly-once pipelined map output | P0 | M12.1 | Needs DIST-011 (P1), as ARCHITECTURE Appendix A notes. |
| FLAG-001–008 and every P0 FLAG | P0 | M9–M11 | Flagship systems need the complete platform. |
| ANA-066 GC safety | P0 | M7.5 | Only meaningful with the Edelweiss rewrites (P1). |
| SEM-011 time skipping | P1 | M15.3 | Observable only with entanglement (LANG-072, P2), as ARCHITECTURE Appendix A notes. |
| SEM-052 Overlog key overwrite | P1 | M8.6 | Implemented as an Upsert mode tested through IR fixtures; its surface frontend (LANG-221) is P2 (M15.2). |
| FLAG-028 CompPaxos | P2 → pulled into P1 | M9.2 | BENCH-177 (P1) requires it. |
| BENCH-221 downgrade | P1 | M12.3 | Rollback and downgrade *before* finalization are in scope; downgrade after finalization is a P2 extension (ARCHITECTURE §14.3). |
| FLAG-152 PBFT | P2 | M15.4 | FEATURES marks it out of scope for v1; built only as a simulation/LDFI demonstrator under SEM-074 Byzantine faults. |

## 7. Milestones at a glance

| M | Title | Work packages |
|---|---|---|
| M1 | Workspace skeleton, blossom-base and the golden corpus | **M1.1** Workspace skeleton, tooling, blossom-base and the blossom-value type surface<br>**M1.2** Golden corpus I: core semantics (BENCH-001–049) and the corpus README<br>**M1.3** Golden corpus II: lattices and lattice provenance (BENCH-050–079, BENCH-300–313)<br>**M1.4** Golden corpus III: asynchrony, analysis oracles, core networking and verification<br>**M1.5** Golden corpus IV: the Molly LDFI corpus (BENCH-130–137) |
| M2 | Values, the core IR, the parser, solvers and storage I/O | **M2.1** blossom-value: encodings, fingerprints, PRF, digests, reference store and externs<br>**M2.2** blossom-ir I: the Dedalus^L IR, IrBuilder, validator, printer, digest and projection<br>**M2.3** blossom-syntax I: lexer, parser, lossless CST and the typed AST<br>**M2.4** blossom-sat: SatSolver trait, CaDiCaL, batsat, exhaustive and DIMACS backends, cardinality encodings<br>**M2.5** blossom-smt: SMT-LIB2 over child processes (z3, cvc5) and the clingo ASP driver<br>**M2.6** blossom-store I: Vfs, RealFs, SimFs crash model, WAL segments, checkpoint files, META and LOCK |
| M3 | Lattices, kernel storage, schemas, name resolution and the Molly frontend | **M3.1** blossom-lattice I: built-in lattices, operation catalogue, dynamic ops, lattice heap and the law harness<br>**M3.2** blossom-ir II: fixtures with expected traces, generators, plan validation and construct checks<br>**M3.3** blossom-kernel I: row storage, epochs, deaths, indexes, the interner and snapshots<br>**M3.4** blossom-schema and blossom-artifact: field numbers, schema hashes, schema.lock, compat rules, catalogs, artifacts<br>**M3.5** blossom-front I: modules, resolution, instantiation, roles and the HIR<br>**M3.6** The Molly `.ded` frontend (syntax::ded and front::ded)<br>**M3.7** blossom-syntax II: the formatter, hash normalization, editions and recovery hardening |
| M4 | The oracle, P0 analyses, kernel execution structures, wire codec, type checking and seams | **M4.1** blossom-oracle: the naive per-tick Dedalus^L evaluator and the choice-validity checker<br>**M4.2** blossom-analysis I: dependency graph, stratification, safety, locality, polarity, labels, ACLs, compat, branching<br>**M4.3** blossom-kernel II: sorted indexes, weighted stores, group tables, choice index, join kernels, provenance records, digests<br>**M4.4** blossom-wire: frames, the field-numbered tuple codec, limits, HELLO negotiation and the codec ABI<br>**M4.5** blossom-front II: type checking, the bang rule, legality and event/standing classification<br>**M4.6** blossom-lattice II: P1 lattices, user lattices, DomPair, groups and rings<br>**M4.7** blossom-trace: the observation vocabulary and the record/replay trace format<br>**M4.8** Engine boundary types: the blossom-engine public surface the node and codegen build against |
| M5 | Planner, lowering, the corpus runner, the sans-IO node and durable recovery | **M5.1** blossom-plan I: growth classes, regimes, versions, joins, indexes, natives selection, profiles<br>**M5.2** blossom-testkit I: corpus runner, status ratchet, oracle harness, xtask corpus/bless/coverage<br>**M5.3** blossom-front III: lowering the core language to IR<br>**M5.4** blossom-store II: durable encoding, identity, recovery order, migrations hook, store tooling, crashcheck<br>**M5.5** blossom-node I: the sans-IO node, Invariant R, admission, timers, probation, MemTransport and ManualDriver<br>**M5.6** blossom-analysis II: CALM certificates, determinism, FDs, streams, taint, CRDT classes<br>**M5.7** blossom-prov I: provenance graphs, why/whynot, Tier B proof search and rendering |
| M6 | The interpreter and the full surface language | **M6.1** blossom-engine I: the tick, staging, the interpreter, maintenance regimes and snapshots<br>**M6.2** blossom-plan II: tick-local fusion, in-out trees, perturbation, plan dumps and hints<br>**M6.3** blossom-driver and CLI I: CompileSession, caching, diagnostics rendering, check/fmt/build/plan/explain<br>**M6.4** blossom-front IV: lowering every advanced construct<br>**M6.5** blossom-front V: specs, schema lock, migrations, translations and the compatibility gate<br>**M6.6** blossom-analysis III: Blazes, Edelweiss analysis, finality, key conflicts, compat P1, lints<br>**M6.7** Corpus triage I: the golden corpus on the oracle |
| M7 | Natives, provenance capture, the simulator, differential testing, the production driver and rewrites | **M7.1** blossom-engine II: P0 native operators, Tier B/C provenance capture, digests, watch taps and counters<br>**M7.2** blossom-sim I: worlds, schedulers, faults over SimFs, CALM pruning, traces, replay and shrinking<br>**M7.3** blossom-testkit II: differential runner, OracleEvaluator, perturbation, conformance, zero-allocation harness<br>**M7.4** blossom-runtime I: the production driver, recovery with migrations, deployment config and the host API<br>**M7.5** blossom-rewrite: Edelweiss rewrites, resend suppression, coordination synthesis, decoupling, distributed provenance<br>**M7.6** blossom-prov II: semirings, semimodules, lattice provenance, exact supports and Nemo algebra |
| M8 | LDFI, specs under simulation, the P0 standard library, code generation, P1 natives and mTLS | **M8.1** blossom-ldfi I: Molly-2 — failure specs, hazard encodings, minimal enumeration, the driver and Molly parity<br>**M8.2** blossom-sim II: the spec engine, ultimate models, confluence, history checkers, finality oracle, diagrams, node crashcheck<br>**M8.3** Standard library I: delivery, multicast, membership, failure detection, timers, voting, 2PC, quorums, coordination<br>**M8.4** Standard library II: ids, queues, sequences, clocks, sealed replies, KVS, lattice KVS, CRDTs<br>**M8.5** blossom-codegen and blossom-build: generated executors, typed host bindings, specialized codecs, codegen corpus<br>**M8.6** blossom-engine III: the P1 native operators, finality maintenance, wrappers, as-of reads and engine P1 features<br>**M8.7** blossom-runtime II: TCP/mTLS transport, SPIFFE identity binding, audit, metrics and health<br>**M8.8** Corpus triage II: the golden corpus on the interpreter and the simulator |
| M9 | Consensus and replicated data: Raft core, Multi-Paxos, Anna KVS, LDFI P1, BMC, P1 library, host embedding | **M9.1** Raft I: elections, log replication, commit and apply, durability (std::consensus::raft + systems/raft)<br>**M9.2** Multi-Paxos: acceptors, leader recovery, phase 2, stable leadership, GC, P1 extensions and CompPaxos<br>**M9.3** Anna-style KVS: coordination-free per-core actors, consistency levels, deletes with reclamation<br>**M9.4** blossom-ldfi II: single-shot mode, vacuity pruning, symmetry, resume, sweeps, estimators, choices, rejections, reports<br>**M9.5** blossom-verify I: bounded model checking, bound certificates, law proofs, confluence certificates, trusted modules, rewrite verification<br>**M9.6** Standard library III: fault-injecting/FIFO/causal delivery, broadcast family, gossip, election, leases, 3PC/CTP, 2PL<br>**M9.7** Standard library IV: MV-KVS, MVCC, Dynamo KVS, consistency levels, causal KVS, registers, lattice GC, Z-set views, authorization<br>**M9.8** Runtime III: host services, output handlers, blobs, stdio and file sources, the `blossom` facade crate |
| M10 | Raft P1, commit protocols, BOOM-FS, the ISR log, SMT/ASP verification, planner performance, applications | **M10.1** Raft II: membership changes, snapshots, client semantics, linearizable reads, extensions, RSM API, KV on Raft, epochs<br>**M10.2** Commit protocols: 2PC over Raft-replicated participants, 3PC and 2PC-CTP as LDFI demonstrators, scalable 2PC<br>**M10.3** BOOM-FS: metadata as relations, heartbeats, re-replication, the data path, HA via Raft, partitioned metadata, client library<br>**M10.4** blossom-verify II: ASP bounded encoding, FOL transition systems, EPR, inductive invariants, lattice axioms, Paxos Made EPR<br>**M10.5** The replicated log with ISR (Kafka 0.8) reproducing its durability bug under LDFI<br>**M10.6** Planner and kernel performance: WCOJ, SIP, adaptive alternatives, subplan sharing, RAM rewrites, specialized representations<br>**M10.7** Standard library V: applications and demonstrators (carts, state machines, Chord, routing, ping, TC, rendezvous)<br>**M10.8** blossom-testkit III: seed sweeps, shuffle checks, input generation from constraints, implementation equivalence |
| M11 | BOOM-MR, the lakehouse, network P1 features, engine incrementality, batch benchmarks and security P1 | **M11.1** BOOM-MR: scheduler state as relations, FCFS with speculation, LATE, the MapReduce data plane, locality and fair share<br>**M11.2** The replicated object store and the Lattice Lakehouse log (ODD-19 M2)<br>**M11.3** Node II: delta shipping, flow control and fragmentation, channel filters, push channels and the adaptive send policy<br>**M11.4** blossom-bench I: harness, datasets, baselines, batch Datalog, tick micro-benchmarks, overheads, soak<br>**M11.5** Runtime IV: sessions and the client listener, the admin plane, certificate lifecycle, redaction, QUIC<br>**M11.6** Engine P1 incrementality and parallelism: recursive views under deletion, elastic recompute, specialized diffs, intra-tick parallelism, range deletion |
| M12 | HOP, the lineage dataflow engine, upgrades, protocol benchmarks, CLI polish and the user documentation | **M12.1** HOP: pipelined shuffle, adaptive flow control, exactly-once pipelined map output, online aggregation, snapshots, continuous scheduling<br>**M12.2** BOOM-2, the lineage dataflow engine: FS2, the stage planner, shuffle by seals, recovery from lineage, deterministic speculation, CALM-minimized fault tolerance<br>**M12.3** Upgrades: cluster versions, version windows, the rolling-upgrade orchestrator, codec translation, migrations at scale, mixed-version simulation<br>**M12.4** CLI and operations polish: the REPL, self-check, trace viewer command, explain long forms, completion<br>**M12.5** User documentation: the Blossom guide, the language tutorial, the standard-library reference<br>**M12.6** blossom-bench II: protocol throughput against DFIR, Blazes sealing vs ordering, latency tails |
| M13 | Tide streaming, Raft rolling upgrades, release engineering and the operations documentation | **M13.1** Tide: watermarks and windows, weighted views, the durable input log, online aggregation and continuous jobs, completion by free termination<br>**M13.2** Raft rolling upgrades (FLAG-016) and the rolling upgrade of a 3-node Raft KV (BENCH-220)<br>**M13.3** Release engineering: fuzzing complete, Miri and mutation testing, cargo-deny, reproducible signed releases<br>**M13.4** Operations documentation: deployment, security, durability, upgrades, observability, runbooks, configuration reference |
| M14 | Release readiness: the P0/P1 audit | **M14.1** Release audit: coverage, corpus, performance sign-off, open bugs |
| M15 | P2: editor support, compatibility frontends, language extensions, P2 engine/analysis/verification, P2 systems | **M15.1** Editor support: language server, tree-sitter grammar, incremental frontend<br>**M15.2** Compatibility frontends: Overlog/NDlog, Hydroflow datalog!, Bloom collection syntax, NDlog link literals<br>**M15.3** Language P2: host-backed collections, entanglement and time skipping, aggregate destinations, metaprogramming and hot install<br>**M15.4** Runtime, security and simulation P2: multi-program runtime, signed values, Biscuit tokens, encryption at rest, OnceTree, non-causal replay, Maelstrom/Jepsen, delegation, threshold trust, Byzantine faults, PBFT<br>**M15.5** Engine and analysis P2: eager mode, subsumption, magic sets, aggregate selections, exchange, bounded annotated fixpoints, in-tick greedy choice, P2 analyses<br>**M15.6** Verification P2: SAT-based BMC, invariant inference, sync-then-lift, exporters, Katara, Lean mechanization, liveness<br>**M15.7** P2 systems and library: stateful functions on the KVS, HDFS shim, scale-to-zero executors, progress tracking, SQL subset, early finalization, chain replication, examples and pipelines |

## 8. Work packages

Every WP below is also in `docs/design/plan.json` with identical content. `owns` entries starting with `!` are exclusions. Every WP additionally owns `docs/plan/notes/<WP-id>.md`.

### M1 — Workspace skeleton, blossom-base and the golden corpus

**Goal.** Create the complete Cargo workspace (every crate compiles as a placeholder), the tooling and lint policy, a complete `blossom-base`, the frozen type surface of `blossom-value`, and, in parallel, the golden conformance corpus written from the literature (not from our implementation).

**Gate.** `scripts/milestone-gate.sh M1` (PLAN §3).

#### M1.1 — Workspace skeleton, tooling, blossom-base and the blossom-value type surface

- **Size:** ~6.5k lines (about 2k of it boilerplate)
- **Depends on:** —
- **Owns:** `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.config/**`, `.gitignore`, `.github/**`, `xtask/**`, `scripts/**`, `crates/**`, `systems/**`, `tests/integration/**`, `tests/codegen-corpus/**`, `tests/fixtures/README.md`, `fuzz/**`, `std/README.md`, `docs/dev/**`, `docs/plan/README.md`, `docs/plan/BUGS.md`, `docs/plan/MILESTONE`, `docs/design/DEPENDENCIES.md`, `docs/plan/notes/M1.1.md`
- **Features:** LANG-024
- **Consumes:**
  - docs/design/ARCHITECTURE.md §1, §2.1, §2.2, §4.1, §12.1, Appendix B
  - docs/design/LANGUAGE.md §5.5, §20
  - docs/design/PLAN.md §2 (agent protocol), §4 (layout decisions)
- **Provides:**
  - the workspace: every crate of ARCHITECTURE §1.2 plus xtask, systems/*, tests/integration, tests/codegen-corpus; all internal dependency edges pre-declared
  - blossom-base (complete): idx, span/SourceDb, QualName, RuleLabel, diag, error (Unimplemented, InternalError, unimplemented_feature!, bug!), codes::REGISTRY, det (DetMap/DetSet), graph
  - blossom-value type surface (frozen): TypeTable/TypeDef (implemented), Value + canonical order (implemented), Word/Lane/encodings/fingerprint/PRF/digest/ValueStore/WordSink/RecordBuilder/Extern* signatures, value::class (MonoClass, LatOpKind, HeightClass, LawStatus, Claim, ProofStatus), time types
  - blossom-std-src: build-time embedding of std/**/*.bls with lazy per-module lookup
  - xtask: check-layers, check-sans-io, check-codes (implemented); dispatch + placeholder modules for every other subcommand
  - blossom-cli: clap dispatch for every subcommand of ARCHITECTURE §12.5; placeholders exit 7
  - scripts: ci.sh (+ ci.d fragments), wp-check.sh, require-tests.sh, milestone-gate.sh, collect-notes.sh, install-dev-tools.sh
  - docs/dev/CONVENTIONS.md, docs/plan/BUGS.md, docs/plan/notes/

**Goal.** After this WP every later WP can work inside its own paths without touching shared files. Nothing here is a
silent stub: placeholders are empty crates or return `Unimplemented`/exit code 7 with the owning WP named.

**1. Workspace root.**
- `Cargo.toml`: `[workspace] resolver = "3"`; members as **globs** so later crates need no root edit:
  `crates/*` (the 35 of ARCHITECTURE §1.2), `xtask`, `systems/*` (placeholders for raft, paxos, commit, kvs, boomfs,
  boommr, boom2, lakehouse, tide, isrlog; package names `blossom-sys-<name>`), `tests/integration` (package
  `blossom-integration-tests`), `tests/codegen-corpus` (package `codegen-corpus`); `exclude = ["fuzz"]`.
- `[workspace.package]`: `edition = "2024"`, `rust-version = "1.96"`, `publish = false` on every package (no license is
  chosen for the project; record that in DEPENDENCIES.md).
- `[workspace.lints.rust]`: `unsafe_code = "deny"`, `unsafe_op_in_unsafe_fn = "deny"`, `unused_must_use = "deny"`.
  `[workspace.lints.clippy]`: deny `unwrap_used, expect_used, panic, todo, unimplemented, unreachable,
  panic_in_result_fn, string_slice, dbg_macro, print_stdout, print_stderr, disallowed_types, disallowed_methods`;
  warn `indexing_slicing`. Binaries (`blossom-cli`, `xtask`) allow `print_stdout`/`print_stderr` locally.
- Profiles: `overflow-checks = true` in dev, test, release and bench; `[profile.release] panic = "abort"`;
  `[profile.dev.build-override] opt-level = 2` and the same for release.
- `[workspace.dependencies]`: every internal crate by path, plus the light external crates of ARCHITECTURE §1.2 with
  versions verified by actually building (`thiserror`, `serde` (derive), `serde_json`, `postcard` (alloc), `toml`,
  `smallvec`, `hashbrown`, `indexmap`, `rowan`, `xxhash-rust` (xxh3), `siphasher`, `blake3`, `crc32c`, `bytes`,
  `proptest`, `insta`, `libtest-mimic`, `clap` (derive), `tracing`). Heavy crates (tokio, rustls, quinn, rayon,
  rustsat*, batsat, criterion, dbsp, timely, differential-dataflow, dfir_rs, codespan-reporting, syn, quote,
  prettyplease, rcgen, x509-parser, hdrhistogram, metrics*) are added later by the owning WP in its own crate manifest.
- `rust-toolchain.toml`: `channel = "1.96.0"`, components rustfmt, clippy. `rustfmt.toml`: `edition = "2024"`,
  `max_width = 120`, stable options only. `clippy.toml`: ARCHITECTURE §12.1 disallowed types/methods (std and
  hashbrown `HashMap`/`HashSet`/`DefaultHashBuilder`, `RandomState`s, `Instant::now`, `SystemTime::now`,
  `thread_rng`), `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`,
  `allow-indexing-slicing-in-tests`. `deny.toml`: the license allowlist of §1.2 (MPL-2.0 only for imbl and
  webpki-roots), advisories, and a ban on `aws-lc-sys`/`aws-lc-rs`. `.config/nextest.toml` with slow-timeouts.
  `.gitignore` adds `/.tools`, `/datasets`, `/target*`.

**2. Crate placeholders.** Each crate gets `Cargo.toml` (name, `version = "0.1.0"`, `edition.workspace`,
`publish = false`, `[lints] workspace = true`, **every internal dependency edge of ARCHITECTURE §1.2/§1.3 pre-declared**
so no later WP edits internal edges; exceptions: `blossom-ir` does *not* depend on `blossom-lattice` (PLAN §4 D2), and
`blossom-front` *also* depends on `blossom-std-src` because ARCHITECTURE §13.1 loads `std::` modules in the frontend;
add that edge to `layers.toml`) and `src/lib.rs` with `#![deny(unsafe_op_in_unsafe_fn)]`, a crate doc
naming its purpose and the WP that implements it. Module skeletons (empty files with a module doc) are pre-declared
where several WPs later share a crate (PLAN §4 D6):
- `blossom-syntax`: `lexer, parser, ast, fmt, ded, overlog, hydro, bloom` (the last three are P2 frontends, M15.2).
- `blossom-front`: `api, modules, items, resolve, instantiate, roles, hir, typeck, classify, lower, spec, lock, ded,
  overlog, hydro, bloom`.
- `blossom-std-host`: one module per std area (`delivery, bcast, membership, fd, timers, election, lease, vote,
  commit, lock, quorum, coord, consensus, ids, queue, seq, clock, seal, kvs, crdt, gc, zset, examples, authz,
  upgrade, crypto, push, oncetree, pipeline`), each with `pub fn register(reg: &mut ExternRegistry) -> Result<(), ValueError>` that registers
  nothing yet, and `pub fn registry() -> ExternRegistry` calling all of them.
- `blossom-cli`: `main.rs` with a clap `Commands` enum covering every subcommand of ARCHITECTURE §12.5 except
  `corpus` (the corpus is driven by `cargo xtask corpus`; PLAN §4 D8), i.e. `check, fmt, build, plan, explain, run,
  deploy, node, config, sim, trace, ldfi, verify, why, whynot, compat, release, store, self-check, repl, upgrade`, plus
  `admin`, `completions` and `lsp` (PLAN §4 D8); each variant wraps `cmd::<name>::Args` from
  `src/cmd/<name>.rs` whose `run(args) -> ExitCode` prints `not implemented yet: <FEATURE-ID> (WP <id>)` to stderr
  and returns exit code 7 (§12.5). `main.rs` never changes again; later WPs own individual `cmd/*.rs` files.
- `xtask`: same pattern: `src/main.rs` dispatch, `src/cmd/<name>.rs` per subcommand.
- `blossom-testkit`: `Cargo.toml` declares `[[test]] name = "corpus" harness = false` and
  `[[test]] name = "molly_parity" harness = false` with placeholder mains that report zero tests.
- `tests/integration`: a library crate depending on every internal **library** crate (not `blossom-cli`, `xtask`) plus
  `proptest`; `src/lib.rs` holds shared
  helpers (empty now). Later WPs add `tests/<prefix>_*.rs` files (auto-discovered) that they own by prefix.
- `systems/<name>`: package `blossom-sys-<name>`, empty `src/lib.rs`; no `build.rs` yet.
- `tests/codegen-corpus`: placeholder package.
- `fuzz/`: its own `[workspace]`, `cargo-fuzz` layout, targets `lexer_parser, formatter, front, wire_decoder,
  wal_recovery, trace_reader, artifact_decoder, smt_response, admission`; each target body panics with
  `fuzz target <name> is not implemented yet (WP <id>)`.

**3. `blossom-base` (complete, frozen after this milestone).** Modules per Appendix B:
- `idx`: `define_idx!` (dense `u32` newtypes with `Debug`, `Ord`, serde), `IndexVec<I, T>`, all id types of
  ARCHITECTURE §2.1 (including `OccId`, `NativeId`, `AggTableId`, `BufferId`, `IndexId`, `GoalId`, `FiringId`).
- `span`: `Span`, `FileId`, `SourceDb` (file text, path, line/column mapping, UTF-8 checked), `Symbol` interner
  (thread-safe; ids are never observable: `Symbol` orders by text), `QualName` (Arc<[Symbol]>, printing with `.`),
  `RuleLabel { text, hash }` (SipHash-1-3 with key 0).
- `diag`: `Diagnostic { code, severity, message, primary, labels, notes, fixits }`, `Severity {Error, Warning,
  Runtime}`, JSON serialization, a `Diagnostics` collection.
- `error`: `FeatureId`, `Unimplemented` (exact shape and message of §12.1), `InternalError { what, file, line,
  backtrace }`, `unimplemented_feature!`, `bug!` (returns `InternalError`; panics when `debug_assertions` or
  `BLOSSOM_PANIC_ON_BUG=1`).
- `codes`: `REGISTRY: &[CodeInfo { code, severity, owner_crate, meaning }]` for every `BLSnnnn`/`BLSRnnn` in LANGUAGE
  §20 plus ARCHITECTURE §0.3 amendments (BLSR011, BLS0908, BLS1009, BLS1010, BLS1003 extension). Owner crates follow
  ARCHITECTURE §13.1 phase table (lexer/parser → syntax, resolve/typeck/classify/spec/lock → front, stratification,
  CALM, determinism, ACL → analysis, BLSR001–011 → engine, etc.).
- `det`: `DetMap`, `DetSet` over `hashbrown::HashTable` with a keyed folded-multiply hasher (`DetState::fixed()` and
  `DetState::from_nonce(u64)`); document that iteration order must never be observable.
- `graph`: Tarjan SCC (iterative, no recursion depth limit), topological sort, condensation DAG, BFS shortest cycle
  through an edge (for witnesses), Hopcroft–Karp maximum bipartite matching, minimum chain cover helper.
- `pub const API_VERSION: u32 = 1;`

**4. `blossom-value` type surface (frozen; bodies of non-trivial functions return `Unimplemented` naming the feature,
implemented by M2.1).** Modules: `types` (TypeTable/TypeDef/IntTy/StructDef/EnumDef/FieldDef/VariantDef/FieldNo/
ExternTypeDef exactly as ARCHITECTURE §2.2; **TypeTable fully implemented**: structural hash-consed insert, get,
iteration), `value` (`Value` covering every `TypeDef`: ints stored with their `IntTy`, f64, Str(Arc<str>),
Bytes(Arc<[u8]>), Duration, Instant, Mod{bits ≤ 256, words}, Blob{hash, len}, Session, Principal, Node(NodeId),
Tuple, Struct, Enum{variant, fields}, Vec, Set(BTreeSet), Map(BTreeMap), Option, `Lattice(LatValue)`,
`Group(GroupValue)`, Extern{codec, bytes}; `LatValue`/`GroupValue` are *data* whose operations live in
blossom-lattice), `order` (**implemented**: the canonical total order of LANGUAGE §5.5 as `impl Ord for Value`,
f64 by IEEE totalOrder, never by any id; LANG-024), `word` (`Word`, `Lane`, `ColEncTag`, `StrWord`, `BytesWord`,
scalar encode/decode signatures), `fp` (`Fingerprint`, fingerprint fns), `prf` (`Seeds`, PRF fns, `PrfStream`),
`digest` (`Digest128`, `Digest256`, set-hash fns), `store` (`ValueStore`, `RecordBuilder`, `RefValueStore` type),
`sink` (`WordSink`, `IngestSlot`, `RowMeta`), `externs` (`ExternFn`, `ExternTableFn` traits, `ExternRegistry`
struct: register/lookup/`unbound(paths)` implemented, signature checks deferred to M2.1), `time` (`Tick`, `NodeId`,
`Instant`, `Duration`, `Incarnation` with serde), `class` (`MonoClass`, `LatOpKind`, `HeightClass`, `LawStatus`,
`Claim`, `ProofStatus`; re-exported later from `blossom-ir::core::lattice` so ARCHITECTURE paths stay valid),
`error` (`ValueError`). Feature `arbitrary` declared (implemented by M2.1). `API_VERSION = 1`.

**5. `blossom-std-src`.** `build.rs` walks `../../std/**/*.bls`, emits `rerun-if-changed` for the directory and each
file, and generates `STD_SOURCES: &[(&str /*module path, e.g. "std::bcast::reliable"*/, &str /*source*/)]` sorted by
path plus `pub fn source(path: &str) -> Option<&'static str>`. Loading is per module and lazy: a broken
`std/foo.bls` must not affect a program that does not import `std::foo`. Works with an empty `std/`.

**6. xtask.** `check-layers`: reads `xtask/layers.toml` (layers L0–L5 of §1.2, plus `tests` and `systems` layers),
runs `cargo metadata --format-version 1`, checks every normal, build **and** dev edge against the allowed edge set
and the forbidden rules of §1.3 (kernel→ir, oracle→{kernel,engine,plan,analysis}, engine→{plan,node,prov,tokio},
node/sim/runtime→{syntax,front,analysis,rewrite,plan,driver}, no systems/* normal path to blossom-driver, testkit
only as dev-dep of integration-test crates), prints each violation with the path. `check-sans-io`: fails on
`std::fs`, `std::net`, `std::thread`, `std::time::Instant`, `tokio` in `crates/blossom-node/src/**`.
`check-codes`: every `BLS[R]?\d{3,4}` literal in `crates/**/src` is registered, is constructed only in its owner
crate, and the registry equals the tables of LANGUAGE §20 (parsed from the markdown) plus the §0.3 amendments. All
other subcommands (`corpus`, `bless`, `coverage`, `crashcheck`, `fetch-datasets`, `gen-ast`, `gen-codegen-corpus`,
`check-codegen-abi`, `bench-report`, `release`, `gen-docs`) are placeholder modules that exit 7 naming their WP.

**7. Scripts.** `scripts/ci.sh [fast|gate]` runs every `scripts/ci.d/NN-*.sh` fragment whose tier matches (fragments
are owned by the WP that adds them; this WP adds `10-fmt`, `20-clippy`, `30-layers` (check-layers, check-sans-io,
check-codes), `40-test`). `scripts/wp-check.sh <crate>...` runs, per crate, `cargo fmt -p C --check`,
`cargo clippy -p C --all-targets --all-features -- -D warnings`, `cargo test -p C --all-features`, then check-layers
and check-codes. `scripts/require-tests.sh <crate> <substring>...` lists the crate's tests
(`cargo test -p C --all-features -- --list`), fails naming every substring that matches no test, then runs them.
`scripts/milestone-gate.sh Mk` implements PLAN §3. `scripts/collect-notes.sh` appends new `## Bugs` entries from
`docs/plan/notes/*.md` to `docs/plan/BUGS.md` with attribution. `scripts/install-dev-tools.sh` installs cargo-deny,
cargo-hack and cargo-nextest with `cargo install --locked --root .tools` (never globally); `ci.sh` prepends
`.tools/bin` to `PATH` and runs cargo-deny only in the `gate` tier when installed (reporting clearly when not).

**8. Docs.** `docs/dev/CONVENTIONS.md`: the coding rules every WP follows (error handling, `// FEATURE: ID` markers at
implementing sites, test naming so `require-tests.sh` substrings match, module layout, logging, no `HashMap`, no
silent defaults, how to add an external dependency, how to record bugs). `docs/plan/BUGS.md` (empty table),
`docs/plan/notes/README.md`, `docs/plan/MILESTONE` (contains `M1`; the gate script advances it),
`docs/design/DEPENDENCIES.md` (every external crate with reason and license).

**Pitfalls.**
- Resolver 3 + edition 2024: verify `cargo build --workspace` from a clean target dir.
- Do not let placeholders pull heavy dependencies; keep `cargo build --workspace` under ~2 minutes from clean.
- `Value`'s `Ord` must be a real total order consistent with `Eq`, including `f64` (totalOrder, `-0.0 < +0.0`,
  NaNs ordered by payload) and ints of different `IntTy` never compared (type error upstream; order by `IntTy` first).
- `check-codes` must parse LANGUAGE §20 tables robustly (pipe tables with a code column).

**Required tests.** base: `idx_`, `symbol_orders_by_text`, `source_db_line_col`, `diagnostic_json_roundtrip`,
`unimplemented_macro_message`, `bug_macro`, `det_map_deterministic`, `tarjan_`, `topo_sort_`, `hopcroft_karp_`
(proptest against brute force), `chain_cover_`, `codes_registry_unique`. value: `type_table_dedup`,
`canonical_order_total` (proptest: antisymmetry, transitivity, consistency with Eq), `canonical_order_f64`,
`canonical_order_nested`, `value_serde_roundtrip`. std-src: `std_sources_lazy_lookup`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo build --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run -q -p xtask -- check-layers
cargo run -q -p xtask -- check-sans-io
cargo run -q -p xtask -- check-codes
scripts/ci.sh fast
scripts/require-tests.sh blossom-base idx_ symbol_orders_by_text source_db_line_col diagnostic_json_roundtrip unimplemented_macro_message det_map_deterministic tarjan_ hopcroft_karp_ chain_cover_ codes_registry_unique
scripts/require-tests.sh blossom-value type_table_dedup canonical_order_total canonical_order_f64 value_serde_roundtrip
scripts/require-tests.sh blossom-std-src std_sources_lazy_lookup
sh -c 'cargo run -q -p blossom-cli -- check examples/e01_kvs.bls; test $? -eq 7'
```

#### M1.2 — Golden corpus I: core semantics (BENCH-001–049) and the corpus README

- **Size:** ~5k lines of cases
- **Depends on:** —
- **Owns:** `tests/corpus/README.md`, `tests/corpus/core/**`, `docs/plan/notes/M1.2.md`
- **Features:** BENCH-001, BENCH-002, BENCH-003, BENCH-004, BENCH-005, BENCH-006, BENCH-007, BENCH-008, BENCH-009, BENCH-010, BENCH-011, BENCH-012, BENCH-013, BENCH-014, BENCH-015, BENCH-016, BENCH-017, BENCH-018, BENCH-019, BENCH-020, BENCH-021, BENCH-022, BENCH-023, BENCH-024, BENCH-026, BENCH-027, BENCH-028, BENCH-029, BENCH-030, BENCH-031, BENCH-032, BENCH-033, BENCH-034, BENCH-035, BENCH-036, BENCH-037, BENCH-038, BENCH-039, BENCH-040, BENCH-041, BENCH-042, BENCH-043, BENCH-044, BENCH-045, BENCH-046, BENCH-047, BENCH-048
- **Consumes:**
  - PLAN §5 manifest schema v1
  - FEATURES.md §1 (CR-xx), §3 (SEM), §11.1
  - LANGUAGE.md
  - R01–R03, R08, R09, R12
- **Provides:**
  - tests/corpus/core/** cases with manifests and programs
  - tests/corpus/README.md (schema v1 for readers)

**Rules for every golden-corpus WP (M1.2–M1.5).**
- Follow the normative manifest schema v1 in PLAN §5 exactly; validate with
  `python3 tests/corpus/tools/check_manifests.py <area>` (provided with the plan; stdlib only).
- Expected results come from the literature (FEATURES.md §11 item text, the cited papers and research reports
  R01–R15), **never** from running our implementation, which does not exist yet. Record the derivation in
  `source` and, for anything non-obvious, a comment in the manifest.
- Programs are **self-contained** Blossom programs (a `program NAME version 1;` root) written exactly to
  LANGUAGE.md; they must not import `std::` (the standard library does not exist yet; std cases are written by the
  std WPs). Port the original program faithfully (Bud, Dedalus, Overlog, paper figure) and cite it.
- Where the source is Dedalus, also add a `program.ded` twin case (suffix `d`) so both frontends are exercised.
- One directory per case: `tests/corpus/<area>/<BENCH-id>[<letter>]-<slug>/` containing `manifest.toml`,
  `program.bls` or `program.ded`, optional `spec.bls`, optional `expected/`. A BENCH item with several scenarios
  gets several cases (`BENCH-020a`, `BENCH-020b`, …).
- Initial status: every listed backend `status = "unimplemented"`, `unimplemented` = the case's `features`, and
  `until` computed by the rule of PLAN §5.4 from `docs/design/plan.json` (the latest primary-implementer milestone
  over the listed non-BENCH features, floored by the backend's floor).
- Every P0 and P1 BENCH id in the WP's range gets at least one case; P2 ids are skipped (they belong to M15).
- Also write `docs/plan/notes/<WP>.md` listing every judgement call (ambiguous expected result, re-derived program)
  so the M6.7/M8.8 corpus triage WPs can revisit them.

**Scope.** Every P0/P1 item of FEATURES §11.1 except BENCH-049 (the replay/oracle harness itself, delivered by
M7.3) and BENCH-025 (P2). Examples of what each case must pin down:
- BENCH-001/002 (DL11 Ex. 3/4): per-tick contents with `holds`/`absent` ranges, including the insert-wins tick.
- BENCH-003/016: compile-only cases (`[backend.compile]` with `[[expect_diag]]`: BLS0500, BLS0400…).
- BENCH-012/013/014: key conflicts (BLSR001 via `[[expect_error]]`), exact-match delete, upsert timing.
- BENCH-015/047: stratification acceptance and rejection (BLS0502/BLS0503 with the cycle).
- BENCH-018/019: P2 seAtomicity and NR09 Figs 1–2 with the CR-03/CR-04 answers (SEM-013).
- BENCH-020–024: path/shortest-path programs with full expected relations.
- BENCH-030: naive vs semi-naive equality (same expected relation; add `[perf]` only if the source gives numbers).
- BENCH-031–035: lattice SSSP, the `size` non-morphism trap, CC/SSSP via MIN.
- BENCH-036/037: tick behavior and incremental maintenance across many ticks (inserts and deletes).
- BENCH-040–048: choice under persistence, retraction of chosen/non-chosen candidates, `index!` slots across ticks,
  `choose_rand!` under a fixed seed (record the exact expected choice only if it is derivable from the PRF
  definition; otherwise assert the *validity* properties, e.g. one winner per group, stability across ticks),
  Raft vote among same-tick RequestVotes, `fold_ordered` over a growing log, multi-FD choice, choice
  stratification, ties in top/sort/percentile by canonical order.
- Also write `tests/corpus/README.md`: a reader's guide to the layout, the schema (verbatim from PLAN §5), the
  status ratchet and the backend floors.

**Acceptance** (every command must pass from the repository root):

```sh
python3 tests/corpus/tools/check_manifests.py core
python3 tests/corpus/tools/check_manifests.py core --require-ids BENCH-001..048 --skip BENCH-025
test -f tests/corpus/README.md
```

#### M1.3 — Golden corpus II: lattices and lattice provenance (BENCH-050–079, BENCH-300–313)

- **Size:** ~4.5k lines of cases
- **Depends on:** —
- **Owns:** `tests/corpus/lattices/**`, `tests/corpus/lprov/**`, `docs/plan/notes/M1.3.md`
- **Features:** BENCH-050, BENCH-051, BENCH-052, BENCH-053, BENCH-054, BENCH-055, BENCH-056, BENCH-057, BENCH-058, BENCH-059, BENCH-060, BENCH-061, BENCH-062, BENCH-063, BENCH-064, BENCH-066, BENCH-067, BENCH-068, BENCH-069, BENCH-070, BENCH-073, BENCH-074, BENCH-075, BENCH-076, BENCH-078, BENCH-079, BENCH-300, BENCH-301, BENCH-302, BENCH-303, BENCH-304, BENCH-305, BENCH-306, BENCH-307, BENCH-308, BENCH-309, BENCH-310, BENCH-311, BENCH-312, BENCH-313
- **Consumes:**
  - PLAN §5 manifest schema v1
  - FEATURES.md §2.7, §3.3, §3.10, §11.2, §11.10
  - R04, R11, R13
- **Provides:**
  - tests/corpus/lattices/** and tests/corpus/lprov/** cases

**Rules for every golden-corpus WP (M1.2–M1.5).**
- Follow the normative manifest schema v1 in PLAN §5 exactly; validate with
  `python3 tests/corpus/tools/check_manifests.py <area>` (provided with the plan; stdlib only).
- Expected results come from the literature (FEATURES.md §11 item text, the cited papers and research reports
  R01–R15), **never** from running our implementation, which does not exist yet. Record the derivation in
  `source` and, for anything non-obvious, a comment in the manifest.
- Programs are **self-contained** Blossom programs (a `program NAME version 1;` root) written exactly to
  LANGUAGE.md; they must not import `std::` (the standard library does not exist yet; std cases are written by the
  std WPs). Port the original program faithfully (Bud, Dedalus, Overlog, paper figure) and cite it.
- Where the source is Dedalus, also add a `program.ded` twin case (suffix `d`) so both frontends are exercised.
- One directory per case: `tests/corpus/<area>/<BENCH-id>[<letter>]-<slug>/` containing `manifest.toml`,
  `program.bls` or `program.ded`, optional `spec.bls`, optional `expected/`. A BENCH item with several scenarios
  gets several cases (`BENCH-020a`, `BENCH-020b`, …).
- Initial status: every listed backend `status = "unimplemented"`, `unimplemented` = the case's `features`, and
  `until` computed by the rule of PLAN §5.4 from `docs/design/plan.json` (the latest primary-implementer milestone
  over the listed non-BENCH features, floored by the backend's floor).
- Every P0 and P1 BENCH id in the WP's range gets at least one case; P2 ids are skipped (they belong to M15).
- Also write `docs/plan/notes/<WP>.md` listing every judgement call (ambiguous expected result, re-derived program)
  so the M6.7/M8.8 corpus triage WPs can revisit them.

**Scope.** FEATURES §11.2 (P0/P1; skip BENCH-065, BENCH-077 which are P2) and §11.10 (T1–T14).
- Lattice programs use only built-in lattices and LANGUAGE §11 syntax; user lattices only where the item requires.
- BENCH-062/066 (law suites) become small programs whose outputs exercise every operation (`[backend.oracle]`);
  the law harness itself is M3.1's.
- BENCH-073/074/075/078 (wrapped channels, W2/W3, dot reuse) use `exactly_once` channels and `[[fault]]` scripts;
  BENCH-078 expects BLSR011.
- BENCH-300–313: each T-case states its expected ultimate value, the expected LDFI falsifier sets (as
  `[expect_ldfi] falsifiers`) where R11 gives them, and for BENCH-313 the FLP-vs-GZ oracle outcome. Use
  `crash_view = "frozen"` for `.bls` programs.
- BENCH-306/307: convergence within a tick and the BLSR007 hard error; the diverging ultimate value (T8) is an
  `[expect_analysis]`/sim expectation of an *inconclusive* or limit result as R11 states it.

**Acceptance** (every command must pass from the repository root):

```sh
python3 tests/corpus/tools/check_manifests.py lattices --require-ids BENCH-050..079 --skip BENCH-065,BENCH-077
python3 tests/corpus/tools/check_manifests.py lprov --require-ids BENCH-300..313
```

#### M1.4 — Golden corpus III: asynchrony, analysis oracles, core networking and verification

- **Size:** ~4k lines of cases
- **Depends on:** —
- **Owns:** `tests/corpus/async/**`, `tests/corpus/net/BENCH-100*/**`, `tests/corpus/net/BENCH-101*/**`, `tests/corpus/net/BENCH-102*/**`, `tests/corpus/verify/**`, `docs/plan/notes/M1.4.md`
- **Features:** BENCH-080, BENCH-082, BENCH-083, BENCH-084, BENCH-085, BENCH-086, BENCH-087, BENCH-088, BENCH-089, BENCH-090, BENCH-091, BENCH-092, BENCH-093, BENCH-094, BENCH-095, BENCH-097, BENCH-098, BENCH-099, BENCH-100, BENCH-101, BENCH-102, BENCH-150
- **Consumes:**
  - PLAN §5 manifest schema v1
  - FEATURES.md §6, §11.3, §11.4, §11.6
  - R02, R03, R05, R10, R13
- **Provides:**
  - tests/corpus/async/**, tests/corpus/net/BENCH-100..102, tests/corpus/verify/** cases

**Rules for every golden-corpus WP (M1.2–M1.5).**
- Follow the normative manifest schema v1 in PLAN §5 exactly; validate with
  `python3 tests/corpus/tools/check_manifests.py <area>` (provided with the plan; stdlib only).
- Expected results come from the literature (FEATURES.md §11 item text, the cited papers and research reports
  R01–R15), **never** from running our implementation, which does not exist yet. Record the derivation in
  `source` and, for anything non-obvious, a comment in the manifest.
- Programs are **self-contained** Blossom programs (a `program NAME version 1;` root) written exactly to
  LANGUAGE.md; they must not import `std::` (the standard library does not exist yet; std cases are written by the
  std WPs). Port the original program faithfully (Bud, Dedalus, Overlog, paper figure) and cite it.
- Where the source is Dedalus, also add a `program.ded` twin case (suffix `d`) so both frontends are exercised.
- One directory per case: `tests/corpus/<area>/<BENCH-id>[<letter>]-<slug>/` containing `manifest.toml`,
  `program.bls` or `program.ded`, optional `spec.bls`, optional `expected/`. A BENCH item with several scenarios
  gets several cases (`BENCH-020a`, `BENCH-020b`, …).
- Initial status: every listed backend `status = "unimplemented"`, `unimplemented` = the case's `features`, and
  `until` computed by the rule of PLAN §5.4 from `docs/design/plan.json` (the latest primary-implementer milestone
  over the listed non-BENCH features, floored by the backend's floor).
- Every P0 and P1 BENCH id in the WP's range gets at least one case; P2 ids are skipped (they belong to M15).
- Also write `docs/plan/notes/<WP>.md` listing every judgement call (ambiguous expected result, re-derived program)
  so the M6.7/M8.8 corpus triage WPs can revisit them.

**Scope.** FEATURES §11.3 P0/P1 items (skip BENCH-081 and BENCH-096, P2), BENCH-100–102 of §11.4 (the rest of §11.4
is written by the std WPs because it tests std modules; BENCH-103 is an integration test of M11.3), and BENCH-150.
- Asynchrony items need `[backend.sim]` with `[run] seeds ≥ 16` and expectations that must hold in *every* seed
  (confluent outputs) or `[backend.analysis]` + `[expect_analysis]` verdicts for non-confluent ones (BENCH-085/086/091: marriage ceremony,
  concurrent arrival, async singleton, Ameloot message join must *fail* to certify fair consistency, CR-29).
- BENCH-087–089 (TPLP/CRON figures): encode the program and the expected ultimate models.
- BENCH-090/093/094/095 (CALM oracles, CIDR'11 golden analyses, Blazes, Edelweiss): `[backend.analysis]` with
  `[expect_analysis]` points of order, labels, certificates, Blazes sink labels, reclaimable relations.
- BENCH-097–099: finality classes, finality soundness under fuzzing (`[backend.sim]`, many seeds), batch completion
  by seals and thresholds.
- Include the CALM-pruning regression program of ARCHITECTURE §6.2 (`on put(k) { emit got(k); }` +
  `while got(1), not got(2) { send alarm(1) to MONITOR; }`) as `async/BENCH-090x-calm-pruning-regression`.
- BENCH-150 (Paxos Made EPR): the Paxos program and a spec with the paper's inductive invariant and
  `check smt { … } expect holds`, `[backend.smt]` with `[expect_verify] check = "smt"`, `result = "holds"`.

**Acceptance** (every command must pass from the repository root):

```sh
python3 tests/corpus/tools/check_manifests.py async --require-ids BENCH-080..099 --skip BENCH-081,BENCH-096
python3 tests/corpus/tools/check_manifests.py net --require-ids BENCH-100..102
python3 tests/corpus/tools/check_manifests.py verify --require-ids BENCH-150
```

#### M1.5 — Golden corpus IV: the Molly LDFI corpus (BENCH-130–137)

- **Size:** ~3k lines of cases
- **Depends on:** —
- **Owns:** `tests/corpus/ldfi/**`, `third_party/molly/**`, `docs/plan/notes/M1.5.md`
- **Features:** BENCH-130, BENCH-131, BENCH-132, BENCH-133, BENCH-134, BENCH-135, BENCH-136, BENCH-137
- **Consumes:**
  - PLAN §5 manifest schema v1
  - FEATURES.md §7.2, §11.5
  - R06 (Molly/LDFI)
  - ARCHITECTURE §8.7, §11.5
- **Provides:**
  - tests/corpus/ldfi/molly/** Molly programs (.ded) with golden verdicts, run counts and normalized falsifier sets

**Rules for every golden-corpus WP (M1.2–M1.5).**
- Follow the normative manifest schema v1 in PLAN §5 exactly; validate with
  `python3 tests/corpus/tools/check_manifests.py <area>` (provided with the plan; stdlib only).
- Expected results come from the literature (FEATURES.md §11 item text, the cited papers and research reports
  R01–R15), **never** from running our implementation, which does not exist yet. Record the derivation in
  `source` and, for anything non-obvious, a comment in the manifest.
- Programs are **self-contained** Blossom programs (a `program NAME version 1;` root) written exactly to
  LANGUAGE.md; they must not import `std::` (the standard library does not exist yet; std cases are written by the
  std WPs). Port the original program faithfully (Bud, Dedalus, Overlog, paper figure) and cite it.
- Where the source is Dedalus, also add a `program.ded` twin case (suffix `d`) so both frontends are exercised.
- One directory per case: `tests/corpus/<area>/<BENCH-id>[<letter>]-<slug>/` containing `manifest.toml`,
  `program.bls` or `program.ded`, optional `spec.bls`, optional `expected/`. A BENCH item with several scenarios
  gets several cases (`BENCH-020a`, `BENCH-020b`, …).
- Initial status: every listed backend `status = "unimplemented"`, `unimplemented` = the case's `features`, and
  `until` computed by the rule of PLAN §5.4 from `docs/design/plan.json` (the latest primary-implementer milestone
  over the listed non-BENCH features, floored by the backend's floor).
- Every P0 and P1 BENCH id in the WP's range gets at least one case; P2 ids are skipped (they belong to M15).
- Also write `docs/plan/notes/<WP>.md` listing every judgement call (ambiguous expected result, re-derived program)
  so the M6.7/M8.8 corpus triage WPs can revisit them.

**Scope.** BENCH-130–137: the delivery family, commit protocols (2PC, 2PC-CTP, 3PC), Kafka ISR, protocols expected
to pass, Nemo case studies, Molly unit tests, run counts, `raft.ded` and `negative_support_test`.
- **Vendoring.** Fetch https://github.com/palvaro/molly (and the Nemo/LDFI artifacts R06 cites). If the repository
  carries a license that permits redistribution, vendor the `.ded` programs verbatim under
  `tests/corpus/ldfi/molly/<case>/program.ded` with the license in `third_party/molly/`; otherwise re-derive each
  program from the paper figures and R06, set `derived = true`, and never copy unlicensed text.
- Manifests use `[backend.ldfi]` and `[expect_ldfi]` with `eot`, `eff`, `crashes`, `nodes`,
  `crash_view = "molly"`, the expected verdict, `runs_max` where BENCH-136 publishes a count, and normalized
  **Appendix-B-minimal** falsifier sets where the source states them (ARCHITECTURE §8.7: never raw Molly output).
- Where the failure-free run's outcome is derivable, add a `[backend.oracle]` sync run with `[[expect]]` rows for
  `post` so the `.ded` frontend and oracle can be checked before LDFI exists.

**Acceptance** (every command must pass from the repository root):

```sh
python3 tests/corpus/tools/check_manifests.py ldfi --require-ids BENCH-130..137
```

### M2 — Values, the core IR, the parser, solvers and storage I/O

**Goal.** Implement the value model, the Dedalus^L IR with its builder, validator, printer and digest, the Blossom lexer and parser, the SAT and SMT layers, and the byte-level durability layer. Every WP here consumes only blossom-base and the blossom-value type surface from M1.

**Gate.** `scripts/milestone-gate.sh M2` (PLAN §3).

#### M2.1 — blossom-value: encodings, fingerprints, PRF, digests, reference store and externs

- **Size:** ~5k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-value/**`, `docs/plan/notes/M2.1.md`
- **Features:** LANG-022, LANG-023, LANG-026, LANG-174, LANG-175, SEM-084, ENG-032, DIST-032
- **Consumes:**
  - blossom-base
  - blossom-value type surface (M1.1)
- **Provides:**
  - complete blossom-value: order-preserving scalar encodings and lanes, xxh3 Merkle fingerprints, SipHash-1-3 PRF with Seeds and PrfStream decision streams, BLAKE3 digests and 128-bit incremental set hashes, RefValueStore (a complete ValueStore), ExternRegistry signature checks, Mod arithmetic and ring intervals, `arbitrary` generators for every TypeDef, known-answer vectors

**Build.** Every body that M1.1 left as `Unimplemented`:
- **Scalar encodings** (ARCHITECTURE §4.1 table): word order = canonical order; u8–u64/Mod≤64 zero-extended;
  i8–i64/Duration/Instant sign-extended then top-bit flipped per lane; f64 totalOrder key; `Node` = dense id;
  `Option` niche (`0 = None`, `enc+1 = Some`) for niche-capable types; lane selection (`U32` iff every column fits).
- **Fingerprints** (ENG-032): xxh3-64 over the canonical encoding; records/tuples/collections Merkle-style over child
  fingerprints (O(arity)); sets/maps over canonically sorted elements; independent of any intern id; `ENCODING_VERSION`.
- **PRF and seeds** (SEM-084, LANG-174/175): SipHash-1-3; `Seeds { deployment, choice (σc), node (σnode) }` derived
  per node and incarnation exactly as FEATURES SEM-084/085 state; `prf(site_key, fp(X̄), fp(Ȳ))` for `$prio`,
  `(σnode, incarnation, tick, site, …)` for `$rprio`/`rand*`; `PrfStream::new(root, purpose, identity)` for
  independent simulator decision streams (ARCH-12); `PRF_VERSION`.
- **Digests**: BLAKE3-256 helpers; `Digest128` as a sum mod 2^128 of `H(domain, fp…)` with `add`/`remove` so digests
  are order independent and incremental (§4.11).
- **RefValueStore**: a straightforward complete `ValueStore` (interning `Value`s, canonical compare through `Value`
  order, fingerprints, `to_value`), used by oracle-free tests, the wire tests and host code.
- **Externs**: `ExternRegistry` checks each registration's signature against the declared `FnDecl` types when bound;
  `unbound()` lists every missing path (a load-time error upstream, never a runtime stub).
- **Mod and ring intervals** (LANG-026): N-bit modular arithmetic (≤ 256 bits), shifts, the `0x…I` literal value
  constructor, ring-interval membership for all four interval kinds with wrap-around (`(n, n]` covers the ring).
- **`arbitrary` feature**: proptest strategies producing values of any given `TypeDef` (including nested and
  lattice/group data shapes).

**Pitfalls.** Endianness independence of fingerprints and PRF (fixed little-endian byte feeding); `i128`/`u128`
columns never fit a word (they are Interned); Duration arithmetic overflow is an error value, not a wrap; the KAT
files are normative once committed (changing them requires bumping `ENCODING_VERSION`/`PRF_VERSION`).

**Required tests.** `encoding_order_preserving` (proptest: `a < b ⇔ enc(a) < enc(b)` for every scalar type, both
lanes), `encoding_roundtrip`, `option_niche_`, `fingerprint_kat` (checked-in vectors), `fingerprint_merkle_`,
`prf_kat`, `prf_stream_independent`, `digest128_order_independent`, `ref_store_`, `extern_registry_unbound_listed`,
`mod_arith_`, `ring_interval_wraps`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-value
scripts/require-tests.sh blossom-value encoding_order_preserving encoding_roundtrip option_niche_ fingerprint_kat fingerprint_merkle_ prf_kat prf_stream_independent digest128_order_independent ref_store_ extern_registry_unbound_listed mod_arith_ ring_interval_wraps
```

#### M2.2 — blossom-ir I: the Dedalus^L IR, IrBuilder, validator, printer, digest and projection

- **Size:** ~8k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-ir/**`, `docs/plan/notes/M2.2.md`
- **Features:** ENG-001, LANG-001, SEM-100
- **Consumes:**
  - blossom-base
  - blossom-value type surface (M1.1): TypeTable, Value (Ord), class enums, time types
- **Provides:**
  - blossom-ir::core (all of ARCHITECTURE §2.1–2.8), ::plan data model (§3.1–3.2 types, CExpr, NativeOp specs, PlanLimits, PlanProfile), ::strata, ::obs, ::spec; IrBuilder/RuleBuilder; ValidatedProgram (validate/get/digest/project); IrError; the LANGUAGE §4 printer; canonical relabeling digest (ProgramDigest, PlanDigest); postcard serialization; Expr::time_varying; API_VERSION

**Build.**
- **Types**, exactly as ARCHITECTURE §2.1–2.8 and §2.11 show them (names, fields, ownership). Re-export
  `blossom_value::class::*` from `core::lattice` so the documented paths hold (PLAN §4 D2). The IR does not depend
  on `blossom-lattice`: `LatticeDef.ops` is data filled by frontends.
- **Plan data model** (§3.1–3.2): `PhysicalProgram`, `ValidatedPlan` (type only; its validator is M3.2), `PhysRel`,
  `RowShapeSpec`, `ColEnc`, `Repr`, `IndexDef`, `PhysStratum`, `RulePlan`, `Regime`, `RegimeReason`,
  `VersionPlan`, `OpTree`, `Op`, `Read`, `Source`, `Sink`, `CExpr` (slots, pre-encoded constants, resolved calls),
  `NativeOp` with one spec variant per native of §3.6, `AggTablePlan`, `BufferPlan`, `IngestPlan` (including
  per-channel `branching` bits), `ProvPlan`, `DigestPlan`, `PlanLimits`, `PlanProfile`, `PlanDigest`.
- **IrBuilder / RuleBuilder** (§2.9) including the construct stack (rules and generated relations declared between
  `begin_construct`/`end_construct` belong to it), sites, invariants, facts, `FrontendKind`.
- **Validator V1–V12** (§2.9) with `IrError(Box<IrErrorKind>)`; V5 here covers `Persist`/`Identity` exactness and
  the structural part for every construct (rules and relations belong to exactly one construct; generated names
  contain `$`); the per-construct expansion agreement checks are M3.2.
- **`ValidatedProgram`**: private `Arc<Program>`; `validate`, `get`, cached `digest`, `project(role)` (§2.10: guarded
  rules, send/receive sides, referenced shared items, `R$members`, guards removed).
- **Canonical digest** (§2.10): BLAKE3-256 over a canonical serialization with spans removed after canonical
  relabeling of relations, rules, sites, constructs, invariants, functions, types, lattices, groups, constants and
  variables, exactly in the order §2.10 specifies.
- **Printer**: LANGUAGE §4 notation (§4.1), showing constructs as comments with surface labels, generated `$` names,
  eliding the received channel's column 0 as §2.5 specifies; deterministic output.
- **Serialization**: serde + postcard for `Program`, `SpecProgram`, `PhysicalProgram` (headers come from
  blossom-artifact in M3).

**Pitfalls.** The digest must not depend on `IndexVec` order, `TypeTable` insertion order, `Symbol` ids or spans.
`IrError`s from the Blossom frontend are internal errors, but the `.ded` frontend renders V1–V4 as user diagnostics:
make the error carry enough span information for both. Keep `Op`/`CExpr` free of engine types.

**Required tests.** `validator_v1_` … `validator_v12_` (at least one rejecting and one accepting program each),
`digest_invariant_under_renumbering` (proptest permuting every id space), `digest_ignores_spans`,
`project_role_`, `printer_` (insta snapshots), `postcard_roundtrip_program`, `builder_construct_membership`,
`time_varying_scalars`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-ir
scripts/require-tests.sh blossom-ir validator_v1_ validator_v2_ validator_v3_ validator_v4_ validator_v5_ validator_v6_ validator_v7_ validator_v8_ validator_v9_ validator_v10_ validator_v11_ validator_v12_ digest_invariant_under_renumbering digest_ignores_spans project_role_ printer_ postcard_roundtrip_program builder_construct_membership time_varying_scalars
```

#### M2.3 — blossom-syntax I: lexer, parser, lossless CST and the typed AST

- **Size:** ~7.5k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-syntax/**`, `!crates/blossom-syntax/src/ded/**`, `xtask/src/cmd/gen_ast.rs`, `fuzz/fuzz_targets/lexer_parser.rs`, `tests/corpus/**/*.bls`, `examples/**`, `docs/plan/notes/M2.3.md`
- **Features:** LANG-002, LANG-084, LANG-208
- **Consumes:**
  - blossom-base (Span, FileId, Diagnostic, codes)
  - LANGUAGE.md §2–§3
  - ARCHITECTURE §13.2–13.3
  - examples/*.bls and tests/corpus/**/program.bls (M1) as parser inputs
- **Provides:**
  - blossom_syntax::{SyntaxKind, lexer, parser::parse(file, text) -> Parse { green, errors }, ast::*} (typed AST generated from crates/blossom-syntax/blossom.ungram)
  - `cargo xtask gen-ast`

**Build.**
- `SyntaxKind` (u16) for every token and node kind of ARCHITECTURE §13.2; hard keywords of LANGUAGE §2.3 each get a
  kind; contextual keywords are `IDENT` recognized by position (LANGUAGE Appendix C).
- Hand-written lexer with the rules of §13.2: `#` comments except before `[`, `![` or a digit (LANG-208);
  `FIELD_NUM`; `BANG_IDENT`; raw identifiers; numeric suffix and duration literals (BLS0003); `0x…I` modular
  literals; strings, raw strings and byte strings with escapes (BLS0002, BLS0005); nesting block comments;
  longest-match punctuation. Trivia is kept.
- Recursive-descent parser for items, declarations, statements and bodies; a Pratt parser with LANGUAGE §3.3
  precedence (LANG-084: standard precedence; non-associative comparisons and ranges report BLS0103); the
  fixed-lookahead rules 1–8 of §13.2 (labels, contextual items, contextual literal prefixes, `no_struct`
  contexts, named arguments, bang-call clauses, `>>` splitting, statement-only blocks with BLS0102). Emit
  events → rowan `GreenNode`; never allocate discarded nodes. Recovery per §13.2 (`;`, closing `}`, item keywords at
  line start, `,` at body depth 0; `MISSING` tokens with BLS0101). Every error carries its span and the expected
  token set.
- `crates/blossom-syntax/blossom.ungram`: the grammar of LANGUAGE §3.2 in ungrammar form; `cargo xtask gen-ast`
  (you own `xtask/src/cmd/gen_ast.rs`) generates `src/ast/generated.rs` (checked in); every accessor returns
  `Option` or an iterator.
- Proptest mirror of the `lexer_parser` fuzz target (and implement the fuzz target): no panic on arbitrary input;
  `print(parse(s)) == s`; every error has a span.
- Leave `src/fmt` as it is (M3.7 writes the formatter) and do not touch `src/ded` (M3.6).

**Corpus and examples.** Every `examples/*.bls` and every `tests/corpus/**/program.bls` must parse with zero
errors, except corpus cases whose manifest expects a BLS00xx/BLS01xx diagnostic (the test reads the manifest and
checks that exactly those diagnostics are produced). You may fix *syntax only* in those files (never expectations or meaning); list each fix in your notes file
with the reason. Also extract every complete ```blossom block from LANGUAGE.md (skip fragments by the rule in
LANGUAGE's "Conventions" paragraph) and parse it; a block that LANGUAGE says is complete but does not parse is a
bug to fix in the parser, or, if LANGUAGE.md itself is inconsistent, a note for the M13.4 docs WP.

**Required tests.** `lexer_`, `parser_items_`, `parser_exprs_precedence`, `parser_recovery_`,
`cst_lossless_roundtrip` (proptest), `examples_parse`, `corpus_parse`, `language_md_blocks_parse`,
`gen_ast_up_to_date` (regenerating yields no diff), one `diag_bls00nn`/`diag_bls01nn` test per lexical and
syntactic code.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-syntax
cargo run -q -p xtask -- gen-ast --check
scripts/require-tests.sh blossom-syntax lexer_ parser_items_ parser_exprs_precedence parser_recovery_ cst_lossless_roundtrip examples_parse corpus_parse language_md_blocks_parse gen_ast_up_to_date diag_bls0001 diag_bls0100
```

#### M2.4 — blossom-sat: SatSolver trait, CaDiCaL, batsat, exhaustive and DIMACS backends, cardinality encodings

- **Size:** ~3k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-sat/**`, `docs/plan/notes/M2.4.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - blossom-base
- **Provides:**
  - blossom_sat::{Var, Lit, SatSolver, SatOutcome, SolveLimits, SatError, backends::{CadicalSolver, BatSolver, ExhaustiveSolver, DimacsDump}, card::{totalizer, generalized_totalizer}, conformance::sat_suite, select_backend(name)}

**Build** (ARCHITECTURE §8.6, ARCH-13).
- `SatSolver` trait exactly as §8.6; `Lit = 2·var + negated`.
- `CadicalSolver` over `rustsat` + `rustsat-cadical` (feature `sat-cadical`, default): incremental solving under
  assumptions, failed assumptions, learned clauses kept between calls. Verify the C++ build works with the system
  clang; pin versions in the crate manifest.
- `BatSolver` over `batsat` (feature `sat-batsat`).
- `ExhaustiveSolver` (always): ≤ 24 variables, the conformance oracle.
- `DimacsDump` (always): records clauses and writes `.cnf`.
- `card::totalizer(lits, k)` and `card::generalized_totalizer(weighted, k)`: incremental-friendly (return output
  literals for "sum ≥ j" so bounds can be asserted by assumptions); weighted sums for LDFI thresholds.
- `SolveLimits` (conflict and time budgets); `SatOutcome::Unknown(LimitHit)`; `SatError::BackendUnavailable
  { backend, feature }` when a backend is selected but not compiled.
- `conformance::sat_suite(make: impl Fn() -> Box<dyn SatSolver>)`: random CNFs vs ExhaustiveSolver, assumptions
  and failed assumptions, incremental clause addition after `solve`, full model enumeration with blocking clauses,
  cardinality encodings vs brute force.

**Pitfalls.** No global state in backends (many solvers per process, ENG-101); `value()` only after `Sat`, error
otherwise; deterministic behavior for a fixed input sequence (record the backend version in reports).

**Required tests.** `sat_suite_cadical`, `sat_suite_batsat`, `sat_suite_exhaustive_selfcheck`, `totalizer_`,
`generalized_totalizer_`, `dimacs_roundtrip`, `backend_unavailable_error`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-sat
cargo test -p blossom-sat --no-default-features --features sat-batsat
cargo clippy -p blossom-sat --no-default-features --features sat-batsat --all-targets -- -D warnings
scripts/require-tests.sh blossom-sat sat_suite_cadical sat_suite_batsat totalizer_ generalized_totalizer_ dimacs_roundtrip backend_unavailable_error
```

#### M2.5 — blossom-smt: SMT-LIB2 over child processes (z3, cvc5) and the clingo ASP driver

- **Size:** ~3.5k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-smt/**`, `scripts/install-solvers.sh`, `fuzz/fuzz_targets/smt_response.rs`, `docs/plan/notes/M2.5.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - blossom-base
- **Provides:**
  - blossom_smt::{Sexp, Term, Sort, SmtSolver, SmtProcess, SmtConfig, SmtAnswer, SmtModel, SmtError, AspSolver, ClingoProcess, AspModel, discover(solver)}, conformance::smt_suite, scripts/install-solvers.sh

**Build** (ARCHITECTURE §9.3, ARCH-14).
- `Sexp`/`Term`/`Sort` builders (`app, forall, exists, let_, eq, and, or, not, implies, ite`, bit-vectors, arrays,
  datatypes) and a printer; a robust s-expression response parser (fuzz target `smt_response` + proptest mirror).
- `SmtSolver` trait of §9.3; `SmtProcess::spawn` for z3 (`-in -smt2`) and cvc5 (`--lang=smt2 --incremental
  --produce-models`) with `(set-option :print-success true)`; every command's reply parsed; `(error …)` →
  `SmtError::Solver { command, message }`.
- Timeouts: solver option plus a watchdog thread that kills and respawns the process; a timeout is
  `Unknown(timeout)`, never success. Models (`get-model`: Int, Bool, BitVec, Array, uninterpreted-sort elements),
  unsat cores (named assertions), push/pop.
- Discovery: `$BLOSSOM_Z3`/`$BLOSSOM_CVC5`/`$BLOSSOM_CLINGO`, then `PATH`, then `.tools/bin`;
  `SmtError::SolverNotFound { solver, searched }`.
- `AspSolver` + `ClingoProcess` (text in, `--outf=2` JSON models out, model limits, time limits).
- `--smt-log DIR` transcripts (`SmtConfig::log_dir`).
- **Solver requirement policy** (PLAN §4 D11): `BLOSSOM_REQUIRE_SOLVERS` = `1`/`all` or a comma list (`z3,cvc5`); a
  test needing an absent solver that is not required is reported by libtest-mimic as ignored with
  "not run: <solver> not found (set BLOSSOM_<SOLVER>)"; a required absent solver is a failure. The conformance suite
  is a `harness = false` test binary `tests/smt_suite.rs`.
- `scripts/install-solvers.sh`: installs cvc5 (official release binary for the host OS/arch) into `.tools/bin` and
  clingo into a venv at `.tools/venv` (`pip install clingo`) with a `.tools/bin/clingo` shim; never installs
  globally; idempotent.

**Required tests.** `sexp_parser_`, `printer_roundtrip`, `z3_suite_`, `cvc5_suite_`, `clingo_suite_`,
`timeout_is_unknown`, `solver_not_found_message`, `unsat_core_`, `model_parse_`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-smt
BLOSSOM_REQUIRE_SOLVERS=z3 cargo test -p blossom-smt --test smt_suite
scripts/install-solvers.sh
BLOSSOM_REQUIRE_SOLVERS=all cargo test -p blossom-smt --test smt_suite
scripts/require-tests.sh blossom-smt sexp_parser_ printer_roundtrip timeout_is_unknown solver_not_found_message unsat_core_ model_parse_
```

#### M2.6 — blossom-store I: Vfs, RealFs, SimFs crash model, WAL segments, checkpoint files, META and LOCK

- **Size:** ~7k
- **Depends on:** M1.1
- **Owns:** `crates/blossom-store/**`, `fuzz/fuzz_targets/wal_recovery.rs`, `docs/plan/notes/M2.6.md`
- **Features:** DIST-020
- **Consumes:**
  - blossom-base
  - blossom-value type surface (time types)
- **Provides:**
  - blossom_store::{Vfs, VfsFile, VfsLock, OpenOpts, RealFs, SimFs (+ crash/fault API), StoreIdentity, OpenMode, MetaStore, StoreLock, WalWriter (+ FileWal), WalRecordBuf, Lsn, SyncedUpTo, SyncedTick, TruncateToken, WalScan (recovery scan), CheckpointWriter (+ FileCheckpoints), CheckpointId, DurableSnapshot (opaque encoded relation files), MemDurability, StoreError, conformance::{vfs_suite, wal_suite}, crash::enumerate_crash_points}

**Build** (ARCHITECTURE §5.6, ARCH-10, ARCH-25). This WP is the byte layer: record payloads and checkpoint relation
files are opaque bytes here; M5.4 encodes them with the wire codec and adds catalogs, identity checks and recovery
order.
- **Vfs** trait of §5.6 (open, rename, remove, list, sync_dir, lock_exclusive; files: pread, append, sync_data,
  len, truncate). `RealFs`: `std::fs`, `FileExt::read_at`, `File::sync_data` (F_FULLFSYNC on Apple),
  `File::try_lock`, directory fsync by opening the directory. No `unsafe`.
- **SimFs**: deterministic in-memory filesystem with the POSIX crash model of §6.1: writes are volatile until
  `sync_data` (and `sync_dir` for creates/renames). SimFs itself draws **no randomness**: `crash(&mut self,
  fate: &mut dyn FnMut(&UnsyncedWrite) -> WriteFate)` asks the caller for each unsynced write's fate (survive, lost,
  torn at a 512-byte sector boundary); the simulator supplies PRF-driven fates. Injectable failures: EIO on sync
  (unsynced pages are dropped, as on Linux), ENOSPC on append, short writes.
- **WAL**: segment files `wal/<seq:020>.seg`, one segment per incarnation, rolled at 64 MiB; segment header (magic
  `BLSW`, format, store_uuid, segment_seq, restarts, boot_nonce, an opaque catalog blob with its digest,
  header_crc); records `len crc lsn batch tick now kind payload` with `crc32c` over `len‖lsn‖batch‖tick‖now‖kind‖
  payload` and `lsn` = byte offset in the log stream. `FileWal: WalWriter` (append/sync/truncate_through); a failed
  sync **poisons** the writer forever (every later call errors). Invariant B is the committer's contract; document
  it on the trait.
- **Recovery scan** (`WalScan`): stop at the first invalid record; torn tail vs corruption by the batch rule of §5.6;
  torn tail → truncate + fsync; corruption → refuse naming file and offset.
- **Checkpoints**: `FileCheckpoints: CheckpointWriter` writes `ckpt/<tick>/rel-<id>.dat` + `MANIFEST` (BLAKE3 per
  file) with the fsync order files → dir → MANIFEST → dir → `CURRENT` tmp → rename → dir; `install` returns a
  `TruncateToken`; reading verifies checksums.
- **META** (identity record, node-id map bytes, restart counter, reserved tick bound, last now, understood_version,
  poison deny-list bytes, clean_shutdown) written tmp → fsync → rename → fsync dir; **LOCK** with holder pid; a held
  lock is a refusal naming the holder.
- `SyncedTick` is constructible only from a `SyncedUpTo` returned by a successful `sync` (so a checkpoint can never
  cover unsynced state). `MemDurability` keeps records in memory with a synced watermark (LDFI/BMC only).
- `conformance::{vfs_suite, wal_suite}` run against RealFs (temp dirs), SimFs and MemDurability.
- `crash::enumerate_crash_points(workload)`: runs a scripted WAL + checkpoint workload over SimFs, crashes after
  every durable syscall with every fate combination bound, recovers, and checks: the recovered records are a prefix
  that includes every record acknowledged by a successful sync; recovery never refuses a crash image produced
  without an injected media fault; it always refuses an image with a media fault injected into synced data.

**Pitfalls.** Never retry a failed fsync. A stale record from a previous incarnation must never be accepted as the
continuation of a new segment (segment identity + LSN check). Directory entries are not durable until the directory
is synced. Keep all file writes behind `Vfs` so the simulator runs the real code.

**Required tests.** `vfs_suite_realfs`, `vfs_suite_simfs`, `wal_suite_file_realfs`, `wal_suite_file_simfs`,
`wal_suite_mem`, `torn_tail_truncated`, `corruption_refused_with_offset`, `failed_sync_poisons`,
`checkpoint_fsync_order`, `meta_atomic_write`, `lock_refuses_second_holder`, `crashcheck_store_workload`,
`wal_recovery_fuzz_mirror`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-store
scripts/require-tests.sh blossom-store vfs_suite_realfs vfs_suite_simfs wal_suite_file_realfs wal_suite_file_simfs wal_suite_mem torn_tail_truncated corruption_refused_with_offset failed_sync_poisons checkpoint_fsync_order meta_atomic_write lock_refuses_second_holder crashcheck_store_workload wal_recovery_fuzz_mirror
```

### M3 — Lattices, kernel storage, schemas, name resolution and the Molly frontend

**Goal.** Build the lattice library, IR fixtures and generators, the kernel's storage layer, schemas and artifacts, the resolver/instantiator of the Blossom frontend, the Molly `.ded` frontend and the formatter.

**Gate.** `scripts/milestone-gate.sh M3` (PLAN §3).

#### M3.1 — blossom-lattice I: built-in lattices, operation catalogue, dynamic ops, lattice heap and the law harness

- **Size:** ~7.5k
- **Depends on:** M2.1
- **Owns:** `crates/blossom-lattice/**`, `docs/plan/notes/M3.1.md`
- **Features:** LANG-124, LANG-125, LANG-126, LANG-129, LANG-130, LANG-131, LANG-281, SEM-033, ENG-031, ENG-044, ENG-045, TEST-083
- **Consumes:**
  - blossom-value (Value, LatValue, Word, ValueStore, RefValueStore, fingerprints, class enums)
- **Provides:**
  - blossom_lattice::{traits::{Lattice, Group, Ring, Atomize}, builtin::* (typed), typed over Value, catalogue::{ops, OpInfo}, dynamic::{LatSlot, LatObj, LatticeHeap, LatticeOps, LatRef, LatArg, LatCx, Joined, LatError, ops_for(def)}, laws (feature `laws`), gen (feature `arbitrary`)}

**Build** (ARCHITECTURE §2.3, §4.5; LANGUAGE §11.4–§11.6; R04 §2.4; R11).
- **Typed traits and built-ins** (LANG-124, 129–131, 281): `LBool`, `LMax<T>`/`LMin<T>` with an adjoined ⊥ (∓∞),
  `LSet`, `LMap<K, L>`, `LBag`, `LPSet`, `Pair`, `Product` (named fields), `Lex { chain, inner }`, `WithBot`,
  `WithTop`, `LPoint`/`Conflict` (conflict → `LatError::PointConflict`, surfaced as BLSR006 upstream), `Unit`,
  `VClock = LMap<Node, LMax<u64>>`, `VecUnion`, `UnionFind`. Each works over Rust types and over `Value`
  (`LatValue`) for the oracle.
- **Operation catalogue** (LANG-125, SEM-033): every operation of LANGUAGE §11.5 with per-argument `MonoClass`,
  `LatOpKind`, `join_prime` for thresholds, `incompatible_thresholds`, optional derivative declaration; `threshold`
  operations (LANG-126); `reveal` is NonMonotone. A snapshot test pins the table against R04 §2.4.
- **Dynamic ops** (ENG-031/044): `LatSlot`, `LatObj` representations (inline words for LMax/LMin/LBool/LPoint over
  direct scalars; inline bitmask word for dense domains ≤ 64 elements; `Bits256` ≤ 256; `SmallSet` sorted ≤ 8
  promoted to `Set`; `Map`, `InlineMap`, `Bag`, `Pair`, `Product`, `Lex`, `UnionFind`), `LatticeHeap` (chunked slab
  of `Arc<LatObj>`, free list, copy-on-write per object while shared by a snapshot), one `LatticeOps` vtable per
  `LatticeDef` (`join_into` reporting `Joined::{Unchanged, Changed, Conflict}` and optionally the replaced value;
  `leq`, `is_top`, `delta`, canonical `fingerprint` maintained incrementally as sums of element fingerprints,
  `apply`), cached summaries (`size`, sums) so thresholds are O(1).
- **Atomize / minimal deltas** (ENG-045): `delta(old, new)` returns the minimal increment for atomizable lattices.
- **Law harness** (TEST-083, feature `laws`): associativity, commutativity, idempotence, ⊥ identity, order agrees
  with merge, partial-order laws, `is_bot`/`is_top`, atomize correctness, morphism/bimorphism claims, `join_prime`
  claims; generic over any lattice with a generator; used by every built-in here and by M4.6/M9.5 later.
- `arbitrary` feature: generators of lattice values via random operation sequences.

**Pitfalls.** ⊥ is never stored in a cell (SEM-101). `f64` is not an `LMax`/`LMin` carrier (typeck rejects; the
library refuses construction too). Set fingerprints must equal the fingerprint of the canonical `Value` form so the
oracle and the engine agree. Bitmask representations must iterate in canonical element order.

**Required tests.** `laws_<name>` for every built-in, `typed_dynamic_cross_check` (random op sequences, canonical
results equal), `catalogue_snapshot`, `lpoint_conflict`, `lmax_adjoined_bottom`, `bitmask_repr_`, `heap_cow_`,
`atomize_minimal_`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-lattice
scripts/require-tests.sh blossom-lattice laws_lbool laws_lmax laws_lmin laws_lset laws_lmap laws_lbag laws_lex laws_vclock typed_dynamic_cross_check catalogue_snapshot lpoint_conflict lmax_adjoined_bottom bitmask_repr_ heap_cow_ atomize_minimal_
```

#### M3.2 — blossom-ir II: fixtures with expected traces, generators, plan validation and construct checks

- **Size:** ~5k
- **Depends on:** M2.1, M2.2
- **Owns:** `crates/blossom-ir/**`, `docs/plan/notes/M3.2.md`
- **Features:** ENG-001
- **Consumes:**
  - blossom-ir core (M2.2)
  - blossom-value (M2.1)
- **Provides:**
  - blossom_ir::fixtures (feature `fixtures`): `all() -> Vec<Fixture { name, program, deployment, script, expected }>` with ~40 programs and hand-derived per-tick expectations
  - blossom_ir::gen (feature `arbitrary`): program(ProgramShape), values, schedules
  - ValidatedPlan::validate(PhysicalProgram, abi_version)
  - V5 construct-expansion agreement for every ConstructKind
  - golden digests for fixtures

**Build.**
- **Fixtures** (ARCHITECTURE §1.6, feature `fixtures`): at least the programs listed there (transitive closure, a
  counter, a keyed table with deletes, a deleted-but-still-derived tuple (the show/hide example of §3.4.4), choose,
  sticky choose, a lattice cell read by lookup, a channel ping-pong, an upsert conflict, a zset with a
  level-triggered writer, a soft table, a seal) plus: `$now` in a guard, lookup of an absent key through a non-strict
  morphism, aggregates of every `AggFunc`, index/top/fold expansions, multi-role choreography with projection, a
  divergent lattice recursion (BLSR007), an invariant violation, a durable relation, timers (logical), bootstrap
  and `bootstrap fresh`. Each fixture carries a deployment (nodes/roles), a per-tick input script and
  **hand-derived expected per-tick contents** of named relations, outbox rows and errors (derive them from Dedalus
  semantics; comment the derivation). These are the first executable truths for the oracle (M4.1) and the engine.
- **Generators** (§11.6): `gen::program(ProgramShape)` builds well-typed, range-restricted, temporally stratified
  programs by construction (stratum ranks first; negation/aggregation/choice read lower ranks only; temporal rules
  unconstrained; weighted menus of relation classes, lattice columns, constructs, lookups, deletion paths,
  channels); shrinking removes rules, then literals, then columns. Plus generators for tick inputs and schedules.
- **`ValidatedPlan::validate(p, abi)`** (§3.1): ids in bounds, slot counts, widths and lanes consistent with
  `ColEnc`, every `Read`'s segments exist, every `Sink` targets a compatible relation, natives' ports and specs
  consistent, ABI version equal.
- **V5 completion**: for every `ConstructKind` check that the construct spec agrees with its rules exactly as the
  LANGUAGE expansion prints them (Persist, Identity, Upsert, Resolve, Choose, MultiChoose, Index, Seq, FoldOrdered,
  ArgExt, AggDefault, SoftTable, Sealed, Range, LogicalTimer, Seal, Snapshot, Wrapped, LatticeFold, Finality,
  Quorum; provenance-only kinds structurally).
- **Golden digests**: pin each fixture's `ProgramDigest` in a checked-in table; a change fails the test with a clear
  message (intentional changes update the table in the same commit).

**Required tests.** `fixtures_validate_all`, `fixture_digests_golden`, `gen_programs_validate` (proptest ≥ 256
cases), `gen_shrinks`, `plan_validate_rejects_`, `v5_<kind>_mismatch_rejected` for every native-capable kind.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-ir
scripts/require-tests.sh blossom-ir fixtures_validate_all fixture_digests_golden gen_programs_validate gen_shrinks plan_validate_rejects_ v5_persist_mismatch_rejected v5_choose_mismatch_rejected v5_upsert_mismatch_rejected v5_index_mismatch_rejected v5_seal_mismatch_rejected v5_wrapped_mismatch_rejected v5_finality_mismatch_rejected
```

#### M3.3 — blossom-kernel I: row storage, epochs, deaths, indexes, the interner and snapshots

- **Size:** ~7.5k
- **Depends on:** M2.1
- **Owns:** `crates/blossom-kernel/**`, `docs/plan/notes/M3.3.md`
- **Features:** ENG-020, ENG-021, ENG-022, ENG-029, ENG-030, SEM-088
- **Consumes:**
  - blossom-value (Word, Lane, ValueStore, fingerprints, sort order)
  - blossom-base (det, graph)
- **Provides:**
  - blossom_kernel::{shape::{Lane, U32, U64, RowShape, DynShape, Words, KeyShape, Keys, Deaths, NoDeaths, WithDeaths}, chunk::*, rows::{RowStore, AnyRows, RelReader, RelWriter, EpochIndex, Epoch}, mutcols::{MutCols, ChangeLog}, table::IncrementalTable, index::{PrimaryIndex, PrimaryEntry, InsertOutcome, RowRef, HashIndex, TickDedup}, intern::Interner (impl ValueStore), arena::TickArena, rel::{RelStore, Segment}, snapshot::{KernelSnapshot}, prefetch}

**Build** (ARCHITECTURE §4.1–4.3, §4.10–4.12, ARCH-05/06/21/22).
- **Shape traits** (§4.10): `Lane` (U32/U64), `RowShape` (static `Words<N, L>` and `DynShape`), `KeyShape`,
  `Deaths` (`NoDeaths` const true / `WithDeaths`); `LatShape`, `Prov`, `DigestSink` are declared by M4.3.
- **Chunks and rows** (ENG-021): two-level radix `ChunkDir` of `Arc<Page>` of `Arc<Chunk>` (4096 rows × width),
  stable addresses, append only past the published length; `EpochIndex` of `(epoch, first_row)` boundaries
  answering `Delta`, `Old`, `All`, `TickNew` as row ranges, collapsed below the oldest reader; `DeathColumn` (one
  epoch per row, written in place, only for relations with a deletion path); `dead_in_chunk` counters.
- **The aliasing contract** of §4.2 (append while scanning): readers are epoch-bounded views; appends are invisible
  to readers of the current epoch; `RelStore` hands out `RelReader`/`RelWriter` from `&self`, `!Sync`.
  `unsafe` only in `rows`, `chunk`, `prefetch`, each with `#[allow(unsafe_code)]` and `// SAFETY:` arguments;
  Miri-clean.
- **MutCols + ChangeLog** (in-place lattice cells and keyed payloads, chunk-level copy-on-write).
- **`IncrementalTable<E>`** over `hashbrown::HashTable`: incremental rehash (migrate a bounded number of buckets per
  insert and via `fuel`), probes consult both tables during migration (ARCH-21).
- **PrimaryIndex** (ENG-022): inline keys ≤ 2 words, `InsertOutcome::{Inserted, Present, Merged, Replaced,
  KeyConflict}`; **HashIndex** with posting lists, back-pointers for O(1) swap-remove on death, `built_upto`
  catch-up at epoch end; **TickDedup** with the peak-window shrink policy.
- **Interner** (ENG-020, SEM-088): implements `ValueStore`; entries `{fp, sort_prefix, ty, payload, refs}`,
  hash-consing by fingerprint, canonical compare by `(sort_prefix, full compare on tie)` — never by id; reference
  counts for long-lived references with an epoch-deferred LIFO free list; tick arena with promotion on long-lived
  insert; interned-bytes hard cap error.
- **RelStore** with frame/standing/carried/transient segments (weighted comes in M4.3), primary index spanning
  long-lived segments, tick dedup for transient.
- **Snapshots** (ENG-030): `(dir root, len)` per segment + death epochs at the snapshot epoch + MutCols chunk Arcs +
  interner chunk Arcs + pinned epoch; no indexes; zero cost to the running store while none exists.
- **History** (ENG-029): birth from epoch boundaries, death stamps; `rows_alive_at(epoch)` iteration for as-of reads
  (lazy history indexes come with M4.3/M8.6).
- **prefetch**: `_mm_prefetch` on x86-64, `prfm pldl1keep` via `core::arch::asm!` on aarch64, no-op elsewhere.

**Pitfalls.** Never reallocate a page a reader holds; never hold a reference into the primary/tick-dedup/MutCols/
change log across an emit; hash keys come from the boot nonce (`DetState::from_nonce`), never from the seed; no
`HashMap`.

**Required tests.** `rowstore_append_while_scan`, `epoch_sources_delta_old_all_ticknew`, `deaths_snapshot_reads`,
`primary_insert_outcomes`, `hash_index_swap_remove`, `incremental_table_bounded_work` (asserts per-op migration
bound), `tick_dedup_shrinks`, `interner_canonical_order_matches_value` (proptest vs `Value` order),
`interner_refcount_reuse_after_unpin`, `snapshot_cow_isolated`, `history_alive_at`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-kernel
scripts/require-tests.sh blossom-kernel rowstore_append_while_scan epoch_sources_delta_old_all_ticknew deaths_snapshot_reads primary_insert_outcomes hash_index_swap_remove incremental_table_bounded_work tick_dedup_shrinks interner_canonical_order_matches_value interner_refcount_reuse_after_unpin snapshot_cow_isolated history_alive_at
cargo +nightly miri test -p blossom-kernel --lib -- rows:: chunk::
```

#### M3.4 — blossom-schema and blossom-artifact: field numbers, schema hashes, schema.lock, compat rules, catalogs, artifacts

- **Size:** ~4.5k
- **Depends on:** M2.2
- **Owns:** `crates/blossom-schema/**`, `crates/blossom-artifact/**`, `fuzz/fuzz_targets/artifact_decoder.rs`, `docs/plan/notes/M3.4.md`
- **Features:** LANG-261
- **Consumes:**
  - blossom-ir core (M2.2)
  - blossom-value
- **Provides:**
  - blossom_schema::{FieldNo, SchemaId, SchemaHash, SchemaCatalog, RelCodec, LockFile, LockEntry, compat::{rules_table, check_compat, CompatFinding}, AclTable, MigrationSet, API_VERSION}
  - blossom_artifact::{ArtifactHeader, ArtifactKind, ArtifactError, encode/decode with header, CompileOutput, RoleArtifact, SpecArtifact, cert::{OutputCertificate, DeterminismVerdict, ConfluenceStatus, ConsistencyStatus, FinalityClass, CalmLabel, Evidence}, API_VERSION}

**Build.**
- **Schema** (LANG-261, R15 §6.3): `FieldNo`; schema hashes (BLAKE3 over a canonical description: names, field
  numbers, structural types, defaults, lattice kinds; independent of TypeId numbering); `SchemaId` (instance path +
  hash, interned per connection by the wire); `SchemaCatalog` (per relation that crosses a boundary — channel,
  durable, interface — its codec description `RelCodec`: columns, field numbers, types, defaults, since/deprecated,
  frame kinds allowed; lookups by RelId and SchemaId; catalog digest); `schema.lock` TOML format (versions, per-item
  field numbers, reserved numbers, hashes) with read/write/`deny_unknown_fields`; the **compatibility rules table**
  of R15 §6.3 as data with `check_compat(old, new) -> Vec<CompatFinding>` (each finding: rule id, item, severity,
  explanation) — the engine of ANA-100; `AclTable` (channel → allowed sender roles/principals, explicit accept
  predicates); `MigrationSet` (the catalogs of every older version and the migration steps' program digests).
- **Artifact**: `ArtifactHeader { magic: *b"BLSA", kind, format, producer }`; postcard encode/decode behind the
  header; a version skew is `ArtifactError::Version { kind, found, supported }`, never garbage; `CompileOutput`
  (validated program, per-role `RoleArtifact { projection, plan: ValidatedPlan, acl, catalog }`, specs as
  `SpecArtifact { spec, plan }`, `SchemaCatalog`, certificates, `features_used`, lock proposal), certificate types of
  §7.3. Decoding re-validates programs and plans (`ValidatedProgram::validate`, `ValidatedPlan::validate`).
- Fuzz target `artifact_decoder` + proptest mirror: typed errors, no panic.

**Required tests.** `schema_hash_kat`, `schema_hash_ignores_typeid_numbering`, `lock_roundtrip`,
`lock_rejects_unknown_fields`, `compat_rule_<n>` (one per rules-table row), `artifact_version_mismatch_error`,
`artifact_roundtrip`, `artifact_decoder_fuzz_mirror`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-schema blossom-artifact
scripts/require-tests.sh blossom-schema schema_hash_kat schema_hash_ignores_typeid_numbering lock_roundtrip lock_rejects_unknown_fields compat_rule_
scripts/require-tests.sh blossom-artifact artifact_version_mismatch_error artifact_roundtrip artifact_decoder_fuzz_mirror
```

#### M3.5 — blossom-front I: modules, resolution, instantiation, roles and the HIR

- **Size:** ~7k
- **Depends on:** M2.1, M2.3
- **Owns:** `crates/blossom-front/**`, `!crates/blossom-front/src/ded/**`, `tests/integration/tests/front1_*.rs`, `docs/plan/notes/M3.5.md`
- **Features:** LANG-003, LANG-004, LANG-005, LANG-006, LANG-007, LANG-008, LANG-009, LANG-010
- **Consumes:**
  - blossom-syntax typed AST (M2.3)
  - blossom-value TypeTable
  - blossom-std-src (lazy std modules)
  - blossom-base
- **Provides:**
  - blossom_front::api::{compile(db, root, &FrontOptions) -> Result<FrontOutput, Diagnostics>, FrontOptions, FrontOutput} with phases wired as they exist (typeck/lowering return BLS0908 until M4.5/M5.3)
  - blossom_front::{modules, items, resolve::{Res, DefMap}, instantiate, roles, hir::*}

**Build** (ARCHITECTURE §13.1, §13.4, §13.5, §13.8; LANGUAGE §6).
- **Source loading** (`modules`): a program root is a file starting with `program NAME version N`; `a::b` resolves to
  `a/b.bls` or `a/b/mod.bls`; `std::…` from `blossom-std-src` lazily (BLS0204 for unknown modules);
  `FrontOptions::std_override` lets tests substitute the std source set; `FrontOptions::module_paths` adds search
  roots (used by `blossom-build` for systems that import each other's `bls/` modules).
- **Items** (`items`): per-module item tables, the placement table of LANGUAGE §6.2 (BLS0110, `pub` never on a
  relation), `include M;` flattening (BLS0201 on duplicates), `include "f.bls"` textual; `include "f.ded"` is wired
  by M4.5 (it needs the M3.6 `.ded` frontend).
- **Resolution** (`resolve`): DefMaps with separate namespaces (types, values, relations, labels), use trees,
  `Res` of §13.4, literal classification (§13.4 bullet list) into HIR literal kinds, variable scoping rules
  (BLS0500 for `any` binding, BLS0501 no shadowing), all BLS02xx diagnostics with evidence spans.
- **Instantiation** (`instantiate`): monomorphization of generic structs/enums/types/fns/lattices/modules/
  choreographies/protocols per distinct argument list; `import M<T…>(K = v, …) as a [with (Role = Role, …)]` →
  renamed instance (`a.r`), value params → per-instance constants, relation params substituted after column-type
  checks (BLS0205; instances may read but never write them, BLS0406), protocol-bounded parameters (LANG-006),
  `override` (BLS0207), `interpose a.i as (outside, inside)` renaming exactly as LANGUAGE §6.9 prints it
  (BLS0208), constants and deploy-time params (LANG-010; BLS0211 SCREAMING_CASE).
- **Roles** (`roles`, LANG-009): `role R[: kind]`, reopenable `at R { }`, shareability outside `at` (BLS0408),
  channel sides (BLS0404), choreography role binding (BLS0206).
- **HIR** (`hir`): the types of ARCHITECTURE §13.8, built untyped here (types filled by M4.5).
- **API** (`api`): `compile()` runs load → items → resolve → instantiate → roles → HIR; later phases are called
  through functions that currently return a BLS0908 diagnostic naming the missing feature (typeck: LANG-021,
  lowering: ENG-001) — replaced by M4.5 and M5.3. `FrontOutput { program: ValidatedProgram, specs, lock_proposal,
  surface: SurfaceMap }`.

**Required tests.** `resolve_examples` (E1–E10: every name resolves, no BLS02xx), one `diag_bls02nn` test per
BLS02xx code, `instance_renaming`, `interpose_renaming_snapshot`, `monomorphization_dedup`, `override_block`,
`protocol_conformance`, `choreography_role_binding`, `std_lazy_loading`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-front
scripts/require-tests.sh blossom-front resolve_examples diag_bls0200 diag_bls0201 diag_bls0205 diag_bls0206 diag_bls0207 diag_bls0208 instance_renaming interpose_renaming_snapshot monomorphization_dedup override_block protocol_conformance choreography_role_binding std_lazy_loading
```

#### M3.6 — The Molly `.ded` frontend (syntax::ded and front::ded)

- **Size:** ~3.5k
- **Depends on:** M2.2, M2.1
- **Owns:** `crates/blossom-syntax/src/ded/**`, `crates/blossom-front/src/ded/**`, `tests/integration/tests/ded_*.rs`, `docs/plan/notes/M3.6.md`
- **Features:** LANG-220, LANG-065, LANG-069
- **Consumes:**
  - blossom-ir IrBuilder/validator (M2.2)
  - blossom-value
  - tests/corpus/ldfi/** Molly programs (M1.5)
  - the pre-declared `ded` modules of blossom-syntax and blossom-front (M1.1)
- **Provides:**
  - blossom_syntax::ded::{lex, parse(file, text) -> DedFile}
  - blossom_front::ded::{compile_ded(db, files, &DedOptions) -> Result<DedOutput { program, spec, nodes, inputs_by_tick }, Diagnostics>}

**Build** (ARCHITECTURE §13.12; LANGUAGE §21.1; R02, R06 §3). This WP makes real programs runnable before the Blossom
surface language is lowered.
- **Parser** (`syntax::ded`): Molly's dialect exactly: `include`, facts with `@k`, rules with `@next`, `@async` and
  `@k` body atoms, `notin`, head aggregates `count<X>`, `max<X>`, `min<X>`, `sum<X>`, right-nested
  precedence-free expressions (Molly's parse, which `.bls` does *not* reproduce), `//`, `/* */` and `#` comments.
  Its own small AST with spans (no rowan needed).
- **Frontend** (`front::ded`): Molly's typer (INT, STRING, LOCATION → `i64`, `String`, `Node`, R06 §3.3); the first
  column of every relation is its location: stripped from IR schemas and restored as the `Node` column of the
  spec's trace relations; every body predicate shares the rule's location; one IR rule per `.ded` rule; `@async`
  → async rule with the head's first column as destination; `@k` facts → input events at tick k (CR-13; tick 0 has
  no events); explicit persistence `p(X)@next :- p(X), notin del_p(X).` and `p(X)@next :- p(X).` wrapped in
  `Persist` constructs (LANG-065); aggregate rules split into a bindings rule and an aggregate rule; `pre`/`post`
  and their helpers become an implicit `SpecProgram` with `crash(From, Node, Time)` as the spec oracle and the
  frontend kind `Ded` (LDFI selects `CrashView::MollyContinue` from it); no `clock` rewrite.
- Validator failures V1–V4 are rendered as user diagnostics with `.ded` spans (`IrBuilder::finish` in `.ded` mode).

**Required tests.** `ded_parse_molly_corpus` (every `tests/corpus/ldfi/**/*.ded` parses), `ded_lower_molly_corpus`
(every program lowers and validates), `ded_typer_`, `ded_persistence_recognized`, `ded_aggregate_split_snapshot`,
`ded_prepost_spec`, `ded_right_nested_expr`, `ded_v1_error_is_user_diagnostic`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-syntax --all-features ded::
cargo test -p blossom-front --all-features ded::
cargo test -p blossom-integration-tests --test ded_molly
cargo clippy -p blossom-syntax -p blossom-front --all-targets --all-features -- -D warnings
scripts/require-tests.sh blossom-front ded_parse_molly_corpus ded_lower_molly_corpus ded_typer_ ded_persistence_recognized ded_aggregate_split_snapshot ded_prepost_spec ded_right_nested_expr ded_v1_error_is_user_diagnostic
```

#### M3.7 — blossom-syntax II: the formatter, hash normalization, editions and recovery hardening

- **Size:** ~3.5k
- **Depends on:** M2.3
- **Owns:** `crates/blossom-syntax/**`, `!crates/blossom-syntax/src/ded/**`, `fuzz/fuzz_targets/formatter.rs`, `docs/plan/notes/M3.7.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - blossom-syntax I (M2.3)
- **Provides:**
  - blossom_syntax::fmt::{format(file) -> String, normalize_for_hash(node) -> String}
  - edition handling

**Build** (LANGUAGE §3.5, §4.3; ARCHITECTURE §13.1–13.2).
- **Formatter**: one canonical style; never reorders items or literals; keeps comments (rewrites `#` comments to
  `//`); idempotent; preserves the CST modulo trivia.
- **`normalize_for_hash`**: the normalized printing LANGUAGE §4.3 uses for rule-id and site-id hashes; invariant under
  whitespace, comments and formatting; documented as normative (changing it changes every rule id).
- **Editions**: the `edition` in the program header; unknown editions are errors; the parser takes the edition.
- **Recovery hardening**: grow the recovery test corpus from mutated examples (delete/insert tokens) so every error
  keeps later items intact.
- Fuzz target `formatter` + proptest mirror.

**Required tests.** `fmt_idempotent` (examples + corpus + LANGUAGE blocks), `fmt_preserves_cst_modulo_trivia`,
`fmt_never_reorders`, `normalize_for_hash_stable`, `edition_unknown_rejected`, `recovery_mutations_`,
`formatter_fuzz_mirror`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-syntax
scripts/require-tests.sh blossom-syntax fmt_idempotent fmt_preserves_cst_modulo_trivia fmt_never_reorders normalize_for_hash_stable edition_unknown_rejected recovery_mutations_ formatter_fuzz_mirror
```

### M4 — The oracle, P0 analyses, kernel execution structures, wire codec, type checking and seams

**Goal.** The naive oracle becomes the executable semantics; P0 analyses produce the stratification; the kernel gains its join, aggregation, provenance and digest structures; the wire codec, trace format and engine boundary types are fixed; the frontend type-checks the whole language.

**Gate.** `scripts/milestone-gate.sh M4` (PLAN §3).

#### M4.1 — blossom-oracle: the naive per-tick Dedalus^L evaluator and the choice-validity checker

- **Size:** ~7k
- **Depends on:** M3.1, M3.2
- **Owns:** `crates/blossom-oracle/**`, `docs/plan/notes/M4.1.md`
- **Features:** ENG-067, SEM-003, SEM-004, SEM-005, SEM-006, SEM-007, SEM-008, SEM-013, SEM-023, SEM-030, SEM-031, SEM-032, SEM-034, SEM-080, SEM-083, SEM-085, SEM-101, SEM-103, SEM-104, LANG-118, LANG-180, TEST-013
- **Consumes:**
  - blossom-ir (ValidatedProgram, fixtures)
  - blossom-lattice typed
  - blossom-value (PRF, fingerprints, RefValueStore, ExternRegistry)
- **Provides:**
  - blossom_oracle::{Oracle, OracleNode, OracleRel, OracleDeployment, OracleTickInput, OracleTickOutput, OracleError, OracleStrata, StratPerturbation, choice_validity::check}

**Build** (ARCHITECTURE §11.2, ARCH-16; FEATURES §1 CR-xx and §3). The oracle is the executable definition of
per-node semantics. It depends only on base, value, lattice and ir — never on the kernel, engine, planner or
analyses.
- `Oracle::new(program, externs, deployment)` and `tick(node, input) -> OracleTickOutput` (relations, outbox,
  choices, durable delta, violations, error) exactly as §11.2.
- **Own stratifier**: repeatedly take every relation whose negative same-tick dependencies are complete; a
  `StratPerturbation` mode picks a different valid linearization (SEM-023).
- **Naive evaluation**: every rule re-run against the full instance until nothing changes; Kleene rounds counted
  against the same iteration bound as the engine; BLSR007 with a witness (cells changed in the last two rounds).
- I = st ⊔ delivered batch; frame and identity rules evaluated **literally**; weighted relations follow the §2.4
  transition; inductive heads → st′; async heads → outbox with lattice columns merged at the sender (CR-52) and the
  channel key checked at the sender (SEM-050).
- All literal forms (§2.5): positive atoms (lattice columns range over non-⊥ cells), negation, refutable bind
  patterns, guards, lookups (⊥ when absent), generators (values, set-like lattices, ranges incl. ring ranges, table
  functions from recorded rows); heads `Insert` (merge; key FD check → BLSR001 with both derivations; upsert
  staging relations of an `Upsert` construct → BLSR002), `ZAdd`, `Violation`.
- **Aggregates** over distinct valuations with canonical tiebreaks (LANG-118): every `AggFunc` including
  `percentile` (nearest rank), `collect_*` (canonical; duplicate map key BLSR005), `Ola*`, `Uda` (fold in canonical
  order unless proved commutative+associative).
- **Expressions**: checked arithmetic (BLSR004), canonical comparisons, IR functions, externs through the shared
  `ExternRegistry` (memoized per tick), every `BuiltinFn` of LANGUAGE Appendix B (LANG-180): `$prio`, `$rprio`,
  `rand*` from the PRF, `Route` (rendezvous hashing over canonically ordered members), `Majority`, `ZWeight`,
  `ZDelta`, `Unwrap`, `Entries`, `ClusterVersionAtLeast`, `PrincipalOf`, `RoleOf`, `Size`, strings/bytes/collections.
- Scalars `$now $tick $self $incarnation $host` and params from the tick input; ⊥ normalization (SEM-034/101);
  `node_dir`/`R$members` from the deployment; every construct evaluated **only through its expansion**.
- `choice_validity::check` (TEST-013): independent checker that a choice log is valid (one winner per group among
  candidates with positive support, FD respected, sticky holds kept).
- `TickDigests` (state/outbox/choices) computed with `Digest128` so digests can be compared with the engine.

**Pitfalls.** Tie-breaks always by canonical order of the whole tuple; set semantics per tick (CR-03) for heads and
aggregates; deletes at t+1 with insert-wins (CR-05); errors are returned at the tick they happen, never panics.

**Required tests.** `fixture_<name>` for every M3.2 fixture (expected traces pass), `strat_perturbation_equal`,
`blsr001_key_violation`, `blsr002_conflicting_upsert`, `blsr004_overflow`, `blsr005_dup_map_key`,
`blsr007_divergence_witness`, `weighted_transition`, `choice_validity_`, `builtin_` (one per library area),
`digests_stable`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-oracle
scripts/require-tests.sh blossom-oracle fixture_ strat_perturbation_equal blsr001_key_violation blsr002_conflicting_upsert blsr004_overflow blsr005_dup_map_key blsr007_divergence_witness weighted_transition choice_validity_ builtin_ digests_stable
```

#### M4.2 — blossom-analysis I: dependency graph, stratification, safety, locality, polarity, labels, ACLs, compat, branching

- **Size:** ~6k
- **Depends on:** M3.2, M3.4
- **Owns:** `crates/blossom-analysis/**`, `docs/plan/notes/M4.2.md`
- **Features:** ANA-001, ANA-002, ANA-004, ANA-020, ANA-022, ANA-023, ANA-024, ANA-100, ANA-105, SEM-020, SEM-021, SEM-022, SEM-086, SEM-102, LANG-151, LANG-242, TEST-003
- **Consumes:**
  - blossom-ir (programs, fixtures, strata type)
  - blossom-schema (rules table, AclTable)
  - blossom-artifact (cert types)
- **Provides:**
  - blossom_analysis::{DepGraph, DepEdge, EdgeTime, Polarity, EdgeKind, Analysis, AnalysisCx, AnalysisError, safety, strata::stratify -> Stratification, locality, polarity, calm::{points_of_order, path_labels, guarded_async}, acl::infer -> AclTable, compat::check, branching::channels -> per-channel bits, diagnostics mapped to surface constructs}

**Build** (ARCHITECTURE §7.1–§7.2 P0 rows; LANGUAGE §13).
- `DepGraph` (CSR, one edge per body occurrence → head, with `EdgeTime`, composed `Polarity`, `EdgeKind`);
  `Analysis` trait with a typed memo in `AnalysisCx`.
- **Polarity composition** (SEM-102): Mon∘Mon = Mon, Anti∘Anti = Mon, Mon∘Anti = Anti, NM absorbs; threshold under
  `not` is Anti; a stable read guarded by its threshold in the same body is Mon.
- **Safety** (ANA-001, BLS0500 backstop), **locality** (ANA-004, LANG-151).
- **Stratification** (ANA-002, SEM-020–022, SEM-086): Tarjan SCC over same-tick edges; an SCC containing a
  negation, aggregate, choice, order, NM/Anti lattice op, reveal, delta read or Z boundary edge is rejected with
  BLS0502/BLS0503 and the shortest witness cycle; strata by longest negative path; temporal pseudo-stratum;
  produces `ir::strata::Stratification`.
- **ANA-020** lattice-aware monotonicity (P0 part), **ANA-022** points of order, **ANA-023** path labels
  (Bot/A/N/D, A then N = D, suggested coordination point = last async edge before the first D), **ANA-024** guarded
  asynchrony.
- **ACL inference** (ANA-105, LANG-242 P0 part): `senders(c)` from the choreography → `AclTable`; an explicit ACL
  excluding a sender is BLS0800.
- **Compat** (ANA-100): wraps `blossom_schema::compat` over two catalogs/locks; JSON report for CI.
- **Branching** (TEST-003): `branching(c)` per channel with the corrected predicate of §6.2 (same-node paths through
  deductive, inductive, persistence and lattice-merge edges to a negative edge, a seed/schedule-dependent site, a
  time-varying read, or a relation a spec reads non-monotonically).
- Diagnostics carry codes and evidence spans mapped back through `Construct::surface` (never generated names).

**Required tests.** `strata_fixtures`, `bls0502_witness_cycle`, `bls0503_choice_cycle`, `polarity_composition_`,
`points_of_order_`, `path_labels_`, `guarded_async_`, `acl_inference_`, `bls0800_explicit_acl_excludes_sender`,
`compat_report_json`, `branching_regression_section_6_2`, `diagnostics_map_to_surface`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-analysis
scripts/require-tests.sh blossom-analysis strata_fixtures bls0502_witness_cycle bls0503_choice_cycle polarity_composition_ points_of_order_ path_labels_ guarded_async_ acl_inference_ bls0800_explicit_acl_excludes_sender compat_report_json branching_regression_section_6_2 diagnostics_map_to_surface
```

#### M4.3 — blossom-kernel II: sorted indexes, weighted stores, group tables, choice index, join kernels, provenance records, digests

- **Size:** ~7.5k
- **Depends on:** M3.3, M3.1
- **Owns:** `crates/blossom-kernel/**`, `docs/plan/notes/M4.3.md`
- **Features:** ENG-024, ENG-025, ENG-026, ENG-085, ENG-091, ENG-120, ENG-121, ENG-112
- **Consumes:**
  - blossom-kernel I (M3.3)
  - blossom-lattice dynamic ops (M3.1)
  - blossom-value
- **Provides:**
  - blossom_kernel::{index::{SortedIndex, SortOrder, Spine, TrieCursor, seek_prefix, Build}, weighted::{WeightedStore, WeightKind, Derivations, UserZ, ZBatch, TickDeathList}, group::{GroupTable, Accumulator, accumulators::*}, choice::ArgminIndex, batch::{BindBatch, MatchBatch, SelVec}, join::{scan, probe_batch, probe_one, lookup_primary, exists_batch, RowIter, PostingIter, ChangeIter}, shape::{LatShape, Prov, DigestSink, NoDigest, IncDigest}, prov::{ProvenanceSink, NullSink, FiringLog, FiringRecord, Contribution, ChoiceRecord, SendRecord, ReceiveRecord, NegRead, ProvRead}, digest::RelDigest, compact::Compactor}

**Build** (ARCHITECTURE §4.3–4.6, §4.9, §4.11–4.12).
- **SortedIndex** (ENG-024/025): STI permutation encoding (one comparator for every order), covering option,
  `SortOrder::{Word, Canonical}` (canonical via `sort_prefix` then full compare), a DD-style fueled spine of runs
  (merge work proportional to each new batch; readers see unfinished merges as their inputs), `TrieCursor`
  (`open/up/next/seek/key/at_end`) over the runs, `seek_prefix`.
- **COLT lazy hash** (ENG-026): `Build::Lazy` indexes built on first probe over Δ/TickNew/transient scopes.
- **WeightedStore<W>**: `Derivations` (unsigned; negative weight is an internal error) and `UserZ` (signed); checked
  i64 weights (overflow → error mapped to BLSR004); rows with weight 0 absent; `ZBatch` per tick; `TickDeathList`
  with a tick-local index for `ZOld` reads (§3.4.5).
- **GroupTable<A: Accumulator>** (ENG-091): fused-group accumulators with retraction where possible — count, sum,
  avg (checked i128 then range-checked), min/max and argmin/argmax (ordered multiset per group), bool_and/or
  (counts), collect_vec/set/map (weighted multiset, canonical sort on output), percentile/top-k (recompute touched
  groups), UDA callbacks; `output` returns `None` for empty groups (CR-08).
- **ArgminIndex** (ENG-068 support): per group either the minimum `(priority, row)` (Growing candidates) or an
  ordered set; retraction re-emits the new minimum; ties by canonical Ȳ.
- **Batches and join kernels** (ENG-085): `BindBatch`/`MatchBatch`/`SelVec`; `scan`, `probe_batch` with group
  prefetching and AMAC-style interleaving, `probe_one`, `lookup_primary`, `exists_batch`, `seek_prefix`;
  specialized by `RowShape`, `KeyShape`, `Deaths`.
- **Provenance records** (ENG-112): `ProvenanceSink` with `ENABLED` const; `NullSink`; columnar per-rule `FiringLog`
  sliced by a rule mask; `FiringRecord` (rule, tick, bindings, negative reads, optional interval), `Contribution`,
  `ChoiceRecord`, `SendRecord`, `ReceiveRecord` (one per sender and send tick); `ProvRead` for Tier B search.
- **Digests** (ENG-120): `DigestSink` with `NoDigest`/`IncDigest`; per-relation incremental 128-bit set hash over
  `H(rel_stable_id, tuple_fp)` and `H(rel, key_fp, value_fp)` for cells, updated on birth/death/overwrite/merge.
- **Compaction** (ENG-121): chunk-granular, fueled, only chunks more than half dead with all deaths older than the
  history frontier; rows re-pointed through posting-list back-pointers.

**Required tests.** `sorted_index_vs_model` (proptest vs BTreeSet), `spine_fuel_bounded`, `trie_cursor_seek_`,
`colt_built_on_first_probe`, `weighted_zero_absent`, `weighted_overflow_error`, `group_retraction_equals_recompute`
(proptest), `accumulator_` per aggregate, `argmin_retraction_`, `probe_batch_equals_probe_one` (proptest),
`firing_log_slice`, `digest_incremental_equals_full`, `compaction_preserves_live_rows`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-kernel
scripts/require-tests.sh blossom-kernel sorted_index_vs_model spine_fuel_bounded trie_cursor_seek_ colt_built_on_first_probe weighted_zero_absent weighted_overflow_error group_retraction_equals_recompute accumulator_ argmin_retraction_ probe_batch_equals_probe_one firing_log_slice digest_incremental_equals_full compaction_preserves_live_rows
cargo +nightly miri test -p blossom-kernel --lib -- rows:: chunk::
```

#### M4.4 — blossom-wire: frames, the field-numbered tuple codec, limits, HELLO negotiation and the codec ABI

- **Size:** ~5k
- **Depends on:** M3.4, M2.1
- **Owns:** `crates/blossom-wire/**`, `fuzz/fuzz_targets/wire_decoder.rs`, `tests/fixtures/wire/**`, `docs/plan/notes/M4.4.md`
- **Features:** DIST-003, DIST-010, DIST-080, LANG-137
- **Consumes:**
  - blossom-schema (SchemaCatalog, RelCodec, SchemaId)
  - blossom-value (Word, WordSink, ValueStore)
  - blossom-ir
- **Provides:**
  - blossom_wire::{frame::{Frame, Hello, HelloOk, Reject, GoAway, BatchHeader, FrameKind}, codec::{encode_tuple, decode_tuple_value}, decode_batch(&AdmittedBytes, &SchemaCatalog, &WireLimits, &mut dyn WordSink), scan_batch (validation without decoding values), WireLimits, WireError, NodeEncoding, hello::negotiate, abi::{VERSION, …}}
  - golden wire encodings tests/fixtures/wire/v1/

**Build** (ARCHITECTURE §5.4, ARCH-09; DIST-003/010/080, LANG-137, LANG-261).
- Frame grammar of §5.4 (little-endian, LEB128 varints, zigzag); HELLO/HELLO_OK/REJECT/GOAWAY/BATCH; frame kinds
  (Plain, LDelta, GDelta, GAck, GCum, GCumDiff, OTAgg, ZBatch, Seal) as codec cases (their protocol semantics come
  with the owning features).
- Tuple codec driven by `SchemaCatalog`: fields in ascending field-number order; wire types 0–6; nested records;
  lattice payloads in the lattice's own encoding (sets sorted, maps as sorted pairs, inline lattices as scalars);
  `NodeEncoding::{Dense, ByName}` (dense on the wire after a matching `directory_digest`, by name in WAL/checkpoints).
- **Evolution** (LANG-261, DIST-010): unknown fields skipped; unknown enum variants decode to the `#[unknown]`
  variant and keep their bytes so re-encoding is identical; defaults fill absent fields; a required absent field
  without default → `decode` rejection, never a crash; channel identity = instance path + schema hash (sid per
  connection).
- `decode_batch` writes straight into a `WordSink` (direct words, `push_bytes` for strings/bytes, records
  bottom-up); `scan_batch` validates a frame (sid, kind, tuple boundaries, limits) **without** decoding values, for
  the admission pipeline; `encode_tuple` from words + `ValueStore`.
- `WireLimits` (max_frame 16 MiB, tuples per batch, nesting, counts checked against remaining bytes before any
  allocation, interned-bytes growth accounting) applied identically everywhere.
- HELLO negotiation (DIST-080): versions, window, deployment/program ids, directory digest, per-channel schema
  hashes; outcome = shared channels or a reject reason.
- `abi` module: `VERSION = 1` and the helper functions generated codecs call.
- Golden encodings of a canonical tuple set in `tests/fixtures/wire/v1/` (decoded by every future version).
- Fuzz target `wire_decoder` + proptest mirror.

**Required tests.** `roundtrip_all_types` (proptest over random schemas and values), `unknown_field_skipped`,
`unknown_variant_preserved_bytes`, `default_fills_absent`, `missing_required_is_decode_reject`,
`limits_checked_before_alloc`, `decode_into_wordsink_equals_value_decode`, `scan_batch_boundaries`,
`hello_negotiation_`, `golden_wire_v1`, `wire_decoder_fuzz_mirror`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-wire
scripts/require-tests.sh blossom-wire roundtrip_all_types unknown_field_skipped unknown_variant_preserved_bytes default_fills_absent missing_required_is_decode_reject limits_checked_before_alloc decode_into_wordsink_equals_value_decode scan_batch_boundaries hello_negotiation_ golden_wire_v1 wire_decoder_fuzz_mirror
```

#### M4.5 — blossom-front II: type checking, the bang rule, legality and event/standing classification

- **Size:** ~7.5k
- **Depends on:** M3.5, M3.6, M3.1
- **Owns:** `crates/blossom-front/**`, `!crates/blossom-front/src/ded/**`, `tests/integration/tests/front2_*.rs`, `docs/plan/notes/M4.5.md`
- **Features:** LANG-020, LANG-021, LANG-025, LANG-066, LANG-101, LANG-120, LANG-121, LANG-127, LANG-182, LANG-204, ANA-005
- **Consumes:**
  - blossom-front I (M3.5) HIR
  - blossom-lattice catalogue (M3.1) for operation classes
  - blossom-front::ded (M3.6) for `include "f.ded"`
- **Provides:**
  - typed HIR for the whole language; `front::typeck::check(hir) -> TypedHir`; `front::classify` (event/standing classes with explanation chains); all typeck/classify diagnostics; `include "f.ded"` wired

**Build** (ARCHITECTURE §13.6–13.7; LANGUAGE §5, §11.4, §12, §13).
- Union-find inference per rule with evidence recorded per union; BLS0300 lists every conflicting piece of evidence
  (LANG-021); arity BLS0301; integer literals default to `i64` only when unconstrained; view schemas inferred
  (BLS0310); `Option` everywhere a value may be absent, never null (LANG-025; CR-28 arity errors).
- Expected types pushed down; lattice lifts (`LMax`/`LMin`/`LPoint`/`LBool`/`LSet`/`LMap`) only with an expected
  lattice type (BLS0311); product-lattice literals may omit fields (⊥).
- Non-⊥ refinement (SEM-101 N4) → `VarDecl::non_bottom`; `reveal!` of `LMax<T>` typed `T` when non-⊥ (LANG-127).
- **Bang rule both directions** (LANG-101/125): missing bang BLS0700 with fix-it, superfluous BLS0701, stable read
  without guard BLS0703; op classes from `blossom_lattice::catalogue` and user fn class declarations; `==`/`!=` on
  lattices BLS0305; threshold-direction comparisons BLS0306; f64 lattices BLS0312; `DomPair` outside `unsafe`
  BLS0706; incompatible `threshold(…)` constants BLS0707.
- Atoms and heads (BLS0301–0304; lattice columns never keys/join keys/group keys, LANG-121); relation declarations
  typed (LANG-020: keys, `like r`, singleton).
- **Legality** (LANG-066, LANGUAGE §12): verb × target matrix BLS0400 and BLS0401–0407, BLS0410.
- Boundary types: group payloads without `exactly_once` BLS0307, enums crossing boundaries without `#[unknown]`
  BLS0308, literal negative bag weights BLS0313.
- Function property attributes (`#[injective]`, `#[commutative]`, …; LANG-182) and `#[nondet("…")]`
  (LANG-204) recorded into the typed HIR as claims (`Claim::Claimed(Unchecked)`).
- **Classification** (§13.7): event/standing greatest fixpoint with the explanation chain; BLS0504 (with the chain),
  BLS0505, BLS0506 (fix-it `next`, `#[allow(self_negation)]`), BLS0600, BLS0409, BLS0507, BLS0500 on HIR; lints
  BLS1003 (syntactic part), BLS1004–1008.
- Wire `include "f.ded"` into `front::ded` (M3.6); `api::compile` now runs typeck and classify.

**Required tests.** `typecheck_examples` (E1–E10 with zero errors), one `diag_bls03nn`/`diag_bls04nn`/
`diag_bls05nn`/`diag_bls07nn`/`diag_bls10nn` test per code this phase owns (a table-driven test is fine, but every
code must appear), `bls0300_lists_all_evidence`, `lattice_lift_`, `non_bottom_reveal_type`,
`classification_chain_snapshot`, `include_ded_file`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-front
scripts/require-tests.sh blossom-front typecheck_examples diag_bls0300 diag_bls0301 diag_bls0304 diag_bls0307 diag_bls0400 diag_bls0504 diag_bls0506 diag_bls0700 diag_bls0701 diag_bls0707 bls0300_lists_all_evidence lattice_lift_ non_bottom_reveal_type classification_chain_snapshot include_ded_file
```

#### M4.6 — blossom-lattice II: P1 lattices, user lattices, DomPair, groups and rings

- **Size:** ~5k
- **Depends on:** M3.1
- **Owns:** `crates/blossom-lattice/**`, `docs/plan/notes/M4.6.md`
- **Features:** LANG-132, LANG-133, LANG-134, LANG-135, LANG-136, LANG-142, LANG-282, LANG-283, LANG-284
- **Consumes:**
  - blossom-lattice I (M3.1)
- **Provides:**
  - LDom/MV-register, tombstone lattices (feature `roaring`), causal dot stores with causal contexts, ExternLatticeDyn + registration for `extern lattice`, DomPairUnsafe, Group/Ring implementations (Z, Zn, ZSet, tuple, map, user) with dynamic ops, LMap::sum_values, join-with-constant defaults, Lex reset, catalogue entries and law coverage for all of them

**Build** (LANGUAGE §11.5, §11.8–§11.10; R04; R13).
- `LDom` / MV-register (LANG-132), tombstone lattices (LANG-133; roaring bitmaps behind feature `roaring`), causal
  dot stores `DotSet`/`DotFun`/`DotMap` with causal contexts (LANG-134), each typed + dynamic + catalogue + laws.
- User-defined lattices (LANG-135): the `ExternLatticeDyn` trait (a Rust type implementing merge, leq, bottom,
  fingerprint, encoding) and a registry; laws run against registered externs.
- `DomPairUnsafe` (LANG-136): available only through the `unsafe` constructor; the law harness must **refute** it
  (the known-bad case of TEST-083/087).
- Groups and rings (LANG-142, CR-35): Z, Zn, ZSet, tuples, maps and user groups with `zero`, `add`, `neg`,
  optional ring `mul`; dynamic ops for `WeightedStore<UserZ>` and group-valued columns; group values are never
  lattices.
- `LMap::sum_values` keyed monotone sum (LANG-282); defaults by joining with a constant (LANG-283); monotone reset
  through `Lex` (LANG-284) with the catalogue classes each operation needs.

**Required tests.** `laws_ldom`, `laws_tombstone`, `laws_dotset`, `laws_dotmap`, `dompair_refuted`, `group_laws_`,
`ring_laws_`, `extern_lattice_laws_`, `sum_values_monotone`, `lex_reset_`, `typed_dynamic_cross_check_p1`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-lattice
scripts/require-tests.sh blossom-lattice laws_ldom laws_tombstone laws_dotset laws_dotmap dompair_refuted group_laws_ ring_laws_ extern_lattice_laws_ sum_values_monotone lex_reset_ typed_dynamic_cross_check_p1
```

#### M4.7 — blossom-trace: the observation vocabulary and the record/replay trace format

- **Size:** ~2.5k
- **Depends on:** M3.4, M2.2
- **Owns:** `crates/blossom-trace/**`, `fuzz/fuzz_targets/trace_reader.rs`, `docs/plan/notes/M4.7.md`
- **Features:** TEST-010
- **Consumes:**
  - blossom-artifact (ArtifactHeader)
  - blossom-ir (ProgramDigest, PlanDigest, obs records)
  - blossom-value
- **Provides:**
  - blossom_trace::vocab::{SchedDecision, MsgId, TickTrigger, DropReason, RejectReason, FaultSchedule, NodeDesc, DeliverySet, PartitionSpec, FsFailure, TimerId, SessionId, TupleBytes}
  - blossom_trace::format::{TraceHeader, TraceEvent, RecordLevel, TraceWriter, TraceReader, to_json}

**Build** (ARCHITECTURE §6.2–§6.4; TEST-010).
- The vocabulary types exactly as §6.2–§6.3 show them (plus any the event list needs), serde-derived.
- `TraceHeader` and `TraceEvent` exactly as §6.4 (every variant); `RecordLevel::{Minimal, Digests, Full}` with the
  rule of which events each level records.
- Format: `ArtifactHeader` (kind `Trace`) followed by length-prefixed postcard records; `TraceWriter` (pooled
  buffers, file mode 0600) and a streaming `TraceReader` (typed errors on truncation or version skew); JSON export.
- Fuzz target `trace_reader` + proptest mirror.
- A completeness checklist in the module docs mapping every future `NodeEvent` variant and `TickHeader` field to its
  `TraceEvent` (the enumeration test itself is added by M5.5 when `NodeEvent` exists).

**Required tests.** `trace_roundtrip` (proptest), `trace_level_filtering`, `trace_version_skew_error`,
`trace_truncated_error`, `trace_json_export_snapshot`, `trace_reader_fuzz_mirror`, `trace_file_mode_0600`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-trace
scripts/require-tests.sh blossom-trace trace_roundtrip trace_level_filtering trace_version_skew_error trace_truncated_error trace_json_export_snapshot trace_reader_fuzz_mirror trace_file_mode_0600
```

#### M4.8 — Engine boundary types: the blossom-engine public surface the node and codegen build against

- **Size:** ~2k
- **Depends on:** M3.3, M3.2, M2.1
- **Owns:** `crates/blossom-engine/**`, `docs/plan/notes/M4.8.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - blossom-ir (plan types, ValidatedPlan, obs)
  - blossom-kernel I shapes
  - blossom-value
- **Provides:**
  - blossom_engine::{io::{TickHeader, TickOutputRef, DurableDeltaRef, OutboxRef, SubDeltaRef, TimerChanges, WakeRequest, HaltRequest, TickStats, DurableImage, RelView}, error::{TickError, TickErrorKind, EngineError, Derivation, Row, SiteRef, DivergenceWitness, ArithKind, DotRef}, exec::{PlanExecutor, ExecutorFactory, ExecutorKindTag}, config::EngineConfig, snapshot::EngineSnapshot, abi::{VERSION, ExecCtx, Resolver, RelHandle, IndexHandle, BufferHandle, NativeHandle, AggHandle, InputRow, OutputRow}, Engine (signatures; uninhabited until M6.1)}

**Build** (ARCHITECTURE §4.7, §12.1; PLAN §4 D3). This WP freezes the engine surface other crates compile against
so M5.5 (node) can be built before the engine exists.
- `io`: `TickHeader` and `TickOutputRef<'e>` with its borrowed component views exactly as §4.7 (durable delta,
  outbox per (dest, channel) as word rows plus the `ValueStore` to encode them, subscription deltas, violations,
  choice log, digests, stats, timer changes, wake and halt requests); `DurableImage<'_>` (recovered durable rows per
  relation, cells, staged next-tick changes, native state blobs); `RelView`.
- `error`: `TickError(Box<TickErrorKind>)` with every variant and message of §12.1 (BLSR001–011, interner cap,
  host, `Unimplemented`, `Internal`); `EngineError` (plan/ABI mismatch, `StalePlan { expected, found }`, capability
  refusal carrying BLS0908 data: feature + rule label).
- `exec`: `PlanExecutor` trait, `ExecutorFactory { kind, make }`, `ExecutorKindTag`.
- `config`: `EngineConfig` with every knob of §12.4 and documented defaults.
- `abi`: `VERSION = 1`; `ExecCtx` and `Resolver` as opaque structs; handle newtypes; the `InputRow`/`OutputRow`
  traits for typed host rows (word-level `to_words`/`from_words` against `ValueStore`) so generated code can
  implement them while naming only the ABI; re-exports of the kernel shape traits that exist (M3.3). Methods whose
  argument types come later (batches, change iterators) are added by M6.1.
- `Engine`: a struct holding an uninhabited field, with every method of §4.7 declared; `Engine::new` returns
  `EngineError::Unimplemented` (feature `SEM-002`), `capabilities()` returns an empty slice, the remaining methods
  are unreachable by construction (`match self.never {}`), so no silent default is possible.

**Required tests.** `tick_error_messages_match_codes` (every variant's message starts with its registered code),
`engine_new_is_unimplemented`, `config_defaults_documented`, `abi_version_const`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-engine
scripts/require-tests.sh blossom-engine tick_error_messages_match_codes engine_new_is_unimplemented config_defaults_documented abi_version_const
```

### M5 — Planner, lowering, the corpus runner, the sans-IO node and durable recovery

**Goal.** The planner turns IR into physical plans; the Blossom frontend lowers the core language; the corpus runner runs `.ded` and `.bls` programs on the oracle under the status ratchet; the node and durable recovery are built against the engine boundary types. At the gate, the first golden cases pass on the oracle.

**Gate.** `scripts/milestone-gate.sh M5` (PLAN §3).

#### M5.1 — blossom-plan I: growth classes, regimes, versions, joins, indexes, natives selection, profiles

- **Size:** ~8.5k
- **Depends on:** M4.2, M3.2, M3.4
- **Owns:** `crates/blossom-plan/**`, `docs/plan/notes/M5.1.md`
- **Features:** ENG-002, ENG-004, ENG-023, ENG-040, ENG-041, ENG-060, ENG-069, ENG-074, ENG-080, ENG-081, ENG-082, SEM-091
- **Consumes:**
  - blossom-ir (programs, fixtures, plan data model, ValidatedPlan::validate)
  - blossom-analysis (Stratification, polarity, branching bits, ACLs)
  - blossom-schema (catalog)
  - blossom-artifact
  - ARCHITECTURE §4.1–4.3 (the planner decides lanes, encodings and index kinds as ir::plan data; it never depends on blossom-kernel, an L2 crate)
- **Provides:**
  - blossom_plan::{plan_program(&ValidatedProgram, &Stratification, &AnalysisFacts, &PlanOptions) -> Result<Vec<(RoleId, ValidatedPlan)>, PlanError>, plan_spec(&SpecProgram, …) -> ValidatedPlan, PlanOptions { profile, capabilities: CapabilitySet (Set(..) | Unchecked), limits }, PlanError}
  - module layout: growth, regimes, support, versions, temporal, natives/<kind>.rs, join, index, encode, cexpr, profile, capability, ingest, prov_slice

**Build** (ARCHITECTURE §3.1–§3.6, §3.8–§3.10, §4.1). One `PhysicalProgram` per role projection and one per spec.
Pipeline, each step its own module:
1. **Projection** per role (`ValidatedProgram::project`).
2. **Growth classes** (§3.4.1): greatest fixpoint over the dependency graph including lookup edges.
3. **Time-varying sites** (§3.4.2, ENG-074): any rule mentioning `$now`, `$tick`, `$incarnation`, `$rand*` or
   `choose_rand` is Transient; constructs whose expansion mentions one mark their stratum `time_varying`.
4. **Regimes** (§3.4.3 table in order, ENG-060/069) with a `RegimeReason` for every decision.
5. **Deductive support** (§3.4.4, CR-05/26): `SupportPlan` for persistent relations with a deletion path; the Counted
   Δ⁻ → frame transfer with the `del(u−1) ∖ V(u−1)` tombstone set; relations whose frame is replaced force their
   Standing/Counted writers to Transient.
6. **Counted versions** (§3.4.5): k versions with `ZNew`/`ZDelta`/`ZOld`; non-invertible lookup keys → Recompute.
7. **Segments and mixed relations** (§3.4.6): per-segment variants of consumer rules (≤ 2 mixed occurrences).
8. **Dirty triggers** (§3.4.7) and stratum order by SCC (ENG-040).
9. **Temporal plan** (§3.5): staging kinds for `next`/delete/upsert/resolve, carried segments for Standing/Counted
   inductive contributions, the outbox with **all** of V(t) each tick (literal resend), frame kind `Plain` by
   default (LDelta decided later by M11.3).
10. **Natives** (§3.6): select a native for a construct **only if** `PlanOptions::capabilities` contains its feature;
    otherwise plan the expansion rules like any other rules (always correct). This WP writes selection for the P0
    natives (Persist, Identity, Choose, Index (sort form), FoldOrdered, Upsert, LogicalTimer, LatticeFold) in
    `natives/<kind>.rs`; P1 natives (M8.6) plug in there.
11. **Semi-naive versions** (§3.3, ENG-041): Δ at r_j, All before, Old after; lattice occurrences read full values
    in Old positions; lookups are occurrences whose Δ version roots at `Op::Changes`; bimorphisms get two versions.
12. **Join planning** P0 (§3.8 steps 1–4 and 8; ENG-080–082): classify literals; root at the Δ occurrence or the
    smallest tick-local occurrence, otherwise the FlowLog structural choice; hoist checks; `IfExistsConversion`;
    "iterate the cover, probe the rest" nodes with batched probes; batch-mode head insert past a threshold.
13. **Index selection** (§3.9, ENG-023): primary per deduplicating relation; one `Hash` per distinct equality column
    set (Lazy over Δ/TickNew/transient scopes, Eager over long-lived segments); minimum chain cover of ordered
    searches (Hopcroft–Karp from blossom-base) → `Sorted` permutations; `SortOrder::Canonical` for ranges over
    interned strings/bytes.
14. **Encodings** (§4.1): lanes, `ColEnc` per column (Interned vs Bulk vs Direct vs LatInline vs LatObj by the
    ARCH-22 rules), hidden columns only when read (sender, principal, weight; SEM-091).
15. **Profiles and capabilities** (§3.10): `Production`, `Literal` (every rule Transient, Recompute inside recursive
    strata, natives and fusion off, frame/identity rules literal); `Perturbed` is M6.2. A needed feature missing from
    the capability set → `PlanError::Capability` rendered as BLS0908 naming the feature and the rule label.
16. **IngestPlan** (slots, per-channel branching bits from analysis), **ProvPlan** (Tier C backward slice of the goal
    relations through every edge kind), **DigestPlan**, **PlanLimits**, `empty_tick_effects`.
17. **CExpr** compilation (constants pre-encoded, calls resolved to kernel builtins / inlined IR fns / extern slots).
18. `ValidatedPlan::validate` on the result.

**Pitfalls.** Every regime must be observationally identical to naive Dedalus (the oracle); when unsure choose
`Recompute`. Never plan a Standing rule over a Shrinking input. Lookup Δ with non-invertible keys falls back to the
base version (or Recompute under Counted). Keep planner output deterministic (no hash-order dependence).

**Required tests.** `growth_classes_`, `regime_table_row_<n>` (one per row of the §3.4.3 table),
`support_show_hide_plan` (the §3.4.4 example), `counted_versions_`, `lookup_delta_versions`,
`time_varying_is_transient`, `literal_profile_all_transient`, `chain_cover_minimal` (vs brute force),
`native_selected_only_if_capable`, `capability_refusal_bls0908`, `hidden_sender_only_when_read`,
`plans_validate_for_all_fixtures`, `planner_deterministic`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-plan
scripts/require-tests.sh blossom-plan growth_classes_ regime_table_row_1 regime_table_row_2 regime_table_row_3 regime_table_row_4 support_show_hide_plan counted_versions_ lookup_delta_versions time_varying_is_transient literal_profile_all_transient chain_cover_minimal native_selected_only_if_capable capability_refusal_bls0908 hidden_sender_only_when_read plans_validate_for_all_fixtures planner_deterministic
```

#### M5.2 — blossom-testkit I: corpus runner, status ratchet, oracle harness, xtask corpus/bless/coverage

- **Size:** ~6k
- **Depends on:** M4.1, M4.2, M3.6, M4.5
- **Owns:** `crates/blossom-testkit/**`, `xtask/src/cmd/corpus.rs`, `xtask/src/cmd/bless.rs`, `xtask/src/cmd/coverage.rs`, `tests/corpus/tools/**`, `tests/corpus/**/manifest.toml`, `scripts/ci.d/50-corpus.sh`, `docs/plan/coverage.md`, `docs/plan/notes/M5.2.md`
- **Features:** BENCH-000
- **Consumes:**
  - blossom-oracle
  - blossom-front (api::compile; ded::compile_ded)
  - blossom-analysis (P0 outputs)
  - PLAN §5 manifest schema v1
  - docs/design/plan.json
  - FEATURES.md
- **Provides:**
  - `cargo test -p blossom-testkit --test corpus` (libtest-mimic; one test per case × backend)
  - `cargo xtask corpus [--lint] [--check] [--filter P] [--backend B] [--require-pass B,...] [--ratchet --milestone Mk] [--status]`
  - `cargo xtask coverage` → docs/plan/coverage.md; fails on a P0/P1 id without a WP
  - `cargo xtask bless`
  - blossom_testkit::{manifest (schema v1 types), ratchet, harness::sync_rounds (multi-node sync-round harness over any `RoundEvaluator`), backends::{compile, analysis, oracle}, value_from_toml}
  - placeholder modules (owned later): backend_interp, backend_analysis_p1, backend_sim, backend_ldfi, backend_verify, differential, oracle_eval, conformance, alloc, molly, inputgen, equiv, sweep

**Build** (ARCHITECTURE §11.4, ARCH-28, BENCH-000; PLAN §5 is normative for the manifest and ratchet).
- **Manifest v1** as Rust types with `deny_unknown_fields`, loaded from every `tests/corpus/**/manifest.toml`;
  `xtask corpus --lint` implements everything `tests/corpus/tools/check_manifests.py` checks (then delete the
  Python tool and switch every reference in scripts to `xtask corpus --lint`). Fix schema-level errors in existing
  manifests (you own every `manifest.toml` in this milestone for schema fixes only; never change expectations).
- **Runner** (`tests/corpus.rs`, libtest-mimic): one trial per (case, backend); name `<area>/<dir>/<backend>`; the
  ratchet of PLAN §5.3 per backend: `pass` must pass; `unimplemented` must fail with `Unimplemented`/BLS0908 whose
  feature is in the list, and an unexpected success or any other failure fails with "stale status: update the
  manifest"; `known-failure` needs `issue`; a case whose `until` has passed (§5.3) fails regardless of status. Nothing is
  ever skipped.
- **Backends implemented here**: `compile` (expected diagnostics via `front::api::compile` or `front::ded`),
  `analysis` (P0 analysis outputs: strata, stratification errors with codes, points of order, path labels, ACLs;
  an unimplemented expectation key fails with `Unimplemented` naming its ANA id), `oracle` (sync rounds). Every other
  backend raises `Unimplemented` with the backend's feature id from PLAN §5.2 until its owning WP implements it.
- **CLI semantics**: `--filter` is a regular expression matched against `<area>/<case-dir>`; `--require-pass B,…`
  fails unless every matching case lists those backends with `status = "pass"` and they pass; `--check` enforces the
  ratchet with `until` *strictly before* the current milestone (read from `docs/plan/MILESTONE`), `--check --gate`
  with `until ≤` current (used only by the milestone gate).
- **Sync-round harness** over a small `RoundEvaluator` trait (implemented for `blossom_oracle::Oracle` here; the
  interpreter implements it in M7.3): deployment from `[deploy]` (nodes, roles, params, statics, `node_dir`,
  `R$members`), host `[[input]]`s at ticks, scripted `[[fault]]`s, each round every live node ticks with its inputs
  and the messages sent to it in the previous round (self-sends included), stop at quiescence or `ticks`.
- **Expectations**: `[[expect]]` (holds/absent ranges, exact contents at a tick, final contents,
  `quiescent_from`), `[[expect_send]]`, `[[expect_error]]`, `[[expect_diag]]`, `[expect_analysis]` (P0 keys); row
  values decoded from TOML by the target column types (`value_from_toml`, PLAN §5.1). Mismatch reports show the
  first differing tick, expected vs actual rows, and a hint of the relevant rule labels.
- **Ratchet updater** `xtask corpus --ratchet --milestone Mk` (used by the milestone gate): mechanical updates only
  — `unimplemented`/`known-failure` → `pass` when a backend now passes; refresh the `unimplemented` list to the
  actual feature(s) when a backend fails with `Unimplemented`; never touch a `pass` backend that fails (regression:
  hard error), never extend an `until` (hard error naming the case).
- **`xtask coverage`**: joins FEATURES.md ids, `docs/design/plan.json` feature lists, `// FEATURE: ID` markers in
  `crates/**`, `std/**`, `systems/**`, and corpus statuses into `docs/plan/coverage.md`; fails if a P0/P1 id has no
  WP in plan.json.
- **`xtask bless`**: writes `expected/` per-tick dumps only for cases whose manifest says
  `expected_from = "blessed"` (reserved for large outputs whose correctness was established another way, e.g. equality
  with a published count); refuses otherwise.
- `scripts/ci.d/50-corpus.sh` runs `xtask corpus --lint` and `--check` in both tiers.

**Triage.** Run the whole corpus on the M5 base: `.ded` cases run through `front::ded` + oracle; `.bls` cases fail
at lowering (BLS0908, M5.3 lands in parallel) — make sure every case's status is consistent so the gate is green.

**Required tests.** `manifest_schema_`, `manifest_rejects_unknown_keys`, `ratchet_rule_` (every rule, on synthetic
outcomes), `ratchet_updater_`, `value_from_toml_` (every type), `sync_rounds_self_send_next_round`,
`sync_rounds_quiescence`, `expect_holds_ranges`, `expect_diag_`, `oracle_backend_ded_program`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-testkit
cargo run -q -p xtask -- corpus --lint
cargo run -q -p xtask -- corpus --check
cargo test -p blossom-testkit --test corpus
cargo run -q -p xtask -- coverage
scripts/require-tests.sh blossom-testkit manifest_schema_ manifest_rejects_unknown_keys ratchet_rule_ ratchet_updater_ value_from_toml_ sync_rounds_self_send_next_round sync_rounds_quiescence expect_holds_ranges expect_diag_ oracle_backend_ded_program
```

#### M5.3 — blossom-front III: lowering the core language to IR

- **Size:** ~7.5k
- **Depends on:** M4.5, M3.2, M3.7, M4.1
- **Owns:** `crates/blossom-front/**`, `!crates/blossom-front/src/ded/**`, `tests/integration/tests/front3_*.rs`, `docs/plan/notes/M5.3.md`
- **Features:** LANG-027, LANG-040, LANG-041, LANG-042, LANG-043, LANG-045, LANG-046, LANG-047, LANG-060, LANG-061, LANG-062, LANG-063, LANG-064, LANG-068, LANG-080, LANG-081, LANG-082, LANG-083, LANG-085, LANG-086, LANG-087, LANG-088, LANG-089, LANG-090, LANG-091, LANG-092, LANG-093, LANG-094, LANG-097, LANG-100, LANG-102, LANG-103, LANG-104, LANG-108, LANG-109, LANG-110, LANG-111, LANG-114, LANG-115, LANG-122, LANG-123, LANG-128, LANG-150, LANG-152, LANG-173, LANG-181, LANG-190, LANG-200, LANG-240, LANG-280
- **Consumes:**
  - typed HIR (M4.5)
  - IrBuilder (M2.2)
  - fmt::normalize_for_hash (M3.7)
  - blossom-oracle (integration tests)
- **Provides:**
  - `front::api::compile` producing a `ValidatedProgram` for programs using core constructs
  - front::lower::{decl, builtins, handler, stmt, body, agg, choice, order, fold, invariant, timer, rule, expr, labels} implemented
  - front::lower::{soft, sealed, range, seal, finality, wrap, zset, resolve, seq, snapshot, delta, localize, service, aggdefault, argext_native_hint, multichoose, uda, ola, catalog, partition, table_fn, extern_lattice, cluster, migrate, translate} and front::{spec, lock} module files with fixed entry signatures that return BLS0908 naming their feature (implemented by M6.4/M6.5)
  - lock-less field-number assignment (LANGUAGE §19.2) in front::lock::assign

**Build** (ARCHITECTURE §13.9, core rows; LANGUAGE §4, §7–§11). One pass over the typed HIR; every generated rule and
relation is created inside the construct LANGUAGE's expansion belongs to; rule labels and site ids follow LANGUAGE
§4.3 exactly (`M::L$when`, `M::L/verb:target[#hash]`, `M::L$if#hash`, `M::V#hash`, `M::bootstrap/…`,
`M::N::op#k`), hashes over `fmt::normalize_for_hash` (LANG-068).
- **Declarations** (`lower::decl`, `builtins`): `table` (+ `r$del`, frame rule, `Persist`), `durable`, `scratch`,
  `view` (+ `v$u` for aggregate columns), `static` + `fact`, root `input`/`output` vs instance interfaces, `channel`
  (column 0 = destination; direction form `hidden_dest`; `#[fault]`, `#[accept]`, `#[replicated]` recorded),
  `loopback`, `cell`/`scratch cell`/`durable cell`/lattice tables (`Identity`), physical timers (`Event(Timer)`),
  logical timers (`name$left` expansion, `LogicalTimer`), `timer … once`, built-ins (`boot`, `recovered`, `stdin`,
  `session_*`, `stdout` host channel, `halt`, `localtick`, `node_dir`), `#[readonly] table`, `extern type`,
  `extern fn` (LANG-027, LANG-181), IR `fn`s.
- **Handlers** (`lower::handler`, `stmt`): the core algorithm of §13.9 steps 1–5 (`H$when` header relation with the
  header's named variables; `outer`/`any` alternatives; statement rules with `:=` bindings; `emit` deductive, `next`
  inductive, `send … to d` async with `D := d` or the `@` column, `delete` → `r$del`, `upsert` → `Upsert` construct
  (`r$ups` keyed, `r$del`, `@next`), `weight w` → `ZAdd`; `if`/`for` blocks `L$if#h`; `else` as the negated scalar
  guard; `bootstrap` (header `boot()`) and `bootstrap fresh` (`boot(), not recovered()`); LANG-190).
- **Bodies** (`lower::body`): positional and named atoms (LANG-080/081), `not r(…)` with `r$p1` projection for
  wildcards, `not { … }` helper, `let`, `where`, membership, generators (values, ranges, ring intervals, roles,
  unary relations, set-like lattices, range scans `r[lo..hi]`), lookups `r[k]`/cell reads/`m.at(k)` → `Lookup`
  (LANG-280), `from`/`principal` → `Atom::sender`/`principal`, `outer` (two header rules + `r$p1`, LANG-087), `any`
  (one rule per alternative, LANG-089), `forall` (`fa$h`, `fa$h$miss`), `R$members` atoms (LANG-152/240).
- **Aggregation, choice, order, folds**: head aggregates (`AggCall` over the header/block relation or `v$u`;
  LANG-100–104), `choose!`/`choose_least!`/`choose_most!`/`choose_rand!`/`sticky` → `Choose` construct with
  `$cand`/`$pmin`/`$chosen` (+ `$held`, `$keep`, `$fresh`, `$ovr`) (LANG-108/114/115), `argmin!`/`argmax!` (`$m`
  aggregate + join; ArgExt construct), `top!`/`limit!`/`percentile!`/`index!` (the quadratic reference expansion;
  `Index` construct; LANG-093/097), `fold!`/`reduce!` → `FoldOrdered` (LANG-109/110), `majority(s, R)` →
  `BuiltinFn::Majority` (LANG-111), lattice folds `lset{…}` etc. → `LatticeFold` + `Lookup` (LANG-123), lattice
  writes/lifts as merging `Insert` heads (LANG-122), canonical `OrderSpec` tiebreaks.
- **Invariants** (`lower::invariant`, LANG-200): `violation(name, key)` rules with `HeadMode::Violation`.
- **Roles**: `Rule::role` guard on every placed rule.
- **Advanced constructs**: create the module files listed under *provides* with their final entry-point signatures
  and wire the dispatch to them; each returns a BLS0908 diagnostic naming its FEATURES id and the construct's label.
  Specs: `spec::lower_specs` returns `Ok(vec![])` when there are no spec items and BLS0908 (`VER-001`) otherwise.
- **Field numbers**: explicit `#n` honored; missing numbers assigned per LANGUAGE §19.2 for a build without a lock;
  `FrontOutput::lock_proposal` carries them (M6.5 adds reading/validating an existing `schema.lock`).

**Pitfalls.** Header variables stay visible to statements so Tier C sees bindings; a choice in a header is computed
once (its own generated relations), not per statement; generated names always contain `$`; the IR validator is a
backstop — an `IrError` here is an internal error naming the construct.

**Required tests.** `lower_e01_kvs`, `lower_e02_reliable_broadcast`, `lower_e07_graph` (full validation + IR
snapshot), `lower_examples_ok_or_bls0908` (every example either lowers and validates or fails only with BLS0908
naming an advanced feature; this stays true after M6.4 lands), `lower_<construct>_snapshot` for every core construct, `rule_labels_language_4_3`,
`field_numbers_lockless`, integration `front3_oracle_e01_put_get` (lowered E1 on the oracle with a scripted
put/get/delete, expected replies), `front3_oracle_bootstrap_fresh`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-front
cargo test -p blossom-integration-tests --test front3_oracle
scripts/require-tests.sh blossom-front lower_e01_kvs lower_e02_reliable_broadcast lower_e07_graph lower_examples_ok_or_bls0908 lower_table_snapshot lower_handler_snapshot lower_choose_snapshot lower_outer_snapshot lower_forall_snapshot lower_fold_snapshot lower_lattice_fold_snapshot rule_labels_language_4_3 field_numbers_lockless
```

#### M5.4 — blossom-store II: durable encoding, identity, recovery order, migrations hook, store tooling, crashcheck

- **Size:** ~6k
- **Depends on:** M2.6, M4.4, M3.4, M4.8
- **Owns:** `crates/blossom-store/**`, `xtask/src/cmd/crashcheck.rs`, `crates/blossom-cli/src/cmd/store.rs`, `scripts/ci.d/80-crashcheck.sh`, `docs/plan/notes/M5.4.md`
- **Features:** DIST-021, DIST-022, DIST-033, DIST-081, SEM-071, LANG-044
- **Consumes:**
  - blossom-store I (M2.6)
  - blossom-wire tuple codec
  - blossom-schema (SchemaCatalog, MigrationSet)
  - blossom-engine boundary types (DurableImage shape)
- **Provides:**
  - blossom_store::{open_node_store(fs, dir, &StoreIdentity, &SchemaCatalog, &MigrationSet, OpenMode, &dyn MigrationRunner) -> OpenedStore, OpenedStore, Recovered, MigrationRunner (trait), FileDurability<F>, encode_wal_record(DurableDeltaRef…), DurableSnapshot encode/decode, NodeNameMap, tools::{inspect, verify, dump, backup, restore, truncate}}
  - `blossom store inspect|verify|dump|backup|restore|truncate`
  - `cargo xtask crashcheck` (store-level workloads)

**Build** (ARCHITECTURE §5.6, ARCH-10/25; DIST-021/022/033/081, SEM-071, LANG-044).
- **Catalog headers** (DIST-081): each WAL segment header and checkpoint MANIFEST carries storage format version,
  program id and version, finalized cluster version, catalog digest and `(relation id, name, schema hash, field
  layout)*` from `SchemaCatalog`.
- **WAL record bodies**: the durable delta of a tick `(relation id, inserts, deletes, cell deltas, in-place payload
  updates)*` encoded with the wire tuple codec, `Node` values **by name** (`NodeEncoding::ByName`); decoding against
  the segment's own header catalog.
- **Checkpoints** (DIST-022): `DurableSnapshot` = per-relation files in the tuple codec + native state blobs;
  encode/decode; taken from a `SyncedTick` only.
- **`open_node_store`** with the recovery order of §5.6 steps 1–6: lock + identity check (`store_uuid`,
  `deployment_id`, `program_id`, `node_name`, `principal`, `directory_digest`); `OpenMode::Existing` refuses a
  missing/empty directory with the exact message of §5.6 (never falls back to `bootstrap fresh`), `InitFresh`
  creates identity; load the checkpoint named by `CURRENT`; replay WAL after the checkpoint LSN, each segment against
  its own catalog; if the recovered catalog is older, call the `MigrationRunner` trait step by step (implemented by
  the runtime in M7.4, since it needs an engine), then write a new-version checkpoint covering the WAL end and start
  a new segment; a catalog that is neither current nor migratable is a refusal listing unknown relations, hashes and
  supported versions; reserve ticks (boot at `reserved + 1`, new bound `boot + 65 536`); increment restarts, new
  boot nonce from the caller's entropy, write META. `Recovered` carries the durable image, last tick, reserved bound,
  last now, restarts and boot nonce (SEM-071, DIST-033).
- **`FileDurability<F: Vfs>`**: the `WalWriter` + `CheckpointWriter` pair used by the runtime and the simulator.
- **Store tooling** + `blossom store` subcommands (`inspect` headers/catalogs/LSN ranges/incarnations, `verify` every
  CRC and checksum offline, `dump --rel`, `backup`/`restore` through a checkpoint, `truncate --at-lsn N
  --accept-data-loss` loud and audited with a META marker).
- **`cargo xtask crashcheck`**: runs the store-level crash-point enumeration (M2.6) over scripted durable workloads
  with catalogs, checkpoints and a migration step, asserting the recovery properties of §11.7; node-level crash
  points are added by M8.2. `scripts/ci.d/80-crashcheck.sh` runs it in the gate tier.

**Required tests.** `recovery_torn_tail_then_new_segment`, `recovery_corruption_refused`,
`identity_mismatch_refused`, `existing_mode_refuses_empty_dir_message`, `lock_held_refusal_names_pid`,
`tick_reservation_monotone_across_restarts`, `checkpoint_plus_wal_equals_full_replay` (proptest),
`segment_decoded_with_own_catalog`, `migration_runner_called_in_order`, `node_values_stored_by_name`,
`store_cli_inspect_verify`, `crashcheck_workloads`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-store
cargo run -q -p xtask -- crashcheck
scripts/require-tests.sh blossom-store recovery_torn_tail_then_new_segment recovery_corruption_refused identity_mismatch_refused existing_mode_refuses_empty_dir_message lock_held_refusal_names_pid tick_reservation_monotone_across_restarts checkpoint_plus_wal_equals_full_replay segment_decoded_with_own_catalog migration_runner_called_in_order node_values_stored_by_name crashcheck_workloads
```

#### M5.5 — blossom-node I: the sans-IO node, Invariant R, admission, timers, probation, MemTransport and ManualDriver

- **Size:** ~8k
- **Depends on:** M4.8, M4.4, M4.7, M3.4, M2.6
- **Owns:** `crates/blossom-node/**`, `tests/integration/tests/node_*.rs`, `docs/plan/notes/M5.5.md`
- **Features:** SEM-001, SEM-009, SEM-072, SEM-090, DIST-001, DIST-002, DIST-030, DIST-031, DIST-040, DIST-062, LANG-052, LANG-067, LANG-171, LANG-172, LANG-206, LANG-241
- **Consumes:**
  - blossom-engine boundary types (M4.8)
  - blossom-wire (decode_batch, scan_batch, encode_tuple, frames)
  - blossom-trace vocabulary
  - blossom-artifact (CompileOutput)
  - blossom-store types (WalRecordBuf, Recovered, SyncedTick, MemDurability)
  - blossom-schema (AclTable, SchemaCatalog)
- **Provides:**
  - blossom_node::{program::{CompiledProgram, CompiledRole, ExecutorKind, GeneratedProgram}, eval::{Evaluator, EvalSnapshot, impl Evaluator for Engine}, node::{Node, NodeConfig, NodePolicy, NodeState, NodeEvent, TickEffects, Effect, Released, NodeFault, NodeError, BootInfo, ShutdownStep, NodeSnapshot}, ingress::{admit, AdmittedBatch, Quotas, PoisonDenyList, ConnInfo, IngressSink}, transport::{Transport, SendReport, MemTransport}, env::{Clock, Entropy, MetricsSink, MetricId}, host::{HostServices, Service, OutputHandler, Row, HostBatch}, timers::TimerTable, manual::ManualDriver}

**Build** (ARCHITECTURE §5.1, §5.3 trait, §5.5, §5.8 admission, §5.12 mechanics; ARCH-03). No I/O, no clock reads, no
randomness: `xtask check-sans-io` must stay green.
- **Programs**: `CompiledProgram::from_output(CompileOutput, ExecutorKind)` validating ABI versions and digests;
  `CompiledRole`; `GeneratedProgram` (static data for codegen).
- **Evaluator** trait (§5.1) and `impl Evaluator for blossom_engine::Engine` delegating to the engine methods
  declared in M4.8 (it runs once the engine exists in M6.1).
- **Node** (§5.1 API): inbox batching into one batch per tick (CR-02) bounded by `max_batch_frames`/
  `max_batch_bytes`; `TickHeader` construction (tick, `now` sampled once per tick by the driver, incarnation,
  seeds, boot/recovered flags, cluster version); decode admitted batches straight into `Evaluator::ingest()` with
  `wire::decode_batch`; after `finish_tick`, encode **before returning**: the WAL record (durable delta) and outbox
  frames per (destination, channel) into pooled buffers; park encoded ticks; **Invariant R**: tick t is released iff
  every tick ≤ t with a WAL record has been reported synced (`wal_synced`), released in tick order with callbacks,
  subscription deltas, service calls and stdout; `wal_failed` faults the node and discards every parked tick;
  synced-but-unreleased ticks are released before a halt.
- **Tick numbering** (SEM-001): monotone across incarnations from `Recovered` (boot at the reserved bound) and
  `Effect::ReserveTicks` before passing it.
- **Timers** (DIST-030, LANG-172): `TimerTable` for `every`, `times`, `once after`, `once`; `next_deadline`; fires
  become `TimerFire { timer, count, at }`; **heartbeat** (SEM-009): `NodeConfig::heartbeat` schedules an empty tick
  when idle for plans with `empty_tick_effects`.
- **Admission** (DIST-062, SEM-090, LANG-241): the pure `admit(frame, conn, catalog, acl, quotas, deny, now,
  limits) -> Result<AdmittedBatch, RejectReason>` (steps 4–7 of §5.8: sid, kind vs channel, ACL, limits via
  `wire::scan_batch`, per-principal quotas of rows and interned bytes refilled from `now`, poison deny-list); every
  rejection is an omission and is counted; `sender`/`principal` attached from the connection identity, never read
  from the payload.
- **Probation** (§5.12 mechanics, ARCH-20): after a tick fault with ingress rows, `NodeState::Probation` runs one
  ingress row per tick for K rows/window; a faulting singleton yields `Effect::PoisonFound`; faults without ingress
  rows go to the policy (breaker lives in the runtime); `Effect::Quarantine` records.
- **Host inputs** (LANG-067): only at future ticks; `halt` (LANG-052) → `HaltRequest` stops after the tick;
  `#[atomic]` outputs (LANG-206) released as one unit.
- **Transport** trait (§5.3, DIST-001), `IngressSink`, `ConnInfo`, `MemTransport` (in-process, identity by
  construction); `Clock`, `Entropy`, `MetricsSink` (handles registered once; no per-tick label hashing);
  `HostServices`, `Service`, `OutputHandler` traits (execution in M9.8).
- **ManualDriver** (DIST-040): `run_tick`, `run_available`, `run_until_quiescent` over `MemTransport` with
  `MemDurability` or `FileDurability<RealFs>` (from M5.4, available after the gate; write it against the store I
  traits so either works).
- Trace completeness: a test enumerating every `NodeEvent` variant and `TickHeader` field against
  `blossom_trace::TraceEvent` (§6.4 rule).

**Pitfalls.** No engine word outlives its tick; pooled buffers everywhere on the steady path (zero-allocation is
asserted in M7.3); release order must be tick order even when syncs complete out of order.

**Required tests.** `invariant_r_property` (proptest: fake evaluator, random WAL/no-WAL ticks and sync completion
orders), `inbox_batch_bounds`, `admit_reject_<reason>` for every `RejectReason`, `quotas_refill_from_now`,
`probation_isolates_poison_row`, `wal_failed_discards_parked`, `timers_fire_order`, `heartbeat_empty_tick`,
`halt_after_tick`, `tick_numbers_monotone_across_incarnations`, `trace_event_completeness`,
integration `node_manual_driver_pingpong` (fake evaluator over MemTransport).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-node
cargo run -q -p xtask -- check-sans-io
cargo test -p blossom-integration-tests --test node_manual
scripts/require-tests.sh blossom-node invariant_r_property inbox_batch_bounds admit_reject_ quotas_refill_from_now probation_isolates_poison_row wal_failed_discards_parked timers_fire_order heartbeat_empty_tick halt_after_tick tick_numbers_monotone_across_incarnations trace_event_completeness
```

#### M5.6 — blossom-analysis II: CALM certificates, determinism, FDs, streams, taint, CRDT classes

- **Size:** ~7k
- **Depends on:** M4.2
- **Owns:** `crates/blossom-analysis/**`, `docs/plan/notes/M5.6.md`
- **Features:** ANA-015, ANA-021, ANA-025, ANA-026, ANA-027, ANA-028, ANA-029, ANA-030, ANA-031, ANA-032, ANA-033, ANA-036, ANA-037, ANA-038, ANA-039, ANA-080, ANA-141, ANA-142, ANA-143, SEM-087
- **Consumes:**
  - blossom-analysis I (M4.2)
  - blossom-artifact cert types
  - blossom-ir fixtures
- **Provides:**
  - blossom_analysis::{polarity (full ANA-021), calm::certificates, determinism::{classes, certificate}, fd::infer, streams, taint, crdt::classify, emission::classes} → OutputCertificate fields

**Build** (ARCHITECTURE §7.1–§7.3 rows for these ids; R05; R12; R13).
- **Polarity** (ANA-021): forward dataflow over `DepGraph` with the {⊥, +, −, ±} lattice; path polarity for every
  (source, sink) using bitsets.
- **Certificates** (ANA-025/026/141–143): Dedalus+ and Dedalus_S (and their ^L versions) as structural checks over
  the graph, polarity and growth classes; ODD-51 (b): ephemeral heads accepted when every consumer is a
  join-morphism into persistent state; a threshold without `join_prime` over an ephemeral lattice fails ANA-141
  (BENCH-302); "confluent but not certified" is its own verdict (ANA-143). Output fills `OutputCertificate`.
- **Membership dependence** (ANA-027), **final outputs** (ANA-028), **early-emission classes** T/L/A/H/W (ANA-036),
  **map determinism and resumability certificate** (ANA-037).
- **Determinism** (ANA-029, 038, 039, SEM-087): the class lattice deterministic < seed-dependent <
  schedule-dependent propagated forward; every output gets **exactly one** of confluent / confluent given seals /
  coordinated at X by protocol Y / nondeterministic by design (CR-29); `#[deterministic]` outputs that come out
  schedule-dependent are BLS0603; ANA-038 decides "ties broken by canonical order" by FD closure (conditions D1–D6)
  with column taint for D3.
- **FD inference** (ANA-080): per-relation FD sets with Armstrong closure over attribute bitsets; sources: keys,
  equalities, injective functions, lattice FDs, aggregate group → value, choose X̄ → Ȳ, channel keys.
- **Streams** (ANA-030/031/015): forward dataflow over (boundedness, order, retries) seeded by channel fault
  models; a fold over at-least-once input needs a **Proved** idempotence claim or a wrapped channel.
- **Taint** (ANA-032) from each point of order to the outputs it reaches, attached to diagnostics as evidence.
- **CRDT query classification** (ANA-033): local vs quorum with the forcing operation.

**Required tests.** `polarity_paths_`, `cert_dedalus_plus_`, `cert_dedalus_s_`, `cert_l_threshold_join_prime`,
`confluent_not_certified_verdict`, `determinism_exactly_one_verdict` (proptest over generated programs),
`bls0603_deterministic_violation`, `fd_closure_`, `ana038_d1_` … `ana038_d6_`, `streams_retry_fold_needs_proof`,
`taint_evidence_`, `crdt_local_vs_quorum`, `emission_classes_`, `membership_dependence_`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-analysis
scripts/require-tests.sh blossom-analysis polarity_paths_ cert_dedalus_plus_ cert_dedalus_s_ cert_l_threshold_join_prime confluent_not_certified_verdict determinism_exactly_one_verdict bls0603_deterministic_violation fd_closure_ ana038_d1_ ana038_d6_ streams_retry_fold_needs_proof taint_evidence_ crdt_local_vs_quorum emission_classes_ membership_dependence_
```

#### M5.7 — blossom-prov I: provenance graphs, why/whynot, Tier B proof search and rendering

- **Size:** ~4.5k
- **Depends on:** M4.3, M3.2
- **Owns:** `crates/blossom-prov/**`, `docs/plan/notes/M5.7.md`
- **Features:** TEST-050, TEST-051, TEST-061, ENG-111
- **Consumes:**
  - blossom-kernel::prov (FiringLog, records, ProvRead)
  - blossom-ir (programs, constructs for surface mapping)
- **Provides:**
  - blossom_prov::{ProvGraph, Goal, Firing, Premise, MessageRecord, build(&FiringLog, messages, choices) -> ProvGraph, why(fact, tick) -> Explanation, whynot(fact, interval) -> WhyNot, tierb::search(&dyn ProvRead, …), render::{dot, json}, normalize (collapse persistence chains)}

**Build** (ARCHITECTURE §4.9, §8.2; TEST-050/051/061, ENG-111 search side).
- `ProvGraph` exactly as §8.2 (goals, firings, alternative derivations per goal at a tick, premises, messages with
  receive tick or LOST), `Premise::{Goal, Clock, Alive, Neg, Contributors, ExactRead, Choice, Leaf}`, built from a
  Tier C `FiringLog` + message log + choice log; memoized DAG; persistence chains stay as frame firings.
- `why(fact@t)` (TEST-050): derivation trees collapsed to surface constructs (generated relations are
  provenance-transparent: reports show labels and statements, never `$` names); `whynot` over time intervals
  (TEST-051): which premises were missing at which ticks.
- Tier B (ENG-111, search side): lazy top-down proof search over `ProvRead` (hidden `(rule, height)` annotations),
  with an enumerate-all mode; the capture side is M7.1.
- Rendering (TEST-061): DOT and JSON; `normalize` collapses persistence chains for display.

**Required tests.** `build_graph_from_synthetic_log`, `goal_alternatives_`, `receive_premise_per_sender_and_tick`,
`why_collapses_generated_relations`, `whynot_interval_`, `tierb_search_finds_all_proofs`, `render_dot_snapshot`,
`render_json_snapshot`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-prov
scripts/require-tests.sh blossom-prov build_graph_from_synthetic_log goal_alternatives_ receive_premise_per_sender_and_tick why_collapses_generated_relations whynot_interval_ tierb_search_finds_all_proofs render_dot_snapshot render_json_snapshot
```

### M6 — The interpreter and the full surface language

**Goal.** The interpreter executes physical plans and must match the oracle tick by tick; the planner gains fusion, perturbation and dumps; the compile pipeline and the first CLI commands exist; the frontend lowers every construct, specs, locks and migrations; the P1 analyses land; the corpus is triaged on the oracle.

**Gate.** `scripts/milestone-gate.sh M6` (PLAN §3).

#### M6.1 — blossom-engine I: the tick, staging, the interpreter, maintenance regimes and snapshots

- **Size:** ~9k
- **Depends on:** M5.1, M4.3, M4.8, M4.1, M5.5
- **Owns:** `crates/blossom-engine/**`, `crates/blossom-node/src/eval/**`, `tests/integration/tests/engine1_*.rs`, `docs/plan/notes/M6.1.md`
- **Features:** SEM-002, SEM-012, SEM-050, SEM-081, SEM-105, ENG-003, ENG-042, ENG-043, ENG-047, ENG-049, ENG-061, ENG-062, ENG-100, ENG-140, LANG-170, DIST-005
- **Consumes:**
  - blossom-plan (plans for tests, via tests/integration)
  - blossom-kernel I+II
  - engine boundary types (M4.8)
  - blossom-oracle (differential integration tests)
  - blossom-ir fixtures
- **Provides:**
  - a working `Engine` (new, load_durable, begin_tick, ingest, finish_tick, snapshot, fork, view, state_digest, wants_tick, values, capabilities)
  - the interpreter executor (`ExecutorFactory::interpreter()`)
  - the complete `engine::abi` (ExecCtx methods of ARCHITECTURE §4.7 incl. batch/iterator types)

**Build** (ARCHITECTURE §3.3–§3.5, §3.11, §4.7, §4.8, §4.11; SEM-002 steps 1–3).
- **Tick** (§3.11): `begin_tick` (new epoch, truncate transient, `apply_staged`: deaths for staged `$del` minus
  cancelled (insert wins, CR-05), the §3.4.4 support transfer, staged `@next` inserts and merges, carried-segment
  deltas, upsert/resolve results computed at t−1, SEM-050 key checks → BLSR001, the death list for `ZOld`; set
  scalars), `ingest() -> &mut dyn WordSink` straight into the ingest arena, `finish_tick` (dirty marking, strata in
  order with only dirty ones run (ENG-061), temporal phase, violations, fueled compaction, borrowed
  `TickOutputRef`). Any `TickError` poisons the engine.
- **Interpreter** (§4.8): each `OpTree` compiled once into a shadow tree with pre-resolved ABI handles and kernels
  specialized from a measured set of shapes plus a slice fallback; push execution over `BindBatch`es (≤ 128);
  scalar path for single bindings; `CExpr` evaluated per batch column where possible.
- **Regimes** (§3.4): Standing continuation from `TickNew`; Transient from tick-local occurrences; Counted delta
  queries with `ZNew`/`ZDelta`/`ZOld` + death list, derivation counts in `WeightedStore<Derivations>`; Recompute
  with diffing; deductive support (§3.4.4) exactly as planned; semi-naive loop (ENG-041/042) with lattice Δ =
  strictly increased cells carrying full values (ENG-043), dioids via strict-improvement joins (ENG-049);
  Kleene-round iteration bound → BLSR007 with witness (ENG-047/140).
- **Temporal phase** (§3.5): staging (Δ⁺V and cancelled deletions only for inductive-into-table), carried segments,
  the outbox with **all** of V(t), merged at the sender for lattice columns (CR-52, SEM-105, DIST-005) and
  key-checked at the sender (SEM-050).
- **Natives**: `Persist` and `Identity` (ENG-003: persistence is storage — rows are not cleared; deaths from staged
  `$del`); `capabilities()` lists exactly what is implemented so the planner plans expansions for everything else.
- **Snapshots/forks** (§4.11): `EngineSnapshot` per §4.11; `fork` rebuilds indexes lazily; `view(rel, None)`
  current rows as `Value`s (as-of reads are M8.6); `state_digest` via `DigestSink` when enabled.
- **ABI** (§4.7): complete `abi::ExecCtx` methods over the store (the interpreter uses the same surface codegen will).
- Adjust `crates/blossom-node/src/eval/**` (the `Evaluator for Engine` impl) if the engine surface needed changes.

**Differential tests** (`tests/integration/tests/engine1_*.rs`, may use plan + oracle): for **every IR fixture**
and every `.ded` corpus program, run the oracle and the interpreter tick by tick under `Production` and
`Literal` profiles and compare every relation, the outbox, the durable delta and errors exactly; the §3.4.4
deletion cases (show/hide), lookup-Δ cases, the `$now` Transient case, the level-triggered zset writer case, BLSR001
and BLSR007 equality with the oracle.

**Required tests.** integration `engine1_fixtures_match_oracle`, `engine1_literal_profile_matches_oracle`,
`engine1_support_show_hide`, `engine1_lookup_delta`, `engine1_now_transient`, `engine1_zset_level_triggered`,
`engine1_errors_match_oracle`; unit `snapshot_fork_equivalent`, `idle_tick_zero_alloc` (counting allocator),
`dirty_scheduling_skips_clean_strata`, `outbox_merge_at_sender`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-engine
cargo test -p blossom-integration-tests --test engine1_differential
scripts/require-tests.sh blossom-engine snapshot_fork_equivalent idle_tick_zero_alloc dirty_scheduling_skips_clean_strata outbox_merge_at_sender
scripts/require-tests.sh blossom-integration-tests engine1_fixtures_match_oracle engine1_literal_profile_matches_oracle engine1_support_show_hide engine1_lookup_delta engine1_now_transient engine1_zset_level_triggered engine1_errors_match_oracle
```

#### M6.2 — blossom-plan II: tick-local fusion, in-out trees, perturbation, plan dumps and hints

- **Size:** ~4.5k
- **Depends on:** M5.1
- **Owns:** `crates/blossom-plan/**`, `docs/plan/notes/M6.2.md`
- **Features:** ENG-006, ENG-007, ENG-083, LANG-053, TEST-063
- **Consumes:**
  - blossom-plan I (M5.1)
- **Provides:**
  - fuse_tick_local pass (inline/buffer+Tee/dedup elision), PipelinePlan in-out trees, PlanProfile::Perturbed { seed }, plan dumps (`dump::{ir, strata, regimes, fusion, plans, indexes, natives, dataflow}`), `#[plan(...)]`/`#[materialize]` hints

**Build** (ARCHITECTURE §3.7, §3.10, §3.12, §11.3).
- **Tick-local fusion** (§3.7, ENG-083): inline (the four conditions), buffer with `Op::Tee` fan-out, dedup elision
  when every consumer is idempotent; runs after growth classes, before regimes; `PlanLimits::fuse` switch.
- **In-out trees** (ENG-006): acyclic tick-local regions as one `PipelinePlan` (pull fan-in, pivot, push fan-out
  over arena buffers, double-buffered for `next`), static topological order.
- **Perturbation** (§11.3): `PlanProfile::Perturbed { seed }` randomizes join order among valid alternatives, hash vs
  sorted, lazy vs eager, natives on/off, fusion on/off, dedup elision on/off, demotion of Standing/Counted to
  Recompute (incl. writers with deductive support), in-place update on/off, batch sizes, the whole `Literal` profile.
- **Dumps** (ENG-007, TEST-063): `--dump {ir|strata|regimes|fusion|plans|indexes|natives|dataflow}`, deterministic,
  mermaid and DOT dataflow graphs.
- **Hints** (LANG-053): `#[plan(...)]` and `#[materialize]` pin choices and never change meaning.
- **E1 cost check**: a plan-level assertion that one `put` in the fused E1 plan costs ≤ 2 hash operations plus the
  choice (§3.7 worked example).

**Required tests.** `fusion_inline_`, `fusion_buffer_tee_`, `dedup_elision_`, `pipeline_plan_`,
`perturbed_plans_validate` (every fixture × 16 seeds), `dump_<kind>_snapshot` for each dump kind,
`hints_do_not_change_meaning`, `e01_put_two_hash_ops`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-plan
scripts/require-tests.sh blossom-plan fusion_inline_ fusion_buffer_tee_ dedup_elision_ pipeline_plan_ perturbed_plans_validate dump_regimes_snapshot dump_dataflow_snapshot hints_do_not_change_meaning e01_put_two_hash_ops
```

#### M6.3 — blossom-driver and CLI I: CompileSession, caching, diagnostics rendering, check/fmt/build/plan/explain

- **Size:** ~5k
- **Depends on:** M5.3, M5.1, M5.6, M3.7, M3.6
- **Owns:** `crates/blossom-driver/**`, `crates/blossom-cli/src/cmd/check.rs`, `crates/blossom-cli/src/cmd/fmt.rs`, `crates/blossom-cli/src/cmd/build.rs`, `crates/blossom-cli/src/cmd/plan.rs`, `crates/blossom-cli/src/cmd/explain.rs`, `crates/blossom-cli/src/common/**`, `tests/integration/tests/cli1_*.rs`, `docs/plan/notes/M6.3.md`
- **Features:** TEST-091
- **Consumes:**
  - blossom-front (api::compile, ded)
  - blossom-analysis
  - blossom-plan
  - blossom-artifact
  - blossom-std-src
  - blossom-syntax::fmt
- **Provides:**
  - blossom_driver::{CompileSession, CompileOptions { target: PlanTarget::{Interpreter(caps), Codegen(caps), Any}, profile, strict, message_format }, compile(...) -> Result<CompileOutput, Diagnostics>, cache::ArtifactCache, render::{human, json}, explain::long_form(code), passes (rewrite hook list, empty)}
  - `blossom check | fmt | build | plan --dump … | explain BLSnnnn`

**Build** (ARCHITECTURE §1.4, §12.1, §12.5).
- **CompileSession**: sources (files + `std::` via std-src) → `front::api::compile` or `front::ded::compile_ded` →
  analyses (stratification required; certificates when requested or `--strict`) → rewrite passes (a `passes`
  module with an ordered hook list, empty now; M7.5 owns it) → planning per role and per spec with the target's
  capability set (`PlanTarget::Any` skips the capability check, for dumps) → `CompileOutput`.
- **Artifact cache** keyed by program digest, plan digest and ABI versions (`target/blossom-cache` by default,
  configurable); unchanged programs are never replanned.
- **Diagnostics** (TEST-091): codespan-reporting rendering with primary/evidence spans, notes, fix-its; JSON with
  `--message-format=json`; `--strict` turns warnings into errors (ODD-10 (c)).
- **`blossom explain BLSnnnn`**: long forms for every registered code (meaning, example, fix) kept as text in the
  driver (`explain/*.md` embedded).
- **CLI** (you own the listed `cmd/*.rs` files and `src/common/**`): `check [--strict] <file|dir>…` (a directory checks
  every `.bls` below it; a module without a `program` header is checked in **library mode**: load, resolve, type-check,
  classify and lint, with generic items checked at their declared bounds; this is how `std/` and `systems/*/bls` are
  checked), `fmt [--check]`, `build`
  (writes the artifact), `plan --dump {…}`, `explain`; exit codes of §12.5; the interpreter capability set comes
  from `blossom_engine::Engine::capabilities()`.

**Required tests.** `compile_examples_supported` (every example that the current lowering supports compiles; the
rest fail only with BLS0908), `cache_hit_skips_planning`, `diagnostics_json_snapshot`, `strict_turns_warnings_into_errors`,
`explain_every_code`, integration `cli1_check_exit_codes`, `cli1_fmt_check`, `cli1_plan_dump`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-driver blossom-cli
cargo test -p blossom-integration-tests --test cli1_commands
scripts/require-tests.sh blossom-driver compile_examples_supported cache_hit_skips_planning diagnostics_json_snapshot strict_turns_warnings_into_errors explain_every_code
```

#### M6.4 — blossom-front IV: lowering every advanced construct

- **Size:** ~7.5k
- **Depends on:** M5.3
- **Owns:** `crates/blossom-front/src/lower/**`, `!crates/blossom-front/src/lower/migrate.rs`, `!crates/blossom-front/src/lower/translate.rs`, `tests/integration/tests/front4_*.rs`, `docs/plan/notes/M6.4.md`
- **Features:** LANG-048, LANG-049, LANG-050, LANG-071, LANG-095, LANG-098, LANG-105, LANG-106, LANG-112, LANG-113, LANG-116, LANG-117, LANG-135, LANG-138, LANG-139, LANG-153, LANG-154, LANG-158, LANG-183, LANG-184, LANG-202, LANG-207, LANG-212, SEM-060, SEM-061, ANA-121
- **Consumes:**
  - front III lowering infrastructure (M5.3)
  - IrBuilder constructs (M2.2/M3.2)
  - blossom-oracle (integration tests)
- **Provides:**
  - lowering for soft/sealed/range tables, delta literals, localize, seq!, UDAs, per/default drivers, combiners, ola aggregates, multi-choice, resolve policies, extern lattices, zset/bag + Z-set views, snapshots, cluster roles, partition by, exactly_once channels, table functions, services, catalog relations, seals, finality

**Build** (ARCHITECTURE §13.9 table rows not covered by M5.3; the exact expansions printed in LANGUAGE).
- `soft table … ttl … max` (LANG-048, SEM-060/061, CR-17): `$b`, `$s`, `$n`, `$all`, `$live`, `$rank` exactly as
  LANGUAGE §7.9 prints them; `SoftTable` construct. `sealed table` (LANG-049): frame without `$del` +
  `r$sealed() :- notin boot()`. `range(c)` (LANG-050).
- `inserted`/`deleted` (LANG-071): `r$prev` + its `@next` rule (durability inherited) + `notin`; `DeltaRead`.
- `#[localize(chain|link)]` (LANG-095): NDlog Algorithm 2, one hop channel per location change; BLS1005.
- `seq!` (LANG-098): `assigned`/`hwm` expansion; `Seq` construct.
- `aggregate` items → `UdaDecl` (LANG-105); `per` drivers and `default` (LANG-106): `v$u`, `v$a`, `v$ak` + default
  rule, `AggDefault`; decomposable aggregates and derived combiners (LANG-112) recorded for sender-side partials;
  `ola_*!`/`scale_by_progress!` (LANG-113) with `#[nondet("progressive")]`.
- Several choice literals (LANG-116): `fold_ordered` scan + `$member`, `MultiChoose`.
- Relation-level `resolve` (LANG-117): `r$cand`, `r$cmin`, the frame-rule replacement, writes target `r$n`; `Resolve`.
- `extern lattice` / user lattice items (LANG-135): `LatticeCtor::Extern`, law obligations recorded.
- `zset table`/`bag table` (LANG-138) with `ZAdd` heads; `distinct!`, `clamped!`, `weights!` Z-set views;
  `exactly_once(…)` channels (LANG-158): wrapper rules `$zdelta`, `$unwrap`, `$entries`, `Wrapped`.
- `snapshot` (LANG-139): `reveal!` gated by the progress threshold; `Snapshot`.
- Cluster roles and projection (LANG-153); `partition by` (LANG-154): `$route(R, e)` destinations and the BLSR008
  ownership check rule for tables.
- `extern table fn` (LANG-183): `FnBody::TableFn` and `GenSource::TableFn`; `service` (LANG-184): `name(@$host, …)`
  channel + `name.result` event, `Service` construct.
- `catalog.*` generated statics (LANG-202).
- `seal`, `sealed … [from m]`, input seals, local seals (LANG-207): `$out`, `$cnt`, `$mine`, `$seal`, `$frozen`,
  `$in`, `$sl`, `$rc`, `$sealed_from`, `$open`, `$sealed`, violations; `line$sealed` from `EventSource::InputSeal`;
  `Seal` construct.
- `final output`, `final`/`final not`, `when_final` (LANG-212, ANA-121): the M⁻/M⁺ bounds programs and the status
  relation; `Finality` construct.
After this WP every example E1–E10 lowers (specs excepted, M6.5).

**Required tests.** `lower_<construct>_snapshot` for every construct above; `lower_all_examples` (E1–E10 validate;
spec items may still be BLS0908 until M6.5 merges); integration on the oracle: `front4_soft_ttl_mocked_now`,
`front4_seal_unanimity`, `front4_resolve_policies`, `front4_seq_stable_across_ticks`,
`front4_zset_level_triggered`, `front4_finality_status`, `front4_exactly_once_dedup`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-front --all-features lower::
cargo clippy -p blossom-front --all-targets --all-features -- -D warnings
cargo test -p blossom-integration-tests --test front4_oracle
scripts/require-tests.sh blossom-front lower_soft_snapshot lower_seal_snapshot lower_finality_snapshot lower_resolve_snapshot lower_wrapped_snapshot lower_seq_snapshot lower_delta_snapshot lower_localize_snapshot lower_service_snapshot lower_all_examples
```

#### M6.5 — blossom-front V: specs, schema lock, migrations, translations and the compatibility gate

- **Size:** ~6k
- **Depends on:** M5.3, M4.2, M3.4
- **Owns:** `crates/blossom-front/src/spec/**`, `crates/blossom-front/src/lock/**`, `crates/blossom-front/src/lower/migrate.rs`, `crates/blossom-front/src/lower/translate.rs`, `crates/blossom-cli/src/cmd/compat.rs`, `crates/blossom-cli/src/cmd/release.rs`, `tests/corpus/upgrade/BENCH-224*/**`, `scripts/ci.d/55-compat.sh`, `tests/integration/tests/front5_*.rs`, `docs/plan/notes/M6.5.md`
- **Features:** VER-001, LANG-070, LANG-201, LANG-260, LANG-262, LANG-265, TEST-080, TEST-108, ANA-010, BENCH-224
- **Consumes:**
  - front III (M5.3)
  - blossom-schema (lock file, rules table)
  - blossom-analysis::compat (M4.2)
- **Provides:**
  - front::spec::lower_specs → SpecProgram (interval trace relations, quorum constructs, checks/expects)
  - front::lock (read/validate/update schema.lock; reserved numbers; version hashes)
  - MigrationDecl/TranslationDecl lowering
  - `blossom compat --check [--transitive]`, `blossom release`
  - tests/corpus/upgrade/BENCH-224* compat corpus

**Build** (ARCHITECTURE §2.8, §13.10, §13.11; LANGUAGE §17, §19).
- **Specs** (VER-001, LANG-201, LANG-070, TEST-080): a spec's target compiled through the same pipeline; `include`
  merges specs; `nodes`/`assign`; `faults` → `FailureModel` (and `CrashView::Frozen` for `.bls` targets); scenario
  facts (`fact r(…) @ n` into statics; `@ n at tick k` into inputs; BLS0405 otherwise); views, invariants and
  liveness as stratified rules over trace relations with every target atom carrying `SpecAt`; `ever`, `sent … @ d
  from s`, `crashed(n)`, `crash(n, t)`, `hb`; lowering to interval relations `r$hist(N, X̄, From, To)` with
  `From <= P < To` (§2.8); `quorum v in R { B }` → `Quorum` construct; `check`/`expect` → `CheckDecl`s; BLS0900
  (pre/post schema), BLS0901, BLS0902, BLS0509 and ANA-010 oracle containment (protocol rules may not read `crash`,
  `r$log`, `hb`).
- **Schema lock** (LANG-260/261/265): read/validate/update `schema.lock` via blossom-schema; reuse numbers; never
  reuse retired ones (`#[reserved]`); `#[since]`, `#[deprecated]`, `#[renamed_from]`, semantic-change annotations;
  a schema change without a version bump BLS0903; incompatible change BLS0904.
- **Migrations and translations** (LANG-262 lowering part): `migrate from N` blocks as separate rule sets over
  `old.r` typed by version N's lock entry (only `while` handlers with `emit`; BLS0906 otherwise); synthesized
  migrations for add-defaulted-field/project/widen/rename; `translate c to/from N` tuple-local handlers → `TranslationDecl`.
- **Compatibility gate** (TEST-108, ANA-100 via M4.2): `blossom compat --check [--transitive]` with JSON output and
  exit codes; `blossom release` appends a version to `schema.lock`; `scripts/ci.d/55-compat.sh` runs compat for every
  program with a lock (none in `systems/` yet; the fragment must handle that).
- **Corpus**: author `tests/corpus/upgrade/BENCH-224*` (the compatibility checker corpus: one case per rules-table
  row, `[backend.compile]` with `[[expect_diag]]` findings) and make them pass on the `compile` backend.

**Required tests.** `lower_e10_specs`, `spec_interval_relations_snapshot`, `spec_quorum_construct`,
`bls0900_pre_post_schema`, `ana010_containment`, `lock_reuse_and_reserved`, `bls0903_no_version_bump`,
`bls0904_incompatible`, `migration_synthesized_`, `bls0906_bad_migration`, `translate_tuple_local`,
integration `front5_compat_cli`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-front --all-features spec:: lock::
cargo clippy -p blossom-front -p blossom-cli --all-targets --all-features -- -D warnings
cargo test -p blossom-integration-tests --test front5_compat
cargo run -q -p xtask -- corpus --check --filter upgrade/BENCH-224 --require-pass compile
scripts/require-tests.sh blossom-front lower_e10_specs spec_interval_relations_snapshot spec_quorum_construct bls0900_pre_post_schema ana010_containment lock_reuse_and_reserved bls0903_no_version_bump bls0904_incompatible migration_synthesized_ bls0906_bad_migration translate_tuple_local
```

#### M6.6 — blossom-analysis III: Blazes, Edelweiss analysis, finality, key conflicts, compat P1, lints

- **Size:** ~7.5k
- **Depends on:** M5.6
- **Owns:** `crates/blossom-analysis/**`, `docs/plan/notes/M6.6.md`
- **Features:** ANA-003, ANA-006, ANA-007, ANA-008, ANA-009, ANA-011, ANA-040, ANA-041, ANA-042, ANA-043, ANA-045, ANA-060, ANA-065, ANA-101, ANA-102, ANA-103, ANA-104, ANA-106, ANA-120, ANA-122, ANA-140, ENG-142, SEM-016, SEM-017, SEM-109
- **Consumes:**
  - blossom-analysis II (M5.6)
  - blossom-schema (compat rules)
  - blossom-artifact cert types
- **Provides:**
  - blossom_analysis::{blazes, edelweiss::{sublanguage, persistence_inference, seal_inference}, finality::{classes, automaton}, keys::{lint, prove_fd}, compat::{rollout_order, gated_writes, migration_classes, lattice_migration_morphisms, sender_binding}, lints::{temporal_safety, soft_state, underspecification, purity, choice_order_random}, termination, delta_ship::channel_frame_kinds}

**Build** (ARCHITECTURE §7.2 rows for these ids; R05 (Blazes, Edelweiss), R13 (finality), R15 (compat)).
- **Blazes** (ANA-040–043, 045): component graph (module instances, SCCs, `#[component]` groups); path annotations
  CR/CW/OR_gate/OW_gate; label propagation (NDRead, Taint, Seal, Async, Run, Inst, Diverge) with rules 1–4 and 1′ and
  the CR-27 resolutions; `#[replicated]` (ANA-041); `compatible(gate, key)` by the injective-FD chase (ANA-043,
  union-find over (relation, column) classes); sink report (ANA-045).
- **Edelweiss analysis** (ANA-060, 065): sublanguage check and persistence inference; seal inference (epoch
  punctuations, `flat_map` seals) marking relations or partitions CLOSED.
- **Finality** (ANA-120, 122, SEM-016/017): static classes POS/NEG/TOP/THRESH/MIXED/FINITE/SEALED/NEVER with
  evidence; BLS0705 inverse curse (Z-set inputs, deletable host inputs, delete/upsert driven by unsealed input,
  PN-style values; weighted relations OPEN unless sealed); ANA-122 exact free termination for `#[finite]`
  components (per-key abstract transition graph under a state cap — exceeding it is a hard error —, Tarjan SCC,
  FT marking in reverse topological order, a collapsed automaton table for an O(1) runtime test).
- **Key conflicts** (ANA-007 lint with the two-message example and the §0.3 L7 extension; ANA-140 proof via FD
  inference; when proved, the plan may drop the runtime BLSR001 check — expose the fact for the planner).
- **Compat P1** (ANA-101 rollout order with cycle report, ANA-102 gated-write dominance check, ANA-103 migration
  classes, ANA-104 lattice migrations must be morphisms, ANA-106 sender binding).
- **Lints** (ANA-003 temporal safety, ANA-006 soft-state incl. expiry-without-timer, ANA-008 underspecification,
  ANA-009 purity, ANA-011 choice/order/randomness), **ENG-142** termination classes of lattice recursion (ACC,
  p-stable, PreM, unknown → warning), **SEM-109** delta-shipping side condition → per-channel frame kind fact.

**Required tests.** `blazes_label_propagation_`, `injective_fd_chase_`, `edelweiss_persistence_inference_`,
`seal_inference_`, `finality_classes_`, `bls0705_inverse_curse`, `ana122_automaton_`, `ana122_state_cap_error`,
`key_conflict_lint_two_messages`, `key_fd_proved_drops_check`, `rollout_order_cycle`, `gated_write_dominance`,
`lattice_migration_must_be_morphism`, `lint_<name>` for each lint, `termination_classes_`, `delta_ship_side_condition`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-analysis
scripts/require-tests.sh blossom-analysis blazes_label_propagation_ injective_fd_chase_ edelweiss_persistence_inference_ seal_inference_ finality_classes_ bls0705_inverse_curse ana122_automaton_ ana122_state_cap_error key_conflict_lint_two_messages key_fd_proved_drops_check rollout_order_cycle gated_write_dominance lattice_migration_must_be_morphism termination_classes_ delta_ship_side_condition
```

#### M6.7 — Corpus triage I: the golden corpus on the oracle

- **Size:** ~1–3k lines of case fixes
- **Depends on:** M5.2, M5.3, M4.1, M1.2, M1.3, M1.4, M1.5
- **Owns:** `tests/corpus/core/**`, `tests/corpus/lattices/**`, `tests/corpus/lprov/**`, `tests/corpus/async/**`, `tests/corpus/net/BENCH-100*/**`, `tests/corpus/net/BENCH-101*/**`, `tests/corpus/net/BENCH-102*/**`, `tests/corpus/verify/**`, `tests/corpus/ldfi/**`, `docs/plan/notes/M6.7.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - the corpus runner (M5.2)
  - core lowering (M5.3)
  - the oracle (M4.1)
  - the M1 corpus
- **Provides:**
  - a triaged corpus: every case either passes on the oracle, or fails with its listed Unimplemented features, or is a documented known-failure pointing at a bug entry

**Corpus triage protocol.** Run `cargo xtask corpus --check` on the milestone base. For every case that fails for a
reason other than a correctly-listed `Unimplemented`:
1. decide whether the **case** is wrong (syntax, a mis-transcribed program, an expected result that contradicts the
   cited source) or the **implementation** is wrong, re-reading the cited source;
2. fix a wrong case (programs, expectations, statuses) and record the reason in the manifest `notes` and in your
   notes file;
3. for an implementation bug, keep the case's status honest (`known-failure` with `issue = "BUGS.md#<n>"` and
   `until` = the milestone of the owning crate's next WP), and file the bug in your notes file under `## Bugs` with
   a minimal reproducer (the gate copies it to docs/plan/BUGS.md);
4. never weaken an expectation to make a case pass.

**Scope.** Every M1-authored case (core, lattices, lprov, async, net BENCH-100–102, verify, ldfi) on the `compile`,
`analysis` and `oracle` backends as they exist at the M6 base (core lowering, P0 analyses, the oracle). Cases that
need advanced lowering (M6.4, landing in parallel) stay `unimplemented` with the right feature ids.
Also check that every manifest's `until` values still follow PLAN §5.4 against `docs/design/plan.json`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p xtask -- corpus --lint
cargo run -q -p xtask -- corpus --check
cargo test -p blossom-testkit --test corpus
```

### M7 — Natives, provenance capture, the simulator, differential testing, the production driver and rewrites

**Goal.** The engine gains its P0 native operators and complete Tier B/C capture; the deterministic simulator runs real nodes over SimFs; every corpus program is differentially tested oracle ⇄ interpreter under plan perturbation; the production runtime runs nodes over real files and TCP; gated IR rewrites land.

**Gate.** `scripts/milestone-gate.sh M7` (PLAN §3).

#### M7.1 — blossom-engine II: P0 native operators, Tier B/C provenance capture, digests, watch taps and counters

- **Size:** ~7k
- **Depends on:** M6.1, M6.2
- **Owns:** `crates/blossom-engine/**`, `crates/blossom-kernel/**`, `tests/integration/tests/engine2_*.rs`, `docs/plan/notes/M7.1.md`
- **Features:** ENG-068, SEM-051, ENG-110, ENG-112, ENG-113, ENG-115, ENG-116, TEST-023, TEST-065, TEST-066
- **Consumes:**
  - engine I (M6.1)
  - plan I/II (natives specs, perturbation)
  - kernel II (ArgminIndex, FiringLog, DigestSink)
- **Provides:**
  - natives Choose, Index (sort form), FoldOrdered (per tick), Upsert, LogicalTimer, LatticeFold; Tier C capture under the Literal profile; Tier B annotation capture; choice/send/receive/contribution records; TickDigests; watch taps and firing counters; updated capabilities()

**Build** (ARCHITECTURE §3.6 P0 rows, §4.6, §4.9, §4.11, §12.2).
- **Choose** (ENG-068, SEM-085): `ArgminIndex` per group over candidates with positive support; priority
  `PRF_σc(site, fp(X̄), fp(Ȳ))`, cost-prefixed for least/most, keyed by `(σnode, incarnation, tick)` for
  `choose_rand!`; ties by canonical Ȳ; the Growing-candidates group-minimum optimization; sticky `held` (durable when
  declared); overrides applied first (a non-candidate override is a hard error); choice log entries.
- **Index** (sort form: tick-local inputs sorted by (keys, canonical) with `sort_prefix`; standing inputs keep the
  expansion until the order-statistic tree of M10.6), **FoldOrdered** (per-tick canonical fold), **Upsert**
  (keyed staging and candidate sets; conflicts found in the temporal phase → BLSR002 naming both statements;
  SEM-051), **LogicalTimer**, **LatticeFold** (cell maintained incrementally, read by `Lookup` Δ versions).
- **Provenance** (ENG-110–113, 116, TEST-023): Tier C `FiringLog` capture restricted to the plan's backward slice,
  with negative reads, send and receive records per (sender, send tick), aggregate contributions split into a
  bindings rule and an aggregate rule, lattice contributions, choice records; under the `Literal` profile every fact
  at tick t is explained by firings at t (complete lineage). Tier B: hidden `(rule, height)` columns with
  update-aware Δ (a height decrease re-enters Δ). Everything compiled out with `NullSink`.
- **Digests**: `TickDigests` (state, outbox, choices, changed relations) via `DigestSink`; equal to the oracle's.
- **Watch taps and firing counters** (TEST-065/066, ENG-115) through `EngineConfig::watch`: per-rule firing counts,
  never-fired rules, messages per protocol step.

**Required tests.** integration `engine2_natives_equal_expansions` (every fixture and corpus program under
perturbation with natives on/off), `engine2_choice_log_matches_oracle`, `engine2_upsert_conflict_blsr002`,
`engine2_tier_c_lineage_complete`, `engine2_tier_b_heights`, `engine2_digests_match_oracle`,
`engine2_nullsink_zero_cost` (no capture calls when disabled); unit `choose_sticky_`, `choose_rand_keyed_by_tick`,
`logical_timer_`, `lattice_fold_delta_`, `firing_counters_`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-engine blossom-kernel
cargo test -p blossom-integration-tests --test engine2_natives
scripts/require-tests.sh blossom-engine choose_sticky_ choose_rand_keyed_by_tick logical_timer_ lattice_fold_delta_ firing_counters_
scripts/require-tests.sh blossom-integration-tests engine2_natives_equal_expansions engine2_choice_log_matches_oracle engine2_upsert_conflict_blsr002 engine2_tier_c_lineage_complete engine2_tier_b_heights engine2_digests_match_oracle engine2_nullsink_zero_cost
```

#### M7.2 — blossom-sim I: worlds, schedulers, faults over SimFs, CALM pruning, traces, replay and shrinking

- **Size:** ~8.5k
- **Depends on:** M6.1, M5.5, M5.4, M4.7, M6.3
- **Owns:** `crates/blossom-sim/**`, `crates/blossom-node/**`, `crates/blossom-cli/src/cmd/sim.rs`, `crates/blossom-cli/src/cmd/trace.rs`, `crates/blossom-testkit/src/backend_sim.rs`, `scripts/ci.d/70-determinism.sh`, `tests/integration/tests/sim1_*.rs`, `docs/plan/notes/M7.2.md`
- **Features:** TEST-001, TEST-002, TEST-006, TEST-011, TEST-021, SEM-040, SEM-041, SEM-042, SEM-043, SEM-070, SEM-073, LANG-155, ENG-101
- **Consumes:**
  - blossom-node (Node, Evaluator)
  - blossom-engine (interpreter)
  - blossom-store (SimFs, FileDurability, MemDurability)
  - blossom-wire
  - blossom-trace
  - blossom-driver (integration tests only)
- **Provides:**
  - blossom_sim::{World, SimConfig, SimNode, NodeStatus, InFlight, Scheduler, ChoicePoint, schedulers::{Seeded, SyncRound, Exhaustive, Scripted}, WorldSnapshot, WorldDigest, RunReport, SimError (ProgramError verdict), shrink::ddmin, replay}
  - `blossom sim`, `blossom sim replay`, `blossom trace {convert, dump}`
  - determinism CI fragment

**Build** (ARCHITECTURE §6.1–§6.4, §6.6; ARCH-12).
- **World** (§6.1): every node of a `CompiledProgram` is a real `Node` with the real evaluator, the real wire codec,
  the pure admission function and **`FileDurability<SimFs>`** (the real WAL/checkpoint code); virtual time advanced
  only by the scheduler; the committer is a simulated actor whose `SyncComplete`/`SyncFail` are scheduler
  decisions (so the pipelined window of ARCH-10 is explored); `MemDurability` option for LDFI/BMC; `snapshot`/
  `restore`; `global()` view including crashed-frozen nodes; `WorldDigest` exactly as §6.2 lists.
- **Scheduling** (§6.2): enabled events; `SchedDecision` from blossom-trace; `SeededScheduler` (every decision
  from a named PRF stream keyed by purpose + stable identity, ARCH-12), `SyncRoundScheduler` (TEST-006, LDFI rounds),
  `ExhaustiveScheduler` (DFS over choice points with `WorldSnapshot` backtracking and a visited set of
  `WorldDigest`s; TEST-002), `ScriptedScheduler` (replay). Swarm testing: each seed first draws a fault profile.
- **CALM pruning** (TEST-003 consumer): only `branching` channels (bits in the plan's `IngestPlan`) are split across
  tick batches; all other arrived messages are delivered canonically in the receiver's next tick.
- **Faults** (§6.3; SEM-043, SEM-070–073, LANG-155, TEST-021): crash-stop (frozen, visible to specs), crash-recovery
  over SimFs's crash model (fates drawn from `("fs", node, file, offset)` streams) with the real recovery path,
  omissions per (sender, receiver, send tick) subject to the channel's fault model (`lossy`, `lossy_delayed`,
  `reliable`, `reliable_ordered`), directional partitions, delay and reorder, storage faults; self-sends are never
  dropped and arrive strictly later (SEM-041); the fault mask sits at the network layer (TEST-021); duplication only
  under `beyond_model`.
- **Traces, replay, shrinking** (§6.4; TEST-010 recording, TEST-011): record at `Minimal`/`Digests`/`Full`; replay
  checks program digest, PRF and encoding versions (hard errors) and compares every `TickEnd`, stopping at the first
  mismatch with node, tick, relations and choice sites; ddmin over injected faults and deviating decisions (dropping
  a crash drops its restart); `.blstrace` reproducers (mode 0600).
- **Program errors are verdicts** (§6.6): `SimError::ProgramError { node, tick, error, reproducer }` +
  `TraceEvent::ProgramError`, never a crash fault.
- **Parallel worlds** (ENG-101): exploration runs independent worlds on a rayon pool; results merged in a
  deterministic order.
- CLI `blossom sim` (run a program + deployment under a scheduler/fault profile), `sim replay`, `trace convert|dump`.
- `scripts/ci.d/70-determinism.sh`: runs one seed in two separate processes and compares every `TickEnd` digest.
- **Corpus `sim` backend** (`crates/blossom-testkit/src/backend_sim.rs`, a module M5.2 pre-declared): `[backend.sim]`
  runs `[run] seeds` seeds of the case on the engine with the `SeededScheduler` (async delivery, the case's
  `[[fault]]`s plus swarm faults only when `[run] swarm = true`); every `[[expect]]` must hold in every seed.
- You own `blossom-node` in this milestone for fixes the simulator needs (snapshots, pending syncs).

**Required tests.** `replay_is_digest_exact`, `replay_divergence_reported`, `shrink_minimal_`, `swarm_profiles_`,
`calm_pruning_regression_6_2` (the §6.2 program still finds the alarm), `crash_recovery_over_simfs`,
`invariant_r_under_explored_fsync`, `self_send_strictly_later`, `reliable_channel_never_drops`,
`program_error_is_verdict`, `world_digest_complete`, integration `sim1_sync_round_equals_oracle_harness` (same outputs
as the testkit sync-round harness on corpus programs), `sim_backend_all_seeds_must_hold`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-sim blossom-node
cargo test -p blossom-integration-tests --test sim1_equivalence
scripts/ci.d/70-determinism.sh
scripts/require-tests.sh blossom-sim replay_is_digest_exact replay_divergence_reported shrink_minimal_ swarm_profiles_ calm_pruning_regression_6_2 crash_recovery_over_simfs invariant_r_under_explored_fsync self_send_strictly_later reliable_channel_never_drops program_error_is_verdict world_digest_complete
```

#### M7.3 — blossom-testkit II: differential runner, OracleEvaluator, perturbation, conformance, zero-allocation harness

- **Size:** ~5k
- **Depends on:** M6.1, M6.2, M6.3, M5.2, M5.5, M6.6
- **Owns:** `crates/blossom-testkit/**`, `!crates/blossom-testkit/src/backend_sim.rs`, `scripts/ci.d/52-differential.sh`, `tests/integration/tests/diff_*.rs`, `docs/plan/notes/M7.3.md`
- **Features:** BENCH-049
- **Consumes:**
  - engine I
  - plan II (perturbation)
  - driver
  - node (Evaluator, ManualDriver, MemTransport)
  - oracle
  - analysis III (P1 outputs)
- **Provides:**
  - blossom_testkit::{oracle_eval::OracleEvaluator (node::Evaluator over the oracle), differential::{run_case, PerturbationSet}, backend_interp, backend_analysis (P1 expectations), conformance::{evaluator_suite, executor_suite, extern_suite}, alloc::CountingAllocator harness, replay_check}
  - differential CI fragment

**Build** (ARCHITECTURE §11.1–§11.3, §11.7).
- **OracleEvaluator**: implements `blossom_node::Evaluator` over the oracle (TickOutputRef views over the oracle's
  output with a `RefValueStore`), so the node, ManualDriver and later the simulator can run on the oracle.
- **Differential runner** (ENG-067): runs a program under the interpreter recording every tick input, feeds the
  recorded inputs to the oracle, and compares every relation's contents, the outbox, the durable delta, the choice
  log, violations and errors **exactly**, reporting the first divergence with a relation diff; under the reference
  plan and N perturbed plans (`N = 8` default, `BLOSSOM_PERTURB=64` nightly). Also the stratifier cross-check: the
  oracle's strata are a valid linearization of `blossom-analysis`'s.
- **Corpus backends**: `interp` (sync rounds through the harness's `RoundEvaluator` implemented over the engine via
  `ManualDriver` + `MemTransport` + `MemDurability`), and `analysis` for P1 expectations (certificates, CALM labels,
  finality classes, Blazes labels, reclaimable relations).
- **Replay check** (BENCH-049): cases with `replay_check = true` run twice (and through recorded inputs) and must
  produce identical per-tick digests.
- **Conformance** composition: `evaluator_suite` (engine vs oracle through the Evaluator trait), `executor_suite`
  (any `PlanExecutor` vs the interpreter, tick by tick; codegen joins in M8.5), `extern_suite` (purity by double
  evaluation, BLSR010 on mismatch).
- **Zero-allocation harness** (§11.7): a test binary with a counting global allocator runs a protocol program to
  steady state through ManualDriver + MemTransport + MemDurability (frames in and out, codec included, production
  tracing configuration) and asserts zero allocations across 1,000 further ticks.
- `scripts/ci.d/52-differential.sh`.

**Required tests.** `oracle_evaluator_conformance`, `differential_fixtures_all_perturbations`,
`differential_corpus_passing_cases`, `stratifier_linearization_check`, `replay_check_digests_equal`,
`extern_purity_double_eval`, `zero_alloc_steady_state_e01`, `interp_backend_sync_rounds`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-testkit
scripts/ci.d/52-differential.sh
cargo run -q -p xtask -- corpus --check
scripts/require-tests.sh blossom-testkit oracle_evaluator_conformance differential_fixtures_all_perturbations differential_corpus_passing_cases stratifier_linearization_check replay_check_digests_equal extern_purity_double_eval zero_alloc_steady_state_e01 interp_backend_sync_rounds
```

#### M7.4 — blossom-runtime I: the production driver, recovery with migrations, deployment config and the host API

- **Size:** ~8k
- **Depends on:** M6.1, M5.5, M5.4, M6.3
- **Owns:** `crates/blossom-runtime/**`, `crates/blossom-cli/src/cmd/run.rs`, `crates/blossom-cli/src/cmd/deploy.rs`, `crates/blossom-cli/src/cmd/node.rs`, `crates/blossom-cli/src/cmd/config.rs`, `tests/integration/tests/runtime1_*.rs`, `docs/plan/notes/M7.4.md`
- **Features:** DIST-004, DIST-041, DIST-043, DIST-082, LANG-185, BENCH-222
- **Consumes:**
  - blossom-node (Node, Transport, ManualDriver patterns)
  - blossom-store (open_node_store, FileDurability, MigrationRunner)
  - blossom-engine
  - blossom-driver (CLI `run` compiles sources)
- **Provides:**
  - blossom_runtime::{Runtime, RuntimeBuilder, NodeHandle (insert, insert_dyn, seal_input, subscribe, sync_do, async_do, status, pause, resume, stop), DeploymentSpec, Secrets, NodeConfig loading, Directory, SystemClock, OsEntropy, PlainTcpTransport, NodePolicy (breaker), migration::EngineMigrationRunner, shutdown}
  - `blossom run`, `deploy init`, `deploy local`, `node init`, `node status` (health via runtime API), `config explain`

**Build** (ARCHITECTURE §5.2, §5.5, §5.9, §5.10, §5.12, §12.4; ARCH-20/25/26).
- **Threads and loops** (§5.2): one engine thread per local node running the node loop exactly as the pseudo-code
  (drain ingress/host/commit rings, timers against `Clock::now`, `run_tick` while ready and within
  `max_inflight_ticks`/`max_inflight_bytes`, park with optional bounded spin); SPSC rings via `rtrb`; a tokio
  multi-thread runtime for I/O; one committer thread per data device obeying Invariant B (append a batch, sync each
  touched WAL once, report `synced(upto)` per node; any error poisons that WAL and reports `failed`); one checkpoint
  thread (triggers by WAL size/age, `SyncedTick`, `TruncateToken` handoff between batches).
- **Clock and entropy** (§5.5): `SystemClock` (wall anchor + monotonic, `now ≥ last_now + 1ns` across incarnations),
  `OsEntropy`; these two modules are the only ones exempt from the determinism lints.
- **Recovery** via `open_node_store` with `OpenMode::Existing` by default (refusal exit code 5), and the
  **migration runner** (DIST-082): each `migrate from N` step on a single-tick engine with the old relations as
  static inputs.
- **Deployment** (DIST-041/004, §5.9): `deploy.toml` (`format = 1`) → `DeploymentSpec` with `deny_unknown_fields`;
  dense NodeIds by (role, name) incl. standby and retired nodes; `directory_digest`; `Directory` (NodeId → addr,
  principal); secrets file (mode 0600 enforced) or `BLOSSOM_SEED`; `[capacity]`; config layering defaults → file →
  env → flags; `blossom config explain` prints each value's source.
- **Fault policy** (§5.12): probation handled by the node; circuit breaker `NodePolicy { on_tick_error: Restart {
  backoff, max_restarts } }`; quarantine files (0600, dedup by batch digest, capped, oldest deleted first);
  `record = "minimal"` input log option.
- **Transports**: `MemTransport` for in-process nodes and `PlainTcpTransport` (development only: refuses to start
  without `--insecure-dev`, sets `security_mode{mode="insecure"}`); mTLS is M8.7.
- **Host API** (LANG-185, §5.10): `Runtime::builder()…build()`, `runtime.start(node, role)` → `NodeHandle` with
  `insert`/`insert_dyn` (future ticks only), `seal_input`, `subscribe` (Full | Deltas, canonical order),
  `sync_do` (result returned after release), `async_do`, `status`, `pause`/`resume`/`stop`; services and output
  handlers are M9.8.
- **Graceful shutdown** (§5.2 steps 1–5; a second SIGTERM exits immediately); `panic = "abort"` in the binary;
  embedders' engine threads under `catch_unwind` mapping to `Faulted(Internal)`.
- **CLI**: `blossom run`, `deploy init`, `deploy local` (DIST-043: N nodes as threads or processes), `node init`,
  `node status`, `config explain`; exit codes of §12.5.

**Required tests.** integration `runtime1_kvs_put_get_plaintcp` (E1 on a 1-server deployment with host clients),
`runtime1_restart_recovers_state`, `runtime1_existing_mode_refusal_exit_5`, `runtime1_breaker_trips_exit_6`,
`runtime1_shutdown_releases_synced_ticks`, `runtime1_migration_on_recovery`, `runtime1_deploy_local_three_nodes`,
`bench_222_durable_table_gains_column` (BENCH-222: v1 writes, stop, v2 adds a column with a migration rule, restart
recovers and migrates, v2 reads the migrated rows);
unit `deployment_spec_validation_`, `node_ids_dense_by_role_name`, `secrets_mode_0600_required`,
`config_layering_explain`, `committer_invariant_b`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-runtime
cargo test -p blossom-integration-tests --test runtime1_cluster
scripts/require-tests.sh blossom-integration-tests runtime1_kvs_put_get_plaintcp runtime1_restart_recovers_state runtime1_existing_mode_refusal_exit_5 runtime1_breaker_trips_exit_6 runtime1_shutdown_releases_synced_ticks runtime1_migration_on_recovery runtime1_deploy_local_three_nodes bench_222_durable_table_gains_column
scripts/require-tests.sh blossom-runtime deployment_spec_validation_ node_ids_dense_by_role_name secrets_mode_0600_required config_layering_explain committer_invariant_b
```

#### M7.5 — blossom-rewrite: Edelweiss rewrites, resend suppression, coordination synthesis, decoupling, distributed provenance

- **Size:** ~6k
- **Depends on:** M6.6, M6.3, M6.1, M5.2
- **Owns:** `crates/blossom-rewrite/**`, `crates/blossom-driver/src/passes/**`, `std/consensus/protocol.bls`, `tests/integration/tests/rewrite_*.rs`, `docs/plan/notes/M7.5.md`
- **Features:** ANA-046, ANA-047, ANA-061, ANA-062, ANA-063, ANA-064, ANA-066, ANA-081, ANA-082, ANA-083, DIST-007, DIST-045
- **Consumes:**
  - blossom-analysis (all preconditions)
  - blossom-driver passes hook
  - engine + oracle (differential checks)
- **Provides:**
  - blossom_rewrite::{Rewrite trait, edelweiss::{arm, dr_plus, dr_minus, joinbuf}, resend::suppress, coord::{synthesize, marczak}, decouple, partition, interlock, prov_distributed}
  - driver passes wired (opt-in flags and `#[rewrite(...)]` attributes)
  - std/consensus/protocol.bls (the `Consensus` protocol the coordination fallback targets)

**Build** (ARCHITECTURE §3.5 resend, §7.2 rows; R05, R08 (SIGMOD'24 rewrites), R06 H45).
- Every rewrite: analysis precondition → IR→IR → re-validate → re-analyze; a rewrite whose precondition fails is not
  applied and the reason is reported (never silently).
- **Edelweiss** (ANA-061–064): ARM (acknowledgement-based reclamation), DR+ (positive difference reclamation),
  join-buffer reclamation, DR−; **GC safety** (ANA-066): leak rather than lose when in doubt, explain why a relation
  cannot be reclaimed, and differentially test every rewrite against the unrewritten program.
- **Resend suppression** (DIST-007): where ARM proves the receiver idempotent, add `c$sent` (persistent at the
  sender, cleared by `c$ack`) and a `notin c$sent(…)` guard; seal resends reuse it; the oracle and the engine then
  evaluate the same rewritten program.
- **Coordination synthesis** (ANA-046/047): seals preferred (unanimous producer votes with digests, skipped with a
  single producer); fallback ordering through the `Consensus` protocol in `std/consensus/protocol.bls` (you write
  the protocol interface; the Raft implementation arrives in M9.1; until then choosing the fallback reports
  BLS0908 naming `FLAG-001`, and the test for that uses a std source override without an implementation so it stays
  valid after M9.1); Marczak's coordination rewrite.
- **Decoupling, partitioning, interlock protection** (ANA-081–083) with their preconditions.
- **Distributed provenance** (DIST-045): opt-in `prov`/`ruleExec` relations keyed by hashed tuple and rule ids
  (ExSPAN), program-level so they work with engine provenance off.
- **Driver wiring**: `crates/blossom-driver/src/passes/**` runs requested rewrites in a fixed order.
- **Verification in this WP**: differential runs (oracle and interpreter sync rounds) of original vs rewritten
  programs on the corpus programs each rewrite applies to: same outputs at the same ticks (resends excepted where
  suppression applies). Full simulation-based VER-016 is M9.5.

**Required tests.** `arm_`, `dr_plus_`, `dr_minus_`, `joinbuf_`, `gc_safety_explains_unreclaimable`,
`resend_suppression_outputs_equal`, `seal_synthesis_`, `consensus_fallback_bls0908_without_impl`, `marczak_`,
`decouple_precondition_`, `partition_`, `interlock_`, `prov_distributed_relations`, integration
`rewrite_differential_corpus`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-rewrite blossom-driver
cargo test -p blossom-integration-tests --test rewrite_differential
scripts/require-tests.sh blossom-rewrite arm_ dr_plus_ dr_minus_ joinbuf_ gc_safety_explains_unreclaimable resend_suppression_outputs_equal seal_synthesis_ consensus_fallback_bls0908_without_impl marczak_ decouple_precondition_ partition_ interlock_ prov_distributed_relations
```

#### M7.6 — blossom-prov II: semirings, semimodules, lattice provenance, exact supports and Nemo algebra

- **Size:** ~4.5k
- **Depends on:** M5.7, M4.6
- **Owns:** `crates/blossom-prov/**`, `docs/plan/notes/M7.6.md`
- **Features:** ENG-114, ENG-145, ENG-146, ENG-147, TEST-052, TEST-145
- **Consumes:**
  - blossom-prov I
  - blossom-kernel::prov (ProvenanceSink)
  - blossom-lattice (P1 lattices for lattice provenance)
- **Provides:**
  - blossom_prov::{semiring::{Semiring, PosBool, Counting, Tropical, Why, SemiringSink}, semimodule, lattice_prov::{supports, exact_threshold_supports, element_supports, all_contributors}, nemo::{diff, repair_suggestions}, why_lattice, whynot_lattice}

**Build** (ARCHITECTURE §4.9; R06 Nemo, R09 provenance tiers, R11 PosBool(X)⊗𝓛).
- **Semiring annotations** (ENG-114): a `SemiringSink` implementing `ProvenanceSink` that propagates annotations per
  derivation (PosBool(X), counting, tropical, why-provenance) for `blossom why --semiring`; **semimodule
  annotations for aggregates** (ENG-147).
- **Lattice provenance** (ENG-145/146): the PosBool(X)⊗𝓛 domain and propagation rules computed offline from Tier C
  contributions: a cell's value at t = join of its logged contributions and its predecessor; exact threshold
  supports on distributive lattices (TEST-140 support), element-counting supports (TEST-141), the all-contributors
  fallback for non-distributive lattices and monotone non-morphisms (ODD-52 (c)), genuine supports only (TEST-142).
  These are the functions `blossom-ldfi` calls.
- **Nemo** (TEST-052): graph algebra over good vs bad lineages (difference, normalize, repair suggestions).
- **Lattice explain** (TEST-145): `why`/`whynot` for lattice cells (which contributions support a threshold).

**Required tests.** `semiring_laws_`, `semiring_sink_counts_derivations`, `semimodule_sum_`, `exact_threshold_supports_`,
`element_supports_majority`, `all_contributors_fallback`, `supports_are_genuine`, `nemo_diff_`, `why_lattice_threshold`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-prov
scripts/require-tests.sh blossom-prov semiring_laws_ semiring_sink_counts_derivations semimodule_sum_ exact_threshold_supports_ element_supports_majority all_contributors_fallback supports_are_genuine nemo_diff_ why_lattice_threshold
```

### M8 — LDFI, specs under simulation, the P0 standard library, code generation, P1 natives and mTLS

**Goal.** Molly-2 reaches verdict parity with Molly; specs, ultimate models and history checkers run in the simulator; the P0 standard library is written and tested; generated code equals the interpreter; the remaining native operators land; the runtime gains mTLS with identity binding and the ops surface.

**Gate.** `scripts/milestone-gate.sh M8` (PLAN §3).

#### M8.1 — blossom-ldfi I: Molly-2 — failure specs, hazard encodings, minimal enumeration, the driver and Molly parity

- **Size:** ~7.5k
- **Depends on:** M7.2, M7.1, M7.6, M2.4, M3.6, M6.3
- **Owns:** `crates/blossom-ldfi/**`, `crates/blossom-prov/**`, `crates/blossom-testkit/src/backend_ldfi.rs`, `crates/blossom-testkit/src/molly.rs`, `crates/blossom-testkit/tests/molly_parity.rs`, `crates/blossom-cli/src/cmd/ldfi.rs`, `crates/blossom-cli/src/cmd/why.rs`, `crates/blossom-cli/src/cmd/whynot.rs`, `tests/corpus/ldfi/**`, `scripts/ci.d/75-ldfi-parity.sh`, `docs/plan/notes/M8.1.md`
- **Features:** TEST-020, TEST-022, TEST-024, TEST-025, TEST-026, TEST-027, TEST-028, TEST-029, TEST-140, TEST-141, TEST-142
- **Consumes:**
  - blossom-sim (SyncRoundScheduler, worlds, snapshots)
  - engine Tier C capture + Literal profile
  - blossom-prov (graphs, supports)
  - blossom-sat
  - front::ded (MollyContinue programs)
  - driver
- **Provides:**
  - blossom_ldfi::{FailureSpec, CrashView, LdfiConfig, Ldfi, Verdict, LdfiReport, Counterexample}
  - corpus `ldfi` backend
  - Molly parity runner (`--test molly_parity`)
  - `blossom ldfi`, `blossom why`, `blossom whynot`

**Build** (ARCHITECTURE §8.1–§8.5, §8.7, ARCH-27; R06).
- **Pipeline** (§8.1): failure-free run under `SyncRoundScheduler`, `Literal` plan profile, Tier C sliced to pre/post,
  `MemDurability`, empty `FaultSchedule` → `ProvGraph` → hazard DAG per post goal → CNF (Plaisted–Greenbaum) +
  crash-order clauses + cardinality constraints → incremental SAT enumeration of **minimal** fault sets → hypothesis
  queue (dedup, canonical order) → runs (resumed from snapshots when `resume_from_snapshots`, parallel on rayon,
  merged in queue order) → oracle.
- **FailureSpec / CrashView** (TEST-020, ARCH-27): `Frozen` (default for `.bls`) and `MollyContinue` (the `.ded`
  profile, chosen from the frontend kind); omissions allowed iff 1 ≤ send tick < EFF, sender ≠ receiver and the
  channel's fault model allows loss; Molly round k = tick k.
- **Hazard encoding** (§8.3 table; TEST-024–027, TEST-140–142): goal = AND over firings; firing = OR over premises;
  `Clock` = O(from,to,t) ∨ K(from,t); `Alive` (Frozen) = K(n,t); `Leaf` = false (CR-22); conservative negative
  support with Molly's time filter (TEST-025, CR-31); contributors: OR (default), cardinality thresholds with
  element variables and a totalizer (TEST-141), exact threshold supports on distributive lattices (TEST-140 via
  blossom-prov), all-contributors fallback with genuine supports only (TEST-142); crash order variables
  K(n,t) → K(n,t+1) (TEST-027) with the per-view creation rules; the crash budget as a totalizer.
- **Minimal enumeration** (§8.4, TEST-028): one incremental solver per run, one activation literal per post goal,
  greedy shrink under assumptions, blocking clauses; conversion to `FaultSchedule` dropping omissions implied by
  crashes; empty models dropped.
- **Driver** (§8.5, TEST-029): priority queue by (fault count, canonical order); explored set; oracle exactly
  Molly's `isGood` (TEST-022); good runs contribute their lineage; bad runs are counterexamples; `FindMode::First`
  stops; program errors are verdicts (§6.6).
- **Reports**: `LdfiReport` JSON (verdict + bound certificate + per counterexample schedule, space-time diagram
  reference and violated-tuple lineage + run counts and timing).
- **Parity** (§8.7, BENCH-130–137): the `ldfi` corpus backend and `tests/molly_parity.rs` run every Molly case under
  **both** SAT backends (CaDiCaL and batsat) in find-all mode; verdicts identical to the goldens, run counts ≤
  published, Appendix-B-minimal falsifier sets equal where stated; the two backends must produce identical sets.
  Fix `.ded` corpus cases (you own `tests/corpus/ldfi/**`) only when a case is demonstrably mis-transcribed.
- **CLI**: `blossom ldfi <files> --eot --eff --nodes --crashes` (Molly's SyncFTChecker), `blossom why`/`whynot`
  (Literal-profile run + provenance query).

**Required tests.** `hazard_encoding_vs_bruteforce` (small random provenance graphs, exhaustive fault sets),
`minimal_models_are_appendix_b_minimal`, `crash_order_implication`, `frozen_alive_premises`,
`molly_continue_senders_only`, `negative_support_time_filter`, `threshold_supports_exact`, `cardinality_totalizer_`,
`both_backends_identical_sets`, `program_error_verdict`, `molly_parity` (the test binary).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-ldfi
cargo test -p blossom-testkit --test molly_parity --features sat-cadical,sat-batsat
cargo run -q -p xtask -- corpus --check --filter ldfi/ --require-pass ldfi
scripts/require-tests.sh blossom-ldfi hazard_encoding_vs_bruteforce minimal_models_are_appendix_b_minimal crash_order_implication frozen_alive_premises molly_continue_senders_only negative_support_time_filter threshold_supports_exact cardinality_totalizer_ both_backends_identical_sets program_error_verdict
```

#### M8.2 — blossom-sim II: the spec engine, ultimate models, confluence, history checkers, finality oracle, diagrams, node crashcheck

- **Size:** ~8k
- **Depends on:** M7.2, M6.5, M6.6, M7.3
- **Owns:** `crates/blossom-sim/**`, `crates/blossom-node/**`, `crates/blossom-testkit/src/backend_sim.rs`, `xtask/src/cmd/crashcheck.rs`, `tests/integration/tests/sim2_*.rs`, `docs/plan/notes/M8.2.md`
- **Features:** SEM-010, SEM-044, SEM-082, SEM-107, TEST-004, TEST-005, TEST-007, TEST-008, TEST-009, TEST-012, TEST-060, TEST-064, TEST-081, TEST-088, TEST-104
- **Consumes:**
  - sim I
  - spec IR + spec plans (front V, driver)
  - analysis finality facts
  - testkit II (OracleEvaluator)
- **Provides:**
  - blossom_sim::{spec::SpecEngine, ultimate::{ultimate_model, UltimateResult}, confluence::test, schedulers::{QuiescentStochastic, ChoicePermutation}, check::{linearizability, elle_cycles}, finality_oracle, stats::harness, diagram::{svg, mermaid}, viewer::html}
  - corpus `sim` backend (async multi-seed, sim-on-oracle ⇄ sim-on-engine)
  - node-level `xtask crashcheck --node`

**Build** (ARCHITECTURE §6.2, §6.5, §9.5 confluence, §11.7 crash consistency).
- **Spec engine** (TEST-081): an ordinary `Engine` over the spec's `ValidatedPlan` from the `SpecArtifact`; at every
  `TickEnd` feed interval updates of born/died tuples, `crash`, `sent$c` and `hb` (virtual, answered by per-(node,
  tick) vector clocks); spec invariants after each global step; `pre`/`post` at EOT or quiescence; O(Δ) per step.
- **Quiescence** (SEM-010) and **ultimate models** (SEM-044, SEM-107): quiescent state; otherwise a lasso (repeated
  world digest) → facts holding at every step around the loop, lattice limits through threshold facts; otherwise
  **inconclusive**, never "confluent".
- **Confluence tester** (TEST-005) comparing ultimate models across explored schedules, with a witness pair;
  **explorer with heartbeat transitions** (TEST-004, `HeartbeatMode::Explore`); **QuiescentStochasticScheduler**
  (TEST-007, BloomUnit); **ChoicePermutationScheduler** (TEST-012) over small choice groups via overrides, with the
  TEST-013 validity checker on every outcome; SEM-082 eventual-consistency property tests for monotone programs.
- **History checkers** (TEST-008): linearizability by WGL/Porcupine search over recorded invoke/ok pairs; Elle-style
  anomaly cycles. **Finality oracle** (TEST-088): every `final_present`/`final_absent` emission checked against the
  ultimate model and other replicas. **Statistical harness** (TEST-009) for progressive outputs with seeded block
  orders.
- **Security faults** (TEST-104): `reject`, `cert_expired`, `acl_misconfig` fault classes through the real admission
  function, logged as `REJECTED(reason)`.
- **Diagrams** (TEST-060): SVG space-time diagrams (lanes, ticks, messages, drops, crashes, syncs, rejections) and
  mermaid sequence export; **replay viewer** (TEST-064): a static HTML page embedding a trace.
- **Corpus `sim` backend** (extends M7.2's): add sim-on-oracle (via `OracleEvaluator`) ⇄ sim-on-engine digest
  equality for passing cases, spec checks (`[expect_verify] check = "sim"`), and ultimate-model expectations.
- **Node-level crashcheck**: extend `xtask crashcheck` with `--node`: crash at every durable syscall of scripted
  multi-node runs over SimFs, checking §11.7's properties (recovered state = uninterrupted run up to the last released
  tick or a later synced tick; no released message depends on a lost tick; refusal exactly for injected media
  faults), migration crash points included (TEST-103 groundwork).

**Required tests.** `spec_engine_invariant_violation_found`, `spec_interval_updates_o_delta`, `ultimate_quiescent`,
`ultimate_lasso`, `ultimate_inconclusive_not_confluent`, `confluence_witness_pair`, `heartbeat_exploration_`,
`quiescent_stochastic_`, `choice_permutations_valid`, `monotone_eventually_consistent` (SEM-082 property),
`linearizability_known_histories`, `elle_cycle_`, `finality_oracle_`, `security_fault_rejections`,
`diagram_svg_snapshot`, `viewer_html_snapshot`, integration `sim2_oracle_vs_engine_digests`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-sim
cargo run -q -p xtask -- crashcheck --node
cargo test -p blossom-integration-tests --test sim2_oracle_vs_engine
scripts/require-tests.sh blossom-sim spec_engine_invariant_violation_found spec_interval_updates_o_delta ultimate_quiescent ultimate_lasso ultimate_inconclusive_not_confluent confluence_witness_pair heartbeat_exploration_ quiescent_stochastic_ choice_permutations_valid monotone_eventually_consistent linearizability_known_histories elle_cycle_ finality_oracle_ security_fault_rejections diagram_svg_snapshot viewer_html_snapshot
```

#### M8.3 — Standard library I: delivery, multicast, membership, failure detection, timers, voting, 2PC, quorums, coordination

- **Size:** ~5k (Blossom + tests)
- **Depends on:** M6.3, M7.2, M7.3, M6.4
- **Owns:** `std/delivery/**`, `std/bcast/**`, `std/membership/**`, `std/fd/**`, `std/timers/**`, `std/vote/**`, `std/commit/**`, `std/quorum/**`, `std/coord/**`, `crates/blossom-std-host/src/delivery.rs`, `crates/blossom-std-host/src/bcast.rs`, `crates/blossom-std-host/src/membership.rs`, `crates/blossom-std-host/src/fd.rs`, `crates/blossom-std-host/src/timers.rs`, `crates/blossom-std-host/src/vote.rs`, `crates/blossom-std-host/src/commit.rs`, `crates/blossom-std-host/src/quorum.rs`, `crates/blossom-std-host/src/coord.rs`, `tests/corpus/std/delivery/**`, `tests/corpus/std/bcast/**`, `tests/corpus/std/membership/**`, `tests/corpus/std/fd/**`, `tests/corpus/std/timers/**`, `tests/corpus/std/vote/**`, `tests/corpus/std/commit/**`, `tests/corpus/std/quorum/**`, `tests/corpus/std/coord/**`, `tests/corpus/net/BENCH-104*/**`, `tests/corpus/net/BENCH-105*/**`, `tests/corpus/net/BENCH-106*/**`, `tests/corpus/net/BENCH-107*/**`, `docs/plan/notes/M8.3.md`
- **Features:** LIB-001, LIB-002, LIB-003, LIB-007, LIB-020, LIB-021, LIB-022, LIB-040, LIB-041, LIB-044, LIB-045, DIST-046, BENCH-104, BENCH-105, BENCH-106, BENCH-107
- **Consumes:**
  - the full compiler (driver)
  - oracle, interpreter and simulator backends of the corpus runner
- **Provides:**
  - std::delivery (interface, best-effort, reliable), std::bcast (multicast), std::membership (static), std::fd (heartbeats + failure detector relations), std::timers, std::vote, std::commit (2PC), std::quorum, std::coord (roll call, barrier, choice, sequence, timeout)

**Rules for standard-library WPs.**
- Modules live in `std/<area>/…` (module path `std::<area>::…`), written to LANGUAGE.md; each builds under
  `blossom check --strict` (ODD-10 (c)); interfaces are `protocol`s where several implementations exist (LANG-006).
- Host functions (`extern fn`) go in `crates/blossom-std-host/src/<area>.rs` (`register` adds them to the registry).
- Tests are corpus cases: `tests/corpus/std/<area>/<LIB-id>[<letter>]-<slug>/` (`id` = the LIB id, `features` = the LIB
  and LANG ids exercised) with
  `oracle`, `interp` and `sim` (async, ≥ 16 seeds) backends passing, plus spec files with invariants checked in
  simulation; the bud-sandbox/Overlog BENCH items listed below are authored here in `tests/corpus/net/`.
- Every module has a doc comment with its interface, its guarantees and its fault model; `docs/std/<area>.md` is
  generated later by M12.5 from these comments.

**Build** (FEATURES §9.1–§9.3 P0 items; R03 bud-sandbox; R07).
- `std::delivery`: the delivery interface (LIB-001), best-effort (LIB-002) and reliable delivery with acks and
  retransmission on a timer (LIB-003).
- `std::bcast::multicast` (LIB-007).
- `std::membership::static` (LIB-020); `std::fd` heartbeats and a failure detector publishing suspect/alive relations
  (LIB-021, DIST-046); `std::timers` idioms (LIB-022).
- `std::vote` (LIB-040), `std::commit::two_phase` (LIB-041), `std::quorum` collection (LIB-044), `std::coord` idioms
  (LIB-045).
- Author and pass BENCH-104 (reliable delivery), BENCH-105 (multicast), BENCH-106 (voting), BENCH-107 (2PC) from the
  bud-sandbox tests (R03), on `oracle`, `interp` and `sim`.

**Acceptance** (every command must pass from the repository root):

```sh
sh -c 'for d in delivery bcast membership fd timers vote commit quorum coord; do cargo run -q -p blossom-cli -- check --strict std/$d || exit 1; done'
cargo run -q -p xtask -- corpus --check --filter 'std/(delivery|bcast|membership|fd|timers|vote|commit|quorum|coord)/' --require-pass oracle,interp,sim
cargo run -q -p xtask -- corpus --check --filter 'net/BENCH-10[4-7]' --require-pass oracle,interp,sim
scripts/wp-check.sh blossom-std-host
```

#### M8.4 — Standard library II: ids, queues, sequences, clocks, sealed replies, KVS, lattice KVS, CRDTs

- **Size:** ~5k (Blossom + tests)
- **Depends on:** M6.3, M7.2, M7.3, M6.4, M4.6
- **Owns:** `std/ids/**`, `std/queue/**`, `std/seq/**`, `std/clock/**`, `std/seal/**`, `std/kvs/**`, `std/crdt/**`, `crates/blossom-std-host/src/ids.rs`, `crates/blossom-std-host/src/queue.rs`, `crates/blossom-std-host/src/seq.rs`, `crates/blossom-std-host/src/clock.rs`, `crates/blossom-std-host/src/seal.rs`, `crates/blossom-std-host/src/kvs.rs`, `crates/blossom-std-host/src/crdt.rs`, `tests/corpus/std/ids/**`, `tests/corpus/std/queue/**`, `tests/corpus/std/seq/**`, `tests/corpus/std/clock/**`, `tests/corpus/std/seal/**`, `tests/corpus/std/kvs/**`, `tests/corpus/std/crdt/**`, `tests/corpus/net/BENCH-108*/**`, `tests/corpus/net/BENCH-109*/**`, `tests/corpus/net/BENCH-110*/**`, `tests/corpus/net/BENCH-111*/**`, `docs/plan/notes/M8.4.md`
- **Features:** LIB-060, LIB-061, LIB-062, LIB-063, LIB-064, LIB-065, LIB-066, LIB-080, LIB-081, LIB-086, BENCH-108, BENCH-109, BENCH-110, BENCH-111
- **Consumes:**
  - the full compiler (driver)
  - P1 lattices (M4.6) for CRDTs
  - corpus backends
- **Provides:**
  - std::ids, std::queue (serializer/atomic dequeue, priority, FIFO), std::seq (counters, sequences), deterministic id assignment, std::clock (Lamport, vector), std::seal (multi-message replies), std::kvs (KVS, lattice KVS), std::crdt

**Rules for standard-library WPs.**
- Modules live in `std/<area>/…` (module path `std::<area>::…`), written to LANGUAGE.md; each builds under
  `blossom check --strict` (ODD-10 (c)); interfaces are `protocol`s where several implementations exist (LANG-006).
- Host functions (`extern fn`) go in `crates/blossom-std-host/src/<area>.rs` (`register` adds them to the registry).
- Tests are corpus cases: `tests/corpus/std/<area>/<LIB-id>[<letter>]-<slug>/` (`id` = the LIB id, `features` = the LIB
  and LANG ids exercised) with
  `oracle`, `interp` and `sim` (async, ≥ 16 seeds) backends passing, plus spec files with invariants checked in
  simulation; the bud-sandbox/Overlog BENCH items listed below are authored here in `tests/corpus/net/`.
- Every module has a doc comment with its interface, its guarantees and its fault model; `docs/std/<area>.md` is
  generated later by M12.5 from these comments.

**Build** (FEATURES §9.4–§9.5 P0 items).
- `std::ids` unique ids and nonces (LIB-060); `std::queue` serializer / atomic dequeue (LIB-061, the CR-02 idiom),
  priority and FIFO queues (LIB-062); `std::seq` counters and sequences (LIB-063); deterministic id assignment
  (LIB-064); `std::clock` Lamport and vector clocks (LIB-065); `std::seal` sealing multi-message replies (LIB-066).
- `std::kvs` KVS (LIB-080) and lattice KVS (LIB-081); `std::crdt` library (LIB-086: G/PN counters, OR-set, 2P-set,
  LWW register, MV-register, maps) built on the lattice library.
- Author and pass BENCH-108 (KVS workloads), BENCH-109 (carts), BENCH-110 (serializer and assigners), BENCH-111
  (Lamport clock) on `oracle`, `interp` and `sim`.

**Acceptance** (every command must pass from the repository root):

```sh
sh -c 'for d in ids queue seq clock seal kvs crdt; do cargo run -q -p blossom-cli -- check --strict std/$d || exit 1; done'
cargo run -q -p xtask -- corpus --check --filter 'std/(ids|queue|seq|clock|seal|kvs|crdt)/' --require-pass oracle,interp,sim
cargo run -q -p xtask -- corpus --check --filter 'net/BENCH-1(08|09|10|11)' --require-pass oracle,interp,sim
```

#### M8.5 — blossom-codegen and blossom-build: generated executors, typed host bindings, specialized codecs, codegen corpus

- **Size:** ~7.5k
- **Depends on:** M6.2, M7.1, M6.3, M4.4, M7.3
- **Owns:** `crates/blossom-codegen/**`, `crates/blossom-build/**`, `tests/codegen-corpus/**`, `xtask/src/cmd/gen_codegen_corpus.rs`, `xtask/src/cmd/check_codegen_abi.rs`, `scripts/ci.d/60-codegen.sh`, `docs/plan/notes/M8.5.md`
- **Features:** ENG-005
- **Consumes:**
  - blossom-ir plan model
  - blossom-engine::abi (M6.1/M7.1)
  - blossom-wire::abi
  - blossom-schema
  - blossom-driver
  - testkit executor_suite
- **Provides:**
  - blossom_codegen::{generate(&CompileOutput, &CodegenOptions) -> GeneratedModule, CodegenOptions, CodegenError}
  - blossom_build::{Builder (program, module_path, provenance_variants, digest_variants, strict, emit_to_out_dir), BuildError}
  - tests/codegen-corpus (interp ⇄ codegen ⇄ oracle per tick)
  - `xtask gen-codegen-corpus`, `xtask check-codegen-abi`

**Build** (ARCHITECTURE §10; ARCH-04/15; ENG-005).
- `generate` is a pure function: `pub fn program() -> CompiledProgram` from the embedded `CompileOutput` artifact
  (postcard with `ArtifactHeader`), digests, one `ExecutorFactory` per role; per role
  `struct <Role>Exec<P: Prov, G: DigestSink>: PlanExecutor` naming **only** `blossom_engine::abi` (fields are ABI
  handles resolved once in `make`); `#[inline(never)] fn stratum_<n>` per large stratum, consecutive tiny
  non-recursive strata merged per wave; one function per rule version: nested loops over the same kernel cursors the
  interpreter drives with static row/key/lattice shapes and `Deaths`; `probe_batch` with prefetching when the driving
  source may exceed ~8 rows, scalar `probe_one`/`lookup` for singleton sources; fused buffers and `Tee` fan-out as
  straight-line code; `run_stratum` as a `match`.
- Typed host bindings (`InputRow`/`OutputRow` from `engine::abi`) for every input/output relation; specialized
  per-channel wire encoders/decoders over `blossom_wire::abi`; `const _: () = assert!(abi::VERSION == … &&
  wire::abi::VERSION == …)` and a runtime plan-digest check (`EngineError::StalePlan`).
- `CodegenOptions { provenance_variants, digest_variants, merge_tiny_strata, interpret_strata_over (P1 hybrid:
  oversized cold strata stay interpreted inside the generated executor) }`.
- **blossom-build**: `Builder` runs `CompileSession` exactly as the CLI does, then `generate`, then `prettyplease`;
  diagnostics as `cargo:warning=` lines; `BuildError` fails the build; regeneration cache keyed by (program digest,
  plan digest, ABI versions, codegen version) so unchanged inputs skip writing; `rerun-if-changed` for every source.
- **`xtask check-codegen-abi`**: parses every generated file with `syn` and rejects any path outside
  `blossom_engine::abi`, `blossom_wire::abi`, `core`, `std::result`, `std::option`.
- **Codegen corpus** (§10.4): `xtask gen-codegen-corpus` writes `tests/codegen-corpus/` whose `build.rs` compiles every
  corpus case that lists `[backend.codegen]` through `blossom-build` (a case that fails to compile or generate is
  recorded as data for the ratchet, never a build failure of the crate); its test binary runs each program under the interpreter and the generated
  executor with identical inputs, seeds and schedules, comparing state, outbox and choice digests **every tick** (and
  the oracle), diffing relations on mismatch; it also implements the corpus `codegen` backend status ratchet for
  those cases. Wire specializations are checked byte-for-byte against the generic codec on random tuples.
- Measure release build time per corpus program; record it in the notes (the 30 s budget of §10.3 is checked
  against flagship systems later).

**Required tests.** `generate_is_pure` (same input → identical tokens), `generated_names_only_abi`,
`stale_plan_rejected`, `codec_specialization_bytes_equal`, `executor_suite_codegen`, `builder_cache_skips_rewrite`,
codegen-corpus test binary green.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-codegen blossom-build
cargo run -q -p xtask -- gen-codegen-corpus --check
cargo test -p codegen-corpus
cargo run -q -p xtask -- check-codegen-abi
scripts/require-tests.sh blossom-codegen generate_is_pure generated_names_only_abi stale_plan_rejected codec_specialization_bytes_equal executor_suite_codegen
scripts/require-tests.sh blossom-build builder_cache_skips_rewrite
```

#### M8.6 — blossom-engine III: the P1 native operators, finality maintenance, wrappers, as-of reads and engine P1 features

- **Size:** ~8k
- **Depends on:** M7.1, M6.4, M6.2
- **Owns:** `crates/blossom-engine/**`, `crates/blossom-kernel/**`, `crates/blossom-plan/**`, `tests/integration/tests/engine3_*.rs`, `docs/plan/notes/M8.6.md`
- **Features:** ENG-046, ENG-048, ENG-070, ENG-071, ENG-073, ENG-075, ENG-141, SEM-036, SEM-052, DIST-015, DIST-016, DIST-023
- **Consumes:**
  - engine II
  - advanced lowering (construct specs)
  - plan natives registry
- **Provides:**
  - natives MultiChoose, Seq, ArgExt, AggDefault, Resolve, SoftTable, Sealed, Range, Seal, Wrapped (W2/W3 + durable outbuf + BLSR011), Snapshot, Finality, prefix-checkpointed FoldOrdered; plan-side selection for each; stop-at-⊤; nullary early exit; base-point derivatives; Upsert both-deltas mode; as-of reads

**Build** (ARCHITECTURE §3.6 P1 rows, §4.5, §4.6; R12, R13).
- For each P1 construct: plan-side native selection in `crates/blossom-plan/src/natives/<kind>.rs` (only when the
  executor's capabilities include it) and the native in the engine, observationally identical to the expansion:
  `MultiChoose` (ENG-075: union-find over conflict components, greedy recompute of touched components), `Seq`
  (assigned map + high-water mark, durable when declared), `ArgExt`, `AggDefault`, `Resolve` (per-key candidate
  sets; conflicts in the temporal phase), `SoftTable` (birth `LMax<Instant>` cells, deadline heap, (birth,
  canonical) eviction, time-varying stratum), `Sealed`, `Range` (disjoint buckets), `Seal` (per-(key, producer)
  counts and votes; violations; resend via the DIST-007 rewrite), `Wrapped` (SEM-036, ENG-070, DIST-015/016: dots
  `(origin, incarnation, seq)`, causal context (contiguous max + exception intervals) per origin, durable `outbuf`,
  per-dot payload fingerprints in the unacked window, `TickError::DotConflict` BLSR011 on reuse with a different
  payload, `unwrap` translation to Z-set Δ), `Snapshot` (progress-threshold crossing log), `Finality` (ENG-071: M⁻
  maintained incrementally, M⁺ demand-driven over requested tuples), prefix-checkpointed `FoldOrdered` (ENG-073).
- **Stop at ⊤** (ENG-046), **early exit for nullary heads** (ENG-048: `Op::EarlyExit` planned and executed),
  **base-point derivatives** for monotone non-morphisms with a declared derivative (ENG-141).
- **Upsert "emit both deltas" mode** (SEM-052, the Overlog key-overwrite semantics) selectable per construct, tested
  through an IR fixture built with `IrBuilder` (its surface frontend is P2).
- **As-of reads** (DIST-023): `Engine::view(rel, Some(tick))` through lazily built history indexes over birth/death
  stamps (kernel); only while an as-of reader exists.

**Required tests.** integration `engine3_native_equals_expansion_<kind>` for every native above under perturbation
(natives on/off) on corpus programs and fixtures, `engine3_dot_reuse_blsr011` (BENCH-078), `engine3_finality_status_matches_oracle`,
`engine3_soft_eviction_order`, `engine3_seal_votes_`, `engine3_as_of_reads`, `engine3_upsert_both_deltas_fixture`;
unit `stop_at_top_`, `nullary_early_exit`, `base_point_derivative_equals_recompute`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-engine blossom-kernel blossom-plan
cargo test -p blossom-integration-tests --test engine3_natives
scripts/require-tests.sh blossom-engine stop_at_top_ nullary_early_exit base_point_derivative_equals_recompute
scripts/require-tests.sh blossom-integration-tests engine3_native_equals_expansion_multichoose engine3_native_equals_expansion_seq engine3_native_equals_expansion_resolve engine3_native_equals_expansion_soft engine3_native_equals_expansion_seal engine3_native_equals_expansion_wrapped engine3_native_equals_expansion_finality engine3_dot_reuse_blsr011 engine3_as_of_reads engine3_upsert_both_deltas_fixture
```

#### M8.7 — blossom-runtime II: TCP/mTLS transport, SPIFFE identity binding, audit, metrics and health

- **Size:** ~6k
- **Depends on:** M7.4, M5.5
- **Owns:** `crates/blossom-runtime/**`, `crates/blossom-cli/src/cmd/node.rs`, `tests/integration/tests/tls_*.rs`, `tests/fixtures/pki/**`, `docs/plan/notes/M8.7.md`
- **Features:** DIST-044, DIST-060, DIST-061, DIST-063, TEST-107, BENCH-227, BENCH-228, BENCH-229
- **Consumes:**
  - runtime I
  - node (Transport, admit, ConnInfo)
  - wire (HELLO)
- **Provides:**
  - blossom_runtime::{transport::TcpTlsTransport, tls::{PeerVerifier, ClientVerifier, SpiffeId}, audit::AuditSink, ops::{metrics, healthz, readyz}, fault_proxy (tests)}
  - `blossom node status`

**Build** (ARCHITECTURE §5.3, §5.8, §12.2–§12.3; ARCH-11; ODD-30/31).
- **TcpTlsTransport** (DIST-060): lazily connected outbound connection per peer with jittered exponential backoff
  (jitter from `Entropy`); u32 length-prefixed frames inside TLS 1.3; `writev` batching; bounded per-peer queues
  with drop counting (`net_dropped_total{reason="queue"}`); connection loss drops that peer's queued frames
  (counted); rustls with the `ring` provider and `default-features = false` (never aws-lc).
- **Identity binding in both directions** (DIST-061): the principal is the single SPIFFE URI SAN (x509-parser);
  accepting: `HELLO` claiming node N accepted only if the connection principal = `principal_of(N)`; dialing: a custom
  `ServerCertVerifier` (rustls-webpki chain to the peer CA, then URI SAN = `principal_of(N)`); client verifier
  requires a URI SAN; CN allow-list fallback only when configured; duplicate instances (`stale_incarnation`,
  `duplicate_node`); two listeners with separate trust roots (peer vs client); private keys readable by group/world
  are refused.
- **Audit** (DIST-063): a dedicated sink (file or separate tracing layer with its own filter) for rejections,
  poison deny-list changes, duplicate-node detections and certificate events, rate-limited per reason; metrics
  `net_rejected_total{reason, channel, peer_role}` etc.
- **Ops** (DIST-044): the `metrics` facade + Prometheus exporter; `/healthz`, `/readyz`; loopback by default, mTLS
  when exposed; cardinality caps (200 series per metric per node, overflow to `other`); `blossom node status`.
- **Tests** (TEST-107): a throwaway `rcgen` PKI (`tests/fixtures/pki` holds only generator inputs); handshake
  failures, SAN binding both directions, expiry, listener separation, duplicate-instance detection, the in-process
  fault proxy (delay, reset, half-open, slow reader) exercising reconnect/backoff/queue drops; BENCH-227 (an
  unauthenticated peer's send is rejected), BENCH-228 (an authenticated but unauthorized sender is rejected),
  BENCH-229 (identity spoofing is detected) as integration tests named `bench_227_*`, `bench_228_*`, `bench_229_*`.

**Required tests.** integration `tls_handshake_`, `tls_san_binding_accepting`, `tls_san_binding_dialing`,
`tls_expiry_`, `tls_listener_separation`, `tls_duplicate_instance`, `fault_proxy_reconnect_backoff`,
`bench_227_unauthenticated_peer_rejected`, `bench_228_unauthorized_sender_rejected`, `bench_229_spoofing_detected`;
unit `metrics_cardinality_cap`, `audit_rate_limited`, `key_permissions_refused`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-runtime
cargo test -p blossom-integration-tests --test tls_security
scripts/require-tests.sh blossom-integration-tests tls_handshake_ tls_san_binding_accepting tls_san_binding_dialing tls_expiry_ tls_listener_separation tls_duplicate_instance fault_proxy_reconnect_backoff bench_227_unauthenticated_peer_rejected bench_228_unauthorized_sender_rejected bench_229_spoofing_detected
scripts/require-tests.sh blossom-runtime metrics_cardinality_cap audit_rate_limited key_permissions_refused
```

#### M8.8 — Corpus triage II: the golden corpus on the interpreter and the simulator

- **Size:** ~1–3k lines of case fixes
- **Depends on:** M7.3, M7.2, M6.4, M6.7
- **Owns:** `tests/corpus/core/**`, `tests/corpus/lattices/**`, `tests/corpus/lprov/**`, `tests/corpus/async/**`, `tests/corpus/net/BENCH-100*/**`, `tests/corpus/net/BENCH-101*/**`, `tests/corpus/net/BENCH-102*/**`, `tests/corpus/verify/**`, `tests/corpus/examples/**`, `docs/plan/notes/M8.8.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - interp backend (M7.3)
  - sim I
  - advanced lowering (M6.4)
- **Provides:**
  - the M1 corpus triaged on interp and (where the backend exists) sim; statuses honest

**Corpus triage protocol.** Run `cargo xtask corpus --check` on the milestone base. For every case that fails for a
reason other than a correctly-listed `Unimplemented`:
1. decide whether the **case** is wrong (syntax, a mis-transcribed program, an expected result that contradicts the
   cited source) or the **implementation** is wrong, re-reading the cited source;
2. fix a wrong case (programs, expectations, statuses) and record the reason in the manifest `notes` and in your
   notes file;
3. for an implementation bug, keep the case's status honest (`known-failure` with `issue = "BUGS.md#<n>"` and
   `until` = the milestone of the owning crate's next WP), and file the bug in your notes file under `## Bugs` with
   a minimal reproducer (the gate copies it to docs/plan/BUGS.md);
4. never weaken an expectation to make a case pass.

**Scope.** Every M1-authored case except `ldfi/**` (owned by M8.1) on the `oracle`, `interp`, `sim` and `analysis`
backends at the M8 base (advanced lowering, engine natives, the differential runner, the simulator). Divergences
between oracle and interpreter are engine or planner bugs: file them with the differential report. Also author
`tests/corpus/examples/E01…E10/` end-to-end cases for the ten example programs (scripted inputs and the behavior
their doc comments state), passing on `oracle`, `interp` and `sim`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p xtask -- corpus --lint
cargo run -q -p xtask -- corpus --check
cargo run -q -p xtask -- corpus --check --filter 'examples/' --require-pass oracle,interp,sim
```

### M9 — Consensus and replicated data: Raft core, Multi-Paxos, Anna KVS, LDFI P1, BMC, P1 library, host embedding

**Goal.** The first flagship systems run under simulation, LDFI and codegen; LDFI gains its P1 optimizations; bounded model checking and law proofs land; the P1 protocol and data-structure libraries are written; the host embedding API is complete.

**Gate.** `scripts/milestone-gate.sh M9` (PLAN §3).

#### M9.1 — Raft I: elections, log replication, commit and apply, durability (std::consensus::raft + systems/raft)

- **Size:** ~6k (Blossom + Rust tests)
- **Depends on:** M8.5, M8.2, M8.1, M7.5, M8.3
- **Owns:** `std/consensus/**`, `crates/blossom-std-host/src/consensus.rs`, `systems/raft/**`, `tests/corpus/std/consensus/**`, `tests/integration/tests/raft_*.rs`, `docs/plan/notes/M9.1.md`
- **Features:** FLAG-001, FLAG-002, FLAG-003, FLAG-004, FLAG-005, FLAG-006, FLAG-007, FLAG-008, BENCH-170, BENCH-171, BENCH-173
- **Consumes:**
  - the full toolchain (build, codegen, sim, ldfi, specs)
  - std::consensus::Consensus protocol (M7.5)
  - std::timers, std::fd
- **Provides:**
  - std::consensus::raft implementing `Consensus`
  - systems/raft (blossom-sys-raft) with generated executors, sim/LDFI suites and deployments

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.1 P0 items; R07 (Raft in declarative languages); examples/e03_raft_election.bls as the
starting point).
- One component holds all Raft state (FLAG-006); roles and terms (FLAG-001); randomized election timeouts from the
  PRF (FLAG-002); the election restriction (FLAG-003); log matching with AppendEntries consistency checks and
  conflict truncation (FLAG-004); commit (majority match index of the current term) and in-order apply (FLAG-005);
  durable current term, vote and log (`durable` relations; SEM-072 before any reply; FLAG-007); a no-op entry when
  a leader starts (FLAG-008).
- `std::consensus::raft` implements the `Consensus` protocol of `std/consensus/protocol.bls`, which makes the
  coordination-synthesis fallback of M7.5 usable; add `tests/integration/tests/raft_coord_fallback.rs` showing a
  program whose seal-based synthesis is impossible now coordinates through `std::consensus::raft`.
- **Tests**: BENCH-170 (Raft core suite), BENCH-171 (the Raft paper scenarios: split votes, leader crash during
  replication, stale leader, log divergence and repair, …), BENCH-173 (Raft restart: recovery from the WAL keeps
  term/vote/log), as simulation tests over SimFs with crash-recovery and partitions under many seeds, both executors;
  spec invariants (election safety, log matching, leader completeness, state machine safety) checked in simulation
  and by LDFI (`check ldfi … expect holds` with crash budget 1 on 3 nodes and omissions); `check bmc` on a 3-node
  bounded configuration where tractable.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/raft/bls
cargo test -p blossom-sys-raft
cargo run -q -p xtask -- corpus --check --filter 'std/consensus/' --require-pass oracle,interp,sim
cargo run -q -p xtask -- check-codegen-abi
```

#### M9.2 — Multi-Paxos: acceptors, leader recovery, phase 2, stable leadership, GC, P1 extensions and CompPaxos

- **Size:** ~6.5k
- **Depends on:** M8.5, M8.2, M8.1, M7.5
- **Owns:** `systems/paxos/**`, `docs/plan/notes/M9.2.md`
- **Features:** FLAG-020, FLAG-021, FLAG-022, FLAG-023, FLAG-024, FLAG-025, FLAG-026, FLAG-027, FLAG-028, BENCH-176, BENCH-177, BENCH-184
- **Consumes:**
  - the full toolchain
  - rewrites (decoupling/partitioning for CompPaxos)
- **Provides:**
  - systems/paxos (blossom-sys-paxos): Multi-Paxos, Kirsch–Amir election, flexible/grid quorums, CompPaxos/ScalablePaxos

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.2; R07 (Overlog Paxos, BOOM), R08 (Hydro Paxos, SIGMOD'24 CompPaxos)).
- Acceptor (FLAG-020), leader recovery with `index!` slot assignment (FLAG-021), phase 2 and preemption (FLAG-022),
  stable leadership (FLAG-023), replicas and GC below the checkpoint (FLAG-024).
- P1: catch-up, reconfiguration with `s + WINDOW`, leases, epochs and recovery (FLAG-025); the Kirsch–Amir election
  module (FLAG-026); flexible/grid quorums, thriftiness, batching and flow control (FLAG-027).
- **CompPaxos/ScalablePaxos** (FLAG-028, P2 pulled forward because BENCH-177, P1, requires it; PLAN §6): built with the
  decoupling and partitioning rewrites of M7.5 where their preconditions hold, by hand otherwise.
- **Tests**: BENCH-176 (Multi-Paxos suite), BENCH-177 (CompPaxos/ScalablePaxos and Hydro Paxos + kv_replica GC below
  the checkpoint), BENCH-184 (monitoring: a trace-rewriting of Paxos through an IR rewrite pass — not surface
  metaprogramming, which is P2 —, the never-fired-rules coverage report from firing counters, and messages per decree
  matching the spec); spec invariants (agreement, validity) in simulation, LDFI and BMC (small bounds).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/paxos/bls
cargo test -p blossom-sys-paxos
```

#### M9.3 — Anna-style KVS: coordination-free per-core actors, consistency levels, deletes with reclamation

- **Size:** ~4.5k
- **Depends on:** M8.5, M8.2, M8.4
- **Owns:** `systems/kvs/**`, `docs/plan/notes/M9.3.md`
- **Features:** FLAG-060, FLAG-061, FLAG-062
- **Consumes:**
  - the full toolchain
  - std::kvs::lattice, std::crdt (M8.4)
  - P1 lattices
- **Provides:**
  - systems/kvs (blossom-sys-kvs)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.4; R04 (Anna, Bloom^L, lattice composition)).
- Coordination-free per-core actors exchanging lattice deltas by gossip (FLAG-060); consistency levels by lattice
  composition (LWW, set-union, causal, read-committed…; FLAG-061); deletes with reclamation by the minimum heard
  vector clock (FLAG-062).
- **Tests**: convergence under random schedules, loss and duplication (`beyond_model`) for every consistency level;
  CALM certificates on the replication path (confluent); reclamation never loses a live key (property test); the
  lattice laws of every composed lattice; interp ⇄ codegen digests.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/kvs/bls
cargo test -p blossom-sys-kvs
```

#### M9.4 — blossom-ldfi II: single-shot mode, vacuity pruning, symmetry, resume, sweeps, estimators, choices, rejections, reports

- **Size:** ~5k
- **Depends on:** M8.1, M8.2
- **Owns:** `crates/blossom-ldfi/**`, `crates/blossom-prov/**`, `crates/blossom-cli/src/cmd/ldfi.rs`, `tests/integration/tests/ldfi2_*.rs`, `docs/plan/notes/M9.4.md`
- **Features:** TEST-030, TEST-031, TEST-032, TEST-033, TEST-034, TEST-035, TEST-036, TEST-037, TEST-039, TEST-040, TEST-062, TEST-105, TEST-143, BENCH-232
- **Consumes:**
  - ldfi I
  - sim II (snapshots, rejections)
  - prov II
- **Provides:**
  - LdfiConfig options single_shot, symmetry, resume_from_snapshots, sweep, estimator, baselines, crash_recovery (experimental), choice reporting, rejection labels; HTML reports; `check ldfi expect holds|fails` as a CI gate

**Build** (ARCHITECTURE §8.5, §8.7; R06).
- **Single-shot mode** (TEST-030): when the backward slice of post has no negation and no non-monotone aggregate, one
  failure-free run plus one enumeration covers every relevant falsifier (each candidate still gets one forward run).
- **Vacuity pruning** (TEST-031): drop a hypothesis when every post tuple it falsifies has its pre tuple falsified.
- **Symmetry reduction** (TEST-032): canonicalize hypotheses under node permutations that fix the EDB and never appear
  as rule literals, bucketed as Molly buckets them.
- **Resume from snapshots** (TEST-033): a hypothesis whose earliest fault is at t starts from the snapshot at the end
  of t − 1 (exact).
- **Parameter sweep** (TEST-034, `Ldfi::sweep`), **run-count estimator** (TEST-035, `num-bigint` grossEstimate),
  **baselines** (TEST-036: random and causal-only), **extensions** (TEST-037: crash-recovery hypotheses, labeled
  experimental).
- **LDFI as a CI gate** (TEST-039): `check ldfi { … } expect holds|fails` in specs becomes a pass/fail with exit code 3.
- **Choices** (TEST-040): hypothesis runs reuse the failure-free run's seeds without overrides; report every choice
  that differs and why.
- **Rejections** (TEST-105): admission rejections appear as omissions labeled `Auth`; BENCH-232: LDFI verdicts are
  unchanged by ACL rejections (integration test).
- **Exact reads in lineage** (TEST-143): `ExactRead`, non-lattice aggregates and choices may be falsified by adding
  a contributor (OR of contributors' hazards and the conservative negative support of the read relation).
- **Reports** (TEST-062): JSON + a static HTML index with per-counterexample space-time diagrams (sim II renderer) and
  the Nemo difference between good and bad lineage (prov II).

**Required tests.** `single_shot_equals_full_enumeration` (on monotone corpus programs), `vacuity_pruning_`,
`symmetry_reduces_runs`, `resume_equals_fresh_run`, `sweep_`, `estimator_`, `choices_reported`,
`exact_read_falsified_by_addition`, `ci_gate_exit_code_3`, `html_report_snapshot`, integration
`ldfi2_bench_232_rejections_do_not_change_verdicts`, and Molly parity still green with the optimizations on.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-ldfi
cargo test -p blossom-testkit --test molly_parity --features sat-cadical,sat-batsat
cargo test -p blossom-integration-tests --test ldfi2_rejections
scripts/require-tests.sh blossom-ldfi single_shot_equals_full_enumeration vacuity_pruning_ symmetry_reduces_runs resume_equals_fresh_run sweep_ estimator_ choices_reported exact_read_falsified_by_addition ci_gate_exit_code_3 html_report_snapshot
```

#### M9.5 — blossom-verify I: bounded model checking, bound certificates, law proofs, confluence certificates, trusted modules, rewrite verification

- **Size:** ~7k
- **Depends on:** M8.2, M2.5, M2.4, M7.5, M6.6
- **Owns:** `crates/blossom-verify/**`, `crates/blossom-testkit/src/backend_verify.rs`, `crates/blossom-cli/src/cmd/verify.rs`, `tests/integration/tests/verify1_*.rs`, `docs/plan/notes/M9.5.md`
- **Features:** VER-002, VER-005, VER-014, VER-015, VER-016, VER-020, TEST-087, LANG-205
- **Consumes:**
  - sim II (ExhaustiveScheduler, ultimate models)
  - blossom-smt
  - blossom-sat
  - rewrites
  - analysis certificates
- **Provides:**
  - blossom_verify::{bmc::{check, BmcBounds, BmcResult}, certificate::BoundCertificate, laws::{prove, LawStatus updates}, confluence::certify, trusted::check_interface, rewrite::verify}
  - corpus `bmc` backend
  - `blossom verify` (bmc; smt/asp in M10.4)

**Build** (ARCHITECTURE §9.1, §9.5; R10).
- **BMC** (VER-002): DFS over `World` states with `ExhaustiveScheduler`, `MemDurability`, digests on; visited set
  keyed by the complete `WorldDigest`; heartbeat transitions for nodes whose empty tick has effects; reductions:
  CALM-POR (only branching messages create alternative batches), DPOR over independent node ticks with sleep sets,
  symmetry over EDB-symmetric nodes, quiescence cut; bounds `check bmc { ticks, delay, in_flight }` + the fault
  budget; every counterexample re-executed in the simulator before it is reported; program errors are "fails".
- **Bound certificates** (VER-005) stating the model (async or sync), node count, ticks, Δ and fault budget, attached
  to every BMC and LDFI result.
- **Law proofs** (VER-014, TEST-087): lattice operations, UDFs, UDAs and group/ring declarations whose IR bodies fall
  in a supported fragment are translated to SMT (LIA, bit-vectors, finite sets/maps via arrays) and proved or
  refuted; otherwise property-tested (TEST-083 laws + commutativity obligations + k-order shuffles); status recorded
  as `Proved | Tested | Refuted` in `FnProps`/`LawStatus`; Refuted is BLS0704; only Proved enables ANA-015 upgrades.
- **Confluence certificates** (VER-015): static certificates combined with bounded confluence testing (sim II);
  disagreement → a witness pair of schedules; unestablished ultimate model → inconclusive.
- **Rewrite verification** (VER-016): original vs rewritten programs compared in simulation on the corpus (same outputs
  at the same ticks) for every rewrite of M7.5.
- **Trusted modules** (VER-020, LANG-205): a `#[trusted("…")]` module's interface spec checked by simulation and LDFI
  against its declared interface; CALM analysis then treats it as opaque.
- Corpus `bmc` backend (`[expect_verify] check = "bmc"`), `blossom verify --bmc`.

**Required tests.** `bmc_finds_known_bug_`, `bmc_holds_within_bounds_`, `bmc_counterexample_replays`,
`bmc_dpor_equals_full_search` (small programs), `bound_certificate_fields`, `law_proof_lia_`, `law_refuted_dompair`,
`law_tested_fallback`, `confluence_certificate_`, `rewrite_verification_`, `trusted_module_interface_`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-verify
BLOSSOM_REQUIRE_SOLVERS=z3 cargo test -p blossom-verify
cargo test -p blossom-integration-tests --test verify1_rewrites
scripts/require-tests.sh blossom-verify bmc_finds_known_bug_ bmc_holds_within_bounds_ bmc_counterexample_replays bmc_dpor_equals_full_search bound_certificate_fields law_proof_lia_ law_refuted_dompair law_tested_fallback confluence_certificate_ rewrite_verification_ trusted_module_interface_
```

#### M9.6 — Standard library III: fault-injecting/FIFO/causal delivery, broadcast family, gossip, election, leases, 3PC/CTP, 2PL

- **Size:** ~5k
- **Depends on:** M8.3, M8.2
- **Owns:** `std/delivery/**`, `std/bcast/**`, `std/election/**`, `std/lease/**`, `std/commit/**`, `std/lock/**`, `crates/blossom-std-host/src/delivery.rs`, `crates/blossom-std-host/src/bcast.rs`, `crates/blossom-std-host/src/election.rs`, `crates/blossom-std-host/src/lease.rs`, `crates/blossom-std-host/src/commit.rs`, `crates/blossom-std-host/src/lock.rs`, `tests/corpus/std/delivery/**`, `tests/corpus/std/bcast/**`, `tests/corpus/std/election/**`, `tests/corpus/std/lease/**`, `tests/corpus/std/commit/**`, `tests/corpus/std/lock/**`, `docs/plan/notes/M9.6.md`
- **Features:** LIB-004, LIB-005, LIB-006, LIB-008, LIB-009, LIB-024, LIB-025, LIB-042, LIB-043
- **Consumes:**
  - std I (M8.3)
  - the simulator (fault models, history checkers)
- **Provides:**
  - std::delivery::{fault_injecting, fifo, causal}, std::bcast::{reliable family, gossip/anti-entropy}, std::election, std::lease, std::commit::{ctp, three_phase}, std::lock::two_phase

**Rules for standard-library WPs.** As in M8.3: modules in `std/<area>/`, `--strict` clean, host functions in
`crates/blossom-std-host/src/<area>.rs`, corpus cases under `tests/corpus/std/<area>/` passing on `oracle`,
`interp` and `sim` (≥ 16 seeds), spec invariants checked in simulation, doc comments on every module.

**Build** (FEATURES §9.1–§9.3 P1 items; R03, R06, R07).
- `std::delivery`: fault-injecting delivery (LIB-004, the Dastardly-style adversarial delivery used by BloomUnit),
  FIFO (LIB-005), causal delivery with vector clocks (LIB-006).
- `std::bcast`: the reliable broadcast family (LIB-008: reliable, uniform, causal, total order through
  `std::consensus` when available), gossip and anti-entropy (LIB-009).
- `std::election` leader election (LIB-024), `std::lease` leases with bounded clock drift (LIB-025),
  `std::commit::{ctp, three_phase}` (LIB-042), `std::lock::two_phase` lock manager (LIB-043).
- Each protocol's safety property as a spec invariant checked in simulation; the commit protocols also get LDFI
  specs (their Molly counterparts are in the BENCH-131 corpus).

**Acceptance** (every command must pass from the repository root):

```sh
sh -c 'for d in delivery bcast election lease commit lock; do cargo run -q -p blossom-cli -- check --strict std/$d || exit 1; done'
cargo run -q -p xtask -- corpus --check --filter 'std/(delivery|bcast|election|lease|commit|lock)/' --require-pass oracle,interp,sim
```

#### M9.7 — Standard library IV: MV-KVS, MVCC, Dynamo KVS, consistency levels, causal KVS, registers, lattice GC, Z-set views, authorization

- **Size:** ~5.5k
- **Depends on:** M8.4, M8.2, M8.6
- **Owns:** `std/kvs/**`, `std/gc/**`, `std/zset/**`, `std/authz/**`, `crates/blossom-std-host/src/kvs.rs`, `crates/blossom-std-host/src/gc.rs`, `crates/blossom-std-host/src/zset.rs`, `crates/blossom-std-host/src/authz.rs`, `tests/corpus/std/kvs/**`, `tests/corpus/std/gc/**`, `tests/corpus/std/zset/**`, `tests/corpus/std/authz/**`, `tests/corpus/net/BENCH-112*/**`, `docs/plan/notes/M9.7.md`
- **Features:** LIB-082, LIB-083, LIB-084, LIB-085, LIB-087, LIB-088, LIB-089, LIB-093, LIB-120, LANG-244, BENCH-112
- **Consumes:**
  - std II (M8.4)
  - P1 natives (wrapped channels for replicated Z-sets)
  - history checkers
- **Provides:**
  - std::kvs::{mv, mvcc, dynamo, consistency, causal, registers}, std::gc, std::zset, std::authz

**Rules for standard-library WPs.** As in M8.3: modules in `std/<area>/`, `--strict` clean, host functions in
`crates/blossom-std-host/src/<area>.rs`, corpus cases under `tests/corpus/std/<area>/` passing on `oracle`,
`interp` and `sim` (≥ 16 seeds), spec invariants checked in simulation, doc comments on every module.

**Build** (FEATURES §9.5, §9.7 P1 items; R04, R13, R15).
- `std::kvs`: multi-version KVS (LIB-082), MVCC (LIB-083), Dynamo-style versioned KVS with `ldom` (LIB-084), a library
  of consistency levels (LIB-085), causal KVS COPS-style (LIB-089), atomic registers (LIB-088; linearizability checked
  with the sim II history checker).
- `std::gc` lattice GC protocols (LIB-087); `std::zset` replicated Z-set collections and views over wrapped channels
  (LIB-093).
- `std::authz` authorization policy library (LIB-120) and the rule-level authorization idiom (LANG-244).
- Author and pass BENCH-112 (MVCC and MV-KVS suites, from bud-sandbox).

**Acceptance** (every command must pass from the repository root):

```sh
sh -c 'for d in kvs gc zset authz; do cargo run -q -p blossom-cli -- check --strict std/$d || exit 1; done'
cargo run -q -p xtask -- corpus --check --filter 'std/(kvs|gc|zset|authz)/' --require-pass oracle,interp,sim
cargo run -q -p xtask -- corpus --check --filter 'net/BENCH-112' --require-pass oracle,interp,sim
```

#### M9.8 — Runtime III: host services, output handlers, blobs, stdio and file sources, the `blossom` facade crate

- **Size:** ~5k
- **Depends on:** M8.7, M8.5, M8.2
- **Owns:** `crates/blossom-runtime/**`, `crates/blossom-node/**`, `crates/blossom/**`, `tests/integration/tests/host_*.rs`, `docs/plan/notes/M9.8.md`
- **Features:** LANG-028, LANG-051, LANG-184, LANG-186, DIST-040
- **Consumes:**
  - runtime II
  - node
  - codegen typed rows (InputRow/OutputRow)
  - driver (facade `compiler` feature)
- **Provides:**
  - blossom (facade): feature `runtime` (Runtime, NodeHandle, typed rows) and feature `compiler` (Compiler)
  - service execution (LANG-184), output handlers (LANG-186), blob handles and blob stores (LANG-028), stdio/file_reader/readonly sources (LANG-051), `run_available`/`run` embedding completeness (DIST-040)

**Build** (ARCHITECTURE §5.10; LANGUAGE §16.4–§16.6, §7.15).
- **Services** (LANG-184): `NodeHandle::register_service`; calls leave through the `name(@$host, …)` channel after
  release, results return as `name.result` events recorded in the trace (§6.4); a failing service yields no result
  row and is logged/counted unless the service declares an error output.
- **Output handlers** (LANG-186): `register_handler(output, h)`; run after release; errors surfaced to the host and
  counted, never altering the tick.
- **Blobs** (LANG-028): content-addressed `Blob` values (BLAKE3 + length) with a `BlobStore` trait (filesystem
  implementation) for handlers and services; bytes never interned.
- **stdio / file sources** (LANG-051): `stdin` lines since the last tick, `stdout` writes at release, `file_reader`
  as a recorded table function, `#[readonly]` host tables written between ticks.
- **Embedding completeness** (DIST-040): `run_tick`/`run_available`/`run`/`pause`/`stop` on both the threaded runtime
  and `ManualDriver`; `sync_do` results after release; subscriptions with per-row finality status.
- **Facade crate `blossom`**: `blossom::Compiler::new().compile_file(…)` (feature `compiler`), `blossom::Runtime`,
  `NodeHandle`, typed `InputRow`/`OutputRow` via generated code, re-exports of `Value`; examples in `examples/`
  of the crate (a KVS server embedding E1).

**Required tests.** integration `host_service_roundtrip`, `host_service_failure_no_row`, `host_handler_after_release`,
`host_blob_store_`, `host_stdin_stdout`, `host_file_reader_recorded_for_replay`, `host_readonly_table`,
`host_sync_do_after_release`, `host_facade_compile_and_run_e01`, `host_typed_rows_generated`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-runtime blossom-node blossom
cargo test -p blossom-integration-tests --test host_embedding
scripts/require-tests.sh blossom-integration-tests host_service_roundtrip host_service_failure_no_row host_handler_after_release host_blob_store_ host_stdin_stdout host_file_reader_recorded_for_replay host_readonly_table host_sync_do_after_release host_facade_compile_and_run_e01 host_typed_rows_generated
```

### M10 — Raft P1, commit protocols, BOOM-FS, the ISR log, SMT/ASP verification, planner performance, applications

**Goal.** Raft reaches the dissertation's feature set; 2PC runs over Raft participants; BOOM-FS and the ISR log run; first-order inductive-invariant proofs and ASP bounded checking land (Paxos Made EPR); the planner gains WCOJ, SIP and adaptive alternatives; the application library and remaining test tooling land.

**Gate.** `scripts/milestone-gate.sh M10` (PLAN §3).

#### M10.1 — Raft II: membership changes, snapshots, client semantics, linearizable reads, extensions, RSM API, KV on Raft, epochs

- **Size:** ~7k
- **Depends on:** M9.1, M9.8, M8.2
- **Owns:** `std/consensus/**`, `crates/blossom-std-host/src/consensus.rs`, `systems/raft/**`, `tests/corpus/std/consensus/**`, `std/membership/epoch/**`, `tests/corpus/std/membership/epoch/**`, `docs/plan/notes/M10.1.md`
- **Features:** FLAG-009, FLAG-010, FLAG-011, FLAG-012, FLAG-013, FLAG-014, FLAG-015, LIB-023, DIST-042, BENCH-172, BENCH-174, BENCH-175
- **Consumes:**
  - Raft I
  - host embedding (blobs for snapshots)
  - history checkers (linearizability)
- **Provides:**
  - full Raft in systems/raft; std::membership::epoch (dynamic membership by epoch over the declared node pool)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.1 P1 items; Ongaro's dissertation; ARCHITECTURE §5.9 node pool).
- Membership changes (FLAG-009: single-server changes with the 2015 bug fix, and joint consensus), snapshots and
  chunked InstallSnapshot through blobs (FLAG-010), client semantics with sessions and exactly-once (FLAG-011),
  linearizable reads (read index and lease reads, FLAG-012), extensions (pre-vote, check-quorum, leadership transfer;
  FLAG-013), the replicated-state-machine API (FLAG-014), linearizable KV on Raft (FLAG-015).
- **Dynamic membership by epoch** (LIB-023, DIST-042): `std::membership::epoch` runs epochs on
  `std::consensus::raft`; each sealed epoch is a subset of the declared node pool (`node_dir` and `R$members` stay
  static).
- **Tests**: BENCH-172 (the 2015 membership bug is found without the fix and absent with it), BENCH-174 (snapshots and
  linearizable KV checked with the Porcupine-style checker), BENCH-175 (negative tests from Bud Raft).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/raft/bls
cargo test -p blossom-sys-raft
cargo run -q -p xtask -- corpus --check --filter 'std/(consensus|membership/epoch)/' --require-pass oracle,interp,sim
```

#### M10.2 — Commit protocols: 2PC over Raft-replicated participants, 3PC and 2PC-CTP as LDFI demonstrators, scalable 2PC

- **Size:** ~4k
- **Depends on:** M9.1, M9.6, M9.4, M7.5
- **Owns:** `systems/commit/**`, `docs/plan/notes/M10.2.md`
- **Features:** FLAG-040, FLAG-041, FLAG-042, BENCH-178
- **Consumes:**
  - std::consensus::raft
  - std::commit (2PC, CTP, 3PC)
  - LDFI II
  - rewrites
- **Provides:**
  - systems/commit (blossom-sys-commit)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.3; R06 (commit protocols under LDFI), R08 (SIGMOD'24 scalable 2PC)).
- 2PC whose participants are Raft groups (FLAG-040): coordinator failover through Raft, durable decisions.
- 3PC and 2PC-CTP (FLAG-041) as LDFI demonstrators: LDFI finds the known counterexamples of plain 2PC/3PC under the
  published failure specs and certifies the fixed variants within bounds.
- Scalable 2PC and voting via the decoupling/partitioning rewrites (FLAG-042).
- BENCH-178: the Dedalus 2PC and voting (autocomp) programs and their expected outcomes.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/commit/bls
cargo test -p blossom-sys-commit
```

#### M10.3 — BOOM-FS: metadata as relations, heartbeats, re-replication, the data path, HA via Raft, partitioned metadata, client library

- **Size:** ~7k
- **Depends on:** M9.1, M9.8, M8.3
- **Owns:** `systems/boomfs/**`, `docs/plan/notes/M10.3.md`
- **Features:** FLAG-080, FLAG-081, FLAG-082, FLAG-083, FLAG-084, FLAG-085, FLAG-086, BENCH-180, BENCH-181, BENCH-182
- **Consumes:**
  - Raft
  - host embedding (handlers, services, blobs)
  - std::fd heartbeats
- **Provides:**
  - systems/boomfs (blossom-sys-boomfs): NameNode metadata program, DataNode host code, client library

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.5; R07 (BOOM Analytics)).
- NameNode metadata as relations (files, directories, chunks, locations; FLAG-080) with the BOOM rules; DataNode
  heartbeats (FLAG-081); re-replication of under-replicated chunks (FLAG-082); the data path outside the engine: chunk
  bytes stream through output handlers, services and blobs (FLAG-083).
- HA metadata via Raft (FLAG-084), metadata partitioned by `hash(fqpath)` (FLAG-085), a Rust client library (FLAG-086).
- **Tests**: BENCH-180 (BFS/BOOM-FS functional suite: create, read, write, delete, list, chunk placement), BENCH-181
  (NameNode failover under crash with no metadata loss), BENCH-182 (partitioned NameNode correctness); simulation
  suites with DataNode crashes and partitions; a real-files integration test through the runtime.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/boomfs/bls
cargo test -p blossom-sys-boomfs
```

#### M10.4 — blossom-verify II: ASP bounded encoding, FOL transition systems, EPR, inductive invariants, lattice axioms, Paxos Made EPR

- **Size:** ~8k
- **Depends on:** M9.5, M9.2
- **Owns:** `crates/blossom-verify/**`, `crates/blossom-smt/**`, `crates/blossom-cli/src/cmd/verify.rs`, `docs/design/SEMANTICS.md`, `tests/integration/tests/verify2_*.rs`, `docs/plan/notes/M10.4.md`
- **Features:** VER-003, VER-006, VER-007, VER-008, VER-009, VER-010, VER-011, VER-040, VER-041, VER-042, VER-043, SEM-045, SEM-106, SEM-108
- **Consumes:**
  - verify I
  - blossom-smt (z3, cvc5, clingo)
  - sim II (exhaustive simulation for cross-checks)
- **Provides:**
  - blossom_verify::{asp::{encode, pure, run}, fol::{translate, epr_check, projections, vcs, regular_invariants}}
  - `blossom verify --smt|--asp`
  - docs/design/SEMANTICS.md (the published pure^L(P) translation)

**Build** (ARCHITECTURE §9.2, §9.4, §9.5; R10, R11).
- **ASP** (VER-003, VER-040/041, SEM-045): the STABLE transformation over bounded time (one ground copy per (node,
  tick)); a choice rule per message choosing arrival in (s, s+Δ] or ⊥ within the omission budget and the channel's
  fault model; causality via `before`; properties as integrity constraints; FLP/Ferraris aggregates (CR-54); lattice
  cells via E-atoms or E-contrib (VER-041); `asp::pure` = the published `pure^L(P)` (SEM-045, SEM-106: `snd_R` as a
  lattice relation, arrival keyed by (x, s, y, t, k̄), arrival merging into the receiver; L-stable models with the
  Ross–Sagiv reduct), documented in `docs/design/SEMANTICS.md`; stable models decoded into fault schedules + schedules
  and replayed in simulation; BENCH-313 as the semantics oracle.
- **Theorem 4^L cross-check** (SEM-108, BENCH-312): clingo models of `pure^L(P)` vs ultimate models of exhaustive
  simulation on the corpus.
- **FOL** (VER-006–011, VER-042/043): sorts from declared types (`Node<R>` per role, `#[ordered]` with total-order
  axioms, opaque types uninterpreted, LIA mode outside EPR); one relation symbol per persistent relation with location
  first; lattice cells as `ge(k̄, v)` with down-closure (VER-042); the network as a grow-only `sent`; actions `tick(n)`
  over an arbitrary subset of delivered messages, deductive strata as definitions, positive in-tick recursion
  over-approximated by pre-models (VER-043) only after the transitive polarity check (VER-006), `env_input`, `crash`;
  choice sites as uninterpreted functions constrained by their FD (from the `Choose` construct spec), `$now`/timers as
  arbitrary non-decreasing inputs; `max`/`min` by EPR eq. (3), `majority`/`quorum` via the quorum sort with the
  intersection axiom (VER-008); EPR check (VER-007) reporting quantifier-alternation cycles with the formula fragments
  and offering auto-derived projection relations (VER-009), semi-bounded mode; VCs INIT ⇒ INV, INV ∧ TR ⇒ INV′,
  INV ⇒ P through `SmtSolver` (VER-010) with counterexamples-to-induction rendered as node/message graphs; regular
  invariants from syntax as lemmas (VER-011); `prove G by induction using L…`; `check smt expect holds` gates CI.
- **Paxos Made EPR** (BENCH-150): the corpus case proves on z3 (and cvc5 when installed).

**Required tests.** `asp_encoding_small_programs`, `asp_models_replay_in_sim`, `pure_l_matches_ultimate_models`
(BENCH-312 style), `bench_313_flp_gz`, `fol_translate_snapshot_`, `epr_cycle_reported`, `projection_breaks_cycle`,
`vc_inductive_counter_invariant`, `cti_rendered`, `regular_invariants_generated`, `paxos_made_epr_proves`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-verify blossom-smt
scripts/install-solvers.sh
BLOSSOM_REQUIRE_SOLVERS=all cargo test -p blossom-verify
cargo run -q -p xtask -- corpus --check --filter 'verify/BENCH-150' --require-pass smt
scripts/require-tests.sh blossom-verify asp_encoding_small_programs asp_models_replay_in_sim pure_l_matches_ultimate_models bench_313_flp_gz fol_translate_snapshot_ epr_cycle_reported projection_breaks_cycle vc_inductive_counter_invariant cti_rendered regular_invariants_generated paxos_made_epr_proves
```

#### M10.5 — The replicated log with ISR (Kafka 0.8) reproducing its durability bug under LDFI

- **Size:** ~3k
- **Depends on:** M9.4, M8.5
- **Owns:** `systems/isrlog/**`, `docs/plan/notes/M10.5.md`
- **Features:** FLAG-150
- **Consumes:**
  - LDFI II
  - codegen/build
  - the simulator
- **Provides:**
  - systems/isrlog (blossom-sys-isrlog)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.8; R06 §12.3).
- A replicated log with in-sync replicas as in Kafka 0.8: leader, followers, ISR shrink/expand, high watermark,
  acks=all semantics.
- LDFI reproduces the published durability bug (an acknowledged write lost after the ISR shrinks to the leader and the
  leader fails) with the minimal falsifier, and certifies the fixed variant (min.insync.replicas) within the stated
  bounds; the counterexample replays in the simulator and renders as a space-time diagram.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/isrlog/bls
cargo test -p blossom-sys-isrlog
```

#### M10.6 — Planner and kernel performance: WCOJ, SIP, adaptive alternatives, subplan sharing, RAM rewrites, specialized representations

- **Size:** ~8k
- **Depends on:** M8.6, M7.3
- **Owns:** `crates/blossom-plan/**`, `crates/blossom-kernel/**`, `tests/integration/tests/perf1_*.rs`, `docs/plan/notes/M10.6.md`
- **Features:** ENG-027, ENG-028, ENG-066, ENG-072, ENG-084, ENG-086, ENG-087, ENG-088, ENG-089, ENG-090
- **Consumes:**
  - plan I/II
  - kernel I/II
  - the differential runner (perturbation must include every new alternative)
- **Provides:**
  - Op::Intersect with treefrog leapers, vectorized Op::Node (Free Join), SIP alternatives, precompiled alternatives with switch rules, subplan sharing, RAM rewrites, fused join-aggregate, RelStore provider trait, EqRel/Brie, order-statistic trees, invalidation sets, inline short strings

**Build** (ARCHITECTURE §3.8 P1 steps, §3.9, §4.3, §4.4; R09).
- **WCOJ** (ENG-084): GYO detects cyclic joins; cycle variables become one `Op::Intersect` node with treefrog leapers
  (`count`, `propose`, `intersect`, plus anti and filter leapers) over `TrieCursor`s.
- **Vectorized Free Join** `Op::Node` (ENG-080 P1 form): probe every subatom for a batch of cover tuples; multi-level
  COLT tries for batch workloads.
- **SIP** (ENG-086): two-pass semijoin-reduced alternatives for expensive rules (cyclic or > 4 atoms).
- **Alternatives** (ENG-089): up to 3 precompiled orders per version with a `SwitchRule` on observed Δ/full sizes and
  hysteresis; hints pin a choice.
- **Subplan sharing** (ENG-087) by canonical hashing; **RAM-level rewrites** (ENG-088: HoistConditions, IfExists
  conversion, …); **fused join and aggregate** (ENG-090) into group tables.
- **Representations** (ENG-027/028): a `RelStore` provider trait enforcing "no fact skips Δ" via `merge(new, Δ, total)`;
  `Nullary`, `EqRel` (union-find with Δ extension), `Brie` (dense low-arity).
- **Order-statistic index** (ENG-072) for `index!`/top-k over standing inputs (a change at rank r re-emits later ranks).
- **Invalidation sets** (ENG-066): statically computed operators downstream of impure or non-incremental code, forced
  to Transient or Recompute.
- **Inline short strings** (≤ 7 bytes packed in an Interned word with a tag bit).
- Every new plan choice is added to the perturbation profile; the whole corpus stays differential-green.

**Required tests.** integration `perf1_differential_with_new_alternatives` (corpus × 16 perturbations);
unit `leapfrog_triangle_`, `treefrog_vs_binary_join` (proptest), `free_join_node_`, `sip_alternative_equal`,
`switch_rule_hysteresis`, `subplan_sharing_`, `eqrel_delta_extension`, `brie_`, `order_statistic_index_`,
`invalidation_sets_`, `inline_short_strings_canonical_order`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-plan blossom-kernel
cargo test -p blossom-integration-tests --test perf1_differential
scripts/require-tests.sh blossom-kernel leapfrog_triangle_ eqrel_delta_extension brie_ order_statistic_index_ inline_short_strings_canonical_order
scripts/require-tests.sh blossom-plan treefrog_vs_binary_join free_join_node_ sip_alternative_equal switch_rule_hysteresis subplan_sharing_ invalidation_sets_
```

#### M10.7 — Standard library V: applications and demonstrators (carts, state machines, Chord, routing, ping, TC, rendezvous)

- **Size:** ~6k
- **Depends on:** M9.6, M9.7, M8.2
- **Owns:** `std/examples/**`, `crates/blossom-std-host/src/examples.rs`, `tests/corpus/std/examples/**`, `tests/corpus/net/BENCH-113*/**`, `tests/corpus/net/BENCH-114*/**`, `tests/corpus/net/BENCH-115*/**`, `tests/corpus/protocols/BENCH-179*/**`, `docs/plan/notes/M10.7.md`
- **Features:** LIB-100, LIB-101, LIB-102, LIB-103, LIB-104, LIB-105, LIB-107, BENCH-113, BENCH-114, BENCH-115, BENCH-179
- **Consumes:**
  - std I–IV
  - sim II (QuiescentStochasticScheduler, history checkers)
- **Provides:**
  - std::examples::{cart, state_machine, chord, routing, ping, tc, rendezvous}
  - BENCH-113/114/115/179 corpus

**Rules for standard-library WPs.** As in M8.3: modules in `std/<area>/`, `--strict` clean, host functions in
`crates/blossom-std-host/src/<area>.rs`, corpus cases under `tests/corpus/std/<area>/` passing on `oracle`,
`interp` and `sim` (≥ 16 seeds), spec invariants checked in simulation, doc comments on every module.

**Build** (FEATURES §9.6; R01 (Overlog corpus), R03 (bud-sandbox, BloomUnit), R08 (Hydro examples)).
- Shopping carts, destructive and monotone (LIB-100); state-machine module (LIB-101); Chord DHT with 160-bit modular
  ids and ring intervals (LIB-102); routing (distance vector, path vector; LIB-103); ping/pong and link liveness from
  soft state (LIB-104); distributed transitive closure, deadlock detection and coordinated GC (LIB-105);
  request/response rendezvous (LIB-107).
- Author and pass: BENCH-113 (other sandbox suites: state machine, heartbeat, membership, Chord with the exact
  tc_chord tuples, leader election, MI cache coherence, BFS), BENCH-114 (BloomUnit specs: the FIFO spec fails under
  Dastardly delivery and never under ordered delivery; CartSpec exploration finds a checkout that overtakes an
  action and the fixed carts pass every schedule), BENCH-115 (Overlog corpus: ping-pong 20/20, soft-state ping-pong
  link expiry ≈10 s, Narada dead-neighbor removal ≈21 s and the count-0 idiom, static ring lookup 289383 →
  localhost:33333, dynamic and robust rings, 3-node Chord `bestSucc` chain), BENCH-179 (Hydro examples, including
  Maelstrom broadcast run in our simulator).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict std/examples
cargo run -q -p xtask -- corpus --check --filter 'std/examples/' --require-pass oracle,interp,sim
cargo run -q -p xtask -- corpus --check --filter '(net/BENCH-11[345]|protocols/BENCH-179)' --require-pass interp,sim
```

#### M10.8 — blossom-testkit III: seed sweeps, shuffle checks, input generation from constraints, implementation equivalence

- **Size:** ~4k
- **Depends on:** M8.2, M8.1, M2.4
- **Owns:** `crates/blossom-testkit/**`, `tests/integration/tests/testkit3_*.rs`, `docs/plan/notes/M10.8.md`
- **Features:** TEST-014, TEST-015, TEST-082, TEST-084
- **Consumes:**
  - sim II
  - blossom-sat
  - oracle
- **Provides:**
  - blossom_testkit::{sweep::{seed_sweep, equivalent_modulo_choices}, shuffle::check_ca_claims, inputgen::{from_pre, enumerate}, equiv::{run_pair, InterfaceMapping}}

**Build** (ARCHITECTURE §11.6, §11.9).
- **Seed sweeps** (TEST-014): equivalence modulo choices across seeds (outputs equal after abstracting choice
  outcomes; choice validity on every run).
- **Shuffle checks** (TEST-015): the oracle evaluates every UDA or `reduce!` declared commutative and associative in k
  random orders; a mismatch is a hard error naming the aggregate.
- **Input generation from constraints** (TEST-082): encode a spec's `pre` over bounded input domains into SAT through
  `blossom-sat` and enumerate satisfying input sets.
- **Implementation equivalence** (TEST-084): run two programs with a declared interface mapping under the same
  generated inputs and schedules and compare outputs.

**Required tests.** `seed_sweep_equivalent_modulo_choices`, `shuffle_detects_false_ca_claim`,
`inputgen_satisfies_pre`, `inputgen_enumerates_all_small`, `equivalence_detects_difference`, integration
`testkit3_carts_equivalent` (destructive vs monotone cart under the manifest fix).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-testkit
scripts/require-tests.sh blossom-testkit seed_sweep_equivalent_modulo_choices shuffle_detects_false_ca_claim inputgen_satisfies_pre inputgen_enumerates_all_small equivalence_detects_difference
```

### M11 — BOOM-MR, the lakehouse, network P1 features, engine incrementality, batch benchmarks and security P1

**Goal.** BOOM-MR reaches parity (ODD-19 M1a); the replicated object store and Lattice Lakehouse log land; channels gain delta shipping, flow control, filters and push semantics; recursive views under deletion and intra-tick parallelism land; the batch Datalog benchmark gate is measured; sessions, the admin plane, certificate lifecycle and QUIC land.

**Gate.** `scripts/milestone-gate.sh M11` (PLAN §3).

#### M11.1 — BOOM-MR: scheduler state as relations, FCFS with speculation, LATE, the MapReduce data plane, locality and fair share

- **Size:** ~6k
- **Depends on:** M10.3, M9.8
- **Owns:** `systems/boommr/**`, `docs/plan/notes/M11.1.md`
- **Features:** FLAG-100, FLAG-101, FLAG-102, FLAG-103, FLAG-104, BENCH-183
- **Consumes:**
  - BOOM-FS
  - host embedding (task execution as services/handlers)
  - Raft (HA JobTracker)
- **Provides:**
  - systems/boommr (blossom-sys-boommr): JobTracker program, TaskTracker host code, map/reduce data plane

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.6 P0 + FLAG-104; R07 (BOOM-MR)).
- Scheduler state as relations (jobs, tasks, attempts, trackers; FLAG-100); FCFS with Hadoop's default speculation
  (FLAG-101); LATE (FLAG-102); the MapReduce data plane (map tasks read BOOM-FS chunks, partition, spill, shuffle,
  reduce; FLAG-103) executed by host services; locality/delay scheduling, fair share and a highly available
  JobTracker via Raft (FLAG-104).
- BENCH-183: FCFS vs LATE with stragglers (LATE finishes earlier; both produce the same output).
- Word count, grep and a small sort run end to end in simulation (virtual time) and in a real-process deployment.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/boommr/bls
cargo test -p blossom-sys-boommr
```

#### M11.2 — The replicated object store and the Lattice Lakehouse log (ODD-19 M2)

- **Size:** ~6k
- **Depends on:** M10.1, M9.3, M9.8
- **Owns:** `systems/lakehouse/**`, `docs/plan/notes/M11.2.md`
- **Features:** FLAG-125, FLAG-126, FLAG-127, BENCH-185
- **Consumes:**
  - Raft (version claims)
  - the Anna KVS (metadata cache)
  - blobs
- **Provides:**
  - systems/lakehouse (blossom-sys-lakehouse): replicated object store + lattice lakehouse log

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.7 M2; R10 C3).
- A replicated object store written in the language (placement, replication, repair, read/write quorums) over blobs.
- The Lattice Lakehouse log (FLAG-125): add/remove as a 2P-set plus metadata, protocol and `txn(appId, version)`; a
  single coordinated claim of each version through Raft or put-if-absent; a conflict rule; checkpoints; time travel.
- Metadata cache as a lattice KVS with gossip (FLAG-126, on systems/kvs); maintenance (compaction, vacuum, overwrite)
  through commit so orphan files are never visible (FLAG-127).
- BENCH-185 lakehouse scenarios: concurrent appends all commit; an append and a conflicting delete — exactly one aborts;
  a crash between writing data and committing never becomes visible; replaying `txn` produces no duplicates; time
  travel works during compaction.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/lakehouse/bls
cargo test -p blossom-sys-lakehouse
```

#### M11.3 — Node II: delta shipping, flow control and fragmentation, channel filters, push channels and the adaptive send policy

- **Size:** ~5k
- **Depends on:** M9.8, M6.6, M8.2
- **Owns:** `crates/blossom-node/**`, `crates/blossom-wire/**`, `std/push/**`, `crates/blossom-std-host/src/push.rs`, `tests/corpus/std/push/**`, `tests/integration/tests/node2_*.rs`, `docs/plan/notes/M11.3.md`
- **Features:** DIST-006, DIST-008, DIST-009, DIST-011, DIST-012, BENCH-103, BENCH-191
- **Consumes:**
  - node
  - wire frame kinds
  - analysis SEM-109 channel frame kinds
  - lattice Atomize
- **Provides:**
  - LDelta frames (node-side per-destination delta state), bounded queues + fragmentation, `ChannelFilter` hook, std::push (batch-granular push channels with retained output, adaptive send policy)

**Build** (ARCHITECTURE §3.5 frame kinds, §5.7; FEATURES DIST-006/008/009/011/012; R14 (HOP send policy)).
- **Ship deltas** (DIST-006): on channels whose frame kind analysis set to `LDelta` (SEM-109 side condition), the node
  keeps per-(destination, channel, key) last-sent lattice values and encodes `LDelta` = Atomize(old, new); a restart
  resends full values (joins are idempotent); receivers merge.
- **Flow control** (DIST-008): bounded per-peer queues (P0 part exists) plus fragmentation of large batches and
  reassembly under `WireLimits`.
- **Channel filter hook** (DIST-009): a `ChannelFilter` trait on the ingress path after admission; BENCH-103 as an
  integration test (`bench_103_channel_filter`).
- **Push channels** (DIST-011) and the **adaptive send policy** (DIST-012) in `std::push` + node support: batch-granular
  push with retained output (resend on reconnect or reducer failure), per-batch policy (combiner reduction, stall
  thresholds, merged spills, eof sentinel with progress 1.0). BENCH-191 scenarios (a)–(e) as corpus cases.

**Required tests.** `ldelta_equals_full_value_ship` (receiver state identical), `ldelta_restart_resends_full`,
`fragmentation_roundtrip`, `queue_bound_drops_counted`, integration `bench_103_channel_filter`,
`bench_191_a` … `bench_191_e` (corpus).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-node blossom-wire
cargo test -p blossom-integration-tests --test node2_network
cargo run -q -p xtask -- corpus --check --filter 'std/push/' --require-pass oracle,interp,sim
scripts/require-tests.sh blossom-node ldelta_equals_full_value_ship ldelta_restart_resends_full fragmentation_roundtrip queue_bound_drops_counted
```

#### M11.4 — blossom-bench I: harness, datasets, baselines, batch Datalog, tick micro-benchmarks, overheads, soak

- **Size:** ~6k
- **Depends on:** M10.6, M8.5, M9.8
- **Owns:** `crates/blossom-bench/**`, `xtask/src/cmd/fetch_datasets.rs`, `xtask/src/cmd/bench_report.rs`, `scripts/install-baselines.sh`, `docs/perf/**`, `docs/plan/notes/M11.4.md`
- **Features:** BENCH-200, BENCH-201, BENCH-203
- **Consumes:**
  - interpreter + codegen
  - the planner P1 features
  - the node loop (ManualDriver) for tick benchmarks
- **Provides:**
  - `cargo bench -p blossom-bench`
  - `cargo xtask fetch-datasets`
  - `cargo xtask bench-report`
  - docs/perf/*.md reports
  - baselines installer (.tools)

**Build** (ARCHITECTURE §4.13–§4.14; BENCH-200/201/203).
- **Harness**: criterion suites + an iai-callgrind suite (Linux only; instruction-count gating 1–2%), exact allocation
  counts, per-phase breakdown (ingest/decode/intern, strata, temporal, outbox encode, WAL encode, fsync wait);
  interpreter, codegen and baselines side by side; results written by `xtask bench-report` to `docs/perf/`.
- **Datasets**: `xtask fetch-datasets` downloads the BENCH-200/201 inputs into `datasets/` (git-ignored) and verifies
  them against the checked-in manifest `crates/blossom-bench/datasets.toml` (URL, size, BLAKE3 per file).
- **Baselines** (`scripts/install-baselines.sh`, into `.tools/`): Soufflé (release binary or source build; Homebrew
  only when `BLOSSOM_ALLOW_BREW=1`), crates.io baselines as dev-dependencies (ascent, datafrog, crepe,
  differential-dataflow + timely, dbsp); DDlog through published numbers normalized by the Soufflé ratio (stated as
  such).
- **Suites** (§4.14 items 1–5, 7): tick micro-benchmarks (idle ticks; 1 and 100 messages; state 10^3…10^7 in and out
  of cache), update-heavy KVS (Zipf 0.99, p99/p999, a soak mode), lattice suites, batch Datalog (TC, SG, Reach, CC,
  SSSP, Andersen, CSPA, CSDA, Galen, Bipartite, CRDT, Polonius, DOOP, DDISASM; BENCH-200), OpenJDK7 points-to
  (BENCH-201), incremental/retraction workloads vs DD and DBSP, overheads (Tier B ≤ 1.3× time / 1.8× memory, Tier C,
  interpreter within 1.5–3× of codegen; BENCH-203).
- **Gates**: the P0 single-threaded gate (≤ 1.0× Soufflé `-j1` on ≥ 2/3 of BENCH-200, ≤ 1.5× on all) is checked by
  `xtask bench-report --gate p0`; the M8/§0.2 gate (≥ Soufflé `-j4`, ≤ 2× memory) by `--gate full` (needs ENG-102
  from M11.6; measured again in M14.1). Record every number with machine details; never claim a gate that was not
  measured.

**Required tests.** `bench_harness_smoke` (each suite runs one tiny iteration in `cargo test`), `dataset_checksums`,
`report_generation_snapshot`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-bench
scripts/install-baselines.sh
cargo run -q -p xtask -- fetch-datasets --suite batch-small
cargo run -q -p xtask -- bench-report --suite batch-small --gate p0
scripts/require-tests.sh blossom-bench bench_harness_smoke dataset_checksums report_generation_snapshot
```

#### M11.5 — Runtime IV: sessions and the client listener, the admin plane, certificate lifecycle, redaction, QUIC

- **Size:** ~6k
- **Depends on:** M9.8, M8.7, M9.7
- **Owns:** `crates/blossom-runtime/**`, `crates/blossom-cli/src/cmd/admin.rs`, `tests/integration/tests/sec2_*.rs`, `docs/plan/notes/M11.5.md`
- **Features:** DIST-064, DIST-065, DIST-066, DIST-069, LANG-243, BENCH-230, BENCH-231, BENCH-235
- **Consumes:**
  - runtime III
  - std::authz (rule-level authorization)
- **Provides:**
  - client listener with sessions (session_open/closed events, egress to sessions), admin plane over the frame protocol restricted to the `admin` role, certificate reload and expiry enforcement, payload redaction on export, QuicTransport (feature `quic`, 0-RTT disabled)

**Build** (ARCHITECTURE §5.3, §5.8; DIST-064–066/069, LANG-243).
- **Sessions** (LANG-243, DIST-065): per-client-connection session ids, `session_open`/`session_closed` events,
  replies to a closed session dropped and counted, `REJECT` frames for clients.
- **Admin plane** (DIST-066): the same frame protocol on the client listener restricted to the `admin` role: node
  control (pause/resume/stop/status), REPL attach (used by M12.4), upgrade control (used by M12.3), provenance/trace
  export with payload redaction (DIST-069); no data-plane path to install rules.
- **Certificate lifecycle** (DIST-064): `ResolvesServerCert` + client-verifier pair reloaded on change or admin
  command; existing connections recycled; expiry enforced (virtual clock under simulation).
- **QUIC** (ARCH-11, feature `quic`): quinn, one stream per channel class, same frames and admission, 0-RTT disabled;
  passes the `transport_suite` conformance.
- **Tests**: BENCH-230 (certificate rotation and expiry), BENCH-231 (rule-level authorization with std::authz),
  BENCH-235 (cost of security: throughput with mTLS vs plaintext recorded; the budget from FEATURES), as integration
  tests `bench_230_*`, `bench_231_*`, `bench_235_*`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-runtime
cargo test -p blossom-runtime --features quic
cargo test -p blossom-integration-tests --test sec2_security
scripts/require-tests.sh blossom-integration-tests bench_230_ bench_231_ bench_235_ sessions_ admin_plane_ redaction_
```

#### M11.6 — Engine P1 incrementality and parallelism: recursive views under deletion, elastic recompute, specialized diffs, intra-tick parallelism, range deletion

- **Size:** ~6.5k
- **Depends on:** M10.6, M8.6
- **Owns:** `crates/blossom-engine/**`, `crates/blossom-plan/**`, `crates/blossom-store/**`, `tests/integration/tests/perf2_*.rs`, `docs/plan/notes/M11.6.md`
- **Features:** ENG-063, ENG-064, ENG-065, ENG-102, DIST-024
- **Consumes:**
  - plan III/kernel III
  - engine III
- **Provides:**
  - FBF / recursive counting for recursive strata under deletion, elastic Counted→Recompute switching, specialized diffs, morsel-parallel versions (feature `parallel`), DeleteRange WAL records and execution

**Build** (ARCHITECTURE §3.4.3, §4.15; R09).
- **Recursive views under deletion** (ENG-063): replace Recompute for recursive strata with delete-rederive (FBF) or
  recursive counting, chosen by the planner; exactness checked against the oracle on the corpus and under deletion-
  heavy generated workloads.
- **Elastic recompute** (ENG-064): switch Counted → Recompute past θ with hysteresis (`ElasticPolicy`).
- **Specialized diffs** (ENG-065) for common shapes.
- **Intra-tick parallelism** (ENG-102, feature `parallel`): for a version whose driving Δ exceeds a threshold, split the
  outer scan into morsels on rayon; thread-local output buffers; end-of-epoch parallel sort/partition, dedup against
  the total (batch-mode insert), append; no shared concurrent index on the hot path; deterministic results.
- **Range deletion** (DIST-024): `DeleteRange { rel, prefix, lo, hi }` WAL record kind in blossom-store, executed
  through a canonical-order sorted index; recovery replays it.
- All new strategies join the perturbation profile; the differential suite stays green.

**Required tests.** integration `perf2_fbf_equals_oracle`, `perf2_recursive_counting_equals_oracle`,
`perf2_parallel_deterministic` (same digests with 1 and N threads); unit `elastic_switch_hysteresis`,
`specialized_diff_`, `delete_range_wal_replay`.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-engine blossom-plan blossom-store
cargo test -p blossom-engine --features parallel
cargo test -p blossom-integration-tests --test perf2_incremental
scripts/require-tests.sh blossom-engine elastic_switch_hysteresis specialized_diff_
scripts/require-tests.sh blossom-store delete_range_wal_replay
```

### M12 — HOP, the lineage dataflow engine, upgrades, protocol benchmarks, CLI polish and the user documentation

**Goal.** HOP parity (ODD-19 M1b) and the BOOM-2 lineage engine land; rolling upgrades with cluster versions, translation and mixed-version simulation land; protocol throughput is measured against DFIR; the REPL and remaining CLI commands land; the user guide and standard-library reference are written.

**Gate.** `scripts/milestone-gate.sh M12` (PLAN §3).

#### M12.1 — HOP: pipelined shuffle, adaptive flow control, exactly-once pipelined map output, online aggregation, snapshots, continuous scheduling

- **Size:** ~6.5k
- **Depends on:** M11.1, M11.3, M8.6
- **Owns:** `systems/boommr/**`, `docs/plan/notes/M12.1.md`
- **Features:** FLAG-105, FLAG-106, FLAG-107, FLAG-108, FLAG-109, FLAG-110, FLAG-111, FLAG-112, FLAG-135, FLAG-136, BENCH-186, BENCH-187, BENCH-189, BENCH-190, BENCH-192
- **Consumes:**
  - BOOM-MR
  - push channels and the adaptive send policy (M11.3)
  - progressive snapshots and estimators
  - seals
- **Provides:**
  - HOP mode in systems/boommr

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.6 P1 items, FLAG-135/136; R14/G4 (MapReduce Online)).
- Pipelined shuffle (FLAG-105), adaptive flow control and combining (FLAG-106), exactly-once pipelined map output
  (FLAG-107, P0 placed here because it needs DIST-011; PLAN §6), tentative map output (FLAG-108), map progress
  checkpoints as prefix seals (FLAG-109), recovery and speculation under pipelining (FLAG-110), pipeline-aware
  scheduling (FLAG-111), reflective monitoring and alert-based speculation (FLAG-112).
- Online aggregation over MapReduce (FLAG-135, MR part): progressive snapshots labeled by class, atomic publication per
  (job, reducer, point), completeness threshold, durable snapshots, estimators, optional seeded random block order;
  snapshot pipelining between jobs (FLAG-136) with `MapLattice<UpstreamPartition, LexPair<Max<progress>, Snapshot>>`.
- BENCH-186 (online WordCount estimates converge), BENCH-187 (pipelined WordCount → Top-100 chain stays exact),
  BENCH-189 (pipelining vs blocking ratios), BENCH-190 (alert-based speculation), BENCH-192 (online and continuous
  scheduling rules).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/boommr/bls
cargo test -p blossom-sys-boommr
```

#### M12.2 — BOOM-2, the lineage dataflow engine: FS2, the stage planner, shuffle by seals, recovery from lineage, deterministic speculation, CALM-minimized fault tolerance

- **Size:** ~7.5k
- **Depends on:** M10.3, M10.1, M11.3, M11.1, M8.6
- **Owns:** `systems/boom2/**`, `docs/plan/notes/M12.2.md`
- **Features:** FLAG-120, FLAG-121, FLAG-122, FLAG-123, FLAG-124, FLAG-132, BENCH-185
- **Consumes:**
  - BOOM-FS metadata
  - Raft
  - push channels
  - seals and finality
  - the Minimal trace format (FLAG-132 causal log)
- **Provides:**
  - systems/boom2 (blossom-sys-boom2)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.7 M1; ARCHITECTURE §1.5 "The lineage engine"; R10 C1).
- **FS2** (FLAG-120): BOOM-FS metadata plus leases and an `lmax` heartbeat on a Raft log; followers serve snapshot reads.
- **Stage planner** (FLAG-121) as Blossom modules: narrow (co-partitioned, pipelined) vs wide (shuffled) dependencies.
- **Shuffle by seals** (FLAG-122): lattice reducers merge before the reducer is ready; non-idempotent aggregates use
  partials keyed by mapper; threshold reads return early; pipelined by default; seals carry a spill-count digest;
  early outputs follow the ANA-036 classes.
- **Recovery from lineage** (FLAG-123): `derived_from(partition, input_partition, stage, attempt)` as program-level
  relations written by the stage rules (works with engine provenance off); lost partitions recomputed recursively;
  wide dependencies checkpointed by the cost model.
- **Deterministic speculation** with an idempotent commit keyed by attempt (FLAG-124).
- **CALM-minimized fault tolerance** (FLAG-132): deterministic operators replayed; only nondeterministic events at
  points of order logged causally (the Minimal trace restricted to ANA-022 points); optional aligned barrier snapshot
  as a seal-driven native; idempotent or transactional sinks.
- BENCH-185 batch parts: WordCount, Grep, a small TeraSort and PageRank equal a single-node reference with workers
  killed and speculation on; recursive views (CC, TC) maintained incrementally under inserts and deletes equal
  recomputation.

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/boom2/bls
cargo test -p blossom-sys-boom2
```

#### M12.3 — Upgrades: cluster versions, version windows, the rolling-upgrade orchestrator, codec translation, migrations at scale, mixed-version simulation

- **Size:** ~8k
- **Depends on:** M11.5, M8.2, M6.5, M10.1
- **Owns:** `crates/blossom-wire/**`, `crates/blossom-store/**`, `crates/blossom-runtime/**`, `crates/blossom-sim/**`, `std/upgrade/**`, `crates/blossom-std-host/src/upgrade.rs`, `crates/blossom-cli/src/cmd/upgrade.rs`, `tests/corpus/upgrade/**`, `!tests/corpus/upgrade/BENCH-224*/**`, `tests/fixtures/storage/**`, `tests/integration/tests/upgrade_*.rs`, `docs/plan/notes/M12.3.md`
- **Features:** DIST-083, DIST-084, DIST-085, DIST-086, DIST-087, LANG-262, LANG-263, LANG-264, SEM-092, SEM-093, TEST-100, TEST-101, TEST-102, TEST-103, TEST-106, LIB-123, BENCH-221, BENCH-223, BENCH-225, BENCH-226, BENCH-233, BENCH-234
- **Consumes:**
  - admin plane (M11.5)
  - sim II
  - front V (migrations/translations lowering)
  - Raft (cluster-version protocol)
- **Provides:**
  - std::upgrade (cluster-version protocol + version-gated feature idioms)
  - wire::translate
  - durable understood_version
  - `blossom upgrade` orchestrator
  - mixed-version worlds in the simulator
  - golden storage fixtures v1

**Build** (ARCHITECTURE §5.11, §6.5 mixed-version worlds, §11.7; R15 upgrades).
- **Cluster-version protocol** (DIST-083, SEM-092/093) in `std::upgrade` + durable `understood_version` in META;
  `cluster_version()` gates (LANG-264) with ANA-102 dominance already checked.
- **Version window** (DIST-084) enforced at HELLO (`version_unsupported`).
- **Codec translation** (DIST-087, LANG-263): `blossom-wire::translate` compiles `TranslationDecl`s to per-channel
  translator functions over `CExpr` (stateless, tuple-local); unmatched tuples are `disallowed` omissions.
- **No format change before finalization** (DIST-086): writes use the old format and `since V` fields are gated until
  finalization; migrations (LANG-262 execution side) run at recovery (runtime) — non-monotone migrations wait for
  finalization (BENCH-223).
- **Rolling-upgrade orchestrator** (DIST-085): `blossom upgrade` driving the admin plane (drain, stop, replace, start,
  wait for health, advance cluster version, finalize, rollback before finalization).
- **Testing**: mixed-version simulation (TEST-100: each `SimNode` carries its own `CompiledRole` version, frames
  between versions go through real translation; `upgrade`/`finalize`/`rollback` as scheduler decisions); upgrade
  scenario generator (TEST-101); differential oracle across versions (TEST-102); migration crash points in
  `crashcheck` (TEST-103); golden storage fixtures per released version (TEST-106, `tests/fixtures/storage/v1`: every
  new binary must decode them or refuse with the documented error); idioms for version-gated features (LIB-123).
- **Corpus/integration**: BENCH-221 (rollback, and downgrade before finalization; downgrade after finalization is P2),
  BENCH-223, BENCH-225 (semantic incompatibility across versions, Hydro's sim_multi), BENCH-226 (a write before
  finalization is caught, CASSANDRA-15794), BENCH-233 (the version window is enforced), BENCH-234 (sweep of upgrade
  scenarios).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-wire blossom-store blossom-runtime blossom-sim
cargo test -p blossom-integration-tests --test upgrade_scenarios
cargo run -q -p xtask -- crashcheck --node --migrations
cargo run -q -p xtask -- corpus --check --filter 'upgrade/' --require-pass sim
```

#### M12.4 — CLI and operations polish: the REPL, self-check, trace viewer command, explain long forms, completion

- **Size:** ~3.5k
- **Depends on:** M11.5, M8.2, M9.8
- **Owns:** `crates/blossom-cli/src/cmd/repl.rs`, `crates/blossom-cli/src/cmd/self_check.rs`, `crates/blossom-cli/src/cmd/explain.rs`, `crates/blossom-cli/src/cmd/completions.rs`, `crates/blossom-cli/src/cmd/trace.rs`, `crates/blossom-cli/src/common/**`, `crates/blossom-driver/src/explain/**`, `tests/integration/tests/cli2_*.rs`, `docs/plan/notes/M12.4.md`
- **Features:** TEST-090
- **Consumes:**
  - admin plane
  - runtime
  - oracle
  - sim II viewer
- **Provides:**
  - `blossom repl` (local embedded node or attached through the admin plane)
  - `blossom self-check`
  - `blossom trace view`
  - complete `blossom explain` long forms
  - shell completion generation

**Build** (ARCHITECTURE §12.5; TEST-090).
- **REPL** (TEST-090): load a program, insert facts into inputs, step ticks (`:tick`, `:run`), query relations as of a
  tick, `:why`/`:whynot`, `:plan`, `:explain`; locally embedded or attached to a running node over the admin plane.
- **`blossom self-check`** (P1): run the embedded program's IR on the oracle against the executor for N ticks with
  generated inputs and report the first divergence.
- **`blossom trace view`**: open the static replay viewer for a `.blstrace`.
- **`blossom explain`**: every registered code has a long form with an example that the test suite compiles and
  checks produces that code.
- Shell completion generation (`blossom completions <shell>`), `--help` printing the exit-code table.

**Required tests.** integration `cli2_repl_script`, `cli2_self_check_passes_e01`, `cli2_explain_examples_produce_code`,
`cli2_completions`.

**Acceptance** (every command must pass from the repository root):

```sh
cargo clippy -p blossom-cli --all-targets -- -D warnings
cargo test -p blossom-integration-tests --test cli2_repl
```

#### M12.5 — User documentation: the Blossom guide, the language tutorial, the standard-library reference

- **Size:** ~4k lines of docs + generator
- **Depends on:** M10.7, M9.8, M8.1
- **Owns:** `docs/guide/**`, `docs/std/**`, `xtask/src/cmd/gen_docs.rs`, `README.md`, `docs/plan/notes/M12.5.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - LANGUAGE.md
  - examples E1–E10
  - std doc comments
  - the CLI
- **Provides:**
  - docs/guide (getting started, the tick model, tables and channels, handlers, lattices, choice and folds, distribution, specs and verification, LDFI, deployment)
  - docs/std (generated)
  - `cargo xtask gen-docs`

**Build.**
- `docs/guide/`: a user guide that walks E1–E10, every code block compiled by a doc test harness (`xtask gen-docs
  --check` extracts ```blossom blocks, compiles complete ones with `blossom check`, and fails on errors).
- `docs/std/`: generated from std module doc comments by `xtask gen-docs` (one page per area; interfaces,
  guarantees, fault models, examples).
- `README.md` at the root: what Blossom is, how to build, test and run an example, where the docs are.
- Use the `stop-slop` discipline for prose: concrete, no filler.

**Required checks.** `cargo xtask gen-docs --check` (all guide blocks compile; generated std docs up to date).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p xtask -- gen-docs --check
```

#### M12.6 — blossom-bench II: protocol throughput against DFIR, Blazes sealing vs ordering, latency tails

- **Size:** ~4k
- **Depends on:** M11.4, M10.2, M9.2, M11.5
- **Owns:** `crates/blossom-bench/**`, `docs/perf/**`, `xtask/src/cmd/bench_report.rs`, `docs/plan/notes/M12.6.md`
- **Features:** BENCH-202, BENCH-204
- **Consumes:**
  - bench harness (M11.4)
  - commit and paxos systems
  - rewrites
  - mTLS transport
- **Provides:**
  - protocol suites (Voting, 2PC with fsync, Paxos; base and rewritten; CompPaxos) vs DFIR and hand-written Rust on the same machine and transport; Blazes sealing vs ordering; open-loop latency with HDR histograms

**Build** (ARCHITECTURE §4.14 item 6; BENCH-202/204).
- DFIR (`dfir_rs`) versions of Voting, 2PC and Paxos from the SIGMOD'24 artifacts over our TCP/TLS transport; hand-
  written Rust floors; ours under interpreter and codegen; base and rewritten protocols; CompPaxos.
- Open-loop constant-rate load (wrk2-style) with coordinated-omission-corrected HDR histograms at 50/80/95% of peak;
  fsync-bound results (2PC) signed off on Linux only.
- BENCH-204: sealing beats ordering (~1.8× at 5 workers, ~3× at 20).
- Reports in `docs/perf/protocols.md` with machine details; gates only on dedicated machines (§4.14 gating rules).

**Required tests.** `protocol_bench_smoke` (each suite runs a tiny load in `cargo test`).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-bench
cargo run -q -p xtask -- bench-report --suite protocols-smoke
```

### M13 — Tide streaming, Raft rolling upgrades, release engineering and the operations documentation

**Goal.** Tide streaming on the lakehouse completes the modern Hadoop successor (ODD-19 M3); Raft supports rolling upgrades; fuzzing, Miri, mutation testing and reproducible signed releases are in place; operators get a complete manual.

**Gate.** `scripts/milestone-gate.sh M13` (PLAN §3).

#### M13.1 — Tide: watermarks and windows, weighted views, the durable input log, online aggregation and continuous jobs, completion by free termination

- **Size:** ~7.5k
- **Depends on:** M12.2, M11.2, M12.1, M8.6
- **Owns:** `systems/tide/**`, `docs/plan/notes/M13.1.md`
- **Features:** FLAG-129, FLAG-130, FLAG-131, FLAG-135, FLAG-137, FLAG-141, BENCH-185, BENCH-188
- **Consumes:**
  - BOOM-2
  - the lakehouse
  - HOP
  - Z-sets, wrappers, seals and finality natives
- **Provides:**
  - systems/tide (blossom-sys-tide)

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES §10.7 M3; R10 C2; R13 §9.6; R14 §7).
- **Tide streaming** (FLAG-129): watermark and frontier lattices with triggers on thresholds; AssignWindows and
  MergeWindows; accumulation modes including retraction; panes over insert-only streams POS-FINAL for thresholds and
  SEALED at the watermark; retracting panes provisional until their watermark seal; late data to a corrections stream
  or dropped — never silently changing a final pane.
- **Weighted collections and incremental views** (FLAG-130): Z-sets, DBSP-style views, shared arrangements; views
  replicated only through wrapped inputs.
- **Durable input log** (FLAG-131): a partitioned log replicated with Raft (reuse the lakehouse/ISR patterns).
- **Online aggregation over Tide** (FLAG-135, Tide part) and **continuous jobs in HOP mode** (FLAG-137: unbounded map
  sources, `flush`, reduce triggers, tumbling-discard compatibility, ring buffer reclaimed by ARM acks from all
  consumers, reducer state checkpointed with upstream cursors through the FLAG-132 barrier snapshots).
- **Job and stage completion by free termination** (FLAG-141): bounded sources seal after their last record; a stage
  is complete exactly when its outputs are final (ANA-121); halting only with no relay obligations.
- BENCH-185 streaming parts (session windows with late data under each accumulation mode; Nexmark Q1–Q8;
  exactly-once output after a crash mid-epoch), BENCH-188 (continuous windowed jobs).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p blossom-cli -- check --strict systems/tide/bls
cargo test -p blossom-sys-tide
```

#### M13.2 — Raft rolling upgrades (FLAG-016) and the rolling upgrade of a 3-node Raft KV (BENCH-220)

- **Size:** ~2.5k
- **Depends on:** M12.3, M10.1
- **Owns:** `systems/raft/**`, `std/consensus/**`, `tests/corpus/upgrade/BENCH-220*/**`, `docs/plan/notes/M13.2.md`
- **Features:** FLAG-016, BENCH-220
- **Consumes:**
  - the upgrade machinery (M12.3)
  - Raft II
- **Provides:**
  - Raft that survives a rolling upgrade with a schema change and a cluster-version-gated feature

**Rules for flagship-system WPs.**
- Layout (ARCHITECTURE §1.5): `systems/<name>/bls/*.bls` (the programs), `build.rs` calling `blossom_build::Builder`
  (generated executors, provenance variants for LDFI), `src/` (host glue: output handlers, blob I/O, services),
  `tests/` (simulation, LDFI, BMC and property tests), `deploy/*.toml` (example deployments).
- Every system also runs in the interpreter; each simulation suite runs under **both** executors and compares
  per-tick digests (interp ⇄ codegen).
- Reusable protocol cores live in `std/` when other systems import them (e.g. `std::consensus::raft`).
- Specs (`spec` items) state the safety properties; `check sim`, `check ldfi` and (where tractable) `check bmc`
  gate CI with `expect holds`.
- The system must build under `blossom check --strict` and within the codegen compile-time budget of ARCHITECTURE
  §10.3 (≤ 30 s release build of the generated crate on the build machine); record the measurement.

**Build** (FEATURES FLAG-016, BENCH-220; R15 (etcd/CockroachDB/hashicorp-raft rolling upgrades)).
- A v2 of the Raft KV with a new log-entry field (`since 2`), a translation for v1 peers, a cluster-version-gated
  feature, and a migration of durable state.
- BENCH-220: a rolling upgrade of a 3-node Raft KV under load loses no committed entry (mixed-version simulation with
  crashes and partitions during the upgrade, plus a real-process run with `blossom upgrade`); rollback before
  finalization restores v1 service.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-sys-raft upgrade
cargo run -q -p xtask -- corpus --check --filter 'upgrade/BENCH-220' --require-pass sim
```

#### M13.3 — Release engineering: fuzzing complete, Miri and mutation testing, cargo-deny, reproducible signed releases

- **Size:** ~3k
- **Depends on:** M12.3, M11.3, M8.1
- **Owns:** `fuzz/**`, `xtask/src/cmd/release.rs`, `.github/**`, `scripts/ci.d/90-nightly.sh`, `scripts/install-dev-tools.sh`, `deny.toml`, `docs/release/**`, `docs/plan/notes/M13.3.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - every fuzz target's library API
  - the full workspace
- **Provides:**
  - all fuzz targets implemented with corpora and a nightly runner
  - Miri job over kernel unsafe modules
  - cargo-mutants configuration for kernel, engine natives, wire and store
  - `cargo xtask release` (cargo-auditable builds, SBOM, reproducibility check, signed artifacts)
  - CI workflows mirroring scripts/ci.sh tiers

**Build** (ARCHITECTURE §11.8, §11.10, Appendix C.4 (production C-10)).
- Every fuzz target of §11.8 implemented (lexer+parser, formatter, front, wire decoder, WAL/checkpoint recovery on raw
  bytes and on SimFs crash images, trace reader, artifact decoder, SMT response parser, admission pipeline) with seed
  corpora from the golden corpus and fixtures; `scripts/ci.d/90-nightly.sh` runs each for a configurable time.
- Miri over `blossom-kernel` unsafe modules; cargo-mutants configuration and a baseline report for kernel, engine
  natives, wire and store (surviving mutants listed as issues in notes).
- `cargo deny check` green (licenses, advisories, bans).
- `cargo xtask release`: builds with `cargo auditable`, emits an SBOM, checks reproducibility (two builds, identical
  hashes), signs artifacts (minisign or Sigstore via a configured key; never a hard-coded key), writes release notes.
- `.github/workflows/*.yml` mirroring the `fast`, `gate` and nightly tiers on macOS arm64 and Linux x86_64.
- Extend `scripts/install-dev-tools.sh` with cargo-fuzz, cargo-mutants, cargo-auditable and an SBOM tool (all into
  `.tools/`).

**Required checks.** each fuzz target runs 60 s without findings; `cargo xtask release --dry-run` reproducible.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/install-dev-tools.sh
sh -c 'PATH=$PWD/.tools/bin:$PATH cargo deny check'
sh -c 'export PATH=$PWD/.tools/bin:$PATH; cd fuzz && for t in $(cargo +nightly fuzz list); do cargo +nightly fuzz run $t -- -max_total_time=60 || exit 1; done'
cargo +nightly miri test -p blossom-kernel --lib
cargo run -q -p xtask -- release --dry-run
```

#### M13.4 — Operations documentation: deployment, security, durability, upgrades, observability, runbooks, configuration reference

- **Size:** ~3k lines of docs
- **Depends on:** M12.3, M11.5, M9.8
- **Owns:** `docs/ops/**`, `docs/design/LANGUAGE-ERRATA.md`, `scripts/check-ops-docs.sh`, `docs/plan/notes/M13.4.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - runtime, store, upgrade, security features as built
  - notes files reporting LANGUAGE.md inconsistencies
- **Provides:**
  - docs/ops/*.md
  - docs/design/LANGUAGE-ERRATA.md (every LANGUAGE.md inconsistency found during implementation, with its resolution)

**Build.**
- `docs/ops/`: deployment (deploy.toml reference generated from `DeploymentSpec`, node identities, `deploy init`,
  `node init`), security (PKI, SPIFFE ids, ACLs, sessions, admin plane, secrets, audit log), durability (WAL,
  checkpoints, recovery refusals and what to do, `blossom store` tooling), upgrades (cluster versions, windows,
  orchestrator, rollback, finalization), observability (every metric with labels, health endpoints, tracing), fault
  policy (probation, breaker, quarantine), runbooks for each exit code of §12.5, the configuration reference
  (`blossom config explain` output documented key by key).
- `docs/design/LANGUAGE-ERRATA.md`: collect every LANGUAGE.md inconsistency reported in notes and BUGS.md with its
  resolution (LANGUAGE.md itself is only amended through DECISIONS.md).
- Every command example in `docs/ops` is executed by a doc test script (`scripts/check-ops-docs.sh`) against a
  scratch deployment.

**Acceptance** (every command must pass from the repository root):

```sh
scripts/check-ops-docs.sh
```

### M14 — Release readiness: the P0/P1 audit

**Goal.** Prove that every P0 and P1 feature is implemented and tested, every golden case passes on every applicable backend, the performance gates are measured, and no open bug blocks a release.

**Gate.** `scripts/milestone-gate.sh M14` (PLAN §3).

#### M14.1 — Release audit: coverage, corpus, performance sign-off, open bugs

- **Size:** ~1–2k (reports, small fixes)
- **Depends on:** M13.1, M13.2, M13.3, M13.4, M12.6, M11.6
- **Owns:** `docs/plan/**`, `docs/release/**`, `tests/corpus/**/manifest.toml`, `xtask/src/cmd/coverage.rs`, `xtask/src/cmd/corpus.rs`, `docs/plan/notes/M14.1.md`
- **Features:** — (infrastructure)
- **Consumes:**
  - everything
- **Provides:**
  - docs/release/v1-readiness.md: per FEATURES id (P0/P1) the implementing WP, code markers, tests and corpus status; the performance gate results; the list of open bugs and their disposition

**Build.**
- `cargo xtask coverage` shows every P0/P1 id with at least one `// FEATURE:` marker in code or std/systems sources and
  at least one passing test or corpus case; investigate and fix gaps (small fixes only; anything larger becomes a
  documented blocker with an owner proposal).
- Every corpus case passes on every backend it lists (`until` ≤ M14 for all P0/P1 cases); no `known-failure` remains
  for a P0/P1 id without an explicit, justified deferral recorded in DECISIONS.md.
- Performance: re-run BENCH-200 (both gates), BENCH-202, BENCH-203 and the tail/soak suites on the reference machine;
  record results; a missed gate is reported as missed, never rounded.
- Triage `docs/plan/BUGS.md`: every open bug fixed, deferred with justification, or marked release-blocking.
- Write `docs/release/v1-readiness.md`.
- Add the audit flags to the tools you own: `xtask coverage --require-markers --require-tests` (every P0/P1 id has a
  `// FEATURE:` marker and at least one passing test or corpus case) and `xtask corpus --require-all-pass-until Mk`
  (every P0/P1 case passes on every listed backend and no `until` exceeds Mk).

**Acceptance** (every command must pass from the repository root):

```sh
cargo run -q -p xtask -- coverage --require-markers --require-tests
cargo run -q -p xtask -- corpus --check --require-all-pass-until M14
scripts/ci.sh gate
```

### M15 — P2: editor support, compatibility frontends, language extensions, P2 engine/analysis/verification, P2 systems

**Goal.** Deliver the nice-to-have and beyond-the-literature items after the P0/P1 release.

**Gate.** `scripts/milestone-gate.sh M15` (PLAN §3).

#### M15.1 — Editor support: language server, tree-sitter grammar, incremental frontend

- **Size:** ~6k
- **Depends on:** M14.1
- **Owns:** `crates/blossom-lsp/**`, `editors/**`, `crates/blossom-cli/src/cmd/lsp.rs`, `docs/plan/notes/M15.1.md`
- **Features:** TEST-092
- **Consumes:**
  - driver, syntax, front (phases up to type checking), analysis
- **Provides:**
  - `blossom lsp`
  - tree-sitter grammar tested against the corpus
  - salsa-based incremental phases

**Build** (ARCHITECTURE §13.13, §13.2 tree-sitter note; TEST-092; LANGUAGE §13.7).
- `blossom-lsp` over lsp-server/lsp-types: diagnostics, go-to-definition, references, hover with types and
  monotonicity classes, the facts LANGUAGE §13.7 lists as semantic tokens (event vs standing, points of order,
  seeded sites), formatting, rename of relations and labels.
- Phases up to type checking wrapped in salsa queries keyed by file and item (reusing CST, AST, resolver and type
  checker unchanged); `blossom check` on flagship systems < 1 s.
- A tree-sitter grammar in `editors/tree-sitter-blossom` tested against examples and the corpus (node kinds from
  `blossom.ungram`).

**Acceptance** (every command must pass from the repository root):

```sh
scripts/wp-check.sh blossom-lsp
```

#### M15.2 — Compatibility frontends: Overlog/NDlog, Hydroflow datalog!, Bloom collection syntax, NDlog link literals

- **Size:** ~7k
- **Depends on:** M14.1
- **Owns:** `crates/blossom-syntax/src/overlog/**`, `crates/blossom-syntax/src/hydro/**`, `crates/blossom-syntax/src/bloom/**`, `crates/blossom-front/src/overlog/**`, `crates/blossom-front/src/hydro/**`, `crates/blossom-front/src/bloom/**`, `crates/blossom-front/src/lower/localize.rs`, `tests/corpus/frontends/**`, `docs/plan/notes/M15.2.md`
- **Features:** LANG-221, LANG-222, LANG-223, LANG-096
- **Consumes:**
  - IrBuilder
  - the Upsert both-deltas mode (SEM-052)
- **Provides:**
  - `.olg`, `.dl` (Hydro), `.rb`-style Bloom frontends lowering to IrBuilder

**Build** (ARCHITECTURE §14.3; LANGUAGE §21.2; R01, R03, R08).
- Overlog/NDlog frontend (LANG-221) with Overlog key-overwrite semantics through the Upsert both-deltas mode (SEM-052)
  and NDlog link literals (LANG-096) via `lower::localize`; Hydroflow `datalog!` frontend (LANG-222); Bloom
  collection-expression syntax (LANG-223). Each lowers into `IrBuilder` with its own validator-diagnostic rendering.
- BENCH-115-style Overlog programs run unchanged through the Overlog frontend and match the `.bls` ports.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-front --all-features overlog:: hydro:: bloom::
```

#### M15.3 — Language P2: host-backed collections, entanglement and time skipping, aggregate destinations, metaprogramming and hot install

- **Size:** ~8k
- **Depends on:** M14.1
- **Owns:** `crates/blossom-front/**`, `!crates/blossom-front/src/overlog/**`, `!crates/blossom-front/src/hydro/**`, `!crates/blossom-front/src/bloom/**`, `!crates/blossom-front/src/lower/localize.rs`, `crates/blossom-ir/**`, `crates/blossom-node/**`, `crates/blossom-kernel/**`, `tests/corpus/async/BENCH-081*/**`, `docs/plan/notes/M15.3.md`
- **Features:** LANG-054, LANG-072, LANG-107, LANG-203, SEM-011, SEM-094, BENCH-081
- **Consumes:**
  - the whole platform
- **Provides:**
  - RelStore providers for host-backed collections, `Term::TickOf` entanglement with time skipping, aggregate-chosen destinations, catalog metaprogramming and hot install

**Build** (ARCHITECTURE §14.3 rows; FEATURES items).
- LANG-054 host-backed collections (a `RelStore` provider trait in blossom-kernel, ENG-027 extension).
- LANG-072 entanglement: `Term::TickOf(var)` gated by an analyzer warning, excluded from certificates and VER-006;
  **SEM-011 time skipping** (a P1 item that only matters once entanglement makes tick values observable; PLAN §6):
  an idle node advances its tick counter by the skipped ticks; BENCH-081 (Lamport `p_wait`).
- LANG-107 aggregate-chosen destinations (lowering in the frontend; `lower::localize` itself belongs to M15.2).
- LANG-203 metaprogramming over the catalog and SEM-094 hot install (`blossom-node::hot_install` reusing migrations
  and ANA-100 checks).

**Acceptance** (every command must pass from the repository root):

```sh
cargo test --workspace
cargo run -q -p xtask -- corpus --check --filter 'async/BENCH-081'
```

#### M15.4 — Runtime, security and simulation P2: multi-program runtime, signed values, Biscuit tokens, encryption at rest, OnceTree, non-causal replay, Maelstrom/Jepsen, delegation, threshold trust, Byzantine faults, PBFT

- **Size:** ~7k
- **Depends on:** M14.1
- **Owns:** `std/crypto/**`, `crates/blossom-std-host/src/crypto.rs`, `std/oncetree/**`, `crates/blossom-std-host/src/oncetree.rs`, `crates/blossom-runtime/**`, `crates/blossom-store/**`, `crates/blossom-sim/**`, `systems/pbft/**`, `tests/corpus/lattices/BENCH-077*/**`, `docs/plan/notes/M15.4.md`
- **Features:** LANG-011, LANG-245, DIST-017, DIST-025, DIST-047, DIST-067, DIST-068, LIB-094, LIB-121, LIB-122, SEM-074, FLAG-152, BENCH-077
- **Consumes:**
  - runtime, store, sim
- **Provides:**
  - std::crypto (signed<T>), Biscuit bearer tokens on the client listener, encryption at rest in blossom-store::crypto, the OnceTree wrapper (W4) + std::oncetree, non-causal replay mode, Maelstrom/Jepsen adapters, delegation and k-of-n trust

**Build** (ARCHITECTURE §5.8, §14.3; R13 (OnceTree), R15 (Biscuit, SeNDlog/LBTrust)).
- LANG-245 `signed<T>`, DIST-067 Biscuit-style attenuable bearer tokens (the `token` client-auth key currently fails
  with `Unimplemented { feature: "DIST-067" }`), DIST-068 encryption at rest, LIB-121 delegation, LIB-122 threshold
  (k-of-n) trust.
- DIST-017 OnceTree transport (W4) as a `Wrapped` native kind + LIB-094 OnceTree aggregates + BENCH-077 (OnceTree
  counter).
- DIST-025 non-causal replay of the log (SEM-043's fourth network mode) in `blossom-sim::replay`; DIST-047
  Maelstrom/Jepsen adapters in `blossom-runtime::maelstrom`; LANG-011 multi-program runtime (`blossom-runtime::multi`).
- SEM-074 Byzantine faults as a `beyond_model` simulator fault class that no certificate assumes, and FLAG-152 PBFT
  (FEATURES marks it out of scope for v1) as a simulation/LDFI demonstrator under that fault class only
  (`systems/pbft`).

**Acceptance** (every command must pass from the repository root):

```sh
cargo test --workspace
```

#### M15.5 — Engine and analysis P2: eager mode, subsumption, magic sets, aggregate selections, exchange, bounded annotated fixpoints, in-tick greedy choice, P2 analyses

- **Size:** ~8k
- **Depends on:** M14.1
- **Owns:** `crates/blossom-engine/**`, `crates/blossom-plan/**`, `crates/blossom-rewrite/**`, `crates/blossom-analysis/**`, `crates/blossom-prov/**`, `tests/corpus/core/BENCH-025*/**`, `tests/corpus/async/BENCH-096*/**`, `docs/plan/notes/M15.5.md`
- **Features:** ENG-050, ENG-051, ENG-092, ENG-093, ENG-103, ENG-148, ANA-034, ANA-035, ANA-044, ANA-048, ANA-049, ANA-067, ANA-084, ANA-085, TEST-038, TEST-053, LANG-099, BENCH-025, BENCH-096
- **Consumes:**
  - engine, planner, rewrites, analyses, provenance
- **Provides:**
  - the P2 items of ARCHITECTURE §14.3 for the engine, rewrites, analyses and provenance

**Build** (ARCHITECTURE §14.3 rows).
- ENG-050 eager work-stealing mode, ENG-051 subsumption clauses, ENG-092 magic sets (`blossom-rewrite::magic`,
  BENCH-025 magic-sets shortest path), ENG-093 aggregate selections, ENG-103 key-sharded exchange, ENG-148 bounded
  annotated fixpoints.
- ANA-034 guess/guarantee taint, ANA-035 policy-aware negation, ANA-044 grey-box annotation files, ANA-048
  I-confluence checker (BENCH-096 I-confluence table pairs), ANA-049 complete-CALM interface check, ANA-067 lattice
  GC, ANA-084 replicate-by-consensus rewrite, ANA-085 profile-driven ILP placement.
- TEST-038 cross-run lineage merging and TEST-053 why-across-time provenance.
- LANG-099 in-tick recursive greedy choice (a planner choice fixpoint plus its native; BLS0503 until then).

**Acceptance** (every command must pass from the repository root):

```sh
cargo test --workspace
```

#### M15.6 — Verification P2: SAT-based BMC, invariant inference, sync-then-lift, exporters, Katara, Lean mechanization, liveness

- **Size:** ~8k
- **Depends on:** M14.1
- **Owns:** `crates/blossom-verify/**`, `verify-lean/**`, `tests/corpus/lattices/BENCH-065*/**`, `tests/corpus/verify/BENCH-151*/**`, `docs/plan/notes/M15.6.md`
- **Features:** VER-004, VER-012, VER-013, VER-017, VER-018, VER-019, VER-021, VER-025, VER-044, BENCH-065, BENCH-151
- **Consumes:**
  - verify I/II
  - blossom-sat
  - blossom-smt
- **Provides:**
  - blossom_verify::{bmc_sat, infer, sync_lift, export, katara}
  - an external Lean project for the semantics and algebra proofs

**Build** (ARCHITECTURE §9.5 P2 list).
- VER-004 SAT-based BMC; VER-012 DuoAI-style invariant inference; VER-013 prove synchronously then lift; VER-017
  TLA+/Ivy/Lean exporters; VER-018 refinement of a sequential spec and CRDT synthesis (Katara; BENCH-065 2P-set agrees
  with a sequential set); VER-019/VER-044 mechanized Dedalus^L semantics in Lean (`verify-lean/`); VER-021 unbounded
  liveness; VER-025 mechanized algebra proofs; BENCH-151 inference and BMC stretch goals.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test -p blossom-verify
```

#### M15.7 — P2 systems and library: stateful functions on the KVS, HDFS shim, scale-to-zero executors, progress tracking, SQL subset, early finalization, chain replication, examples and pipelines

- **Size:** ~9k (may be split when scheduled)
- **Depends on:** M14.1
- **Owns:** `systems/kvs/**`, `systems/boomfs/**`, `systems/boom2/**`, `systems/tide/**`, `systems/lakehouse/**`, `systems/chainrep/**`, `std/examples/**`, `std/pipeline/**`, `crates/blossom-std-host/src/pipeline.rs`, `tests/corpus/net/BENCH-116*/**`, `crates/blossom-bench/**`, `docs/plan/notes/M15.7.md`
- **Features:** FLAG-063, FLAG-087, FLAG-128, FLAG-133, FLAG-134, FLAG-138, FLAG-151, LIB-106, LIB-108, BENCH-116, BENCH-205
- **Consumes:**
  - the flagship systems
  - std
- **Provides:**
  - the P2 flagship and library items

**Build** (ARCHITECTURE §14.3 rows; FEATURES §9.6, §10).
- FLAG-063 stateful functions on the KVS (Cloudburst style); FLAG-087 permissions and an HDFS-compatible shim;
  FLAG-128 stateless executors that scale to zero; FLAG-133 progress tracking (pointstamps/frontiers, written in the
  language and verified); FLAG-134 SQL frontend subset (TPC-H, TPC-DS, Nexmark); FLAG-138 bound-based early
  finalization.
- FLAG-151 chain replication, primary/backup (Elastic) and Flux as LDFI showcases (`systems/chainrep`).
- LIB-106 other examples, LIB-108 a Chukwa-style log and metric collection pipeline (`std::pipeline`); BENCH-116 Chord
  at scale; BENCH-205 data-plane benchmark.
This WP is large; the scheduler may split it by system without changing ownership rules.

**Acceptance** (every command must pass from the repository root):

```sh
cargo test --workspace
```

## 9. FEATURES id coverage

Every P0 and P1 id of FEATURES.md is assigned to at least one WP; the first WP listed is the primary implementer. Ranges cover every id that exists between their ends. BENCH ids are assigned to the WP that authors the case; the milestone in which a case must pass on each backend is the case's `until` (PLAN §5.4). `xtask coverage` (M5.2) regenerates this table from plan.json and fails on an unassigned id.

### LANG

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| LANG-001 | P0 | M2.2 | M2 |
| LANG-002 | P0 | M2.3 | M2 |
| LANG-003–004 | P0 | M3.5 | M3 |
| LANG-005–010 | P1 | M3.5 | M3 |
| LANG-011 | P2 | M15.4 | M15 |
| LANG-020–021 | P0 | M4.5 | M4 |
| LANG-022–023 | P0 | M2.1 | M2 |
| LANG-024 | P0 | M1.1 | M1 |
| LANG-025 | P1 | M4.5 | M4 |
| LANG-026 | P1 | M2.1 | M2 |
| LANG-027 | P1 | M5.3 | M5 |
| LANG-028 | P1 | M9.8 | M9 |
| LANG-040–043 | P0 | M5.3 | M5 |
| LANG-044 | P0 | M5.4 | M5 |
| LANG-045 | P0 | M5.3 | M5 |
| LANG-046–047 | P1 | M5.3 | M5 |
| LANG-048–050 | P1 | M6.4 | M6 |
| LANG-051 | P1 | M9.8 | M9 |
| LANG-052 | P1 | M5.5 | M5 |
| LANG-053 | P1 | M6.2 | M6 |
| LANG-054 | P2 | M15.3 | M15 |
| LANG-060–064 | P0 | M5.3 | M5 |
| LANG-065 | P0 | M3.6 | M3 |
| LANG-066 | P0 | M4.5 | M4 |
| LANG-067 | P0 | M5.5 | M5 |
| LANG-068 | P0 | M5.3 | M5 |
| LANG-069 | P1 | M3.6 | M3 |
| LANG-070 | P1 | M6.5 | M6 |
| LANG-071 | P1 | M6.4 | M6 |
| LANG-072 | P2 | M15.3 | M15 |
| LANG-080–083 | P0 | M5.3 | M5 |
| LANG-084 | P0 | M2.3 | M2 |
| LANG-085–086 | P0 | M5.3 | M5 |
| LANG-087–094 | P1 | M5.3 | M5 |
| LANG-095 | P1 | M6.4 | M6 |
| LANG-096 | P2 | M15.2 | M15 |
| LANG-097 | P1 | M5.3 | M5 |
| LANG-098 | P1 | M6.4 | M6 |
| LANG-099 | P2 | M15.5 | M15 |
| LANG-100 | P0 | M5.3 | M5 |
| LANG-101 | P0 | M4.5 | M4 |
| LANG-102–104 | P1 | M5.3 | M5 |
| LANG-105–106 | P1 | M6.4 | M6 |
| LANG-107 | P2 | M15.3 | M15 |
| LANG-108 | P0 | M5.3 | M5 |
| LANG-109 | P1 | M5.3 | M5 |
| LANG-110 | P0 | M5.3 | M5 |
| LANG-111 | P1 | M5.3 | M5 |
| LANG-112–113 | P1 | M6.4 | M6 |
| LANG-114–115 | P1 | M5.3 | M5 |
| LANG-116–117 | P1 | M6.4 | M6 |
| LANG-118 | P0 | M4.1 | M4 |
| LANG-120–121 | P0 | M4.5 | M4 |
| LANG-122–123 | P0 | M5.3 | M5 |
| LANG-124–126 | P0 | M3.1 | M3 |
| LANG-127 | P0 | M4.5 | M4 |
| LANG-128 | P0 | M5.3 | M5 |
| LANG-129–131 | P0 | M3.1 | M3 |
| LANG-132–134 | P1 | M4.6 | M4 |
| LANG-135 | P1 | M4.6, M6.4 | M4, M6 |
| LANG-136 | P1 | M4.6 | M4 |
| LANG-137 | P1 | M4.4 | M4 |
| LANG-138–139 | P1 | M6.4 | M6 |
| LANG-142 | P1 | M4.6 | M4 |
| LANG-150 | P0 | M5.3 | M5 |
| LANG-151 | P0 | M4.2 | M4 |
| LANG-152 | P0 | M5.3 | M5 |
| LANG-153–154 | P1 | M6.4 | M6 |
| LANG-155 | P1 | M7.2 | M7 |
| LANG-158 | P1 | M6.4 | M6 |
| LANG-170 | P0 | M6.1 | M6 |
| LANG-171–172 | P0 | M5.5 | M5 |
| LANG-173 | P0 | M5.3 | M5 |
| LANG-174–175 | P0 | M2.1 | M2 |
| LANG-180 | P0 | M4.1 | M4 |
| LANG-181 | P0 | M5.3 | M5 |
| LANG-182 | P1 | M4.5 | M4 |
| LANG-183 | P1 | M6.4 | M6 |
| LANG-184 | P1 | M6.4, M9.8 | M6, M9 |
| LANG-185 | P0 | M7.4 | M7 |
| LANG-186 | P1 | M9.8 | M9 |
| LANG-190–200 | P0 | M5.3 | M5 |
| LANG-201 | P0 | M6.5 | M6 |
| LANG-202 | P1 | M6.4 | M6 |
| LANG-203 | P2 | M15.3 | M15 |
| LANG-204 | P1 | M4.5 | M4 |
| LANG-205 | P1 | M9.5 | M9 |
| LANG-206 | P1 | M5.5 | M5 |
| LANG-207 | P1 | M6.4 | M6 |
| LANG-208 | P0 | M2.3 | M2 |
| LANG-212 | P1 | M6.4 | M6 |
| LANG-220 | P1 | M3.6 | M3 |
| LANG-221–223 | P2 | M15.2 | M15 |
| LANG-240 | P0 | M5.3 | M5 |
| LANG-241 | P0 | M5.5 | M5 |
| LANG-242 | P0 | M4.2 | M4 |
| LANG-243 | P1 | M11.5 | M11 |
| LANG-244 | P1 | M9.7 | M9 |
| LANG-245 | P2 | M15.4 | M15 |
| LANG-260 | P0 | M6.5 | M6 |
| LANG-261 | P0 | M3.4 | M3 |
| LANG-262 | P0 | M6.5, M12.3 | M6, M12 |
| LANG-263–264 | P1 | M12.3 | M12 |
| LANG-265 | P1 | M6.5 | M6 |
| LANG-280 | P0 | M5.3 | M5 |
| LANG-281 | P0 | M3.1 | M3 |
| LANG-282–284 | P1 | M4.6 | M4 |

### SEM

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| SEM-001 | P0 | M5.5 | M5 |
| SEM-002 | P0 | M6.1 | M6 |
| SEM-003–008 | P0 | M4.1 | M4 |
| SEM-009 | P0 | M5.5 | M5 |
| SEM-010 | P0 | M8.2 | M8 |
| SEM-011 | P1 | M15.3 | M15 |
| SEM-012 | P0 | M6.1 | M6 |
| SEM-013 | P0 | M4.1 | M4 |
| SEM-016–017 | P1 | M6.6 | M6 |
| SEM-020–022 | P0 | M4.2 | M4 |
| SEM-023 | P1 | M4.1 | M4 |
| SEM-030–032 | P0 | M4.1 | M4 |
| SEM-033 | P0 | M3.1 | M3 |
| SEM-034 | P0 | M4.1 | M4 |
| SEM-036 | P1 | M8.6 | M8 |
| SEM-040–043 | P0 | M7.2 | M7 |
| SEM-044 | P0 | M8.2 | M8 |
| SEM-045 | P1 | M10.4 | M10 |
| SEM-050 | P0 | M6.1 | M6 |
| SEM-051 | P0 | M7.1 | M7 |
| SEM-052 | P1 | M8.6 | M8 |
| SEM-060–061 | P1 | M6.4 | M6 |
| SEM-070 | P0 | M7.2 | M7 |
| SEM-071 | P0 | M5.4 | M5 |
| SEM-072 | P0 | M5.5 | M5 |
| SEM-073 | P1 | M7.2 | M7 |
| SEM-074 | P2 | M15.4 | M15 |
| SEM-080 | P0 | M4.1 | M4 |
| SEM-081 | P0 | M6.1 | M6 |
| SEM-082 | P0 | M8.2 | M8 |
| SEM-083 | P0 | M4.1 | M4 |
| SEM-084 | P0 | M2.1 | M2 |
| SEM-085 | P0 | M4.1 | M4 |
| SEM-086 | P0 | M4.2 | M4 |
| SEM-087 | P1 | M5.6 | M5 |
| SEM-088 | P0 | M3.3 | M3 |
| SEM-090 | P0 | M5.5 | M5 |
| SEM-091 | P0 | M5.1 | M5 |
| SEM-092–093 | P1 | M12.3 | M12 |
| SEM-094 | P2 | M15.3 | M15 |
| SEM-100 | P0 | M2.2 | M2 |
| SEM-101 | P0 | M4.1 | M4 |
| SEM-102 | P0 | M4.2 | M4 |
| SEM-103–104 | P0 | M4.1 | M4 |
| SEM-105 | P0 | M6.1 | M6 |
| SEM-106 | P0 | M10.4 | M10 |
| SEM-107 | P0 | M8.2 | M8 |
| SEM-108 | P1 | M10.4 | M10 |
| SEM-109 | P1 | M6.6 | M6 |

### ENG

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| ENG-001 | P0 | M2.2, M3.2 | M2, M3 |
| ENG-002 | P0 | M5.1 | M5 |
| ENG-003 | P0 | M6.1 | M6 |
| ENG-004 | P0 | M5.1 | M5 |
| ENG-005 | P1 | M8.5 | M8 |
| ENG-006–007 | P1 | M6.2 | M6 |
| ENG-020–022 | P0 | M3.3 | M3 |
| ENG-023 | P0 | M5.1 | M5 |
| ENG-024–026 | P1 | M4.3 | M4 |
| ENG-027–028 | P1 | M10.6 | M10 |
| ENG-029 | P0 | M3.3 | M3 |
| ENG-030 | P1 | M3.3 | M3 |
| ENG-031 | P1 | M3.1 | M3 |
| ENG-032 | P0 | M2.1 | M2 |
| ENG-040–041 | P0 | M5.1 | M5 |
| ENG-042–043 | P0 | M6.1 | M6 |
| ENG-044 | P0 | M3.1 | M3 |
| ENG-045 | P1 | M3.1 | M3 |
| ENG-046 | P1 | M8.6 | M8 |
| ENG-047 | P0 | M6.1 | M6 |
| ENG-048 | P1 | M8.6 | M8 |
| ENG-049 | P1 | M6.1 | M6 |
| ENG-050–051 | P2 | M15.5 | M15 |
| ENG-060 | P0 | M5.1 | M5 |
| ENG-061–062 | P0 | M6.1 | M6 |
| ENG-063–065 | P1 | M11.6 | M11 |
| ENG-066 | P1 | M10.6 | M10 |
| ENG-067 | P0 | M4.1 | M4 |
| ENG-068 | P0 | M7.1 | M7 |
| ENG-069 | P0 | M5.1 | M5 |
| ENG-070–071 | P1 | M8.6 | M8 |
| ENG-072 | P1 | M10.6 | M10 |
| ENG-073 | P1 | M8.6 | M8 |
| ENG-074 | P0 | M5.1 | M5 |
| ENG-075 | P1 | M8.6 | M8 |
| ENG-080–082 | P0 | M5.1 | M5 |
| ENG-083 | P0 | M6.2 | M6 |
| ENG-084 | P1 | M10.6 | M10 |
| ENG-085 | P1 | M4.3 | M4 |
| ENG-086–090 | P1 | M10.6 | M10 |
| ENG-091 | P1 | M4.3 | M4 |
| ENG-092–093 | P2 | M15.5 | M15 |
| ENG-100 | P0 | M6.1 | M6 |
| ENG-101 | P0 | M7.2 | M7 |
| ENG-102 | P1 | M11.6 | M11 |
| ENG-103 | P2 | M15.5 | M15 |
| ENG-110 | P0 | M7.1 | M7 |
| ENG-111 | P0 | M5.7 | M5 |
| ENG-112 | P0 | M4.3, M7.1 | M4, M7 |
| ENG-113 | P1 | M7.1 | M7 |
| ENG-114 | P1 | M7.6 | M7 |
| ENG-115–116 | P1 | M7.1 | M7 |
| ENG-120 | P0 | M4.3 | M4 |
| ENG-121 | P1 | M4.3 | M4 |
| ENG-140 | P0 | M6.1 | M6 |
| ENG-141 | P1 | M8.6 | M8 |
| ENG-142 | P1 | M6.6 | M6 |
| ENG-145–147 | P1 | M7.6 | M7 |
| ENG-148 | P2 | M15.5 | M15 |

### DIST

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| DIST-001–002 | P0 | M5.5 | M5 |
| DIST-003 | P0 | M4.4 | M4 |
| DIST-004 | P0 | M7.4 | M7 |
| DIST-005 | P1 | M6.1 | M6 |
| DIST-006 | P1 | M11.3 | M11 |
| DIST-007 | P1 | M7.5 | M7 |
| DIST-008–009 | P1 | M11.3 | M11 |
| DIST-010 | P1 | M4.4 | M4 |
| DIST-011–012 | P1 | M11.3 | M11 |
| DIST-015–016 | P1 | M8.6 | M8 |
| DIST-017 | P2 | M15.4 | M15 |
| DIST-020 | P0 | M2.6 | M2 |
| DIST-021 | P0 | M5.4 | M5 |
| DIST-022 | P1 | M5.4 | M5 |
| DIST-023 | P1 | M8.6 | M8 |
| DIST-024 | P1 | M11.6 | M11 |
| DIST-025 | P2 | M15.4 | M15 |
| DIST-030–031 | P0 | M5.5 | M5 |
| DIST-032 | P0 | M2.1 | M2 |
| DIST-033 | P0 | M5.4 | M5 |
| DIST-040 | P0 | M5.5, M9.8 | M5, M9 |
| DIST-041 | P0 | M7.4 | M7 |
| DIST-042 | P1 | M10.1 | M10 |
| DIST-043 | P1 | M7.4 | M7 |
| DIST-044 | P1 | M8.7 | M8 |
| DIST-045 | P1 | M7.5 | M7 |
| DIST-046 | P1 | M8.3 | M8 |
| DIST-047 | P2 | M15.4 | M15 |
| DIST-060–061 | P0 | M8.7 | M8 |
| DIST-062 | P0 | M5.5 | M5 |
| DIST-063 | P0 | M8.7 | M8 |
| DIST-064–066 | P1 | M11.5 | M11 |
| DIST-067–068 | P2 | M15.4 | M15 |
| DIST-069 | P1 | M11.5 | M11 |
| DIST-080 | P0 | M4.4 | M4 |
| DIST-081 | P0 | M5.4 | M5 |
| DIST-082 | P0 | M7.4 | M7 |
| DIST-083–087 | P1 | M12.3 | M12 |

### ANA

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| ANA-001–002 | P0 | M4.2 | M4 |
| ANA-003 | P1 | M6.6 | M6 |
| ANA-004 | P0 | M4.2 | M4 |
| ANA-005 | P0 | M4.5 | M4 |
| ANA-006–009 | P1 | M6.6 | M6 |
| ANA-010 | P1 | M6.5 | M6 |
| ANA-011 | P1 | M6.6 | M6 |
| ANA-015 | P1 | M5.6 | M5 |
| ANA-020 | P0 | M4.2 | M4 |
| ANA-021 | P1 | M5.6 | M5 |
| ANA-022–024 | P0 | M4.2 | M4 |
| ANA-025–033 | P1 | M5.6 | M5 |
| ANA-034–035 | P2 | M15.5 | M15 |
| ANA-036–039 | P1 | M5.6 | M5 |
| ANA-040–043 | P1 | M6.6 | M6 |
| ANA-044 | P2 | M15.5 | M15 |
| ANA-045 | P1 | M6.6 | M6 |
| ANA-046–047 | P1 | M7.5 | M7 |
| ANA-048–049 | P2 | M15.5 | M15 |
| ANA-060 | P1 | M6.6 | M6 |
| ANA-061–064 | P1 | M7.5 | M7 |
| ANA-065 | P1 | M6.6 | M6 |
| ANA-066 | P0 | M7.5 | M7 |
| ANA-067 | P2 | M15.5 | M15 |
| ANA-080 | P1 | M5.6 | M5 |
| ANA-081–083 | P1 | M7.5 | M7 |
| ANA-084–085 | P2 | M15.5 | M15 |
| ANA-100 | P0 | M4.2 | M4 |
| ANA-101–104 | P1 | M6.6 | M6 |
| ANA-105 | P0 | M4.2 | M4 |
| ANA-106–120 | P1 | M6.6 | M6 |
| ANA-121 | P1 | M6.4 | M6 |
| ANA-122–140 | P1 | M6.6 | M6 |
| ANA-141–143 | P1 | M5.6 | M5 |

### TEST

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| TEST-001–002 | P0 | M7.2 | M7 |
| TEST-003 | P0 | M4.2 | M4 |
| TEST-004–005 | P1 | M8.2 | M8 |
| TEST-006 | P0 | M7.2 | M7 |
| TEST-007–009 | P1 | M8.2 | M8 |
| TEST-010 | P0 | M4.7 | M4 |
| TEST-011 | P0 | M7.2 | M7 |
| TEST-012 | P1 | M8.2 | M8 |
| TEST-013 | P0 | M4.1 | M4 |
| TEST-014–015 | P1 | M10.8 | M10 |
| TEST-020 | P0 | M8.1 | M8 |
| TEST-021 | P0 | M7.2 | M7 |
| TEST-022 | P0 | M8.1 | M8 |
| TEST-023 | P0 | M7.1 | M7 |
| TEST-024–029 | P0 | M8.1 | M8 |
| TEST-030–037 | P1 | M9.4 | M9 |
| TEST-038 | P2 | M15.5 | M15 |
| TEST-039–040 | P1 | M9.4 | M9 |
| TEST-050 | P0 | M5.7 | M5 |
| TEST-051 | P1 | M5.7 | M5 |
| TEST-052 | P1 | M7.6 | M7 |
| TEST-053 | P2 | M15.5 | M15 |
| TEST-060 | P0 | M8.2 | M8 |
| TEST-061 | P0 | M5.7 | M5 |
| TEST-062 | P1 | M9.4 | M9 |
| TEST-063 | P1 | M6.2 | M6 |
| TEST-064 | P1 | M8.2 | M8 |
| TEST-065–066 | P1 | M7.1 | M7 |
| TEST-080 | P0 | M6.5 | M6 |
| TEST-081 | P0 | M8.2 | M8 |
| TEST-082 | P1 | M10.8 | M10 |
| TEST-083 | P0 | M3.1 | M3 |
| TEST-084 | P1 | M10.8 | M10 |
| TEST-087 | P1 | M9.5 | M9 |
| TEST-088 | P1 | M8.2 | M8 |
| TEST-090 | P1 | M12.4 | M12 |
| TEST-091 | P0 | M6.3 | M6 |
| TEST-092 | P2 | M15.1 | M15 |
| TEST-100–103 | P1 | M12.3 | M12 |
| TEST-104 | P1 | M8.2 | M8 |
| TEST-105 | P1 | M9.4 | M9 |
| TEST-106 | P1 | M12.3 | M12 |
| TEST-107 | P1 | M8.7 | M8 |
| TEST-108 | P0 | M6.5 | M6 |
| TEST-140–142 | P0 | M8.1 | M8 |
| TEST-143 | P1 | M9.4 | M9 |
| TEST-145 | P1 | M7.6 | M7 |

### VER

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| VER-001 | P0 | M6.5 | M6 |
| VER-002 | P1 | M9.5 | M9 |
| VER-003 | P1 | M10.4 | M10 |
| VER-004 | P2 | M15.6 | M15 |
| VER-005 | P1 | M9.5 | M9 |
| VER-006–011 | P1 | M10.4 | M10 |
| VER-012–013 | P2 | M15.6 | M15 |
| VER-014–016 | P1 | M9.5 | M9 |
| VER-017–019 | P2 | M15.6 | M15 |
| VER-020 | P1 | M9.5 | M9 |
| VER-021–025 | P2 | M15.6 | M15 |
| VER-040–043 | P1 | M10.4 | M10 |
| VER-044 | P2 | M15.6 | M15 |

### LIB

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| LIB-001–003 | P0 | M8.3 | M8 |
| LIB-004–006 | P1 | M9.6 | M9 |
| LIB-007 | P0 | M8.3 | M8 |
| LIB-008–009 | P1 | M9.6 | M9 |
| LIB-020–022 | P0 | M8.3 | M8 |
| LIB-023 | P1 | M10.1 | M10 |
| LIB-024–025 | P1 | M9.6 | M9 |
| LIB-040–041 | P0 | M8.3 | M8 |
| LIB-042–043 | P1 | M9.6 | M9 |
| LIB-044–045 | P0 | M8.3 | M8 |
| LIB-060–081 | P0 | M8.4 | M8 |
| LIB-082–085 | P1 | M9.7 | M9 |
| LIB-086 | P0 | M8.4 | M8 |
| LIB-087–093 | P1 | M9.7 | M9 |
| LIB-094 | P2 | M15.4 | M15 |
| LIB-100–105 | P1 | M10.7 | M10 |
| LIB-106 | P2 | M15.7 | M15 |
| LIB-107 | P1 | M10.7 | M10 |
| LIB-108 | P2 | M15.7 | M15 |
| LIB-120 | P1 | M9.7 | M9 |
| LIB-121–122 | P2 | M15.4 | M15 |
| LIB-123 | P1 | M12.3 | M12 |

### FLAG

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| FLAG-001–008 | P0 | M9.1 | M9 |
| FLAG-009–015 | P1 | M10.1 | M10 |
| FLAG-016 | P1 | M13.2 | M13 |
| FLAG-020–024 | P0 | M9.2 | M9 |
| FLAG-025–027 | P1 | M9.2 | M9 |
| FLAG-028 | P2 | M9.2 | M9 |
| FLAG-040 | P0 | M10.2 | M10 |
| FLAG-041–042 | P1 | M10.2 | M10 |
| FLAG-060 | P0 | M9.3 | M9 |
| FLAG-061–062 | P1 | M9.3 | M9 |
| FLAG-063 | P2 | M15.7 | M15 |
| FLAG-080–083 | P0 | M10.3 | M10 |
| FLAG-084–086 | P1 | M10.3 | M10 |
| FLAG-087 | P2 | M15.7 | M15 |
| FLAG-100–103 | P0 | M11.1 | M11 |
| FLAG-104 | P1 | M11.1 | M11 |
| FLAG-105–106 | P1 | M12.1 | M12 |
| FLAG-107 | P0 | M12.1 | M12 |
| FLAG-108–112 | P1 | M12.1 | M12 |
| FLAG-120–124 | P1 | M12.2 | M12 |
| FLAG-125–127 | P1 | M11.2 | M11 |
| FLAG-128 | P2 | M15.7 | M15 |
| FLAG-129–131 | P1 | M13.1 | M13 |
| FLAG-132 | P1 | M12.2 | M12 |
| FLAG-133–134 | P2 | M15.7 | M15 |
| FLAG-135 | P1 | M12.1, M13.1 | M12, M13 |
| FLAG-136 | P1 | M12.1 | M12 |
| FLAG-137 | P1 | M13.1 | M13 |
| FLAG-138 | P2 | M15.7 | M15 |
| FLAG-141 | P1 | M13.1 | M13 |
| FLAG-150 | P1 | M10.5 | M10 |
| FLAG-151 | P2 | M15.7 | M15 |
| FLAG-152 | P2 | M15.4 | M15 |

### BENCH

| Ids | Priority | WP (primary first) | Milestone |
|---|---|---|---|
| BENCH-000 | P0 | M5.2 | M5 |
| BENCH-001–004 | P0 | M1.2 | M1 |
| BENCH-005 | P1 | M1.2 | M1 |
| BENCH-006–008 | P0 | M1.2 | M1 |
| BENCH-009 | P1 | M1.2 | M1 |
| BENCH-010–016 | P0 | M1.2 | M1 |
| BENCH-017 | P1 | M1.2 | M1 |
| BENCH-018–022 | P0 | M1.2 | M1 |
| BENCH-023–024 | P1 | M1.2 | M1 |
| BENCH-025 | P2 | M15.5 | M15 |
| BENCH-026–028 | P1 | M1.2 | M1 |
| BENCH-029–032 | P0 | M1.2 | M1 |
| BENCH-033–034 | P1 | M1.2 | M1 |
| BENCH-035–036 | P0 | M1.2 | M1 |
| BENCH-037–039 | P1 | M1.2 | M1 |
| BENCH-040–045 | P0 | M1.2 | M1 |
| BENCH-046 | P1 | M1.2 | M1 |
| BENCH-047 | P0 | M1.2 | M1 |
| BENCH-048 | P1 | M1.2 | M1 |
| BENCH-049 | P0 | M7.3 | M7 |
| BENCH-050–053 | P0 | M1.3 | M1 |
| BENCH-054–056 | P1 | M1.3 | M1 |
| BENCH-057 | P0 | M1.3 | M1 |
| BENCH-058–061 | P1 | M1.3 | M1 |
| BENCH-062 | P0 | M1.3 | M1 |
| BENCH-063–064 | P1 | M1.3 | M1 |
| BENCH-065 | P2 | M15.6 | M15 |
| BENCH-066–067 | P1 | M1.3 | M1 |
| BENCH-068 | P0 | M1.3 | M1 |
| BENCH-069 | P1 | M1.3 | M1 |
| BENCH-070–073 | P0 | M1.3 | M1 |
| BENCH-074–076 | P1 | M1.3 | M1 |
| BENCH-077 | P2 | M15.4 | M15 |
| BENCH-078–079 | P1 | M1.3 | M1 |
| BENCH-080 | P1 | M1.4 | M1 |
| BENCH-081 | P2 | M15.3 | M15 |
| BENCH-082–083 | P0 | M1.4 | M1 |
| BENCH-084–091 | P1 | M1.4 | M1 |
| BENCH-092–093 | P0 | M1.4 | M1 |
| BENCH-094–095 | P1 | M1.4 | M1 |
| BENCH-096 | P2 | M15.5 | M15 |
| BENCH-097 | P0 | M1.4 | M1 |
| BENCH-098–099 | P1 | M1.4 | M1 |
| BENCH-100–102 | P0 | M1.4 | M1 |
| BENCH-103 | P1 | M11.3 | M11 |
| BENCH-104–107 | P0 | M8.3 | M8 |
| BENCH-108–111 | P0 | M8.4 | M8 |
| BENCH-112 | P1 | M9.7 | M9 |
| BENCH-113–115 | P1 | M10.7 | M10 |
| BENCH-116 | P2 | M15.7 | M15 |
| BENCH-130–132 | P0 | M1.5 | M1 |
| BENCH-133–134 | P1 | M1.5 | M1 |
| BENCH-135 | P0 | M1.5 | M1 |
| BENCH-136–137 | P1 | M1.5 | M1 |
| BENCH-150 | P1 | M1.4 | M1 |
| BENCH-151 | P2 | M15.6 | M15 |
| BENCH-170–171 | P0 | M9.1 | M9 |
| BENCH-172 | P1 | M10.1 | M10 |
| BENCH-173 | P0 | M9.1 | M9 |
| BENCH-174–175 | P1 | M10.1 | M10 |
| BENCH-176 | P0 | M9.2 | M9 |
| BENCH-177 | P1 | M9.2 | M9 |
| BENCH-178 | P1 | M10.2 | M10 |
| BENCH-179 | P1 | M10.7 | M10 |
| BENCH-180–182 | P1 | M10.3 | M10 |
| BENCH-183 | P1 | M11.1 | M11 |
| BENCH-184 | P1 | M9.2 | M9 |
| BENCH-185 | P1 | M11.2, M12.2, M13.1 | M11, M12, M13 |
| BENCH-186–187 | P1 | M12.1 | M12 |
| BENCH-188 | P1 | M13.1 | M13 |
| BENCH-189–190 | P1 | M12.1 | M12 |
| BENCH-191 | P1 | M11.3 | M11 |
| BENCH-192 | P1 | M12.1 | M12 |
| BENCH-200 | P0 | M11.4 | M11 |
| BENCH-201 | P1 | M11.4 | M11 |
| BENCH-202 | P0 | M12.6 | M12 |
| BENCH-203 | P1 | M11.4 | M11 |
| BENCH-204 | P1 | M12.6 | M12 |
| BENCH-205 | P2 | M15.7 | M15 |
| BENCH-220 | P1 | M13.2 | M13 |
| BENCH-221 | P1 | M12.3 | M12 |
| BENCH-222 | P0 | M7.4 | M7 |
| BENCH-223 | P1 | M12.3 | M12 |
| BENCH-224 | P0 | M6.5 | M6 |
| BENCH-225–226 | P1 | M12.3 | M12 |
| BENCH-227–229 | P0 | M8.7 | M8 |
| BENCH-230–231 | P1 | M11.5 | M11 |
| BENCH-232 | P1 | M9.4 | M9 |
| BENCH-233–234 | P1 | M12.3 | M12 |
| BENCH-235 | P1 | M11.5 | M11 |
| BENCH-300–306 | P0 | M1.3 | M1 |
| BENCH-307 | P1 | M1.3 | M1 |
| BENCH-308 | P0 | M1.3 | M1 |
| BENCH-309–312 | P1 | M1.3 | M1 |
| BENCH-313 | P0 | M1.3 | M1 |

## 10. P2 placement

All P2 ids are in M15 except the ones pulled forward because a P1 item requires them (PLAN §6):

- FLAG-028 (Compartmentalized/scalable Paxos) → M9.2
