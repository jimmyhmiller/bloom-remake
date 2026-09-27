# The golden corpus

`tests/corpus` holds Blossom's golden conformance corpus (BENCH-000): one directory per test case, each a program
from the literature with the result the literature publishes for it. The corpus runner (`cargo xtask corpus`, and
`cargo test -p blossom-testkit --test corpus`, WP M5.2) runs every case on every backend its manifest lists, under a
status ratchet that only ever tightens. This file is a reader's guide: where things are, what a manifest says, how
the statuses move and when a case must pass. The normative definitions are in `docs/design/PLAN.md` §5 (reproduced
below verbatim) and `docs/design/ARCHITECTURE.md` §11.4.

## Where expected results come from

Every expectation comes from the literature: the item text of `docs/research/FEATURES.md` §11, the papers it cites
and the research reports `docs/research/01-*.md` … `15-*.md`. It never comes from running Blossom. Each manifest
names its origin in `source`, and explains anything that had to be derived, adapted or decided in `notes`. The
corpus WPs record every judgement call in `docs/plan/notes/<WP>.md` so that the triage WPs (M6.7 on the oracle, M8.8
on the interpreter and the simulator) can revisit them. Triage may fix a case that contradicts its cited source; it
never weakens an expectation to make an implementation pass.

## Layout

```
tests/corpus/
    README.md                     this file
    tools/check_manifests.py      schema v1 validator (until M5.2 replaces it with `cargo xtask corpus --lint`)
    <area>/<ID>[<letter>]-<slug>/ one case
        manifest.toml             what to run and what must happen
        program.bls | program.ded the program (or several files, named in `programs`)
        spec.bls                  optional spec (LDFI, BMC, SMT, simulation checks)
        expected/                 optional per-tick dumps, only with expected_from = "blessed"
```

| Area | Contents | Written by |
|---|---|---|
| `core` | core semantics: ticks, persistence and deletion, keys, stratification, choice, numbering, folds, Datalog evaluation (BENCH-001–048) | M1.2 |
| `lattices`, `lprov` | lattices and lattice provenance (BENCH-050–079, BENCH-300–313) | M1.3 |
| `async`, `net`, `verify` | asynchrony and confluence oracles, core networking, verification (BENCH-080–102, BENCH-150) | M1.4 |
| `ldfi` | the Molly LDFI corpus (BENCH-130–137) | M1.5 |
| `std/<area>`, `protocols`, `examples`, `upgrade`, `security`, `frontends` | standard library, flagship systems, the example programs, later milestones | the std and system WPs, M8.8 |

A case directory is named after the FEATURES id it belongs to (`BENCH-001`, or `LIB-…` for library cases), an
optional lower-case letter when the item has several scenarios, and a slug. By convention of the M1 corpus WPs the
letter `d` is reserved for the `.ded` twin of a case whose source is a Dedalus program, so `.bls` scenarios skip it;
an item with more than one twin gives the others the next free letters (BENCH-001e, BENCH-007e, BENCH-015f). Every
other letter is an ordinary scenario, and the manifest's `program` field, not the letter, decides which frontend a
case runs. Programs are self-contained: `.bls` programs start with `program NAME version N;` and import nothing from
`std::` (the standard library has its own cases).

## The schema (PLAN §5, verbatim)

#### 5.1 Layout and fields

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

#### 5.2 Backends

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

#### 5.3 The status ratchet (per backend)
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

#### 5.4 Computing `until`
For a case and backend: `until = max(floor(backend), max{ M(f) : f ∈ features, f not a BENCH id })`, where `M(f)`
is the milestone of the **first** WP listing `f` in plan.json (its primary implementer, §9). A case whose features
include a P2 id belongs to M15. Corpus authors compute it from plan.json; the corpus runner re-checks it.

## Conventions used by the M1 cases

PLAN §5 leaves a few things to the runner. The M1 corpus relies on the readings below; each is stated here so that
the runner (M5.2) and the triage WPs can check the cases against it rather than against guesswork.

