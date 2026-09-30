# The Kafka goal: a Kafka-compatible broker written in Blossom

This is the second flagship goal, after the Raft key-value store. It records what we are building, every decision the
user has made, the architecture, and how the work is checked. It is the document to read first when resuming.

Status: planned; no code yet. Slices 6–10 in `docs/design/SLICES.md` build it, and `docs/plan/notes/S6.md` is where
work starts.

## The goal

A broker cluster written in Blossom that stock Kafka clients connect to and use without modification: producers,
consumers and consumer groups, with idempotent producers. It must hold Kafka's guarantees under failures: no
acknowledged record lost, and per-partition order kept, while brokers are killed with kill -9 and the network is
partitioned.

It is a stretch goal, and it gives the language a real, externally checkable target, not a demo propped up by host
code. It comes before the BOOM-style Hadoop work (BOOM-FS, BOOM-MR) in the original plan.

## Decisions (user, 2026-09-29, through the question tool)

| # | Question | Decision |
|---|---|---|
| K1 | Scope | **Stock clients work.** This means metadata, produce, fetch, list-offsets and topic creation; consumer groups with rebalancing; idempotent producers (on by default since Kafka 3.0); record batch v2; real partition placement. Transactions (exactly-once), ACLs, SASL/TLS, quotas and the full admin surface are out of scope for this goal. |
| K2 | Host code | **No Rust translation layer.** "I want Blossom to be able to fully implement all of this without cheating." The Kafka wire protocol (framing, decoding, encoding, version negotiation) and all broker logic are Blossom code. Nothing Kafka-specific is written in Rust inside the broker. |
| K3 | Platform additions allowed | These are generic, never Kafka-specific: a protocol-agnostic raw TCP byte-stream primitive in the runtime, and `extern fn` standard-library functions (LANGUAGE §16.2) for CRC32C and the compression codecs (gzip, snappy, lz4, zstd). |
| K4 | Protocol code | **Hand-written Blossom**, not generated from Kafka's JSON message schemas. |
| K5 | Simulated clients | **Both.** One Kafka client is implemented in Blossom, and a Rust test harness is an independent oracle. The harness is test-only code that never ships in the broker. It checks the Blossom codecs and client and drives the broker in the deterministic cluster simulator. Real clients gate the process-level tests. |
| K6 | Partition placement | **Real placement from the start.** Topics choose a replication factor, a controller assigns partitions to subsets of brokers, and partitions can be reassigned. |
| K7 | Client generation | **Current clients: Kafka 3.7+ and 4.0.** The gates use the current Java client, current librdkafka (through `kcat`) and franz-go. These clients dropped very old protocol versions (KIP-896), so the broker implements recent "flexible" versions: compact encodings and tagged fields. |

Where a later detail isn't covered by these decisions, the rule in K2 and K3 decides it. If a feature seems to need
host code, it becomes a generic language, runtime or standard-library feature, or the question goes to the user.

## What "stock clients work" means (the final gate)

With three to five `blossom run` brokers of the broker program:

- **Clients:**
  - `kcat` produces and consumes, including balanced consumption with `-G`.
  - The Java client's console producer and consumer, and a Java program using `KafkaProducer` (idempotent, the
    default) and `KafkaConsumer` in a group, work with default settings.
  - A franz-go program produces and consumes in a group.
  - `kafka-topics.sh --create/--describe/--list` works.
- **Consumer groups:** they rebalance when members join, leave or die. Committed offsets survive a coordinator
  failover.
- **Idempotence:** producer retries across a leader failover produce no duplicates.
- **Faults:** under kill -9 of any broker, network partitions and a partition reassignment, no acknowledged record is
  lost (`acks=all`), and every consumer sees each partition's records in offset order, with no gaps and no
  divergence.

## Architecture

### Layers, all in Blossom

1. **Connection layer.** It runs on the generic byte-stream primitive (FOREIGN-PROTOCOLS.md §1).
   - For each connection it accumulates received bytes and cuts frames: a 4-byte big-endian size, then the frame.
   - It hands each request frame to the protocol layer together with its per-connection sequence number.
   - It writes responses in request order per connection, which Kafka requires.
2. **Protocol layer.** Pure Blossom functions (FOREIGN-PROTOCOLS.md §2–3).
   - Each supported API has a decoder from request bytes to a typed request, and an encoder from a typed response
     to bytes.
   - The request and response headers are covered too: api key, version, correlation id, client id, and tagged
     fields.
   - ApiVersions negotiation, including the rule that its response header is always v0.
