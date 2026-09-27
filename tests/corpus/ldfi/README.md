# The LDFI golden corpus (`tests/corpus/ldfi`)

This area holds the golden cases for LDFI ("Molly-2", FEATURES.md §7.2) and for verdict parity with Molly
(FEATURES.md §11.5, BENCH-130–137; ARCHITECTURE §8.7, §11.5). It was written by work package M1.5 before any of Blossom
existed: every expectation comes from the literature (the LDFI paper, SIGMOD 2015; the Netflix LDFI paper, SoCC 2016;
Nemo, CIDR 2019; the research report `docs/research/06-molly-ldfi.md`, R06), never from running an implementation.
The cases follow manifest schema v1 (docs/design/PLAN.md §5) and are validated by
`python3 tests/corpus/tools/check_manifests.py ldfi`.

- [`molly/`](molly/README.md): the cases, the programs they include (`molly/lib/`) and golden data tables that
  manifests cannot hold (`molly/golden/`).
- [`tools/ldfi_ref.py`](tools/ldfi_ref.py): a reference checker for this corpus (below).
- [`third_party/molly/`](../../../third_party/molly/README.md): where the programs come from and under which terms.
  The Molly repository has no license, so its programs are re-derived; Nemo's case studies are vendored under
  GPL-3.0.

## Conventions

These refine PLAN.md §5 for LDFI cases; where §5 is silent they are this corpus's choices, and
`docs/plan/notes/M1.5.md` lists them for the runner (M5.2) and LDFI (M8.1) work packages.

**Programs.** Every case runs `program.ded`, a Molly-dialect program (LANGUAGE §21.1, the `.ded` frontend, LANG-220).
For the re-derived Molly programs it is a few `include` lines, resolved relative to the case directory, that assemble
a protocol, its input and its specification from `molly/lib/`; the Nemo case studies are complete files. Every `.ded`
relation is located at its first column; the node names of a case are the `nodes` of its `[expect_ldfi]` (or its
`[deploy]` nodes) and may be upper case (`"C"`, `"Z"`, `"FF"`), as in Molly and Nemo.

**LDFI cases** (`[backend.ldfi]`, floor M8). `[expect_ldfi]` gives the failure spec (EOT, EFF, maxCrashes, nodes),
`crash_view = "molly"` (CrashView::MollyContinue, the `.ded` profile: a crashed node keeps receiving and computing and
sends nothing from its crash tick; R06 §3.4, ARCHITECTURE §8.1), and the verdict. Molly round k is tick k (CR-13); an
omission O(from,to,t) is admissible iff from != to and 1 <= t < EFF (CR-21); at most `crashes` nodes crash. The
oracle is Molly's (TEST-022): a run is a counterexample iff some `post` tuple of the failure-free run is missing from
`post` at EOT while present in `pre`.

- `falsifiers` (only where the source states them): the union over the failure-free run's `post` tuples g of the
  **Appendix-B-minimal falsifiers** of g (TEST-028: one problem per goal, results unioned). A falsifier of g is an
  admissible fault set after which g does not hold at EOT; fault sets are compared by the clock facts they remove
  (ARCHITECTURE §8.3): O(f,t,s) removes clock(f,t,s), and C(n,c) removes clock(n,x,s) for every x != n and every
  s >= c. Each set is written with `O(from,to,send_tick)` and `C(node,crash_tick)` labels; sets and labels are
  compared as sets. Never raw Molly output (ARCHITECTURE §8.7).
- `runs_max` (only where BENCH-136 publishes a count, and for the claims that single out a count): the number of
  concrete executions, the failure-free run included, up to and including the first counterexample for
  `counterexample`, or until the hypotheses are exhausted for `no_counterexample`.

**Failure-free cases** (`[backend.oracle]`, floor M5) run one program without faults so that the `.ded` frontend and
the oracle can be checked before LDFI exists (PLAN §8, M1.5). They include the protocol and its input but not its
specification. `[deploy]` lists the nodes with `role = "Node"`: a `.ded` program has no roles, and every node runs
every rule. `[run] ticks = n` runs ticks 0 to n − 1 (tick 0 has no events; the `@1` facts arrive at tick 1). `[[expect]]`
rows omit the location column, which is the node named by `node`.

**Status.** Every backend starts `unimplemented` with `unimplemented` = the case's `features` and `until` computed from
`docs/design/plan.json` as PLAN §5.4 defines. The ldfi cases need M8 (TEST-020–029, M8.1); the run-count cases of
BENCH-136 and BENCH-130q also list the P1 search reductions (TEST-030–032, M9.4) and wait for M9.

## Checking the corpus against the literature

