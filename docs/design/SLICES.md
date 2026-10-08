# Blossom delivery by vertical slices

Status: **normative for delivery order from 2026-09-28**. It replaces the milestone *ordering* of PLAN.md §4–§8 (M3
onward) and plan.json. It does not replace what anything means: LANGUAGE.md, ARCHITECTURE.md and FEATURES.md are still
the specification, the golden corpus is still the executable spec, and CONVENTIONS.md still governs code.

## Why

After M1 and M2 (roughly 31k lines of Rust in six crates, plus the corpus) no Blossom program ran. The milestone plan
built each crate to its full specification before anything consumed it: the first program would run on the oracle at
the end of M5, the first multi-node run under simulation in M7–M8, Raft in M9. Every crate interface was fixed from the
design documents alone, so the first test of whether they fit together would come as late, and as expensively, as
possible. The parallel work packages kept many agents busy without shortening the critical path (parse → resolve →
lower → evaluate → time → network), which is sequential.

The user chose (2026-09-28): **deliver by vertical slices, Molly parity first.**

## Rules

1. **A slice is gated by behaviour.** Its gate is a list of corpus cases and CLI demos that run end to end from
   source text. Crate-level acceptance criteria from PLAN.md §8 apply only as far as the slice exercises them.
2. **Build what the slice exercises, fully.** No layer is built ahead of a consumer, and what is built is built
   properly: real data structures, real error handling, tests. Everything outside the slice fails loudly with
   `Unimplemented` / BLS0908 / exit code 7 naming the feature id and the slice that will deliver it (the no-stub rule
   of CONVENTIONS.md is unchanged; only the "owning WP" in the message becomes the owning slice).
3. **Interfaces are provisional until a second consumer exists.** When a slice finds that ARCHITECTURE.md's shape is
   wrong, it changes the code and records the deviation in its notes file; the next slice that consumes the
   interface a second time freezes it.
4. **The corpus ratchet still applies.** A case that passes on a backend has its manifest status flipped to `pass`
   and must keep passing; a case never moves back to `unimplemented`.
5. **One driver per slice's critical path.** Work fans out to parallel agents only where it is genuinely wide
   (corpus triage, independent standard-library modules, independent systems).
6. **The slice gate** is `scripts/ci.sh gate` (fmt, workspace Clippy, all tests, corpus lint, cargo-deny) plus the
   slice's own acceptance command, followed by an adversarial review of the slice's diff. A slice ends with one commit
   on `main` titled `Slice N: <title>`, and `docs/plan/MILESTONE` names the slice being built.

## The slices

### Slice 1: Molly parity (LDFI end to end on `.ded` programs)

Molly's programs, run unchanged, through the whole LDFI pipeline, reproducing the published verdicts.

| Component | Crate / module | Specification |
|---|---|---|
| Molly-dialect lexer, parser, `include` | `blossom-syntax::ded` | LANGUAGE §21.1, LANG-220 |
| Lowering `.ded` to the IR: location column, rule kinds, type inference, aggregates, `crash`, `pre`/`post` as the implicit spec, `@k` facts as input events | `blossom-front::ded` | LANGUAGE §21.1, ARCHITECTURE §8.1, §13 |
| Stratification sufficient for the oracle (its own naive algorithm) | `blossom-oracle` | ARCHITECTURE §11.2 |
| The naive per-node tick evaluator over the IR, with a firing log (Tier C, literal profile) for provenance | `blossom-oracle` | ARCHITECTURE §11.2, §4.9, §3.10 |
| Synchronous-round world: nodes, message delivery at t+1, omissions, crashes under `CrashView::MollyContinue`, fault schedules | `blossom-sim` (minimal) | ARCHITECTURE §6, §8.1 |
| Provenance graph from the firing and message logs | `blossom-prov` | ARCHITECTURE §8.2 |
| Hazard encoding, Plaisted–Greenbaum CNF, crash order variables, crash budget totalizer, minimal enumeration, driver, Molly's oracle, reports | `blossom-ldfi` | ARCHITECTURE §8.3–§8.5, §8.7 |
| `blossom sim <file.ded> --nodes … --ticks n [--omit a:b:1] [--crash a:2]` (prints every node's relations tick by tick, and `pre`/`post`) and `blossom ldfi <files> --eot --eff --crashes --nodes` | `blossom-cli` | ARCHITECTURE §12 (`run` stays the production runtime's command) |
| Corpus runner for `.ded` cases (`[backend.oracle]` failure-free and `[backend.ldfi]`), with the ratchet | `blossom-testkit`, `xtask corpus` | PLAN §5 |

**Gate.**

