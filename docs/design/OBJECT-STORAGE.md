# Object storage: how databases use buckets, and what Blossom could do with one

**Status: research only, not planned.** Nothing here is built or scheduled. The user (2026-10-08), after reading
it: "We'll deal with this later, but I want some docs for it."

This document replaces `SERVERLESS.md` (S27.3). That document planned Blossom as an embedded library (S28), then a
store whose commits were object PUTs (S29), then a node inside a function (S30). Two things sank it. The library
slice was mostly packaging of what exists (§6.1). And the commit path it designed, where every commit is an object
PUT, is what the field mostly avoids. The user saw that first: "Number two does not sound like how things typically
do this stuff on Object Store … that makes no sense … timing, latency, cost, et cetera, wise." Their request was to
research how databases actually do this, and the research below (2026-10-08, three surveys from primary sources)
agrees with them.

Contents:
- §1 is the finding in a few lines.
- §2 is the research, system by system.
- §3 is what the systems share.
- §4 is what Blossom's store already has.
- §5 covers the three options for Blossom, with tiered storage worked out.
- §6 records what was concluded about serverless and the embedded library.
- §7 is where to start if this resumes.
- §8 lists the sources.

## 1. The finding

Almost no serious system puts object storage on the commit path. There are three designs:

| Design | A write waits for | The bucket gets | Used by |
|---|---|---|---|
| **A. Fast replicated log, asynchronous tiering** | A quorum of replicas syncing to local disk (≈ 1 ms; Socrates targets < 0.5 ms) | Immutable files (sealed segments, SSTs, page images), asynchronously, in large batches | **The mainstream.** Aurora, Neon, Azure SQL Hyperscale, TiDB Serverless, Durable Objects, Kafka tiered storage, Confluent Kora, Redpanda, Pulsar |
| **B. The bucket is the log** | A buffer window (≈ 250 ms), one PUT of a multi-partition object, then a metadata commit. 0.4–2 s p99 on S3 Standard, ≈ 150 ms on S3 Express | Everything | WarpStream, Bufstream, Confluent Freight, Kafka's diskless topics (KIP-1150), Turso Cloud, SlateDB, turbopuffer. A cost play: it trades latency for no cross-zone replication traffic |
| **C. Backup** | A local sync | Log segments and snapshots, seconds to minutes behind | Litestream, CockroachDB, PlanetScale, MongoDB Atlas, FoundationDB backup |

Stream processors (RisingWave, Flink, Arroyo) are a variant of A. They keep no log of their own: the bucket holds a
consistent snapshot every 1–60 s, and the tail is recovered by replaying the upstream Kafka log from offsets kept in
the snapshot.

For Blossom, design A fits the store almost exactly (§5.1). It would give the Kafka broker, and every other program,
tiered storage with no change to the program and no change to commit latency. Its benefits are operational: retention
beyond local disk, cheaper storage, fast replica replacement and history that survives the cluster. They show at
cloud scale, not in this project's benchmarks (§5.1.6).

## 2. The research

### 2.1 Transactional and key-value databases

- **Neon** (Postgres)
  - **Commit:** compute streams WAL to *safekeepers* (Paxos over 3), and a commit is acknowledged once a quorum
    has it.
  - **Into the bucket:** pageservers turn the WAL into immutable layer files (delta and image layers) and upload
    them asynchronously. Safekeepers also back up the WAL and track how far that has got (`backup_lsn`).
  - **Local disk:** the pageserver's SSD is a cache over S3.
  - **Fencing, two layers:** Paxos terms at the safekeepers stop two computes from committing. A per-tenant
    *generation number* from the control plane is a suffix on every S3 key, so two attached pageservers never write
    the same key. Deletes wait until no newer generation can still reference an object (RFC 025).
  - **Cold start:** 500 ms to a few seconds, with pages fetched lazily.
- **Aurora**
  - **Commit:** redo records go to 6 storage nodes over 3 zones, and a write needs 4 of the 6. The storage nodes
    are EC2 machines with local SSDs.
  - **Into the bucket:** storage nodes "periodically stage log and new pages to S3", off the commit path.
  - **Fencing:** a new writer increments a volume epoch in a write quorum of every protection group, and storage
    rejects a stale epoch. No lease is involved.
  - **Recovery:** a read quorum per group, truncating above the durable point. There is no redo replay before
    opening.
- **Aurora DSQL**
  - Commit is durable in the Journal, an internal multi-zone log.
  - Storage and the Journal snapshot to S3, and recovery is a snapshot plus Journal replay.
