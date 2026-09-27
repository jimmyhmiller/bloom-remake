# M1 gate — Workspace skeleton, blossom-base and the golden corpus

## What was done

- **Merge.** `wp/M1.1` … `wp/M1.5` were merged into `main` with `--no-ff` (PLAN §3 step 1). The WPs' paths are
  disjoint, so nothing conflicted; `cargo metadata` + `cargo check --workspace --all-targets` left `Cargo.lock`
  unchanged.
- **Gate as merged: green.** `scripts/ci.sh gate` (fmt, clippy `-D warnings` with all features, `check-layers`,
  `check-sans-io`, `check-codes`, nextest + doctests, the corpus lint, `cargo deny check`), plain `cargo test
  --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and every acceptance command of M1.1–M1.5
  passed on the merged tree before any gate change. No review had a blocking finding.
- **Gate changes.** The merged tree had one real gate defect: `scripts/collect-notes.sh` collected **none** of the
  34 bugs the corpus WPs recorded, because they wrote numbered lists without an owner crate, and the script dropped
  them without a word. The gate fixed the script so that this cannot pass silently again, gave every item its owner,
  and collected them. It then fixed the review findings that are cheap and clearly right before the `blossom-base`
  freeze, settled the cross-WP corpus vocabulary conflict, replaced the corpus files that transcribed unlicensed
  Molly test inputs, and recorded the rest below (`## Bugs`, collected into `docs/plan/BUGS.md`).
- **Gate after the changes: green.** `scripts/ci.sh gate` (nextest: 125 tests, all passed; `cargo deny check`:
  advisories, bans, licenses and sources ok), plain `cargo test --workspace`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --all --check`, every acceptance command of M1.1–M1.5, and
  `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps`.
- **Environment.** The dev tools M1.1 installed (cargo-deny 0.20.2, cargo-hack 0.6.45, cargo-nextest 0.9.146) were
  copied from the M1.1 worktree's `.tools/` into the main tree's git-ignored `.tools/`, so the gate tier runs
  nextest and cargo-deny.

## Changes

Every path the gate modified, by topic.

**Collecting WP notes** (PLAN §3 step 2).
- `scripts/collect-notes.sh`: bug items may be `- …` or `1. …`; indented lines continue an item across blank lines
  (M1.2's item 5 lost its last paragraph before); an item must start with its backticked owner; a `## Bugs` section
  with text but no item, an item without an owner, or an already-collected item whose text changed (source ids are
  positions, so a reordered notes file would have been misattributed) stops the script and changes nothing.
- `docs/plan/notes/M1.2.md`, `M1.3.md`, `M1.4.md`, `M1.5.md`: each `## Bugs` item now starts with the crate of the
  WP that must settle it (text otherwise unchanged).
- `docs/plan/notes/README.md`, `docs/dev/CONVENTIONS.md` §9: the rules above.
- `docs/plan/BUGS.md`: the collected rows.

**`blossom-base`, before the freeze** (review of M1.1).
- `graph.rs`: `topo_sort` now returns what its doc promised, a shortest cycle through the smallest node of the
  first cyclic component (it returned the shortest cycle through the edge to that node's smallest in-component
  successor: edges 0→1→3→4→0 and 0→2→0 gave `[0, 1, 3, 4]` instead of `[0, 2]`), through the new public
  `shortest_cycle_through_node`; `GraphError::Cycle` no longer claims a globally shortest cycle; `min_chain_cover`'s
  doc says chains are ordered by their first (least) element. Tests: the reviewer's reproducers, a brute-force
  proptest of `shortest_cycle_through_node`, and the `topo_sort` proptest now checks the witness is the shortest
  cycle through the right node.
- `det.rs`: `det_map_platform_independent_hash` pins five hash values (it had the comment "Pinned values" but
  compared the hasher only with itself).
- `span.rs`: `SourceDb::line_text` reports a bad line as the new `SourceError::LineOutOfRange { path, line, lines }`
  (it said "offset N is past the end"), and returns an error instead of a silent `""` on an impossible slice.