- **`.ded` cases** take their nodes and their inputs from the program. The nodes are the constants the `.ded`
  typer infers as locations in the program's facts: the first column of every fact, and any other column unified
  with a location (in BENCH-008d the requesters `"c1"` and `"c2"` of `request("s", "c1")`, which
  `response(From, X)@async` addresses). `p(…)@k` facts are input events at tick k (LANGUAGE §21.1, CR-13; tick 0
  has none). Their manifests have no `[deploy]` nodes and no `[[input]]`. Rows are written **without** the location
  column, which the `.ded` frontend strips from every relation.
- **Deployment.** A program with no roles runs on the default single node `n1`. Every multi-node case declares
  roles in its program (`role Peer: cluster;`), so that `[deploy] nodes` can name each node's role.
- **Run length.** `[run] ticks = N` is chosen with a margin: no expectation refers to a tick later than N − 2, so
  the case holds whether the runner counts ticks `0..N` or `0..=N`. Cases with future inputs use `stop = "ticks"`.
- **Host inputs** arrive from tick 1 on; tick 0 is the boot tick.
- **Node-local ticks under the simulator.** Scripted inputs, faults and per-tick expectations name node-local ticks.
  When a node has nothing to do before such a tick (no message, timer, input or staged change), the simulator runs
  empty ticks up to it. By SEM-009 an idle stretch is observationally equivalent to a run of empty ticks **only when
  an empty tick has no effect on that node** (ARCHITECTURE §0.2 L4: no level-triggered `send`, timer or state change
  fires in it). The cases that rely on this reading (BENCH-040b, 041b, 043c, 044c, 046b, 047c) have no empty-tick
  effect at the ticks involved, so it changes no result there; a case whose node does have one must script its
  ticks explicitly. Messages still arrive whenever the schedule delivers them.
- **Ticks a node never runs.** A node that has halted (LANGUAGE §7.15) or crashed without a restart runs no later
  tick. No relation holds at a tick the node never runs: an `absent` range covering such ticks holds there, and a
  `holds` range covering them fails. BENCH-017 relies on the first half (`tbl(3)` absent from tick 2 on a node that
  halts at the end of tick 1).
- **Faults.** `[[fault]] kind = "restart"` at tick k means the node's tick k is the first tick of a new incarnation:
  durable relations are reloaded, everything else starts empty, and `boot()` and `recovered()` hold at tick k. A
  scripted restart happens at a commit point: tick k − 1's durable deltas, including its `next`, `delete` and
  `upsert` effects, are synced before the crash, so durable relations hold at tick k what they would have held
  without the restart. It drops no message either: a message in flight to the node is delivered after the restart.
  The losses SEM-071 and SEM-072 allow (unsynced writes, in-flight messages) come only from the fault kinds that
  model them: omissions and partitions, scripted or swarm, and the swarm's storage faults (TEST-002). BENCH-044c
  relies on this reading for its non-vacuous final expectation.
- **Row values** follow PLAN §5.1. A lattice column compares by its revealed value (`LMin<u64>` as an integer,
  `LSet<T>` as an array); an `Option` is written `{ some = v }` or `{ none = true }`; a zero-column relation's row
  is `[]`; `Mod<N>` ids are integers.
- **`[[expect_diag]]` lines.** Every expected diagnostic with a `line` points at a program line that carries a
  trailing `// expect: BLSnnnn` comment, and the construct the diagnostic is about is kept on that one line, so the
  line does not depend on which span an implementation reports as primary.
- **Compile cases are otherwise clean.** A case with a `compile` backend is written so that its `[[expect_diag]]`
  entries are the only diagnostics a conforming compiler reports, warnings included. Where LANGUAGE leaves no
  choice, the case therefore holds whether the runner compares the reported set with the expected set exactly or
  checks inclusion (the M1 gate fixed exact comparison, see "Diagnostics" below). These cases rest on a reading of a
  point LANGUAGE leaves open, recorded in their notes and in `docs/plan/notes/M1.2.md` (Bugs 3–5) with the WP that
  settles it:
  - BENCH-016b expects BLS0406 for a write into the program's own `input`, which the §12 matrix would also call
    BLS0400;
  - BENCH-047a expects BLS0503 for a choice on a same-tick cycle, which §13.3 would also call BLS0502;
  - BENCH-026f expects BLS1005 only on the localized handler the compiler rewrites, not on the two it rejects with
    BLS0805;
  - BENCH-003d expects BLS0500 for the `.ded` frontend's validator failure V1 (ARCHITECTURE §13.12 renders V1–V4
    as user diagnostics; LANGUAGE §20 has no separate `.ded` code);
  - BENCH-045c expects BLS0704 (a refuted algebraic claim, LANGUAGE §16.1) where FEATURES and R12 T6 say the false
    commutativity declaration "fails TEST-015", the oracle's runtime shuffle check, which has no code.

  An implementation that makes the other choice by reporting both codes (016b, 047a) or the extra lints (026f)
  passes an inclusion check and fails an exact comparison. One that reports only the general code (BLS0400 or
  BLS0502) fails both. Either way the triage WP settles the reading; the expectation is not loosened to match.