- **Azure SQL Hyperscale (Socrates, SIGMOD'19)**
  - **Commit:** the primary writes log blocks synchronously to a *landing zone* on premium storage (3 replicas).
    The target is under 0.5 ms commit.
  - **Into blob storage:**
    - The log service destages the log to an SSD cache and to blob storage (30 days kept). Writes stall if the
      landing zone fills before destaging catches up.
    - Page servers checkpoint modified pages to blob storage in large writes.
    - Backups are constant-time blob snapshots.
  - **Cold start:** a new page server serves at once and warms its SSD cache in the background.
- **RocksDB-Cloud (Rockset)**
  - "S3 contains the entire database; local storage contains only the working set."
  - The WAL stays local or goes to Kafka or Kinesis.
  - **Fencing:** each open is a new epoch, and every SST and MANIFEST name carries it. A `CLOUDMANIFEST` maps file
    numbers to epochs, so a zombie writes different names instead of overwriting. Rockset probably also relied on
    external leader election; this is unclear.
- **TiDB Cloud Serverless**
  - The Raft log and WAL are on EBS, replicated by Raft.
  - Data files go to S3 asynchronously, after being cached on the instance's disk.
  - Followers load data from S3 rather than from the leader. A replacement node reattaches the EBS volume and pulls
    the rest from S3.
- **CockroachDB, PlanetScale, MongoDB Atlas, FoundationDB:** S3 is backup only (design C): snapshots plus a
  continuous log, replayed for point-in-time restore.
- **Turso Cloud (diskless)**
  - **Commit:** a transaction is acknowledged once it is in S3 Express One Zone.
  - **Measured upload latency (4 KB):**

    | Target | avg | p99 |
    |---|---|---|
    | S3 Express, same zone | 6.4 ms | 7 ms |
    | S3 Express, cross-zone | 7 ms | 8 ms |
    | S3 Standard | 31 ms | 102 ms |

  - **Cost control:** uploads are batched across every active database on a node. Turso estimates PUT cost at about
    $57 per database-month without batching and $0.57 with it.
  - **Local disk:** a write-through cache.
- **SlateDB** (an LSM on object storage)
  - **Commit:** `put` returns from memory. Durability means waiting for a WAL object, flushed every 100 ms by
    default.
  - **Fencing:** an open CAS-bumps `writer_epoch` in the manifest, then writes an empty fencing object at the next
    WAL id with put-if-absent. A zombie's next WAL write collides and it stops.
- **Litestream / LiteFS** (SQLite)
  - **Litestream:** local commit, then WAL shipped asynchronously as LTX files. The default interval is 1 s, which
    costs about $13/month in PUTs under constant writes, against $0.22 at 1 min. Files are compacted at 30 s, 5 min
    and 1 h levels.
  - **Litestream fencing:** a lease file taken with conditional writes.
  - **LiteFS:** asynchronous replication under a Consul lease. No fencing; divergence is detected by checksum.
- **Cloudflare Durable Objects** (SQLite in an actor)
  - **Commit:** WAL frames go to 5 followers in other data centres, and a write is confirmed by 3. An output gate
    holds responses until then.
  - **Into object storage:** batches every 10 s or 16 MB, with snapshots once the log outgrows the database.
  - **Fencing:** 3 of the 5 followers are told to stop confirming the old instance before a new one starts.
- **Datomic Cloud**
  - The transaction log is in DynamoDB, for its low-latency CAS.
  - Index segments are in S3, behind memory, SSD and EFS caches.

### 2.2 Log and streaming systems

- **Kafka tiered storage (KIP-405)**
  - **Commit:** the normal replicated path; tiering does not touch the ack.
  - **Upload:** the partition **leader** uploads *inactive* segments below the last stable offset, asynchronously,
    one segment at a time. Each upload carries the segment's indexes, a producer snapshot and the leader-epoch
    checkpoint.
  - **Local disk:** still the primary, trimmed by `local.retention.*`, and never trimmed before an upload has
    succeeded.
  - **Fencing:** every upload attempt gets a fresh UUID, so nothing is overwritten. Upload states go to an internal
    topic (`__remote_log_metadata`). Duplicate uploads after a leader change are tolerated and left to retention to
    clean up.
  - **Recovery:** a new follower replicates only the local tail. It gets the epoch checkpoint and producer snapshot
    from remote storage, and older reads are served from the bucket with locally cached indexes.
- **Confluent Kora (VLDB'23)**
  - The same pattern as KIP-405. Confluent's stated wins:
    - local volumes hold only the active set, so faster disks are affordable;
    - rebalancing moves only the active set.
  - The paper reports a data-loss bug from tiering metadata diverging between leader and follower. It also notes
    that backups restore only a prefix of the log, because the un-tiered suffix exists only on replicas.
- **Confluent Freight:** "direct write" to object storage before the ack, skipping local disks and cross-zone
  replication. Latency goes from under 100 ms to "up to a second or two", for up to 90% lower total cost.
- **Redpanda tiered storage**
  - **Commit:** a Raft majority.
  - **Upload:** the leader uploads segments asynchronously (by size, or at least every hour). The manifest is a
    Raft-replicated state machine in the partition's own log, with a fence command for concurrency control.
  - **Local disk:** primary, plus a read cache that fetches 16 MiB chunks.
  - **Restore:** whole-cluster restore is explicitly not snapshot-consistent across partitions.
- **Redpanda Cloud Topics:**
  1. All partitions are buffered for about 0.25 s or 4 MB.
  2. They are uploaded as one object.
  3. A placeholder batch holding the object's location is Raft-replicated into each partition's log.
  4. Then the producer is acked.

  Ordering and idempotence stay on the Raft path. End-to-end latency is about 1–2 s.
- **Pulsar / BookKeeper**
  - **Commit:** a quorum of bookies (Qa of Qw).
  - **Offload:** only *sealed* ledgers are offloaded, by multipart upload, and the bookie copy is deleted after a
    delay.
  - **Fencing:** BookKeeper's ledger fencing on recovery, plus a CAS on ledger metadata to close.
- **WarpStream**
  1. Stateless agents buffer every partition for 250 ms or 4 MiB.
  2. They PUT one file.
  3. The metadata store sequences it.
  4. Then they ack.

  - **Ordering and idempotence:** decided at commit, with duplicates tombstoned in metadata.
  - **No local disk at all.** Reads are sharded through a per-zone cache ring.
  - **Latency:** p99 about 400 ms on S3 Standard (≈ 1 s end to end), and 105/169 ms p50/p99 on S3 Express.
  - **Cost:** one file per partition would cost about $50 per partition-month in PUTs. The reference workload is
    under $40/day in S3 calls, against $641/day of cross-zone traffic for tuned Kafka.
- **AutoMQ**
  - **Commit:** a per-broker WAL on EBS (sub-millisecond, replicated by the cloud), or on S3 (≈ 300 ms p99), then an
    asynchronous upload of multi-partition objects.
  - **Object lifecycle:** prepare, upload, then commit through the KRaft controller, with uncommitted objects
    collected. Per-stream epochs fence.
  - **Failover:** re-attach the EBS volume to another broker.
- **Bufstream**
  - **Commit:** leaderless. One intake file is uploaded and recorded in etcd, Postgres or Spanner, then acked.
    About 260 ms median and 500 ms p99 end to end.
  - **Jepsen** found a lease-expiry bug and offsets falsely reported when etcd timed out.
- **Kafka diskless topics (KIP-1150/1163, accepted)**
  1. Brokers buffer for 250 ms or 4 MiB.
  2. They upload a multi-partition object named by UUID, without coordination.
  3. A coordinator assigns offsets.
  4. Then the produce is acked.

  - **Failure:** an upload whose commit fails is garbage that only the coordinator may clear.
  - **Targets:** produce p50 about 500 ms, p99 1–2 s.

### 2.3 Analytic databases and stream processors

- **RisingWave (Hummock)**
  - **Checkpoint:** at each checkpoint barrier (default 1 s), a node's writes for the epoch become one SST,
    uploaded asynchronously. The meta service then commits a new *version*, a manifest of SSTs.
  - **Visibility:** reads see the last completed checkpoint.
  - **Recovery:** load the checkpoint and replay Kafka from the offsets stored with it.
  - **Local disk:** a cache.
- **Apache Flink**
  - **1.x:** barrier-aligned snapshots to S3 with source offsets; incremental for RocksDB. Exactly-once output to
    Kafka becomes visible when a checkpoint completes. Recovery is the last checkpoint plus source replay, with no
    WAL of its own.
  - **2.0 (ForSt, VLDB'25):** state lives in the bucket and local disk is an optional cache. A checkpoint is
    reference creation, hard-linking existing files. Recovery reads the linked files in place.
  - **2.0 numbers (290 GB state):** checkpoints under 4 s, recovery 16× faster than 1.20, scale-out 49× faster.
  - **Latency:** a single access costs 23 ms on object storage against 68 µs on NVMe, so asynchronous execution is
    required.
- **Materialize (persist)**
  - **Commit:** immutable batch parts are written to the blob store. They become part of the data only when a
    compare-and-set at `seqno + 1` in Consensus (CockroachDB) succeeds.
  - **Why it's simple:** blob keys are write-once, so the blob store need not be linearizable.
  - **Fencing:** writer leases in shard state; a stale writer finds out on its next CAS.
- **ClickHouse Cloud (SharedMergeTree):** an insert is acknowledged after its part is in S3 and its metadata is in
  Keeper. Asynchronous inserts batch for 200 ms to 1 s. Servers are effectively stateless.
- **Snowflake:** immutable micro-partitions in S3. A commit is a FoundationDB transaction over the table's file
  list. Local SSDs are a write-through cache, with 60–80% hit rates.
- **Delta Lake / Iceberg**
  - **Delta:** a commit is creating `_delta_log/<v>.json`, which must never be overwritten (put-if-absent). Before
    S3's conditional writes it needed DynamoDB for mutual exclusion.
  - **Iceberg:** a commit is an atomic swap of the catalog's pointer to the new metadata file.
- **turbopuffer:** every write is a WAL object, committed by CAS on object storage and grouped behind one broker.
  Write p50 is 165 ms. A single CAS object manages about 5 commits/s.
- **Arroyo:** asynchronous incremental checkpoints every 10 s, and recovery "only needs to replay 10 seconds of data".

### 2.4 The primitives that changed in 2024

| Store | Primitive | Available since |
|---|---|---|
| S3 | `If-None-Match: *` (create only if absent) | 2024-08 |
| S3 | `If-Match` (compare-and-swap on the ETag) | 2024-11 |
| S3 | Conditional deletes | 2025-09 |
| S3 Express One Zone | Append at an offset | 2024-11 |
| GCS | Generation preconditions | for years |

These are why bucket-only commit protocols (SlateDB, Delta, turbopuffer) are now possible without a separate lock
service. Each commit object still manages only a few commits a second at 100–200 ms per CAS, so they batch.

## 3. What the systems share

1. **The log and the state are stored apart.** A small, latency-critical log goes on fast replicated storage. Bulk
   state goes in immutable files in the bucket. Design B puts the log in the bucket too, and pays for it in latency.
2. **Files in the bucket are immutable and uniquely named.** The names come from a UUID per upload (Kafka), a
   generation suffix (Neon), an epoch suffix (RocksDB-Cloud), a content hash, or a writer-unique prefix
   (Materialize). Two writers can never corrupt each other's data; at worst they leave garbage.
3. **One small atomic step makes new files part of the data.** The step is a CAS in a consensus store, a
   Raft-replicated manifest, a Keeper write, an FDB transaction, or a conditional PUT on the next manifest. All
   single-writer logic lives in that step.
4. **Fencing is an epoch or generation checked at that step, not a lock.** Leases appear where there is no CAS, and
   their expiry is downtime.
5. **Deletes are deferred.** An object is removed only once no manifest of any newer generation can reference it.
   Orphans from failed commits are collected by whoever owns the metadata.
6. **Local disk is a cache for everything except the log tail.** Replacing or rebalancing a node copies only the
   hot set, and older data is read lazily. Kora names this as tiering's main operational win.
7. **Request cost is controlled by batching.** That means one object per broker per window rather than per
   partition, objects of 4–64 MiB, and cached, sharded reads.
8. **Effects wait for durability.** Aurora's asynchronous commit, Durable Objects' output gate and the stream
   processors' epoch-gated output all do this, and so does Blossom's Invariant B already.

## 4. What Blossom's store has already

| Mechanism (§3) | Blossom today |
|---|---|
| A log apart from the state | The WAL (`blossom-store::wal`): one record per tick, pipelined group commit (ARCH-10). The versioned LSM (`blossom-store::lsm`, DATABASE.md §2) holds the state |
| Immutable files | SSTs (`db/sst/<id>.sst`) are immutable sorted runs, and the `MANIFEST` names the live ones |
| Content-addressed files | Blobs (`blossom-store::blob`): one file each under `<store>/blobs/`, named by hash and length, durable before the WAL record that references them. Kafka's record batches are blobs |
| An output gate | Invariant B: a tick's sends, stream writes, egress and replies are released only after its WAL record syncs |
| Lazy reads through a cache | Tiered tables read the database on demand through a block cache and a hot tier (DATABASE.md §7); durable views live in the database (§8) |
| Fast recovery | The flushed version plus the WAL tail. Views catch up from the WAL's written rows (S26) |
| A storage seam | Every file the store touches goes through the `Vfs` trait; `SimFs` crashes at every point |
| Replication | In the program, not the store: Raft written in Blossom (`examples/e12_raft.bls`, `examples/kafka/raft.bls`). Each node's store is local and single-writer (`StoreLock`) |

The last row matters most. In design A the "fast replicated log" is what makes the tail durable before it reaches
the bucket. For a replicated Blossom program, that is the program's own Raft quorum over each node's local WAL. This
is exactly the position of Kafka, Kora and Redpanda when they added tiering, so the store needs no replication of its
own.

## 5. The options for Blossom

### 5.1 Option A: tiered storage (the one that fits)

The commit path does not change. A tick is durable when its local WAL record syncs; nothing in the bucket is on the
latency path. Below that, the store moves what is immutable into a bucket.

#### 5.1.1 What goes to the bucket

```
<bucket>/<deployment>/<node>/
  sst/<gen>-<id>.sst        immutable, uploaded after the flush or compaction that made it
  blob/<hash>-<len>         immutable, content-addressed: a duplicate PUT is harmless
  manifest/<seq>            the tree's manifest: live SSTs (local or remote), flushed version, generation
  wal/<gen>-<first>-<last>  sealed WAL segments, archived (point-in-time restore; §5.3)
```

- **After a flush or compaction:** the new SSTs are uploaded, then `manifest/<seq + 1>` is written with a
  conditional create (`If-None-Match: *`) naming them. A local SST named by a committed remote manifest becomes a
  cache entry the node may evict.
- **Blobs** are uploaded once the SST or WAL segment that references them is. A blob is never deleted locally
  before its upload has succeeded, as in KIP-405.
- **Uploads are batched and asynchronous.** They are bounded by bytes in flight, and they never block a tick. A
  slow bucket can only grow the local disk, never the latency. Socrates stalls writes when its landing zone fills;
  the same backpressure applies here at a disk-usage threshold, and it must be explicit, never a silent drop.

#### 5.1.2 One writer: generations and fencing

The pattern is Neon's generations plus SlateDB's manifest CAS (§2.1):

- **Each open of a node's store takes a new generation.** It reads the newest `manifest/<seq>` and writes
  `manifest/<seq + 1>` with `generation + 1`, conditionally. Losing that race means another writer opened first,
  and the open is refused.
- **Every object the node writes carries its generation in its key**, so a zombie (an old process on a lost
  machine) never overwrites anything. Its next manifest write targets a sequence number that exists, the
  conditional PUT fails, and it stops uploading.
- **Deletes are deferred.** An SST is deleted from the bucket only when no manifest at or after the oldest one any
  reader may still hold names it. The same rule protects as-of queries (DATABASE.md §5): the manifest a query pinned
  keeps its SSTs alive.
- **No lease, no clock, no lock service.** The bucket's conditional create is the only coordination.

The local `StoreLock` stays. The generation adds protection across machines, which the lock cannot give.

#### 5.1.3 Reads

`ColdTables` (DATABASE.md §7) reads SST blocks through the block cache. A block of an evicted SST becomes a ranged GET,
with whole SSTs (or large chunks of them, Redpanda uses 16 MiB) fetched into a local disk cache. Fetch latency
is 10–100 ms, against microseconds locally (Flink's paper: 23 ms vs 68 µs), so:

- **Hot data must stay local.** The eviction policy keeps the newest SSTs and anything the hot tier touched recently.
- **Kafka's fetch path is where it shows.** A consumer reading old offsets waits on GETs, as in every tiered Kafka.
  The broker already sends stored batches as blob ranges, never copied through the program (`examples/kafka/
  fetch_node.bls`), so a remote blob is a remote byte range read the same way.
- **The engine must not block a tick on a remote read.** A tick that needs a cold block either waits (and holds
  every later tick), or the read is started ahead and the tick deferred. Which of the two is a real design question,
  and the tick model makes it sharper than in Kafka, where a slow fetch delays only that fetch.

#### 5.1.4 Restore and replacement

- **Restore a node from the bucket:** read the newest manifest. SSTs and blobs are fetched lazily, and the WAL is
  replayed from the archived segments after the manifest's flushed version. This needs no other replica: it is a
  backup.
- **Replace a replica of a replicated program:** restore from the bucket as above, then catch up the tail through
  the program's own protocol. This is where Kora's fast rebalancing comes from, and **in Blossom it is not automatic**:
  - Today a new Kafka replica gets the partition's retained history entry by entry through AppendEntries. Raft
    compaction in `examples/kafka/raft.bls` only happens at retention, so the snapshot point "holds nothing" for a
    Kafka partition.
  - Starting from the bucket, the replica would need the program to treat "these entries are already in the
    bucket" as a valid snapshot point. That means InstallSnapshot carrying blob references (hashes) instead of
    bytes.
  - Blobs are already content-addressed, so a hash is enough to fetch the bytes from the bucket. But it is a change
    to the Raft program and to what a node may assume about another node's store. It is not just a store feature.

#### 5.1.5 Testing

- **A simulated bucket beside `SimFs`:** conditional creates, LIST consistency, latency, and crash points between
  any two operations.
- **Two faults:** a zombie writer (a second process replaying an old open), and a bucket that is slow or
  unavailable for a stretch.
- **The properties:**
  - no tick is lost or reordered whatever the upload timing;
  - a zombie never makes the bucket's manifest name a file the live writer did not write;
  - a restore from the bucket at any crash point equals the store as of some released tick;
  - a slow bucket never changes latency, only disk usage, until the explicit backpressure threshold.

#### 5.1.6 What it gives, and to whom

| Benefit | What it means | Seen in this project? |
|---|---|---|
| Retention beyond local disk | Each broker keeps hours locally and the bucket keeps months. Today every retained byte is on every replica's disk | Only with a scenario built for it (hundreds of GB retained) |
| About 10× cheaper storage for old data | S3 ≈ $0.023/GB-month stored once, against EBS gp3 ≈ $0.08/GB-month × 3 replicas. At 10 TB, ≈ $230 against ≈ $2,400 a month | Only on a cloud deployment |
| Fast replica replacement and rebalancing | Copy the recent tail, not the history | Only after the Raft change in §5.1.4 |
| History survives the cluster | Losing every broker loses only the un-tiered tail; the bucket is also a backup | Yes: restore from the bucket is testable here |

It would be generic: the Kafka broker, the Raft KV and the database would all get it, with no change to the program.
Real Kafka needed a large KIP and broker changes for the same feature. That is the strongest argument for it within
this project, as a demonstration of what living below the program buys. The operational benefits themselves only
show at a scale this project does not run at (3 processes on one server, benchmarks of about 300 000 records).

### 5.2 Option B: the bucket is the log (diskless)

The WAL would become batched objects: one per node per window (≈ 100–250 ms, or per N bytes), committed by a
conditional create of the next WAL sequence number (SlateDB) or through a metadata store (WarpStream, KIP-1150).
Invariant B would hold with "synced" meaning "the commit returned".

- **What it gives:** stateless nodes, any machine able to open any node, and no cross-zone replication traffic,
  which is the dominant cost of Kafka on AWS.
- **What it costs:** every acknowledged write waits for the window plus a PUT. That is about 0.4–2 s p99 on S3
  Standard and about 150 ms on S3 Express, against about 1–5 ms for Blossom's Kafka today. It is a different
  product tier: right for logs and analytics pipelines, wrong for anything interactive or for Raft.
- **It also changes the program's world:** with the bucket as the shared log, replication by the program (Raft) is
  redundant. A diskless Kafka would be a different Kafka program, not the same program on a different store.

Not recommended.

### 5.3 Option C: backup only

This is the archive half of option A without eviction. Sealed WAL segments, SSTs, blobs and manifests are uploaded
asynchronously, and the local disk stays the full copy.

- **What it gives:** restore, point-in-time restore (as-of queries already exist), and moving a node to a new
  machine.
- **The loss window:** a node that loses its disk loses the ticks not yet uploaded. That is seconds, as in
  Litestream, and it is covered by replicas in a replicated program.
- **Fencing:** the generations of §5.1.2 are still needed. A restored node and a still-running old one would
  otherwise write the same prefix.

It is the first step of option A, useful on its own, and much smaller.

## 6. What was concluded about serverless and the embedded library

### 6.1 The embedded library (the old S28) is not worth a slice

- **It already exists.** `ManualDriver` (`crates/blossom-node/src/manual.rs`) is already open-with-recovery, offer
  inputs, run ticks synchronously, query the database and flush. The simulator uses it. A public API, a C ABI and
  an example would be packaging with no consumer, which breaks SLICES.md rules 2 and 3.
- **"Rebuild the runtime on it" conflicts with performance.** `ManualDriver` does no pipelining: each tick is
  durable before the next. `blossom-runtime`'s server computes tick t+1 while tick t syncs (ARCH-10), and the Kafka
  throughput work depends on that.
- **It gives up what makes Blossom distinct.** With several roles as several embedded databases, the sends between
  them are delivered by host code the simulator and LDFI cannot see. What is left is a single-node embedded Datalog
  store.
- **Where embedding matters, it is already done:** the client roles' engine-only wasm in the page (S22).

### 6.2 Serverless needs a fast log service

The serverless databases that work well keep a fast replicated log outside the function: Neon's safekeepers,
Aurora's storage quorum, Durable Objects' followers. "The function is the database" works only in design B, at
100 ms to 2 s per write. For Blossom that means:

- **A node inside a function** (Lambda) is design B plus a scheduler to wake it for timers. It suits programs with
  no streams, no peers and no latency-sensitive timers.
- **A node as a Durable Object** is the closest platform fit. The platform provides the single instance, the input
  and output gates, alarms as timers, storage and hibernating sockets.
- **A container that scales to zero** (Cloud Run, Fly Machines) plus option A is the shape that fits programs with
  streams, such as Kafka.

The durable client outbox from the old plan is independent of all this, and worth doing on its own. It makes the
replay buffer of the S27 link durable, so `resumed` holds across a server restart.

## 7. If this resumes

1. **Choose the scope:** option A, or option C as its first step. Choose the first target store: S3's API covers
   S3, R2, MinIO and GCS's S3 mode. MinIO runs locally and on the build server, without an AWS account.
2. **An `ObjectStore` trait:** GET, ranged GET, PUT-if-absent, PUT-if-match, LIST and DELETE. Build it with a
   local-directory implementation, a simulated one (§5.1.5) and an S3 one, at the level the store needs (whole
   immutable objects and conditional creation), not a filesystem emulation behind `Vfs`.
3. **Option C:** generations and fencing, archiving, and restore from the bucket. Gate: kill a broker, delete its
   store, restore it from the bucket, and lose no acknowledged record.
4. **Option A:** eviction, remote reads through the block cache, a disk cache, the non-blocking cold read of
   §5.1.3, and explicit backpressure. Gate: a Kafka broker retaining several times its local disk, with produce
   latency unchanged and old offsets fetchable.
5. **Replica replacement from the bucket** (§5.1.4): a change to the Raft program, measured as replacement time
   before and after.

## 8. Sources

Read 2026-10-08.

**Transactional and key-value:**
- Neon:
  [architecture](https://neon.com/docs/introduction/architecture-overview),
  [walservice](https://github.com/neondatabase/neon/blob/main/docs/walservice.md),
  [generation numbers RFC 025](https://github.com/neondatabase/neon/blob/main/docs/rfcs/025-generation-numbers.md),
  [architecture decisions](https://neon.com/blog/architecture-decisions-in-neon),
  [connection latency](https://neon.com/docs/connect/connection-latency)
- Aurora:
  [SIGMOD'17](https://web.stanford.edu/class/cs245/readings/aurora.pdf),
  [SIGMOD'18](https://www.cs.purdue.edu/homes/bb/cs542-20Spr/readings/impl/sigmod-18-amazon-aurora-avoiding-consensus.pdf)
- Aurora DSQL:
  [Brooker on writes](https://brooker.co.za/blog/2024/12/05/inside-dsql-writes/),
  [components](https://aws.amazon.com/blogs/database/everything-you-dont-need-to-know-about-amazon-aurora-dsql-part-4-dsql-components)
- Socrates: [SIGMOD'19](https://www.microsoft.com/en-us/research/uploads/prod/2019/05/socrates.pdf)
- RocksDB-Cloud: [README](https://github.com/rockset/rocksdb-cloud),
  [cloud_manifest.h](https://github.com/rockset/rocksdb-cloud/blob/master/cloud/cloud_manifest.h)
- TiDB Serverless:
  [AWS blog](https://aws.amazon.com/blogs/storage/how-pingcap-transformed-tidb-into-a-serverless-dbaas-using-amazon-s3-and-amazon-ebs/)
- Turso:
  [diskless](https://turso.tech/blog/turso-cloud-goes-diskless),
  [durability](https://turso.tech/blog/how-does-the-turso-cloud-keep-your-data-durable-and-safe)
- SlateDB:
  [manifest RFC](https://github.com/slatedb/slatedb/blob/main/rfcs/0001-manifest.md),
  [writes](https://slatedb.io/docs/design/writes/)
- Litestream:
  [how it works](https://litestream.io/how-it-works/),
  [config](https://litestream.io/reference/config/),
  [S3 advanced](https://litestream.io/guides/s3-advanced/)
- LiteFS: [how it works](https://docs.fly.io/litefs/how-it-works)
- Durable Objects: [SQLite in Durable Objects](https://blog.cloudflare.com/sqlite-in-durable-objects/)
- PlanetScale: [Metal](https://planetscale.com/metal)
- Datomic: [architecture](https://docs.datomic.com/whatis/architecture.html)
- FoundationDB: [backups](https://apple.github.io/foundationdb/backups.html)
- MongoDB Atlas: [continuous backup](https://mongodb.com/docs/atlas/recover-pit-continuous-cloud-backup)

**Log and streaming:**
- Kafka tiered storage: [KIP-405](https://cwiki.apache.org/confluence/display/KAFKA/KIP-405%3A+Kafka+Tiered+Storage)
- Kafka diskless topics:
  [KIP-1150](https://cwiki.apache.org/confluence/display/KAFKA/KIP-1150%3A+Diskless+Topics),
  [KIP-1163](https://cwiki.apache.org/confluence/display/KAFKA/KIP-1163%3A+Diskless+Core)
- Confluent:
  [Kora, VLDB'23](https://www.vldb.org/pvldb/vol16/p3822-povzner.pdf),
  [Freight](https://www.confluent.io/blog/introducing-confluent-cloud-freight-clusters/)
- Redpanda:
  [tiered storage](https://docs.redpanda.com/current/manage/tiered-storage/),
  [archival_metadata_stm](https://github.com/redpanda-data/redpanda/blob/dev/src/v/cluster/archival/archival_metadata_stm.h),
  [Cloud Topics](https://www.redpanda.com/blog/cloud-topics-architecture)
- Pulsar:
  [tiered storage](https://pulsar.apache.org/docs/next/tiered-storage-overview/),
  [BookKeeper protocol](https://bookkeeper.apache.org/docs/development/protocol)
- WarpStream:
  [write path](https://docs.warpstream.com/warpstream/overview/architecture/write-path),
  [S3 costs](https://www.warpstream.com/blog/minimizing-s3-api-costs-with-distributed-mmap),
  [S3 Express benchmark](https://www.warpstream.com/blog/warpstream-s3-express-one-zone-benchmark-and-total-cost-of-ownership),
  [Kafka is dead](https://www.warpstream.com/blog/kafka-is-dead-long-live-kafka)
- AutoMQ:
  [WAL storage](https://docs.automq.com/automq/architecture/s3stream-shared-streaming-storage/wal-storage),
  [metadata](https://www.automq.com/blog/insight-metadata-management-in-automq)
- Bufstream: [Jepsen analysis](https://jepsen.io/analyses/bufstream-0.1.0)

**Analytic and stream processing:**
- RisingWave:
  [checkpoint design](https://github.com/risingwavelabs/risingwave/blob/main/docs/dev/src/design/checkpoint.md),
  [state store](https://github.com/risingwavelabs/risingwave/blob/main/docs/dev/src/design/state-store-overview.md)
- Flink:
  [stateful stream processing](https://nightlies.apache.org/flink/flink-docs-stable/docs/concepts/stateful-stream-processing/),
  [changelog backend](https://flink.apache.org/2022/05/30/improving-speed-and-stability-of-checkpointing-with-generic-log-based-incremental-checkpoints/),
  [ForSt, VLDB'25](https://www.vldb.org/pvldb/vol18/p4846-mei.pdf)
- Materialize: [persist design](https://github.com/MaterializeInc/materialize/blob/main/doc/developer/design/20220330_persist.md)
- ClickHouse:
  [SharedMergeTree](https://clickhouse.com/blog/clickhouse-cloud-boosts-performance-with-sharedmergetree-and-lightweight-updates),
  [async inserts](https://clickhouse.com/docs/optimize/asynchronous-inserts)
- Snowflake:
  [NSDI'20](https://www.usenix.org/system/files/nsdi20-paper-vuppalapati.pdf),
  [FoundationDB metadata](https://www.snowflake.com/en/blog/how-foundationdb-powers-snowflake-metadata-forward/)
- Delta Lake: [protocol](https://github.com/delta-io/delta/blob/master/PROTOCOL.md)
- Iceberg: [spec](https://iceberg.apache.org/spec/)
- turbopuffer:
  [architecture](https://turbopuffer.com/docs/architecture),
  [object storage queue](https://turbopuffer.com/blog/object-storage-queue)
- Arroyo: [stateful stream processing](https://www.arroyo.dev/blog/stateful-stream-processing)

**Object-storage primitives:**
[S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html),
[S3 pricing](https://aws.amazon.com/s3/pricing/),
[GCS preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
