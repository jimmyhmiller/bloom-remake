# Bloom physical execution implementation plan

Starting point: `kafka-gap-analysis` (`f226107e`); worktree `.worktrees/kafka-physical-engine`.
Authorization: implement the complete measured redesign, keeping the Kafka program in Bloom,
relational semantics, small source, no names-based fast paths or application FFI escapes.

## Acceptance contract

Preserve tick/next-state semantics, duplicate support, retractions, aggregation, negation,
recursion, externally observed effects, strict durable acknowledgement and recovery.
Retain reference evaluation and existing formats unless migration/recovery is explicitly tested.
No unmeasured speedup claims. Throughput target is parity with fresh Kafka controls on equal
resources; efficiency target is engine CPU <=100 aggregate microseconds/acknowledged record.
Targets are gates, not expected or promised gains. Record work counters, latency and storage costs.

## Work packages

1. Asynchronous durable metadata reservation: one writer, bounded outstanding request, explicit
   completion/failure, no publication before durable acknowledgement, safe exhaustion and shutdown.
2. Repair replay history cloning overhead without weakening checks, making measurements scale.
3. Typed physical maintenance planning: explicit keys/types/support and observation boundaries;
   shared compatible arrangements; selective materialization; fuse hot eligible paths based on
   semantic properties. Preserve necessary counts and barriers. Explain decisions in plan output.
4. Specialized representations and execution: compact typed keys/rows; generated update kernels
   encompassing probes, expressions and index/support changes. Reference differential tests.
5. Kafka Bloom program: eliminate repeated immutable parsing where valid; audit authoritative
   partition/producer state, fetch range access and control/timer work. No protocol weakening.
6. Persistent payload/storage redesign: canonical durable payload backing, compact references,
   crash-safe publication/reclamation and bounded background work. Fault tests before defaults.
7. Partition-aware execution: explicit owned state and exchanges, with proof of independence and
   barriers for cross-partition/recursive/nonmonotonic work. Use existing CPU allocation.
8. Repeated end-to-end controls and diagnostic accounting, with stable disk headroom and binary
   identities; crash checks and checked replays. Report both throughput and efficiency.

## Working discipline

Land independently reviewable commits. Update the project pad after meaningful findings.
Every package remains incomplete until implementation and relevant validation are present.
Avoid equating flags, generated code volume or renamed operators with improvements.
If research/implementation exposes a necessary prerequisite, record it and implement it rather
than introducing a special case. Keep main and the prior analysis worktree untouched.

## Status

The implementation is on `kafka-physical-engine`; main remains unchanged. The complete
redesign and the performance acceptance contract have **not** been achieved.

| Package | Implemented and validated | Remaining |
|---|---|---|
| 1 | Bounded metadata writer; durable completion; normal and panic shutdown joins; metadata unit tests and live kill/restart verification | Writer leases preserve exclusion on startup failure; partial-startup listener cleanup remains open |
| 2 | Replay borrows the historical blob map; full outcome checks retained | Complete |
| 3 | Key-proven fusion, pure expression consumers, explicit logical observation reconstruction, hashed borrowed equality arrangements | Cross-relation arrangement ownership and a costed typed maintenance plan are incomplete |
| 4 | Source-column operations for field bindings, comparisons and range probes; independent oracle/reference tests | Whole-update native code generation and unboxed row storage are not implemented; placeholder compiler crates remain placeholders |
| 5 | Kafka Fetch checks the watermark before batch probes; Fetch/read-after-ack and live crash tests pass | Authoritative partition/producer-state redesign and the complete parsing/control-work audit remain |
| 6 | Group payloads synced once to packs before reference-only WAL records; exhaustive crash cuts; per-key LSM version histories and memory-hit pruning | Canonical payload write path implemented; LSM write amplification remains; no storage parity claim |
| 7 | No new partition-owned execution implementation | Ownership/exchanges/coordination barriers and differential validation remain |
| 8 | Four fresh interleaved controls verified on roomy physical NVMe; option matrix checked | Acceptance failed: Bloom 19,143.5 records/s versus Kafka 42,920; engine CPU 146.88 µs/record |

The initial candidate (`88802e4c`) delivered 16,363 records/s, 157.63 engine CPU
microseconds/record and 7,178 block-write bytes/record. All 1,229,892 acknowledged records
survived the tested kill/restart with no verification discrepancies. Earlier Kafka controls
were 22,219 and 24,350 records/s. This is a storage-write reduction, not Kafka parity.

The expanded executor (`47390bda`) executes 410,970 compiled terms in the historical early
window, versus 185,356 before. That window still takes approximately 1.54 seconds of engine
execution. More compiled terms alone did not remove the 575,293 support updates in the
window (down only 5.26% from 607,221 in the original baseline). Future work must reduce
or specialize those updates and their indexes, rather
than treating interpreter coverage as an acceptance metric.

Next implementation prerequisites:

1. Lower the actual engine maintenance plan into a typed, versioned operator IR, retaining
   support/distinct, error-order, recursion and observation boundaries. The currently
   placeholder planner/codegen crates need actual lowering and code generation.
2. Generate native update kernels over that IR, including support and arrangement updates;
   provide typed physical rows and probes. Verify whole ticks against both existing engines.
3. Select materialization with a cost model that includes recomputation and reverse probes.
   In particular, eliminating a tuple-destructuring view can destroy an indexed partition
   lookup unless the compiler supplies its inverse key construction or a compatible arrangement.
4. Express owned partitions and cross-partition exchanges in the plan and prove their
   dependencies before concurrent mutation. Existing read-only valuation workers are not
   partition ownership.
5. Repeat the same live acceptance workload after each structural change, retaining exact
   binary identities and full crash verification.

Final live acceptance result: **not met**. Two Bloom runs averaged 19,143.5 records/s;
two Kafka runs averaged 42,920 records/s on the same roomy physical NVMe. All four passed
exact record verification after kill/restart. The original-drive disk-full run is invalid
and excluded. See [results and evidence](KAFKA-PHYSICAL-ENGINE-RESULTS.md).
