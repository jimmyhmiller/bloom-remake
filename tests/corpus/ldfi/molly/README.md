# The Molly LDFI corpus: programs and golden data

The case directories next to this file (`BENCH-130a-…` to `BENCH-137j-…`) are the golden corpus for LDFI verdict
parity with Molly (FEATURES.md §11.5, BENCH-130–137; ARCHITECTURE §8.7, §11.5). The conventions they follow, and an
index of every case, are in [`../README.md`](../README.md). This file describes the programs the cases include and the
golden data tables in `golden/`. Where each program comes from, and under which terms, is recorded in
[`third_party/molly/README.md`](../../../../third_party/molly/README.md).

Every case's `program.ded` is a Molly-dialect program (LANGUAGE §21.1). For the re-derived Molly programs it is a few
`include` lines that assemble one protocol, its input and its specification from `lib/`, relative to the case
directory, exactly as Molly assembles a run from several files. The vendored Nemo case studies are complete programs.

## `lib/`: re-derived programs

Every file starts with a comment that cites its source and says what it models. Relations are located at their first
column; every protocol rule reads relations of a single node, and specification rules (`pre`, `post` and the helpers
only they use) may join across nodes and read the `crash` oracle, as ARCHITECTURE §13.12 requires. The first body
predicate of every rule is located at a node (or is `_`), so Molly's clock guard and a global spec engine evaluate
the specifications alike.

| Directory | Files | What they are |
|---|---|---|
| `delivery/` | `simple_deliv.ded`, `retry_deliv.ded`, `redun_deliv.ded`, `classic_deliv.ded`, `ack_deliv.ded`, `group.ded`, `delivery_spec.ded`, `fig3_spec.ded` | The reliable-broadcast family of LDFI §3 on the paper's input (a, b, c fully connected; a broadcasts "data" at time 1). `delivery_spec.ded` is the delivery oracle with a monotone `post` (proved equivalent to Molly's on this input in the file); `fig3_spec.ded` is Figure 3 verbatim. |
| `commit/` | `two_pc.ded`, `two_pc_timeout.ded`, `ctp.ded`, `three_pc.ded`, `group.ded`, `commit_spec.ded`, `optimist_spec.ded` | Two-phase commit, 2PC with a coordinator timeout, the collaborative termination protocol and three-phase commit (LDFI §5.1.1), with Molly's termination and agreement checks and the "optimist" variant. |
| `kafka/` | `replication.ded`, `group.ded`, `kafka_spec.ded` | Kafka 0.8 ISR replication with a sketched Zookeeper and client (LDFI §1.1, §5.1.3, Figure 10) and the durability spec. |
| `paxos/` | `synod.ded`, `group.ded`, `paxos_spec.ded` | Single-decree Paxos with the paper's two proposals and seeds, and Paxos agreement observed through learners. |
| `bully/` | `election.ded`, `group.ded`, `bully_spec.ded` | A bully leader election (highest id wins, one vote per node, majority elects) with the agreement spec. |
| `flux/` | `pairs.ded`, `group.ded`, `flux_spec.ded` | Flux process pairs (ingress, primary and secondary replica with takeover, egress) and Flux's no-loss, no-duplication, in-order guarantee at the egress. |
| `raft/` | `raft.ded`, `vote_checked.ded`, `vote_unchecked.ded`, `vote_twice.ded`, `commit_majority.ded`, `commit_eager.ded`, `group.ded`, `election_timing.ded`, `commit_timing.ded`, `raft_spec.ded` | A compact Raft (elections with terms and the log up-to-date check, whole-log AppendEntries, majority commit of current-term entries) split into modules so that seeded bugs replace one rule each; two input scenarios; Election Safety and State Machine Safety as the spec. |
| `unit/` | `netflix_toy.ded`, `two_proofs.ded`, `join_firings*.ded`, `wildcard_derivations*.ded`, `agg_contributors*.ded`, `agg_grouping.ded`, `negative_support.ded`, `pods07.ded`, `points_to.ded` | The unit-level examples of BENCH-135 and BENCH-137: the SoCC'16 Netflix lineage, the LDFI §4.3 formula, the properties of Molly's provenance tests on programs written for the corpus (Molly's test programs are not copied, see `third_party/molly/README.md`) and their `_net` variants that make provenance counts observable to LDFI, a negative-support test written the same way, the PODS'07 semiring query and the Soufflé points-to analysis. |
| `util/` | `timer.ded` | A logical countdown timer (`arm_timer`, `cancel_timer`, `timeout`) with a purpose column. |

## `golden/`: golden data that manifests cannot hold

Manifest schema v1 (PLAN.md §5) states verdicts, run counts, falsifier sets and relation contents. BENCH-135 also
pins values that belong to the implementation's units rather than to a run; they are kept here as TOML tables, each
naming its source and the feature (and so the work package) that consumes it:

| File | Consumer | Contents |
|---|---|---|
| `gross_estimate.toml` | TEST-035 (M9.4) | Molly's run-count estimate: its unit-test values and the exact values behind every Combinations entry of Figures 12 and 13. |
| `fault_normalization.toml` | TEST-028 (M8.1) | Turning a model into a fault schedule: Molly's unit test, with the result for each crash view. |
| `symmetry.toml` | TEST-032 (M9.4) | Molly's symmetry-reduction unit tests. |
| `provenance.toml` | TEST-023, ENG-112 (M7.1, M8.1); ENG-114 (M7.6); ENG-111 (M5.7) | Rule firings, derivations and aggregate contributors of Molly's provenance tests, the PODS'07 provenance polynomials and bag values, and the Soufflé minimal proof heights. |
| `run_counts.toml` | TEST-029–TEST-032 (M8.1, M9.4) | Every run count Figures 12 and 13 publish, and the case that pins it or the reason it is not pinned. |