- `error.rs`, `idx.rs`: rustdoc links to the exported macros; `RUSTDOCFLAGS=-D warnings cargo doc --workspace
  --no-deps` is clean (also `blossom-lsp`'s crate doc, and `blossom-cli`'s binary no longer collides with the
  `blossom` facade crate's docs: `[[bin]] doc = false`).

**`blossom-value`.** `Value::tuple([])` returns `Value::Unit`, and deserialization rejects an empty `Tuple`, so the
empty tuple has one representation (`serde_util::nonempty_tuple`; test `value_serde_rejects_empty_tuple`).

**Dispatch files, frozen after M1.** `crates/blossom-cli/src/main.rs` and `xtask/src/main.rs` carry
`#![deny(unsafe_op_in_unsafe_fn)]` like every library root; `crates/blossom-std-host/src/lib.rs` lists the WP of
every area.

**xtask.** `rustsrc` also scans byte and C string literals (`b"BLS0502"` was invisible to `check-codes`) and expands
`extern crate x as y;` aliases for `check-sans-io`; `check-codes` treats the files of out-of-line test modules
(`#[cfg(test)] mod tests;`, transitively, `#[path]` honoured) as test code instead of production code. Tests for
each.

**CI.** `scripts/ci.d/45-corpus-lint.sh` passes without the Python checker only when `50-corpus.sh` exists and runs
`xtask corpus --lint`; before, deleting the checker without adding M5.2's fragment would have passed silently.
`.gitignore` ignores `__pycache__/`.

**Corpus vocabulary** (review of M1.4, M1.3). The four corpus WPs wrote `[expect_analysis]` values independently,
and M1.3 and M1.4 disagreed on the shapes of `points_of_order`, `strata`, `calm_labels` and on the finality class
names, so no runner could satisfy both. The gate fixed one vocabulary, from their conventions:
- `tests/corpus/README.md`: new section "Expectation vocabularies" (every key's shape and comparison, the diagnostic
  comparison of each backend, `quiescent_from`); PLAN §5.1 (and the README's verbatim copy) points to it.
- Class names follow ARCHITECTURE §7.2 (`POS` … `NEVER`), which outranks the body of FEATURES (the M1.4 review
  suggested the opposite; PLAN's precedence order decides). `confluent` follows CR-29; `certificates` lists the
  Dedalus-family certificates exactly, which reconciles BENCH-091a (`confluent = certified`) with BENCH-089h/089d
  (`certificates = []`), since message join is monotone (confluent by CALM) but has unguarded asynchrony.
- Converted, without changing what they assert: `core/BENCH-002`, `002d`, `015c`, `027b` and `lattices/BENCH-052a`,
  `lprov/BENCH-305a` (`strata` → `{ of = … }`); `lattices/BENCH-064a` (`calm_labels` → `{ outputs = … }`);
  `lattices/BENCH-050a`, `050b`, `064a`, `lprov/BENCH-305c`, `308c` (`points_of_order` lists → `{ complete = true,
  sites = … }`; 050a's empty report → empty `edges`, `clusters` and `sites`); `lattices/BENCH-075b`,
  `lprov/BENCH-307c` (`NEVER-FINAL` → `NEVER`, `POS-FINAL` → `POS`).
- `tests/corpus/tools/check_manifests.py` validates every key's value shape (the old shapes, unknown edge kinds,
  certificate kinds, verdicts, finality classes, Blazes labels and Edelweiss reasons are rejected).
- `docs/plan/notes/M1.3.md`, `M1.4.md`: a pointer from their vocabulary sections to the README.

**Corpus README fixes** (review of M1.2). The SEM-009 reading is qualified by ARCHITECTURE §0.2 L4 (an idle stretch
equals empty ticks only when an empty tick has no effect) and names the cases that rely on it; a new convention
says no relation holds at a tick a halted or crashed node never runs (BENCH-017's `absent = "2.."`); the list of
compile cases resting on an open reading adds BENCH-003d and BENCH-045c. `core/BENCH-043b`'s note no longer claims
the 128 simulator seeds are 128 choice seeds.

**Molly unit tests** (review of M1.5). Five `lib/unit` programs transcribed Molly's test inputs (the four
`ProvenanceSuite` programs and `negative_support_test.ded`, the latter with renamed relations), although Molly has
no license and PLAN §8 (M1.5) forbids copying its text; the review found three, the gate found the other two
(`join_firings.ded`, `wildcard_derivations.ded`). Each was rewritten for the corpus under its own relation names
and data, testing the same property, and so were the three `_net` variants built on them: `join_firings*.ded`,
`wildcard_derivations*.ded`, `agg_contributors*.ded`, `agg_grouping.ded`, `negative_support.ded`. The manifests of
BENCH-135f–m and BENCH-137i, `golden/provenance.toml`, `tests/corpus/ldfi/molly/README.md` and
`third_party/molly/README.md` follow. Every expectation keeps its form (same verdicts, run bounds, falsifier-set
shapes, contributor and derivation counts); `ldfi_ref.py check` confirms each, and that the new negative-support
program still needs negative support (without it: no counterexample). The new `negative_support.ded` also no
longer uses an absolute-time body atom in a helper rule, which LANGUAGE §21.1 and ARCHITECTURE §13.12 read
differently (another M1.5 review point).

**`tests/corpus/ldfi/tools/ldfi_ref.py`.** `check` no longer crashes on a case outside `tests/corpus/ldfi`, and a
case whose run count exceeds `runs_max`, whose check was too large to run, or whose crash view is not modelled is
reported as `WARN` instead of `ok` (the summary counts them; `--strict-runs` still makes an exceeded count a
problem); `tests/corpus/ldfi/README.md` describes the statuses. The gate reran `check` on 92 of the 96 LDFI cases
(all but the four longest searches, BENCH-133c, 133d, 137g and 137h, whose programs it did not touch): 0 problems,
and three cases that the old output called `ok` are now `WARN`: BENCH-133b (Paxos 7/6/1) and BENCH-137e (Raft 12/6/0),
where the reference lineage search exceeds its 20 000-run budget while the exhaustive search confirms the verdict,
and BENCH-136i (ack-deliv 8/7/1: 5055 runs against the published 673, a documented M9.4 target, item below).

**Documents.** `docs/DECISIONS.md` (section "M1 gate"): the xxhash-rust BSL-1.0 exception M1.1 asked the gate to
record, the frozen `blossom-base` surface, the corpus vocabulary, the diagnostic comparison, the Molly unit tests,
the notes format. `docs/design/ARCHITECTURE.md` §2.1: `Symbol` is an interned text handle, not a `define_idx!` id
(what M1.1 built and the PLAN spec requires).

## Review findings not fixed here

Everything a later WP owns is a `## Bugs` item below. The rest needs no change:
- M1.1: `Value::tuple` and the placeholder feature ids of `blossom-value` (ENG-020, ENG-120, TEST-010 name WP M2.1,
  whose spec implements them, while plan.json lists them first under M3.3, M4.3 and M4.7): the stubs disappear with
  M2.1, and no corpus `until` is computed from them. `docs/plan/notes/README.md` was written by M1.1 without being in
  its `owns` list; PLAN §8 item 8 requires the file and no M1 WP owned it, so this is a plan inconsistency, recorded
  here as PLAN §2.7 asks.
- M1.2: BENCH-023's "with reordering" reading and BENCH-024a's `holds = "4..=11"` (weaker than tick 3) are
  documented judgement calls; the triage WPs may tighten them (items below).
- M1.3: BENCH-300b's `runs_max` stand-in is documented; M1.3's notes bugs 6 and 7 (BENCH-079f, BENCH-307b) are
  collected.
- M1.4: `quiescent_from` and the BENCH-150b/c `reason` key are documented (the latter is an item below).
- M1.5: the unpinned BENCH-136 counts, the failure-free cases pinning protocol relations, Figure 12's minimality and
  the unverified 3pc CR-20 remark are documented in the M1.5 notes and change no expectation.

## Bugs

- `blossom-value`: `Value`'s derived serde recurses once per nesting level with no bound, so decoding a crafted
  artifact or trace aborts the process with a stack overflow instead of returning an error. Reproducer: postcard
  bytes `[20, 1]` repeated 100 000 times followed by `[0]` (100 000 nested `Option(Some(…))` around `Unit`) passed
  to `postcard::from_bytes::<Value>` abort with "stack overflow"; serializing such a value does the same. M2.1 owns
  the crate and its decoding: bound the nesting depth on decode (also through `LatValue` and `GroupValue`, which
  recurse without passing through `Value`) and report an error; the M13.3 fuzz targets `artifact_decoder` and
  `trace_reader` must cover it.
- `blossom-fuzz`: `libfuzzer-sys` is licensed `(MIT OR Apache-2.0) AND NCSA`; NCSA is not on the ARCHITECTURE §1.2
  allowlist, and `fuzz/` is its own workspace, outside `cargo deny check`. Reproducer: `cargo deny check` from the
  repository root never sees `fuzz/Cargo.lock`. Owner M13.3 (fuzzing, cargo-deny): run cargo-deny over `fuzz/`
  and settle NCSA with a DECISIONS line or another fuzzing harness.
- `blossom-testkit`: the corpus vocabulary fixed at the M1 gate (`tests/corpus/README.md`, "Expectation
  vocabularies") needs two things only M5.2 can supply: `xtask corpus --lint` must port the new value-shape checks of
  `check_manifests.py` (M5.2's spec already requires everything the Python tool checks), and the `analysis`
  backend's diagnostic rule needs a table from diagnostic codes to the ANA features that report them (LANGUAGE §20,
  ARCHITECTURE §7.2), which no document has. Reproducer: BENCH-093g asserts "no BLS1002" only through ANA-008 in its
  `features`.
- `blossom-testkit`: schema v1's `[expect_verify] result = "fails"` cannot say why: BENCH-150b (outside EPR) and
  BENCH-150c (a counterexample to induction) are indistinguishable. A `reason = "outside_fragment" |
  "counterexample"` key (a PLAN §5 change at a gate) would let the runner check it (M1.4 follow-up). Owner M5.2.
- `blossom-analysis`: ARCHITECTURE §7.3's `ConfluenceStatus::Certified(CertKind)` leaves `CertKind` undefined. The
  corpus needs `dedalus_plus` (ANA-025), `dedalus_s` (ANA-026), `dedalus_plus_l` (ANA-141), `dedalus_s_l` (ANA-142)
  and a kind for confluence by CALM monotonicity, which BENCH-091a's `confluent = "certified"` requires for a program
  with no point of order but unguarded asynchrony. Owner M5.6 (CALM certificates): define the enum and amend
  ARCHITECTURE §7.3.
- `blossom-sim`: FEATURES SEM-044 defines confluence as "exactly one ultimate model" per input, while CR-29 (which
  outranks it) reports confluence in Ameloot's sense separately from consistency under fair runs; message join
  (BENCH-091a) is confluent under CR-29 and has two ultimate models over fair runs. The simulator's ultimate-model
  confluence check (M8.2) must implement CR-29's reading (the corpus's `confluent` key follows it), and FEATURES
  SEM-044 should be reworded at a gate. Reproducer: BENCH-091a, BENCH-091b.
- `blossom-sim`: BENCH-090x (the CALM-pruning regression of ARCHITECTURE §6.2) cannot tell sound from unsound
  pruning under the default asynchronous network; it needs an equal-delay network model that schema v1 cannot
  express, so until then it passes without guarding TEST-003 (M1.4 follow-up). Owner M7.2 (simulator; with M5.2 for
  a schema key).
- `blossom-ldfi`: two pinned `runs_max` targets are well below what the ARCHITECTURE-conformant reference search
  reaches: ack-deliv 8/7/1 (BENCH-136i: 673 vs more than 5000) and Kafka 6/4/1 (BENCH-136g: 38 vs 52). They are the
  published Molly counts, so they stay (M1.5 notes, "Notes for the work packages"); meeting them needs negative
  support more precise than CR-31's rule (TEST-051). Reproducer: `python3 tests/corpus/ldfi/tools/ldfi_ref.py check
  tests/corpus/ldfi/molly/BENCH-136i-ack-deliv-8-7-1-runs --strict-runs`. Owner M9.4.
- `blossom-prov`: BENCH-134e implements Nemo's ZK-1270 repair as a persisted `ack_seen` plus a changed `end_proto`,
  not FEATURES' one-line `success(L) :- sent_flag(L), ack(F)`, and no case pins Nemo's repair suggestion for
  ZK-1270 or MR-2995 (schema v1 cannot express it). Owner M7.6 (TEST-052, Nemo algebra): add the suggestion checks
  as integration tests.
- `tests/corpus/lattices`: several BENCH-051/052/054/056 cases re-derive Bud's lattice tests from R04's summaries
  instead of porting `bloom-lang/bud` `test/tc_lattice.rb`, which is public:
  051c is not Bud's EmbedMax (cells embedded as values of table `t`, t[m1] 10→16, t[m2] 13→17), 051e is not
  MaxConstructorImplicit (5/6/−7), 051f omits Bud's first scenario (sending `m` while it is ⊥, where Blossom's
  SEM-101 sends nothing and Bud delivers ⊥ — a divergence worth pinning), 052c uses `next_hop` = first hop and the
  acyclic input instead of `test_spath_cyclic_variant`, and 054a/b, 056a/b invent data where `test_maxcap_simple`
  and `test_all_paths` (including its second tick with e→f) have exact expectations. All are marked `derived =
  true` and correct for the programs as written. Owner M6.7: add verbatim ports as new cases (never weakening the
  existing ones).
- `tests/corpus/lattices`: BENCH-060's "at most one response per distinct summary" has no assertion; a case listing
  DIST-007 with an `[[expect_send]] count` would cover it (the rewrite changes the outbox, ARCHITECTURE §3.5). And
  BENCH-050a's `holds = "7.."` for `result_chn` bakes in the literal resend semantics, which the DIST-007 rewrite
  would change for an idempotent receiver. Owner M6.7.
- `tests/corpus/lattices`: BENCH-055b lists ENG-141 (first implementer M8.6), which moves its oracle `until` to M8
  although the oracle needs only ENG-043 (M6.1) for this size-regression case; split the case so the oracle half
  is due at M6. BENCH-304a's `m_size` row `{ some = 1 }` assumes `reveal!` types an LMax size as `Option`, which
  LANGUAGE §5.6 leaves open. Owner M6.7.
- `tests/corpus/async`: BENCH-086a/086d pin `fair_consistency = "not_certified"` but no `confluent` verdict, although
  R02 T19 calls the async singleton diffluent (`confluent = "not_confluent"` would be the stronger pin). Owner M6.7.
- `tests/corpus/ldfi`: BENCH-133d/133g (Flux) can never violate the spec: the egress alone delivers each sequence
  number once, in order and only with the ingress's data, so `broken` is underivable and the cases test only that
  the engine terminates or certifies, not the replica pair's takeover. A spec that observes the takeover would test
  it. Owner M6.7 (with M8.1).

## New dependencies

None.

## Follow-ups

- The gate did not add a CI job for `cargo doc` with `-D warnings`; ARCHITECTURE §11.10 has none. The workspace is
  clean now; M13.3 (release engineering) or M12.5 (documentation) should decide whether to gate it.
- M5.2 deletes `tests/corpus/tools/check_manifests.py`; `45-corpus-lint.sh` then requires `50-corpus.sh` to run
  `xtask corpus --lint`, which M5.2's spec already asks for.