- **Level-triggered ports.** Dedalus rules that re-derive every tick are ported as `while` handlers, following the
  explicit-persistence idiom of LANGUAGE §7.2. When such a handler reads a scratch whose only writers are
  event-driven, the greatest-fixpoint classification of LANGUAGE §8.5 makes that scratch an event relation and a
  compiler may warn BLS0505 ("write `on`"). No compile case contains such a handler, and the runtime backends do not
  check warnings.
- **Validity as invariants.** Where the literature states a property rather than a value (the seeded `choose!`,
  `choose_rand!`, several choices at once), the program states it as `invariant … : never …;`. A violation aborts
  the tick with BLSR003, which fails the case on every backend and under every simulated seed.
- **Canonical-priority mode.** R12's choice tests give exact answers "in canonical-priority mode", a test-only
  priority table. A manifest cannot select it, so the exact-answer cases write the choice as `choose_least!` /
  `least v`, which is that mode's choice under every seed; a multi-FD site puts a `least` cost equal to the whole
  candidate on one of its literals (BENCH-046c). Separate cases check the seeded `choose!` itself.
- **`[expect_analysis] strata`** is a table from relation names in the source to their stratum under SEM-022: the
  longest same-tick path to the relation counting negative edges, numbered from 0.
- **`unimplemented` lists** hold the case's `features` plus, for the `interp`, `sim`, `ldfi`, `codegen`, `bmc`,
  `smt` and `asp` backends, the feature id that backend reports while it is a placeholder (the "Unimplemented as"
  column of §5.2), because the ratchet requires the reported id to be listed.
- **`features`** lists the FEATURES ids a case exercises: the semantic properties it pins (SEM, ENG, ANA, TEST) and
  the language constructs it uses (LANG). Conventions of the synchronous harness itself (self-sends delivered in the
  next round, quiescence detection) belong to BENCH-000 and M5.2 and are not listed. Some construct ids also carry
  a semantic property, which the case then lists through them:
  - facts and `bootstrap` blocks list LANG-190, whose definition includes their evaluation in the boot tick, and
    `static` relations list LANG-045. The engine's boot tick, SEM-012 (M6.1), is listed only by BENCH-011, the case
    written to pin it. Many cases check tick-0 contents that come from facts or a `bootstrap` block; listing SEM-012
    on each would move their oracle deadlines to M6 (PLAN §5.4), although the oracle runs facts and bootstrap rules
    in the boot tick through their lowering (LANG-190, M5.3) and the sync-round harness (M5.2);
  - a head aggregate (`v = agg!(…)` in a view head) lists LANG-100, which fixes its grouping by the other head terms
    and its deduplicated input, besides the id of its aggregate family (LANG-102, LANG-104, LANG-110, LANG-097, …).

## Expectation vocabularies (fixed at the M1 gate)

PLAN §5.1 names the `[expect_analysis]` keys but not their values, and the four M1 corpus WPs wrote them
independently. The M1 gate fixed one vocabulary (`docs/plan/notes/M1-gate.md`, DECISIONS.md) from their conventions,
converted the cases that used another shape (without changing what they assert), and made
`tools/check_manifests.py` validate it. M5.2 (`xtask corpus --lint` and the `analysis` backend) and M7.3 implement it
as written here; a change to it is a plan change made at a gate.

### `[expect_analysis]`

Every key is optional, and a key that is present is checked. A table keyed by relation checks the relations it
names and no others. Relations are named as in the source: a relation, view or output as declared, `inst.rel` for
one of a module instance. Lists are exact unless the row says otherwise.

