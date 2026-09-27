# Blossom coding conventions

The rules every work package follows. They implement ARCHITECTURE §12 and the user's rules (DECISIONS.md: "No silent
stubs", trait-based, production-grade code), and they complement PLAN §2, the agent protocol. Where this file and a
normative document disagree, the normative document wins (DECISIONS.md, FEATURES §1, LANGUAGE.md, ARCHITECTURE.md,
PLAN.md, in that order).

Most rules are enforced mechanically: `clippy.toml` and `[workspace.lints]`, `cargo run -p xtask -- check-layers`,
`check-sans-io` and `check-codes`, and `scripts/ci.sh`. Run `scripts/wp-check.sh <crate>…` before you finish.

## 1. Workflow

- Work in your WP's git worktree (`.worktrees/<WP-id>`, branch `wp/<WP-id>`), with a private target directory:
  `export CARGO_TARGET_DIR=$PWD/target` inside the worktree (it is git-ignored).
- Touch only the paths your WP `owns` plus `docs/plan/notes/<WP-id>.md` (PLAN §2.2). Shared files are listed in
  PLAN §2.4; the dispatch files (`crates/blossom-cli/src/main.rs` and `src/cmd/mod.rs`, `xtask/src/main.rs` and
  `src/cmd/mod.rs`, `crates/blossom-std-host/src/lib.rs`, and the module declarations of `blossom-syntax` and
  `blossom-front`) are frozen after M1.
- `xtask/Cargo.toml` is owned by M1.1. If the task file you own needs another dependency, add it there as a minimal
  out-of-scope fix and record it in your notes (PLAN §2.7).
- Every acceptance command of your WP must pass from the repository root in your worktree. Finish with the notes
  file (format: `docs/plan/notes/README.md`).

## 2. Errors and unimplemented paths

- **Library crates define `thiserror` enums**, one per crate (`IrError`, `EngineError`, …). No `anyhow` in libraries.
  Every crate error wraps the two shared errors:

  ```rust
  #[derive(Debug, thiserror::Error)]
  pub enum PlanError {
      // … the crate's own variants …
      #[error(transparent)]
      Unimplemented(#[from] blossom_base::Unimplemented),
      #[error(transparent)]
      Internal(#[from] blossom_base::InternalError),
  }
  ```

  Box errors that carry rows or derivations (`TickError(Box<TickErrorKind>)`), so `clippy::result_large_err` stays
  quiet and the happy path stays small.
- **No silent stubs.** A path that is not implemented returns `Unimplemented`:

  ```rust
  unimplemented_feature!("ENG-063", "the counted maintenance regime (WP M6.1)");
  ```

  The first argument is a FEATURES id (checked at compile time); the message says what is missing and which WP
  implements it. `unimplemented_error!` is the expression form. At build or load time, report the same thing as a
  BLS0908 diagnostic with `Diagnostic::not_implemented(feature, what, needed_by)` or
  `Diagnostic::from_unimplemented(&err, needed_by)`. A CLI or xtask command that is not implemented prints
  `not implemented yet: <FEATURE-ID> (WP <id>)` and exits with code 7. Never `todo!()`, never a plausible default
  (an empty result, `0`, `-1`, `None` standing for "not done"), never a panic.
- **Violated internal invariants** use `bug!("…")` (returns `Err(InternalError)`) or `internal_error!("…")` (the
  value, for `ok_or_else`). They panic when the calling crate is built with `debug_assertions` or when
  `BLOSSOM_PANIC_ON_BUG=1`, so bugs are loud in development and reported as errors in production. Use them instead
  of `assert!` in library code. `IndexVec::get_or_bug(id)` is the lookup for ids that must exist.
- Denied in library code (`[workspace.lints]`): `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`,
  `unreachable!`, panics in `Result` functions, string slicing, `dbg!`, printing. Tests may unwrap, expect, panic
  and index (`clippy.toml`). Helper functions in an integration-test file need `#[cfg(test)]` to count as tests.