`tools/ldfi_ref.py` is a stdlib-only reference for this corpus: an evaluator of Molly's dialect under Molly's
synchronous semantics (with the MollyContinue crash view), an exhaustive search over every admissible fault schedule
(the ground truth for a verdict), an exhaustive computation of Appendix-B-minimal falsifiers, and a lineage-driven
search that follows ARCHITECTURE §8.3–§8.5. It is a validation aid for corpus authors and triage: it proves that a
re-derived program reproduces its published verdict and that stated falsifier sets are exact. It is not Blossom and
no expectation is taken from it; a disagreement means a program or the tool is wrong, and the literature decides.

```sh
python3 -B tests/corpus/ldfi/tools/ldfi_ref.py selftest
python3 -B tests/corpus/ldfi/tools/ldfi_ref.py check                      # every case (slow: see below)
python3 -B tests/corpus/ldfi/tools/ldfi_ref.py check tests/corpus/ldfi/molly/BENCH-131*
python3 -B tests/corpus/ldfi/tools/ldfi_ref.py verdict tests/corpus/ldfi/molly/BENCH-130a-simple-deliv-6-3-0/program.ded \
    --eot 6 --eff 3 --crashes 0 --nodes a,b,c
```

`check` reports the exhaustive verdict, the lineage-driven verdict and run count, and the falsifier comparison of
every case. The exhaustive search of the largest configurations (Paxos 7/6/1, bully 10/9/1, ack-deliv 8/7/1) takes
minutes; Flux 22/21/1 is beyond it, and its verdict rests on Flux's safety argument and on exhaustive checks at
smaller bounds (docs/plan/notes/M1.5.md).

## Cases

EOT/EFF/crashes are the failure spec; "derived" says whether the program is re-derived (yes) or vendored verbatim.