- Every failure-free `.ded` case in `tests/corpus/ldfi` passes on the oracle backend.
- Every `[backend.ldfi]` case in BENCH-130–134 and BENCH-137 gives the published verdict, and every case that states
  `falsifiers` produces exactly that set of Appendix-B-minimal falsifiers. **Exception (amended at the slice gate):**
  BENCH-133d, Flux at 22/21/1. No search here or in the reference checker decides it within reasonable resources
  (the lineage-driven search does not converge within 30,000 runs of its 22-tick, 4-node program; exhaustive
  certification's frontier exceeds 10 GB; the corpus README already records it as beyond the reference checker, its
  verdict resting on Flux's safety argument). It stays `unimplemented` and moves to the verification slice
  (inductive invariants, VER-010), where a proof, not a search, can certify it.
- The demo: `blossom ldfi tests/corpus/ldfi/molly/BENCH-130a-simple-deliv-6-3-0/program.ded --eot 6 --eff 3
  --crashes 0 --nodes a,b,c` prints the counterexample `O(a,b,1)` and its lineage; the same command on
  retry-deliv certifies it.
- Agreement with `tests/corpus/ldfi/tools/ldfi_ref.py` on every case it can check (it is a validation aid, not an
  expectation source; a disagreement is resolved from the literature).

**Stretch (not gating).** BENCH-136 run counts at most the published ones; those need the P1 search reductions
(TEST-030–032). Cases whose features list those reductions (BENCH-130q and BENCH-136a–i) are outside the gate:
they pass, and are ratcheted, only when their lineage-driven run count is at most the published one. Cases the gate
cannot reach are listed with the reason in the slice notes.

Former work packages covered in part: M3.6, M4.1, M5.2, M5.7, M7.2, M8.1 (and the SAT layer from M2.4, now consumed).

### Slice 2: the Blossom language on the same core

The `.bls` frontend lowered onto the IR that slice 1 already executes: programs, relation declarations (`table`,
`scratch`, `static`, `input`, `output`, `channel`), handlers and views, joins, stratified negation, recursion,
aggregates, `let`/`where`, ticks, `send … to`, facts and bootstrap. Type checking and resolution cover this subset
fully, with loud errors for the rest.

**Gate.** The `core/` and `async/` corpus cases that use only this subset pass on the oracle; `e02_reliable_broadcast`
and `e04_two_phase_commit`, written in Blossom, run under `blossom sim` and `blossom ldfi` with the verdicts their
`.ded` counterparts have.

Former work packages covered in part: M3.5, M4.2, M4.5, M5.3, M6.3, M6.7.

### The flagship goal (user decision, 2026-09-29)

**A linearizable, Raft-replicated key-value store written in Blossom, running as real processes on the network, and
measured against etcd.** Three `blossom run` processes over TCP with durable state on the M2 WAL; clients through the
host-facing API. Correctness evidence: LDFI finds the seeded bugs of Molly's Raft cases and certifies the correct
version at its bounds; a history checker finds every client history linearizable under `kill -9` and partitions; the
engine that runs it agrees with the oracle on the whole corpus. Speed: there is no fixed bar (the program runs on an
interpreter); the same workload runs against etcd on the same machine and both results are reported, with the
numbers kept honest (same durability settings, same client, same hardware).

Slices 3 and 4 below are its first two steps. The fast engine (semi-naive, indexed, planned; "Slice 5 onward")
comes before the etcd comparison, which closes the flagship.

### Slice 3: real processes

The sans-IO node over the oracle evaluator, TCP transport, durable tables over the M2 WAL and checkpoint layer,
recovery, `blossom run` as a real server with a host-facing client API. Durable-before-release (SEM-072).

**Gate.** `e01_kvs` (with the constructs it needs: upsert, `choose_most!`, `outer`, sessions) runs as separate
processes; a client workload with `kill -9` of the server at random points loses no acknowledged write, checked by a
history checker; the same program passes under the simulator over `SimFs` crash images.

Former work packages covered in part: M4.4, M4.7, M5.4, M5.5, M7.4, M6.4.

### Slice 4: Raft

`std::consensus::raft` in Blossom: elections, replication, commit, durability, then a KV service on top.

**Gate.** Raft under the simulator with partitions and crashes passes a linearizability checker; LDFI finds the seeded
bugs of the Molly Raft cases and certifies the correct version at its bounds; a 3-node Raft KV runs as real processes
and survives leader `kill -9`.

Former work packages covered in part: M8.3, M9.1, M8.2.

### Slice 5: the fast engine (merged, `b69c573`)

The incremental, indexed engine runs nodes by default and agrees with the oracle on every runnable corpus case. The
etcd comparison: `docs/plan/notes/etcd-comparison.md`. Notes and review: `docs/plan/notes/S5.md`.

### The Kafka goal (user decision, 2026-09-29)

**A Kafka-compatible broker written entirely in Blossom.** Stock clients (`kcat`, the Java client, franz-go) connect
and use it without modification. No Rust translation layer: the wire protocol and all broker logic are Blossom. The
design, the decisions (K1–K7) and the test strategy are in `docs/design/KAFKA.md`. The generic language and runtime
additions (byte streams, functions, byte primitives, the `extern fn` standard library, blobs) are in
`docs/design/FOREIGN-PROTOCOLS.md`.

### Slice 6: foreign protocols in Blossom

- **Byte streams.** Listen and connect, with ordered writes released after the tick's sync, in the runtime and both
  simulators.
- **Functions** (LANG-180) in the frontend, oracle and engine, with ranges and recoverable failure.
- **Byte primitives:** big-endian integers, varints, patching.
- **`extern fn`** and the standard library: CRC32C, CRC32, the gzip, snappy, lz4 and zstd codecs, and hashes.
- **Kafka protocol library in Blossom:** request and response headers, flexible versions (compact types, tagged
  fields), ApiVersions (including its v0 fallback) and Metadata, for a single broker with no topics.
- **Rust codec oracle** (test-only) and a **Blossom Kafka client** for simulator workloads.

**Gate.**
- `kcat -L` and `kafka-broker-api-versions.sh` (a current Kafka) list the Blossom broker.
- The codec oracle agrees with the Blossom encoders and decoders on randomized and captured requests.
- Corpus cases for functions, bytes and streams pass on both evaluators.
- Stream writes are released only after sync, mutation-checked.

### Slice 7: a single-node broker

- **Topics:** CreateTopics, DeleteTopics, DescribeConfigs.
- **Partition logs** as blob-backed durable relations.
- **Produce:** offset assignment by patching the batch header, and CRC32C validation.
- **Fetch,** with long polls and fetch sessions disabled; **ListOffsets.**
- **Retention.**
- **Store work:** incremental checkpoints, deletion at scale, blob durability and collection.

**Gate.**
- `kcat` and the Java console producer and consumer round-trip messages with explicit partitions and offsets, and
  franz-go produces and fetches.
- kill -9 of the broker loses no acknowledged record; the log checker validates each run.

### Slice 8: replication and placement

- **Metadata Raft group (controller):** broker registration, topics with a replication factor, partition assignment,
  the producer-id allocator.
- **One Raft group per partition** over its replica set: dynamic membership, elections over a relation.
- **Leader epochs** (KIP-320) and fencing.
- **acks** 0, 1 and all.
- **Reassignment** by membership change.
- Metadata and DescribeCluster report placement and leaders.

**Gate.**
- Three to five brokers under the S4 nemesis (kill -9 with downtime, crashes between append and sync, splits, one-way
  cuts) plus reassignment under load.
- The log checker and per-group safety observers find nothing; the directed scenarios are mutation-checked.
- Real clients follow leaders across failovers.

### Slice 9: consumer groups and idempotent producers

- **`__consumer_offsets`,** with the coordinator on its partition leaders.
- **The classic group protocol:** FindCoordinator, JoinGroup, SyncGroup, Heartbeat, LeaveGroup, OffsetCommit,
  OffsetFetch, ListGroups, DescribeGroups.
- Idempotent producers across failover: the producer state (built on one broker in S7, where InitProducerId moved
  because the Java 4.0 producer is idempotent by default) derived from the replicated log.

**Gate: the Kafka goal's final gate** (`docs/design/KAFKA.md`, "What stock clients work means"):
- `kcat -G`, the Java client (idempotent producer, group consumer) and franz-go work unmodified.
- Groups rebalance across joins, leaves and deaths; committed offsets survive coordinator failover.
- No duplicates across producer retries and failovers.

### Slice 10: the Kafka comparison

The same workload against Apache Kafka (KRaft mode) and Redpanda on the same machine, with the same client and
durability settings. The numbers are reported honestly, as in the etcd comparison.

### Later (order to be set when the Kafka goal closes)

- **The rest of the engine.** Plan perturbation tests, the word-level kernel (M4.3, M5.1, M6.1, M6.2, M7.1, M7.3).
- **Lattices and analyses.** Bloom^L lattices, CALM certificates, Blazes, Edelweiss (M3.1, M4.6, M5.6, M6.6, M7.5).
- **Verification.** BMC, SMT inductive invariants, Paxos Made EPR (M9.5, M10.4).
- **Code generation** equal to the interpreter (M8.5).
- **Systems.** Multi-Paxos, the Anna-style KVS, BOOM-FS, BOOM-MR/HOP, the lineage dataflow engine and Tide
  (M9.2, M9.3, M10.3, M11.1, M12.1, M12.2, M13.1).
- **Operations and release** (M8.7, M11.5, M12.3, M12.4, M13.3, M13.4, M14.1).
- **Object storage** (OBJECT-STORAGE.md): tiered storage under the store, so a node's immutable files live in a
  bucket. Researched 2026-10-08, not planned.

Each is still a slice: a named end-to-end demo and corpus gate, built through every layer it touches.
