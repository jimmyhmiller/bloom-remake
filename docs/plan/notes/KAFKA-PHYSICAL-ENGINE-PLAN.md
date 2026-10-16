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

- Plan recorded; implementation started.
- No work package is claimed complete yet.