| Key | Shape | What is compared |
|---|---|---|
| `strata` | `{ count = n, of = { rel = k } }`, both optional | SEM-022 / ANA-002. `of`: the 0-based stratum of each listed relation, the longest same-tick path to it counting negative edges (ARCHITECTURE §7.2). `count`: the number of strata **including** the final temporal pseudo-stratum that holds every `next` and async rule (Bud's `stratified_rules.length`, R03 §4.3). |
| `points_of_order` | `{ complete, edges, clusters, sites, crossing, free }`; `complete` is required, each list optional | ANA-022. `edges = [{ from, to, kind, reason? }]`: negative dependency edges between relations; `kind` is `negation`, `aggregate`, `deletion`, `choice`, `order`, `lattice_op` (a non-monotone or antitone lattice operation), `reveal`, `delta_read` or `z_boundary`; `reason` qualifies it (`membership`). `clusters = [[rel, …]]`: temporal clusters, each the relations of one component with temporal edges, compared as sets. `sites = [{ at, op }]`: the handler label or view name holding a point of order and its surface construct (a bang call such as `count!` or `reveal!`, or a keyword of LANGUAGE §13.2 such as `not`). With `complete = true` each of these three lists that is present is the exact report of its kind; with `false`, each listed element must be reported and others may be. `crossing = [{ from, to }]`: every dependency path from `from` to `to` passes a point of order. `free = [{ from, to }]`: no such path does. |
| `calm_labels` | `{ outputs = { rel = label }, paths = [{ from, to, label }], races = [{ channels = [a, b], meet, guarded }] }` | ANA-023 / ANA-024. Labels `Bot`, `A`, `N`, `D` (A then N gives D). `outputs`: each listed output's label, the disjunction of its paths. `paths`: the label of each listed path; **not exhaustive** (bud's labeling tests assert that the report *contains* a path, BENCH-092). `races`: the exact set of meetings of two channel streams at `meet` (channels in either order), each with its guarded-asynchrony verdict, for every `meet` relation listed. |
| `certificates` | `{ rel = [kind, …] }` | The exact set of Dedalus-family certificates the output receives: `dedalus_plus` (ANA-025), `dedalus_s` (ANA-026), `dedalus_plus_l` (ANA-141), `dedalus_s_l` (ANA-142). `[]` asserts that none applies. They rest on guarded asynchrony, so they also certify consistency under fair runs. A `confluent = "certified"` verdict can rest on a certificate outside this family: BENCH-091a's message join has no point of order, so it is monotone and confluent by CALM, but its asynchrony is unguarded, and BENCH-089h/089d (the same shape) pin `certificates = []`. |
| `confluent` | `{ rel = verdict }` | ANA-029's `ConfluenceStatus` (ARCHITECTURE §7.3) in CR-29's sense (Ameloot: any two finite runs can be extended to agree): `certified`, `confluent_not_certified` (ANA-143), `not_confluent`, `inconclusive`. SEM-044's "exactly one ultimate model" is read the same way. In a case whose only backend is `sim` (BENCH-307b), the verdict is the simulator's ultimate-model verdict (M8.2). |
| `fair_consistency` | `{ rel = "certified" \| "not_certified" }` | Consistency under fair runs, reported separately from confluence (CR-29). |
| `deterministic` | `{ rel = verdict }` | ANA-029's determinism verdict, one per output: `confluent`, `confluent_given_seals`, `coordinated`, `nondeterministic_by_design`. No M1 case uses it. |
| `finality` | `{ rel = [CLASS, …] }`, or `{ "inst.rel" = { class = "FINITE", ft = { state = value }, abstraction = "…" } }` for an ANA-122 component (`abstraction` optional) | ANA-120: the exact set of classes of the output, spelled as ARCHITECTURE §7.2 spells them: `POS`, `NEG`, `TOP`, `THRESH`, `MIXED`, `FINITE`, `SEALED`, `NEVER` (FEATURES' POS-FINAL … NEVER-FINAL). A list has two classes when an output is final early one way and at a seal the other (`["THRESH", "SEALED"]`). The ANA-122 form is the exact table of free-termination states: for each state of the component's state register that is FT, the query's value there; unlisted states are not FT. |
| `blazes` | a table (BENCH-094) | ANA-040–045. `paths = [{ component, from, to, annotation, gate? }]`: components are module instances (ARCHITECTURE §7.2), `from`/`to` the instance's input and output interfaces, `annotation` one of `CR`, `CW`, `OR`, `OW`, `gate` the sorted gate columns. `streams = { channel = label }`, `sinks = { output = label }` with labels `NDRead`, `Taint`, `Seal(k1, …)`, `Async`, `Run`, `Inst`, `Diverge`. `coordination = [{ at, mechanism = "ordering" \| "sealing", key? }]`. `cycles`: the collapsed components; `collapsed = [{ members, annotation, gate }]`. The lists are exact; the two maps check the entries they list. |
| `reclaimable` | a table (BENCH-095) | ANA-060–066. `reclaimed = { rel = "dr_plus" \| "dr_minus" \| "join_seal" \| "join_pullup" \| "join_semijoin" \| "join_keys" }`; `kept = { rel = reason }` with the reasons listed in `docs/plan/notes/M1.4.md`; `channels = { channel = "arm" }`; `storage = [{ node, rel, tick \| final, rows }]`, the contents of a relation **in the Edelweiss-rewritten program**; `ranges = [{ node, channel \| rel, tick \| final, buckets }]`, ARM buckets as `[other columns…, lo, hi]`. `storage` and `ranges` are run-time facts of the rewritten program, checked where M7.5 runs the original and the rewritten program side by side; each listed entry must hold. |

### Diagnostics

- **`compile`** runs the frontend only (ARCHITECTURE §13.1's frontend phases) and compares **every** diagnostic it
  reports, warnings included, with `[[expect_diag]]` exactly, as a multiset of codes (with `line` and `severity`
  where given). An empty list expects no diagnostic (PLAN §5.2). The M1.2 and M1.4 compile cases were written this
  way. The M1.3 compile cases list only the errors they are about; when a conforming frontend also reports a
  warning for one of them, the triage WP adds that warning or changes the program so it no longer draws it, and
  never removes an expected error.
- **`analysis`** compares the diagnostics of `blossom-analysis` (stratification, CALM, determinism, finality,
  lints). Every listed diagnostic must be reported, and, for the codes of the ANA features the case lists in
  `features`, no other: listing ANA-008 makes BLS1002 exhaustive (BENCH-093e: exactly two, BENCH-093g: none),
  listing ANA-120 does the same for BLS0705, and ANA-002 for BLS0502/BLS0503. The codes of analyses the case does
  not list are ignored, so a case does not depend on the precision of unrelated lints. M5.2 defines the table from
  codes to ANA features that this needs, from LANGUAGE §20 and ARCHITECTURE §7.2.
- **Runtime backends** compare no diagnostics. A hard runtime error is expected only through `[[expect_error]]`;
  any other one fails the case.
- **`line`** is the line of the construct the diagnostic's primary span names: the statement for a statement-level
  error, the relation's declaration for a relation-level report. The M1.2 cases also mark each such line with a
  trailing `// expect: BLSnnnn` comment (see above). A case leaves `line` out where the construct is not one line
  (a cycle through several statements).

### Quiescence

`quiescent_from = q` holds when, from tick q on, on every node, no relation (persistent or tick-local) differs from
the previous tick and no message is in flight. This implies ARCHITECTURE §4.11's digest condition at every tick from
q on. q may be later than the first quiescent tick. A runner that stops at quiescence (`stop = "quiescent"`) before
q checks the claim through the determinism of an empty tick: a quiescent node repeats its last tick.

## Checking manifests

```sh
python3 tests/corpus/tools/check_manifests.py core
python3 tests/corpus/tools/check_manifests.py core --require-ids BENCH-001..048 --skip BENCH-025
```

The validator checks every key and value shape of schema v1, including the `[expect_analysis]` vocabulary above,
that each `id` exists in FEATURES.md with the stated priority and each listed feature exists, that referenced files
exist, and, with `--require-ids`, that every P0/P1 id in the range has a case. It does not recompute `until`; the
corpus runner does (§5.4).