| Case | Derived | Backend | EOT/EFF/crashes | Expectations | Until |
|---|---|---|---|---|---|
| `BENCH-130a-simple-deliv-6-3-0` | yes | ldfi | 6/3/0 | CE, 2 falsifier sets | M8 |
| `BENCH-130b-simple-deliv-4-2-0` | yes | ldfi | 4/2/0 | CE, 2 falsifier sets | M8 |
| `BENCH-130c-retry-deliv-6-3-0` | yes | ldfi | 6/3/0 | no CE | M8 |
| `BENCH-130d-retry-deliv-25-23-0` | yes | ldfi | 25/23/0 | no CE | M8 |
| `BENCH-130e-retry-deliv-6-3-1` | yes | ldfi | 6/3/1 | CE | M8 |
| `BENCH-130f-retry-deliv-4-2-1` | yes | ldfi | 4/2/1 | CE, 2 falsifier sets | M8 |
| `BENCH-130g-classic-deliv-6-3-0` | yes | ldfi | 6/3/0 | CE | M8 |
| `BENCH-130h-classic-deliv-5-3-0` | yes | ldfi | 5/3/0 | CE | M8 |
| `BENCH-130i-classic-deliv-6-0-2` | yes | ldfi | 6/0/2 | no CE | M8 |
| `BENCH-130j-redun-deliv-6-3-0` | yes | ldfi | 6/3/0 | no CE | M8 |
| `BENCH-130k-redun-deliv-6-3-1` | yes | ldfi | 6/3/1 | no CE | M8 |
| `BENCH-130l-redun-deliv-8-6-1` | yes | ldfi | 8/6/1 | no CE | M8 |
| `BENCH-130m-redun-deliv-11-10-1` | yes | ldfi | 11/10/1 | no CE | M8 |
| `BENCH-130n-ack-deliv-6-3-1` | yes | ldfi | 6/3/1 | no CE | M8 |
| `BENCH-130o-ack-deliv-8-6-1` | yes | ldfi | 8/6/1 | no CE | M8 |
| `BENCH-130p-ack-deliv-8-7-1` | yes | ldfi | 8/7/1 | no CE | M8 |
| `BENCH-130q-redun-deliv-6-3-1-failure-free-certifies` | yes | ldfi | 6/3/1 | no CE, runs ≤ 1 | M9 |
| `BENCH-130r-simple-deliv-4-2-0-figure-3-spec` | yes | ldfi | 4/2/0 | CE | M8 |
| `BENCH-130s-simple-deliv-failure-free` | yes | oracle | — | 5 row expectations | M5 |
| `BENCH-130t-retry-deliv-failure-free` | yes | oracle | — | 4 row expectations | M5 |
| `BENCH-130u-classic-deliv-failure-free` | yes | oracle | — | 7 row expectations | M5 |
| `BENCH-130v-redun-deliv-failure-free` | yes | oracle | — | 3 row expectations | M5 |
| `BENCH-130w-ack-deliv-failure-free` | yes | oracle | — | 7 row expectations | M5 |
| `BENCH-131a-2pc-7-3-0` | yes | ldfi | 7/3/0 | no CE | M8 |
| `BENCH-131b-2pc-6-3-1` | yes | ldfi | 6/3/1 | CE | M8 |
| `BENCH-131c-2pc-6-0-1` | yes | ldfi | 6/0/1 | CE | M8 |
| `BENCH-131d-2pc-6-0-2` | yes | ldfi | 6/0/2 | CE | M8 |
| `BENCH-131e-2pc-5-0-1` | yes | ldfi | 5/0/1 | CE | M8 |
| `BENCH-131f-2pc-optimist-6-0-1` | yes | ldfi | 6/0/1 | CE | M8 |
| `BENCH-131g-2pc-optimist-6-0-2` | yes | ldfi | 6/0/2 | CE | M8 |
| `BENCH-131h-2pc-timeout-optimist-6-0-1` | yes | ldfi | 6/0/1 | no CE | M8 |
| `BENCH-131i-2pc-timeout-optimist-6-0-2` | yes | ldfi | 6/0/2 | no CE | M8 |
| `BENCH-131j-2pc-timeout-6-0-1` | yes | ldfi | 6/0/1 | CE | M8 |
| `BENCH-131k-2pc-timeout-6-0-2` | yes | ldfi | 6/0/2 | CE | M8 |
| `BENCH-131l-2pc-ctp-6-0-1` | yes | ldfi | 6/0/1 | CE | M8 |
| `BENCH-131m-2pc-ctp-6-0-2` | yes | ldfi | 6/0/2 | CE | M8 |
| `BENCH-131n-2pc-ctp-8-0-1` | yes | ldfi | 8/0/1 | CE | M8 |
| `BENCH-131o-3pc-8-0-1` | yes | ldfi | 8/0/1 | no CE | M8 |
| `BENCH-131p-3pc-8-0-2` | yes | ldfi | 8/0/2 | CE | M8 |
| `BENCH-131q-3pc-9-7-1` | yes | ldfi | 9/7/1 | CE | M8 |
| `BENCH-131r-2pc-failure-free` | yes | oracle | — | 8 row expectations | M5 |
| `BENCH-131s-2pc-ctp-failure-free` | yes | oracle | — | 7 row expectations | M5 |
| `BENCH-131t-3pc-failure-free` | yes | oracle | — | 10 row expectations | M5 |
| `BENCH-132a-kafka-7-4-1` | yes | ldfi | 7/4/1 | CE | M8 |
| `BENCH-132b-kafka-7-4-0` | yes | ldfi | 7/4/0 | no CE | M8 |
| `BENCH-132c-kafka-6-4-1` | yes | ldfi | 6/4/1 | CE | M8 |
| `BENCH-132d-kafka-failure-free` | yes | oracle | — | 6 row expectations | M5 |
| `BENCH-133a-paxos-synod-8-3-1` | yes | ldfi | 8/3/1 | no CE | M8 |
| `BENCH-133b-paxos-synod-7-6-1` | yes | ldfi | 7/6/1 | no CE | M8 |
| `BENCH-133c-bully-le-10-9-1` | yes | ldfi | 10/9/1 | no CE | M8 |
| `BENCH-133d-flux-22-21-1` | yes | ldfi | 22/21/1 | no CE | M8 |
| `BENCH-133e-paxos-synod-failure-free` | yes | oracle | — | 6 row expectations | M5 |
| `BENCH-133f-bully-le-failure-free` | yes | oracle | — | 6 row expectations | M5 |
| `BENCH-133g-flux-failure-free` | yes | oracle | — | 7 row expectations | M5 |
| `BENCH-134a-nemo-pb-asynchronous-6-4-1` | no | ldfi | 6/4/1 | CE | M8 |
| `BENCH-134b-nemo-pb-asynchronous-ack-log-repair-6-4-1` | yes | ldfi | 6/4/1 | no CE | M8 |
| `BENCH-134c-nemo-ca-2083-6-4-0` | no | ldfi | 6/4/0 | CE | M8 |
| `BENCH-134d-nemo-zk-1270-6-3-0` | no | ldfi | 6/3/0 | CE | M8 |
| `BENCH-134e-nemo-zk-1270-repair-6-3-0` | yes | ldfi | 6/3/0 | no CE | M8 |
| `BENCH-134f-nemo-mr-2995-8-4-1` | no | ldfi | 8/4/1 | CE | M8 |
| `BENCH-134g-nemo-ca-2434-7-5-1` | no | ldfi | 7/5/1 | CE | M8 |
| `BENCH-134h-nemo-mr-3858-8-4-1` | no | ldfi | 8/4/1 | CE | M8 |
| `BENCH-135a-netflix-toy-3-0-2` | yes | ldfi | 3/0/2 | CE, 2 falsifier sets | M8 |
| `BENCH-135b-two-proofs-formula-4-3-1` | yes | ldfi | 4/3/1 | CE, 1 falsifier set | M8 |
| `BENCH-135c-two-proofs-formula-4-2-1` | yes | ldfi | 4/2/1 | CE, 1 falsifier set | M8 |
| `BENCH-135d-two-proofs-formula-4-0-1` | yes | ldfi | 4/0/1 | no CE, 0 falsifier sets | M8 |
| `BENCH-135e-two-proofs-formula-4-0-2` | yes | ldfi | 4/0/2 | CE, 1 falsifier set | M8 |
| `BENCH-135f-provenance-join-firings` | yes | oracle | — | 2 row expectations | M5 |
| `BENCH-135g-provenance-join-firings-lineage-3-2-0` | yes | ldfi | 3/2/0 | CE, runs ≤ 2, 1 falsifier set | M8 |
| `BENCH-135h-provenance-wildcard-derivations` | yes | oracle | — | 2 row expectations | M5 |
| `BENCH-135i-provenance-wildcard-derivations-lineage-3-2-0` | yes | ldfi | 3/2/0 | CE, runs ≤ 2, 1 falsifier set | M8 |
| `BENCH-135j-provenance-aggregate-contributors` | yes | oracle | — | 1 row expectation | M5 |
| `BENCH-135k-provenance-aggregate-contributors-lineage-3-2-0` | yes | ldfi | 3/2/0 | CE, 4 falsifier sets | M8 |
| `BENCH-135l-provenance-aggregate-grouping-3-2-0` | yes | ldfi | 3/2/0 | CE, 2 falsifier sets | M8 |
| `BENCH-135m-provenance-aggregate-grouping-failure-free` | yes | oracle | — | 2 row expectations | M5 |
| `BENCH-135n-semiring-pods07-query` | yes | oracle | — | 1 row expectation | M5 |
| `BENCH-135o-souffle-points-to` | yes | oracle | — | 2 row expectations | M5 |
| `BENCH-136a-simple-deliv-4-2-0-runs` | yes | ldfi | 4/2/0 | CE, runs ≤ 2 | M9 |
| `BENCH-136b-retry-deliv-4-2-1-runs` | yes | ldfi | 4/2/1 | CE, runs ≤ 3 | M9 |
| `BENCH-136c-classic-deliv-5-3-0-runs` | yes | ldfi | 5/3/0 | CE, runs ≤ 5 | M9 |
| `BENCH-136d-2pc-5-0-1-runs` | yes | ldfi | 5/0/1 | CE, runs ≤ 2 | M9 |
| `BENCH-136e-2pc-ctp-8-0-1-runs` | yes | ldfi | 8/0/1 | CE, runs ≤ 3 | M9 |
| `BENCH-136f-3pc-9-7-1-runs` | yes | ldfi | 9/7/1 | CE, runs ≤ 55 | M9 |
| `BENCH-136g-kafka-6-4-1-runs` | yes | ldfi | 6/4/1 | CE, runs ≤ 38 | M9 |
| `BENCH-136h-redun-deliv-11-10-1-runs` | yes | ldfi | 11/10/1 | no CE, runs ≤ 11 | M9 |
| `BENCH-136i-ack-deliv-8-7-1-runs` | yes | ldfi | 8/7/1 | no CE, runs ≤ 673 | M9 |
| `BENCH-137a-raft-election-8-5-0` | yes | ldfi | 8/5/0 | no CE | M8 |
| `BENCH-137b-raft-vote-twice-8-5-0` | yes | ldfi | 8/5/0 | CE | M8 |
| `BENCH-137c-raft-commit-8-4-0` | yes | ldfi | 8/4/0 | no CE | M8 |
| `BENCH-137d-raft-eager-commit-8-4-0` | yes | ldfi | 8/4/0 | CE | M8 |
| `BENCH-137e-raft-commit-12-6-0` | yes | ldfi | 12/6/0 | no CE | M8 |
| `BENCH-137f-raft-no-log-check-12-6-0` | yes | ldfi | 12/6/0 | CE | M8 |
| `BENCH-137g-raft-commit-12-6-1` | yes | ldfi | 12/6/1 | no CE | M8 |
| `BENCH-137h-raft-election-10-7-1` | yes | ldfi | 10/7/1 | no CE | M8 |
| `BENCH-137i-negative-support-3-2-0` | yes | ldfi | 3/2/0 | CE, 1 falsifier set | M8 |
| `BENCH-137j-raft-failure-free` | yes | oracle | — | 7 row expectations | M5 |