3. **Broker layer.** Relations and rules: metadata, the partition logs, produce and fetch, groups, and producer state.
4. **Replication layer.** Two kinds of Raft groups, generalized from e11:
   - one metadata group, the controller;
   - one group per partition, whose members are that partition's replica set.

### Protocol surface (APIs by key)

The broker implements one version of each API. That version is the highest one every target client (K7) supports
and actually uses. S6 fixes the exact numbers by capturing what each client sends, and they are recorded in
`docs/plan/notes/S6.md`. ApiVersions is the exception: it also answers v0 with UNSUPPORTED_VERSION so that clients
can fall back.

| Slice | APIs |
|---|---|
| S6 | ApiVersions (18), Metadata (3) |
| S7 | Produce (0), Fetch (1), ListOffsets (2), CreateTopics (19), DeleteTopics (20), DescribeConfigs (32), for the admin tools; InitProducerId (22), since the Java 4.0 producer is idempotent by default and refuses a broker without it (moved from S9, 2026-09-30) |
| S8 | the same APIs across a replicated cluster; Metadata reports placement, leaders and leader epochs; DescribeCluster (60) |
| S9 | FindCoordinator (10), JoinGroup (11), Heartbeat (12), LeaveGroup (13), SyncGroup (14), OffsetCommit (8), OffsetFetch (9), ListGroups (16), DescribeGroups (15) |

Unsupported APIs are left out of the ApiVersions response, so clients never call them. A request for an unsupported
key or version gets Kafka's error response, never a silent default.

Protocol features to handle deliberately:

| Feature | How it is handled |
|---|---|
| Fetch sessions (KIP-227) | Always session id 0, which the protocol allows. Clients fall back to full fetches |
| Long-poll fetch (`max_wait_ms`, `min_bytes`) | Parked fetches, completed by new data or a timer |
| Leader epochs (KIP-320) | The partition's Raft term. Stale epochs get FENCED_LEADER_EPOCH or UNKNOWN_LEADER_EPOCH |
| Topic IDs (UUIDs) | Assigned by the controller from a seeded PRF, deterministic under simulation |

### Record batches and offsets

- **Opaque storage.** Record batches (magic 2) are stored as they arrive, as opaque bytes.
- **Offset assignment.** On append, the broker assigns offsets by writing `baseOffset` and `partitionLeaderEpoch` into
  the batch header.
  - Both fields lie before the CRC32C, which covers from `attributes` to the end. So assigning offsets never
    recomputes the checksum.
  - With CreateTime timestamps (the default), nothing inside the CRC changes.
  - LogAppendTime topics would rewrite `maxTimestamp` and recompute the CRC through the standard library.
- **Validation.** The broker checks a batch's CRC32C through the standard library, and rejects corrupt batches with
  CORRUPT_MESSAGE.
- **Compression.** Compressed batches are kept compressed and returned as they are. Decompression is needed only for
  compacted topics (to read keys), and those arrive after this goal.
- **Offsets and Raft indexes.** A partition's Kafka offset is not its Raft index: the log can also hold
  configuration and no-op entries. Each data entry carries its base offset and record count, fixed by the leader at
  append time from the committed prefix.
- **High watermark.** It is the end of the committed prefix, and consumers read up to it.

### Storage

A broker's retained data exceeds RAM, so partition data can't live as ordinary in-memory rows.

- **Out-of-line blobs.** Batches go to an out-of-line blob store (FOREIGN-PROTOCOLS.md §5, LANGUAGE §16.6).
  - Log tuples hold `Blob` handles, offsets, counts and timestamps.
  - Stream writes accept `Blob`s, so a fetch response can send stored bytes without copying them through the engine.
- **Retention.** `retention.ms` and `retention.bytes` delete old entries and advance the log start offset.
- **Raft log compaction.** The partition group keeps a snapshot marker: the last included index and term.
  - A follower behind the log start is caught up from the retained suffix.
  - That is a partition log's whole state; the producer state and the group state are snapshotted with it.
- **Checkpoints.** Checkpoint cost must follow change, not state size: incremental checkpoints or segment-based
  durable relations. This is platform work in the store, scheduled in S7.

### Replication, placement and the controller

- **Metadata group (controller).** A Raft group over the controller brokers holds, as replicated state:
  - broker registrations;
  - topics (name, id, partition count, replication factor, configs);
  - partition assignments (replica sets);
  - the producer-id allocator.
  - Every broker learns committed metadata from it; brokers that aren't voters follow as observers.
- **Partition groups.** One Raft group per partition, whose members are the partition's replica set.
  - The replica set is data, a relation, not a role.
  - So elections, quorums and membership changes work over a relation: `majority` over a set or relation, which is
    LANG-113 generalized.
  - The group's leader is the partition leader, and its term is the leader epoch.
