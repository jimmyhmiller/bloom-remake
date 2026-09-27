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
- **Faults.** `[[fault]] kind = "restart"` at tick k means the node's tick k is the first tick of a new incarnation:
  durable relations are reloaded, everything else starts empty, and `boot()` and `recovered()` hold at tick k.
- **Row values** follow PLAN §5.1. A lattice column compares by its revealed value (`LMin<u64>` as an integer,
  `LSet<T>` as an array); an `Option` is written `{ some = v }` or `{ none = true }`; a zero-column relation's row
  is `[]`; `Mod<N>` ids are integers.
- **`[[expect_diag]]` lines.** Every expected diagnostic with a `line` points at a program line that carries a
  trailing `// expect: BLSnnnn` comment, and the construct the diagnostic is about is kept on that one line, so the
  line does not depend on which span an implementation reports as primary.
- **Compile cases are otherwise clean.** A case with a `compile` backend is written so that its `[[expect_diag]]`
  entries are the only diagnostics a conforming compiler reports, warnings included; the case holds whether the
  runner compares the reported set with the expected set exactly or checks inclusion.
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
  next round, quiescence detection) belong to BENCH-000 and M5.2 and are not listed.

## Checking manifests

```sh
python3 tests/corpus/tools/check_manifests.py core
python3 tests/corpus/tools/check_manifests.py core --require-ids BENCH-001..048 --skip BENCH-025
```

The validator checks every key and value shape of schema v1, that each `id` exists in FEATURES.md with the stated
priority and each listed feature exists, that referenced files exist, and, with `--require-ids`, that every P0/P1 id
in the range has a case. It does not recompute `until`; the corpus runner does (§5.4).