- `clippy::indexing_slicing` is on (warnings are errors): use `get`/`get_mut`, iterators, or slice patterns. The
  only exception is a kernel hot loop, with `#[allow(clippy::indexing_slicing)]` and a comment giving the bounds
  argument (ARCHITECTURE §12.1).
- Arithmetic: `overflow-checks = true` in every profile. User arithmetic is always checked and reports BLSR004.
- `unsafe` is denied workspace-wide; `blossom-kernel` opts in per module (`rows`, `chunk`, `prefetch`) with a
  `// SAFETY:` argument at every block, and is Miri-tested.

## 3. Diagnostics and the code registry

- Every user-facing diagnostic has a registered code (LANGUAGE §20). Construct codes with the `code!` macro, which
  rejects an unregistered code at compile time: `Diagnostic::new(code!("BLS0502"), "…").with_primary(span)`.
- `blossom_base::codes::REGISTRY` records each code's owner crate and the other crates allowed to construct it.
  `check-codes` fails when a code is written outside those crates (in non-test code), when any code in
  `crates/*/src` is unregistered (test code included: build a deliberately bogus code with `format!`), and when the
  registry drifts from LANGUAGE §20 and ARCHITECTURE §0.3. Tests may mention codes owned by other crates.
- Adding or reassigning a code changes a frozen crate: an ARCHITECTURE amendment, a DECISIONS.md line and a
  `blossom_base::API_VERSION` bump in one commit (ARCHITECTURE §1.6).
- Runtime hard errors (BLSRnnn) are engine `TickErrorKind` variants; the oracle reports the same codes in its
  `ProgramErrorRecord`s so the differential runner can compare them.

## 4. Determinism

- Never use `std::collections::{HashMap, HashSet}`, `hashbrown::{HashMap, HashSet}` or a `RandomState`
  (`clippy::disallowed_types`). The hashed collections are `blossom_base::det::{DetMap, DetSet}`; ordered ones are
  `BTreeMap`/`BTreeSet`. Use `DetState::from_nonce(boot_nonce)` for tables fed by network input and
  `DetState::fixed()` (the default) elsewhere.
- **Hash iteration order is never observable.** Anything that leaves a component — dumps, traces, messages,
  diagnostics, digests, test snapshots — goes through a canonical order (`DetMap::sorted`, `DetSet::sorted`, a
  `BTreeMap`). `DetMap`/`DetSet` already print and serialize sorted.
- No ambient time or randomness: `Instant::now`, `SystemTime::now` and thread RNGs are banned
  (`clippy::disallowed_methods`). Time comes from the node's `Clock`, randomness from the SEM-084 PRF
  (`blossom_value::prf`) or the node's `Entropy`. The exemptions are `runtime::clock` and `runtime::entropy`, each
  with `#[allow(clippy::disallowed_methods)]` and a comment.
- The canonical order of values is `impl Ord for Value` (LANGUAGE §5.5, LANG-024): it never uses intern ids,
  hashes, pointers or arrival order. Order `Symbol`s by text (their `Ord` already does).
- Serde output is deterministic and strict: sets and maps serialize sorted and reject duplicates when read back;
  `f64` values serialize as bits so every NaN payload and `-0.0` round-trips.

## 5. Features, tests and naming

- Mark every site that implements a FEATURES id with a line comment `// FEATURE: <ID>` (one id per comment); the
  coverage tool counts them (`cargo run -p xtask -- coverage`, M5.2).
- Unit tests live next to the code (`#[cfg(test)] mod tests`). Each crate has at most one integration-test binary,
  `tests/it/main.rs`, plus the `harness = false` binaries its plan entry names. Cross-crate tests live in
  `tests/integration/tests/<prefix>_*.rs`, owned by prefix (PLAN §4 D4).
