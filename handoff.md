# Handoff: Blossom (bloom-remake)

Last updated: 2026-09-27, while M2 was running.

## What this is

Blossom is a from-scratch Rust implementation of the Berkeley BOOM line of work (Overlog → Dedalus → Bloom →
Bloom^L → Blazes → Edelweiss → Molly → Hydro), built as its own statically typed language (`.bls`, CLI
`blossom`). The goals:

- a Dedalus-semantics engine with lattices
- a distributed runtime
- CALM, Blazes and Edelweiss analyses
- LDFI (Molly-2), plus bounded model checking and SMT invariant verification
- an interpreter and a Rust code generator
- full Raft, Multi-Paxos, 2PC/3PC, BOOM-FS, BOOM-MR/HOP, and a lineage dataflow engine (the modern Hadoop successor)

## Where things are

| What | Where |
|---|---|
| Decisions (user choices + defaults adopted) | `docs/DECISIONS.md` |
| Research reports and master feature spec (839 ids) | `docs/research/01..15-*.md`, `docs/research/FEATURES.md` |
| Language reference (normative) | `docs/design/LANGUAGE.md`; syntax panel record in `docs/design/syntax/` |
| Architecture (35 crates, traits, IR, engine, runtime) | `docs/design/ARCHITECTURE.md` |
| Build plan: 15 milestones, 94 work packages | `docs/design/PLAN.md` (§2 agent protocol, §3 gates, §8 WPs), `docs/design/plan.json` |
| Example programs | `examples/e01_kvs.bls` … `examples/e10_specs.bls` |
| Golden test corpus (~380 cases) | `tests/corpus/{core,lattices,lprov,async,net,verify,ldfi}/` + `tests/corpus/README.md` |
| Per-WP notes, known bugs, current milestone | `docs/plan/notes/*.md`, `docs/plan/BUGS.md`, `docs/plan/MILESTONE` |
| Coding conventions for all WPs | `docs/dev/CONVENTIONS.md` |
| Status board | pad `bloom-remake` (`pad use bloom-remake`) |

## Progress

| Milestone | State |
|---|---|
| Research | ✅ `08c08e9` |
| Design | ✅ `4b0efe4` |
| M1: workspace skeleton, blossom-base, blossom-value type surface, golden corpus I–IV | ✅ gate green, `85060ad` |
| M2: value encodings, Dedalus^L IR, parser/CST/AST, SAT, SMT (z3), storage I/O | ⏳ running (workflow run `wf_61663d67-5c5`) |
| M3–M15 | not started |

Nothing executes Blossom programs yet. The first runnable programs arrive with:

- the oracle (M4.1)
- lowering (M5.3)
- the interpreter (M6.1)

## How the build runs

Each milestone is one run of a reusable workflow script:

`~/.claude/projects/-Users-jimmyhmiller-Documents-Code-projects-bloom-remake/7c62f673-7a00-4ab4-bb70-d79b68ddaa9e/workflows/scripts/blossom-milestone-wf_b7d601d2-a98.js`

Per milestone:

1. Create one worktree per WP off `main`:
   ```sh
   for w in M3.1 M3.2 ...; do git worktree add -q .worktrees/$w -b wp/$w main; done
   ```
2. Launch the workflow with `scriptPath` = the path above and args
   `{"milestone": "M3", "wps": [{"id": "M3.1", "title": "..."}, ...]}`. Ids and titles come from `plan.json`.
3. Inside the workflow, each WP runs in parallel:
   - an implement agent works in `.worktrees/<id>` (private `CARGO_TARGET_DIR`) and commits on `wp/<id>`;
   - an adversarial reviewer (structured verdict) checks it;
   - up to 3 fix/re-review rounds follow.
4. After all WPs finish, a gate agent:
   - merges every `wp/*` branch into `main` and runs the PLAN §3 gate (`scripts/milestone-gate.sh`, `scripts/ci.sh gate`);
   - fixes any failures;
   - commits `"Mk: <title>"`, advances `docs/plan/MILESTONE`, and removes the worktrees and branches.
5. Afterwards the orchestrator checks the gate result, updates the pad and this file, and starts the next milestone.

To resume an interrupted run, re-invoke Workflow with the same `scriptPath` and args plus
`resumeFromRunId`. Finished agents replay from cache. State that matters lives in git: the `wp/*` branches and
`main`.

## Next steps

1. **Wait for M2 to finish.** Check its gate result (`git log main`, `docs/plan/notes/M2-gate.md`) and update the
   pad.
2. **Run M3** (7 WPs):
   - lattices I
   - IR fixtures
   - kernel storage I
   - schema/artifact
   - front I (modules/resolution)
   - the Molly `.ded` frontend
   - syntax II (formatter)
3. **Run M4** (8 WPs):
   - the naive Dedalus^L oracle
   - analysis I (stratification, locality)
   - kernel II (indexes, joins)
   - the wire codec
   - type checking
   - lattices II
   - the trace format
   - the engine boundary types
4. **Run M5**: planner, corpus runner (`cargo xtask corpus`), lowering, durable recovery, the sans-IO node, CALM
   certificates, provenance I. **This is the first milestone where corpus cases pass on the oracle.** From here on,
   the gate also runs the corpus ratchet and coverage.
5. **Run M6**: the interpreter plus the full surface language. **This is the first point where Blossom programs
   run on the real engine.** Spot-check `examples/` by hand here.
6. **Continue M7–M15** in order, per `PLAN.md` §7:
   - M7: simulator, differential testing, production driver
   - M8: LDFI/Molly parity, std lib I–II, codegen, mTLS
   - M9: Raft core, Multi-Paxos, Anna KVS, BMC
   - M10: Raft P1, commit protocols, BOOM-FS, SMT/ASP verification
   - M11: BOOM-MR, lakehouse, benchmarks
   - M12: HOP, the lineage dataflow engine, upgrades
   - M13: Tide streaming, release engineering
   - M14: P0/P1 audit
   - M15: P2
7. **After each gate:**
   - skim new `docs/plan/BUGS.md` rows (50 rows so far, all minor review findings from M1) and route real ones into
     the owning WP's prompt;
   - make sure `cargo test --workspace` is green on `main`.

## Things to watch

- **Throughput.** M1 took about 9 hours of wall-clock (including one usage-limit pause) and about 11M subagent
  tokens. Usage limits are the main throttle. Workflows pause and resume on their own when the limit resets.
- **Disk.** About 105 GB was free at the start. Each worktree gets its own `target/`, and gates delete the
  worktrees. If space gets tight, run `cargo clean` in finished worktrees.
- **Worktrees.** They live under `.worktrees/` (gitignored). Never `cd` into one from the orchestrator; use
  absolute paths.
- **Ownership.** Only the gate agent may touch shared files (root `Cargo.toml`, `Cargo.lock`) or paths owned by
  another WP (PLAN §2.2–2.4).
- **No stubs.** Unimplemented paths must return `Unimplemented` / BLS0908 / exit code 7 naming the feature id and
  owning WP. Reviewers fail any WP that fakes behavior or weakens tests.
- **Known minor issues from M1 review, not yet fixed:**
  - `graph::topo_sort` and `min_chain_cover` doc comments are inaccurate;
  - `Value` serde deserialization has unbounded recursion (fuzz targets in M13.3 should catch it);
  - several golden cases in `tests/corpus/lattices` re-derive Bud tests instead of porting them verbatim (flagged
    for the M6.7/M8.8 corpus triage).
- **No license chosen.** Every package is `publish = false` until the user picks one.
