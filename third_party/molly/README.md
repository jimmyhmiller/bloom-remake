# Third-party material of the Molly LDFI corpus

This directory records where the programs of `tests/corpus/ldfi/molly/` come from and under which terms. It is owned by
work package M1.5 (docs/design/PLAN.md §8).

## Molly (github.com/palvaro/molly): nothing vendored

The Molly repository (checked on 2026-09-27; last commit `a3a6d79`, 2018-11-04) carries **no license**: GitHub's
license API reports none and the tree has no LICENSE, COPYING or license header. Without a license its files cannot
be redistributed, so, as PLAN.md §8 (M1.5, "Vendoring") prescribes, no Molly file is copied into this repository.
Every Molly case of the corpus is **re-derived** (`derived = true` in its manifest) from the published papers and
from the research report `docs/research/06-molly-ldfi.md` (R06):

- programs printed in the LDFI paper (Alvaro, Rosen, Hellerstein, SIGMOD 2015) are used as printed and cited by
  figure: simple-deliv (Figure 2), the reliable-broadcast spec (Figure 3), ack-deliv (Figure 5), and the one-line
  additions of §3.1.2 and §3.1.3;
- the other programs (classic-deliv, the commit protocols, Kafka, Paxos, the bully election, Flux, Raft, the
  negative-support test, the timers and every specification) are written for the corpus from the papers' prose and
  R06's descriptions, and checked against the published verdicts with `tests/corpus/ldfi/tools/ldfi_ref.py`;
- the unit tests of Molly's `ProvenanceSuite` and its `negative_support_test.ded` (BENCH-135, BENCH-137) are
  represented by the property each checks (two firings through a join column, two derivations through a wildcard
  column, aggregate contributors, grouping with an extra body variable, lineage only through negation), each on a
  program written for the corpus under its own relation names and data (`tests/corpus/ldfi/molly/lib/unit/`). The
  first versions of these five files transcribed Molly's test inputs, which R06 §12.6 quotes; the M1 gate replaced
  them (`docs/plan/notes/M1-gate.md`).

Molly's expectations (its CounterexampleSuite verdicts, the Figure 12 and 13 counts, the unit-test values) are
facts reported in the literature and in R06; the corpus states them as data with their sources.

## Nemo (github.com/numbleroot/nemo): case studies vendored under GPL-3.0

The Nemo repository is licensed under the **GNU General Public License, version 3**; the license text is
[`nemo/LICENSE`](nemo/LICENSE), copied from the repository. Its six protocol case studies (the programs of Oldenburg,
Zhu, Ramasubramanian, Alvaro, "Fixed It For You: Protocol Repair Using Lineage Graphs", CIDR 2019) are vendored
**verbatim**, byte for byte, from `case-studies/` at commit `18fe0a36a2f647cc15f58eb866a148a79ba64bfb`
(2019-12-10), each as the `program.ded` of one corpus case:

| Upstream file | Corpus file | SHA-256 |
|---|---|---|
| `case-studies/pb_asynchronous.ded` | `tests/corpus/ldfi/molly/BENCH-134a-nemo-pb-asynchronous-6-4-1/program.ded` | `0325c825b0ec9f31c3bd1b20fc5253b6294cf0fcf2f5d50d96f522b898070104` |
| `case-studies/CA-2083-hinted-handoff.ded` | `tests/corpus/ldfi/molly/BENCH-134c-nemo-ca-2083-6-4-0/program.ded` | `f7b97415fe601ad1ed9a7e696e6386e4b6a51606a6e74e5244eb50f58ea3f2c0` |
| `case-studies/ZK-1270-racing-sent-flag.ded` | `tests/corpus/ldfi/molly/BENCH-134d-nemo-zk-1270-6-3-0/program.ded` | `023c3c6009473e4bcb571cbf5e916972d2b5a7023d3d1751f14cc4adc449c97c` |
| `case-studies/MR-2995-failed-after-expiry.ded` | `tests/corpus/ldfi/molly/BENCH-134f-nemo-mr-2995-8-4-1/program.ded` | `098713a42a2f783c05e9c0b431aeef4f0ec6167fb7956110de2a6ab4fd9ab4b0` |
| `case-studies/CA-2434-bootstrap-synchronization.ded` | `tests/corpus/ldfi/molly/BENCH-134g-nemo-ca-2434-7-5-1/program.ded` | `442945ae150772057cf9d9e1ac270293103dfb92edd7f347d475152cace20791` |
| `case-studies/MR-3858-hadoop.ded` | `tests/corpus/ldfi/molly/BENCH-134h-nemo-mr-3858-8-4-1/program.ded` | `3ea297bfbfa9c884ee9ba94dd87387964c0d459df325c5e993c1317dde3250ad` |

Two corpus programs are **modified versions** of those files (GPL-3.0 §5): each carries a prominent notice at its top
stating the modification, its date (2026-09-27) and its source, marks every changed line `REPAIR`, and remains under
GPL-3.0:

| Modified from | Corpus file | Modification |
|---|---|---|
| `case-studies/pb_asynchronous.ded` | `tests/corpus/ldfi/molly/BENCH-134b-nemo-pb-asynchronous-ack-log-repair-6-4-1/program.ded` | the `ack_log` repair of Nemo §4 |
| `case-studies/ZK-1270-racing-sent-flag.ded` | `tests/corpus/ldfi/molly/BENCH-134e-nemo-zk-1270-repair-6-3-0/program.ded` | the `success(L) :- sent_flag(L), ack(F)` repair of Nemo §4 |

The vendored and modified files are test inputs: they are read by the corpus runner and by the checker, and they are
not compiled into, linked with or distributed as part of any Blossom crate. The repository itself has no license yet
(docs/DECISIONS.md, D19); these eight files stay under GPL-3.0 whatever license is chosen for the rest.
