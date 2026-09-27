# Handoff: Blossom (bloom-remake)

Last updated: 2026-09-27, after the M2 gate.

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
| M2: value encodings, Dedalus^L IR, parser/CST/AST, SAT, SMT/ASP, storage I/O | ✅ gate green, `8fc2268` |
| M3–M15 | not started |

Nothing executes Blossom programs yet. M2 supplies the value model, validated IR, lossless parser, solvers and durable byte layer. The first runnable programs arrive with:

- the oracle (M4.1)
- lowering (M5.3)
- the interpreter (M6.1)

M2 verification: all six WP acceptance commands passed, followed by `scripts/milestone-gate.sh M2 --no-merge`. The gate passed formatting, workspace Clippy, 245 tests across 54 binaries, corpus lint and cargo-deny. The lexer/parser, SMT-response and WAL-recovery fuzz targets compile. M2.6's crash workload exhaustively checked over 5,000 crash and acknowledged-byte media-fault images. `docs/plan/MILESTONE` is now `M3`; the M2 worktrees and branches were removed.

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

## Reassessment: one incomplete end-to-end demo

The smallest useful target is **source to answer on the oracle**: feed an actual `.bls` file with a few static facts and one view through parsing, resolution, type checking, lowering and the naive evaluator; assert the resulting relation rows from a repeatable command. This proves one complete language path while leaving the optimized engine, durability, networking and broad language coverage for later. Use one small checked-in example/fixture rather than the full KVS program.

Under the current `plan.json` dependencies, the remaining closure for that target is **eight WPs**: M3.1, M3.2, M3.5, M3.6, M3.7, M4.1, M4.5 and M5.3. M3.6 is in M5.3's declared dependency closure even though this `.bls` demo does not exercise `.ded`; changing that ordering would require a reviewed plan amendment. Execute these WPs with their specified acceptance and an integration test that starts at source text and checks exact oracle rows. If maintaining whole-milestone gates, finish the other M3–M5 WPs too.

The next, stronger demo is **source to in-memory engine output** through M6.1 and M6.3 (20 remaining WPs in their combined declared closure). It should drive two ticks with `ManualDriver`/`MemTransport`, inject an input, observe an output and compare each tick with the oracle. The original `examples/e01_kvs.bls` put/get/restart demo needs M5.4 durable recovery and M7.4's host-facing production runtime (22 WPs in M7.4's closure), plus its upsert, outer-join, session and ACL lowering. It is a good later acceptance target, not the shortest first demo.

This handoff changes no M3 implementation or milestone ordering. Before beginning a selected vertical slice, record its narrower acceptance scope in the plan; otherwise follow the complete M3, M4 and M5 gates. Continue to route open `docs/plan/BUGS.md` rows to each owning WP.

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
- **Remaining M1 review notes:**
  - `graph::topo_sort` and `min_chain_cover` doc comments are inaccurate;
  - several golden cases in `tests/corpus/lattices` re-derive Bud tests instead of porting them verbatim (flagged
    for the M6.7/M8.8 corpus triage).
- **No license chosen.** Every package is `publish = false` until the user picks one.