- Name tests so that the substrings of your plan entry's **Required tests** match them:
  `scripts/require-tests.sh <crate> <substring>…` fails when a substring names no test.
- Property tests use generators from lower crates (their `arbitrary` feature), never from `blossom-testkit`.
- Snapshot tests use `insta`; review every new snapshot for correctness instead of accepting it.
- **Tests must survive later milestones** (PLAN §2.6). Never assert that something a later WP implements is
  unimplemented; assert "works, or fails only with `Unimplemented` naming feature X":

  ```rust
  match encode_scalar(kind, lane, &value) {
      Ok(word) => assert_eq!(decode_scalar(kind, lane, word)?, value),
      Err(ValueError::Unimplemented(u)) => assert_eq!(u.feature.as_str(), "ENG-020"),
      Err(other) => panic!("unexpected error: {other}"),
  }
  ```

- Never weaken a test, an expectation or a corpus status to make something pass.
- Performance numbers are recorded with machine details and never gate on shared machines.

## 6. Module layout, logging and APIs

- Each crate's `lib.rs` starts with `#![deny(unsafe_op_in_unsafe_fn)]` and a crate doc naming its purpose and the
  WPs that implement it. Public items have doc comments.
- Everything logs through `tracing` (spans `node` > `tick` > `stratum`); libraries never print. Binaries
  (`blossom-cli`, `xtask`) allow `print_stdout`/`print_stderr` at their crate root.
- Prefer traits at seams (`Vfs`, `Transport`, `Evaluator`, `SatSolver`, `SmtSolver`, `PlanExecutor`, `ExternFn`, …)
  and ship a conformance suite with each trait (ARCHITECTURE §11.1).
- The frozen crates (`blossom-base`, `-value`, `-ir`, `-schema`, `-artifact`, `-trace`) change their public API
  only through ARCHITECTURE §1.6 once their implementing milestone has closed; other public APIs of earlier
  milestones change additively only (PLAN §2.3).

## 7. Dependencies and tools

- Internal edges are pre-declared by M1.1 and must stay within `xtask/layers.toml` (`check-layers`): no edge up a
  layer, no `kernel → ir`, no `oracle → {kernel, engine, plan, analysis}`, no `engine → {plan, node, prov, tokio}`,
  no compiler crates under `node`, `sim` or `runtime`, `blossom-testkit` only as a dev-dependency of the
  integration-test crates, and no normal dependency path from `systems/*` to `blossom-driver`.
- External crates: use `dep.workspace = true` when the root declares the crate, otherwise an explicit version in your
  crate's manifest; record every new crate (name, version, license, reason) under `## New dependencies` in your
  notes. Licenses must be on the allowlist of `deny.toml` (ARCHITECTURE §1.2).
- Nothing is installed globally. Tools go into the git-ignored `.tools/` (`scripts/install-dev-tools.sh`,
  `scripts/install-solvers.sh`); `scripts/ci.sh` puts `.tools/bin` first on `PATH`. A missing tool that an
  acceptance needs is a failure, never a silent pass.

## 8. CI and gates

- `scripts/ci.sh fast` runs every `scripts/ci.d/NN-*.sh` fragment marked `# tier: fast`; `scripts/ci.sh gate` adds
  the `gate` fragments and `cargo deny check`; `nightly` runs the `nightly` fragments. Add a fragment only if your
  `owns` lists it.
- `scripts/milestone-gate.sh Mk` merges the milestone's WP branches, collects notes, runs the gate tier and, from
  M5, the corpus ratchet and coverage (PLAN §3).

## 9. Recording bugs

A bug in code you do not own goes under `## Bugs` in your notes as `` - `crate-name`: summary. Reproducer: … ``;
the gate copies it into `docs/plan/BUGS.md`. If it blocks you and the path is owned by no WP of your milestone, make
the minimal fix, list it under `## Out-of-scope fixes` and add a regression test; if a sibling WP owns it, do not
touch it (PLAN §2.7).