- **Reassignment.** A Raft membership change (joint consensus, or one server at a time) moves a partition to a new
  replica set under load.
- **acks.**
  - `acks=all`: acknowledged after the partition group commits.
  - `acks=1`: acknowledged after the leader's durable append.
  - `acks=0`: no response.
- **Leader routing.** A broker that isn't the leader answers NOT_LEADER_OR_FOLLOWER. Clients then refresh their
  metadata.

### Consumer groups

- **Internal topic.** `__consumer_offsets` is an internal topic placed like any other. A group's coordinator is the
  leader of the partition its id hashes to.
- **Protocol.** The coordinator runs the classic group protocol as rules:
  - member ids and generations;
  - leader election among members;
  - SyncGroup assignment distribution;
  - Heartbeat, with session and rebalance timeouts as timers;
  - LeaveGroup.
- **Offsets.** Offset commits are records in that partition, so they survive coordinator failover. Membership is
  volatile and rebuilt after a failover, as Kafka does.

### Idempotent producers

- **Ids.** InitProducerId allocates producer ids from the controller's allocator.
- **Sequence checks.** Each partition keeps per-producer state: the epoch and the last five batches' sequences.
  - This state is derived from the partition log as entries commit, so it survives failover.
  - A duplicate batch returns its original offset.
  - A gap returns OUT_OF_ORDER_SEQUENCE_NUMBER.
  - A stale epoch returns INVALID_PRODUCER_EPOCH.

## How it is checked

- **Codec oracle (K5).** A Rust test harness uses an independent Kafka protocol implementation (for example the
  `kafka-protocol` crate).
  - It encodes requests; the Blossom decoders must agree with the harness's structures.
  - The Blossom encoders' responses must decode in the harness to the expected structures.
  - Tests also replay golden byte captures from real clients.
- **Blossom client (K5).** A Kafka client written in Blossom, a second program on the stream primitive's `connect`
  side, drives workloads in the cluster simulator. The Rust harness client runs beside it as the oracle client, and
  both must see the same log.
- **Cluster simulator.**
  - Simulated byte streams between client and broker programs, including connection drops.
  - The nemesis from S4: crashes, some between a WAL append and its sync, downtime, splits and one-way cuts, now also
    reassignments.
  - Observers check replication safety per partition group and in the metadata group: election safety, state machine
    safety over committed entries, and leader completeness.
- **Log checker** (the Kafka analogue of the linearizability checker):
  - every acknowledged produce appears exactly once at the acknowledged offset;
  - all observers agree on the record at each offset;
  - consumers see offsets in order, with no gaps below the high watermark;
  - committed group offsets never move backwards;
  - with idempotence, there are no duplicates.
- **Real clients:** `kcat`, the Java client (3.7+/4.0) and franz-go, against real processes under kill -9 and a
  partition proxy, as `raft3.rs` does for the Raft KV.
- **LDFI (optional):** the replication protocol as a spec, like e12. The planned Kafka 0.8 ISR study (M10.5, which
  reproduces its durability bug) can run beside it.

Every durability or consistency test is mutation-checked, as in S3–S5.

## What is out of scope for this goal

- transactions and exactly-once semantics;
- ACLs, SASL and TLS (TLS needs DIST-060 first);
- quotas;
- compacted topics;
- LogAppendTime;
- tiered storage;
- KRaft wire compatibility with Kafka controllers (our controller is internal);
- the new consumer protocol (KIP-848);
- admin APIs beyond those the listed tools need.

## Risks

- **Protocol breadth.** Flexible versions, tagged fields and per-version field sets are a lot of careful byte-level
  code in a new language, which is why the codec oracle exists.
- **Storage.** Beyond-RAM relations, blobs and incremental checkpoints are new platform subsystems, and S7 depends on
  them.
- **Multi-Raft with dynamic membership** is new to Blossom. e11 is one static group.
- **Performance.** Group commit worked for the Raft KV; Kafka moves much larger payloads. The blob path must avoid
  copying bytes through the engine.

## How to resume

1. Read this file, then `docs/design/FOREIGN-PROTOCOLS.md`, then the current slice's notes (start:
   `docs/plan/notes/S6.md`).
2. Check `docs/STATUS.md` and the `bloom-remake` pad for the latest state.
3. The Raft KV (`examples/e11_raft_kv.bls`) and its tests (`tests/integration/tests/raft_kv.rs`,
   `crates/blossom-cli/tests/it/raft3.rs`) are the templates for replication and fault testing.
