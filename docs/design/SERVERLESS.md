# Serverless Blossom: a node on a bucket, a node in a function, a node as a library

The user (2026-10-08): "I think you need to go look into how databases usually do this serverless thing, because
they're usually a little smarter than I think what you're talking about. And then once you have that, come up with a
plan for how we could make this kind of work. Ideally I want it for the plain functions-as-a-service kind of setup,
but it doesn't have to be that way. SQLite is kind of a thing: could we be like SQLite?"

This document is research and a plan; nothing here is built yet. §1 is what serverless databases actually do (with
sources), §2 is what of it Blossom already has, §3–§6 the design, §7 the plan in slices.

## 0. What "serverless" would mean for Blossom

Three shapes, from most to least constrained:

1. **A node in a function** (AWS Lambda, Cloud Run, a Vercel/Netlify function): no machine of our own, no local disk
   that outlives the invocation, no process between requests, possibly two invocations at once. State lives in object
   storage (S3, GCS, R2) or a managed log.
2. **A node as an actor** (Cloudflare Durable Objects): one instance per name, globally, with storage of its own,
   alarms, and connections held by the platform while it sleeps. The platform gives the single writer we need.
3. **A node as a library** (SQLite's shape): no server at all. The program's database is a directory (or a bucket) a
   process opens, feeds inputs, queries and closes. A function is then just a short-lived process that opens it.

The user wants 1 if possible and is open to 3. The design below makes 3 the foundation, because 1 is 3 plus a store
that lives in a bucket plus a way to be woken; and 2 falls out of the same pieces.

## 1. How serverless databases do it

The full survey (primary sources, read 2026-10-08) is summarized here; the systems differ in product but agree on
mechanism. Numbers are as published.

### 1.1 The systems

- **Neon** (Postgres): compute is stateless. The WAL goes to *safekeepers* (Paxos over 3; a record is durable when 2
  store it), not to S3; *pageservers* turn WAL into immutable layer files (a key range × an LSN range: image layers
  and delta layers) uploaded to S3. A read is `GetPage@LSN`: the newest image at or below the LSN plus the deltas
  above it. Scale to zero after 5 idle minutes; a cold start went from 3–6 s to about 500 ms with a pool of warm VMs
  stamped with the tenant's identity. Single writer: each compute start is a Paxos election with a higher term, and
  safekeepers refuse a stale term. ([walservice](https://github.com/neondatabase/neon/blob/main/docs/walservice.md),
  [safekeeper protocol](https://github.com/neondatabase/neon/blob/main/docs/safekeeper-protocol.md),
  [pageserver storage](https://github.com/neondatabase/neon/blob/main/docs/pageserver-storage.md),
  [cold starts](https://neon.com/blog/cold-starts-just-got-hot))
- **Aurora**: "the log is the database". Only redo records cross the network, to a 4-of-6 quorum over three zones;
  pages are built in the background. Commit is asynchronous to the worker: a client is acknowledged once the volume's
  durable LSN passes its commit. No leases: after a recovery the volume epoch is bumped in storage, and storage
  refuses requests at a stale epoch. Serverless v2 scales in place and to zero (resume ≈ 15 s; scheduled jobs are
  *skipped* while paused). ([SIGMOD'17](https://www.cs.purdue.edu/homes/csjgwang/CS592DisaggregatedDB/AuroraSIGMOD17.pdf),
  [SIGMOD'18](https://www.cs.purdue.edu/homes/bb/cs542-20Spr/readings/impl/sigmod-18-amazon-aurora-avoiding-consensus.pdf),
  [auto-pause](https://docs.aws.amazon.com/AmazonRDS/latest/AuroraUserGuide/aurora-serverless-v2-auto-pause.html))
- **SlateDB** (an LSM directly on object storage, the closest to us): `manifest/<id>` and `wal/<id>.sst` objects with
  contiguous ids; a writer buffers writes and PUTs a WAL object every `flush_interval` (100 ms by default); memtables
  flush to L0 SSTs; a compactor (possibly elsewhere) merges them. **Fencing**: on open, the writer reads the
  manifest, increments `writer_epoch`, writes the next manifest with put-if-absent, then writes an empty WAL object at
  the next WAL id; every WAL PUT is `If-None-Match`, so a zombie writer's next write collides and it stops. Cost of a
  100 ms interval on S3 Standard ≈ $130/month in PUTs; latency is the interval plus a PUT (S3 Standard 50–100 ms,
  S3 Express 5–10 ms). ([manifest RFC](https://slatedb.io/rfcs/0001-manifest/),
  [tuning](https://slatedb.io/docs/operations/tuning/), [config.rs](https://github.com/slatedb/slatedb/blob/main/slatedb/src/config.rs))
- **Turso** (libSQL): diskless since 2025: every commit is durable on S3 Express One Zone (a 4 KB PUT averaged 6.4 ms,
  p99 7 ms, against ~2 ms for a local fsync); the WAL of dozens of databases is batched into one PUT over a few
  milliseconds, which takes the cost from ≈ $57 to ≈ $0.57 per database-month. Local disk is a cache; a replacement
  reads from S3 while warming. ([Turso diskless](https://turso.tech/blog/turso-cloud-goes-diskless),
  [AWS on Turso](https://aws.amazon.com/blogs/storage/how-turso-built-a-transactional-database-using-amazon-s3-express-one-zone/))
- **Litestream / LiteFS** (SQLite, embedded): Litestream ships SQLite's WAL as LTX files to S3 every second, compacts
  them in levels (30 s, 5 min, 1 h), and since v0.5 takes a time-based lease made of S3 conditional writes. LiteFS
  captures each transaction with FUSE and replicates it asynchronously under a Consul lease, accepting a loss window.
  ([Litestream revamped](https://fly.io/blog/litestream-revamped/), [LiteFS](https://docs.fly.io/litefs/how-it-works))
- **Cloudflare Durable Objects** (SQLite inside an actor): one instance per name. **Input gate**: no other event is
  delivered while a storage operation runs. **Output gate**: outgoing messages wait until the writes before them are
  durable (3 of 5 nearby followers; batches go to object storage every 10 s or 16 MB). One alarm per object, at least
  once, which wakes a sleeping object. WebSocket hibernation: the platform holds the sockets while the object is
  evicted and wakes it per message. ([SQLite in DO](https://blog.cloudflare.com/sqlite-in-durable-objects/),
  [easy, fast, correct](https://blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three/),
  [alarms](https://developers.cloudflare.com/durable-objects/api/alarms/),
  [websockets](https://developers.cloudflare.com/durable-objects/best-practices/websockets/))
- **WarpStream** (Kafka on S3, relevant since our Kafka is in Blossom): stateless agents buffer every partition's
  writes into one file per agent every 250 ms or 4 MiB, PUT it, then a metadata store sequences it; produce p99 ≈ 400
  ms. Per-partition files would cost ≈ $50 per partition-month. ([lazy log](https://www.warpstream.com/blog/the-art-of-being-lazy-log-lower-latency-and-higher-availability-with-delayed-sequencing))
- **Netherite / Durable Functions** (event-sourced partitions on cloud storage): partitions hold storage leases so
  each is loaded on one node at most; scale to zero leaves every partition in storage; the conservative mode persists
  before sending (an output gate); speculation sends early, tagged with the sender's log position, and rewinds
  receivers on a crash. Workflows recover by deterministic replay. ([paper](https://arxiv.org/pdf/2103.00033))
- **DynamoDB, CockroachDB Serverless, PlanetScale**: "serverless" is billing on a shared, always-on fleet behind a
  proxy (CockroachDB stamps a warm SQL pod with the tenant's identity in milliseconds). Not our shape.

The object-storage primitives that make the bucket-only designs possible are recent: S3 `If-None-Match: *` on PUT
(2024-08), `If-Match` (compare-and-swap, 2024-11), conditional deletes (2025-09); S3 Express One Zone appends at a
given offset (2024-11). GCS has had generation preconditions (`ifGenerationMatch`, `0` = must not exist) for years.
([S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html),
[GCS preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions))

### 1.2 What they agree on

1. **The log and the state are stored apart.** A small, latency-critical log (a replica quorum, or batched object
   PUTs) and bulk immutable sorted files in object storage. Nobody puts S3 Standard on a fast commit path; those that
   put object storage on it at all batch, and accept tens to hundreds of milliseconds.
2. **The single writer is enforced by the storage, not trusted to the compute.** An epoch the storage checks (Aurora,
   Neon), the conditional creation of the next object (SlateDB, Materialize's `compare_and_set`), or a lease (LiteFS,
   Litestream, Netherite, DynamoDB). Epochs need no clock; leases need a clock bound and their expiry is downtime.
3. **Group commit to object storage, and the batching interval is a cost knob.** Batch many ticks into one object,
   and many databases' logs into one PUT when a process holds several.
4. **Effects wait for durability.** Aurora's asynchronous commit, Durable Objects' output gate, Netherite's
   conservative mode: computation goes on, what leaves waits.
5. **Local caches are disposable; object storage is the truth.** Cold reads go to storage while a cache warms.
6. **Cold start is mostly the control plane.** The data path is lazy (Aurora replays nothing before opening; Neon
   fetches pages on demand); the rest is warm pools stamped with an identity, a proxy holding connections, and durable
   timers that wake a sleeper (Aurora's skipped jobs are what happens without them).
7. **Embedded library, or service.** SlateDB, Litestream, libSQL and SQLite-in-DO put the database in the
   application's process and coordinate only through the bucket or a lease. Neon, Aurora and CockroachDB are services
   behind proxies. A single-writer deterministic node is naturally the first.

## 2. What Blossom has already

Much of the mechanism above is already in Blossom, on a local disk:

| Serverless mechanism | Blossom today |
|---|---|
| A log apart from the state | The WAL (a record per tick, `blossom-store::wal`) and the versioned LSM (`blossom-store::lsm`: a `MANIFEST`, immutable `sst/<id>.sst`, compaction; reads "as of tick t"). DATABASE.md §2 |
| Group commit | The committer appends every tick submitted as one batch and syncs once (runtime `server.rs`) |
| Output gate | Invariant B: a tick's sends, stream writes, egress and client replies are released only after its WAL record syncs |
| Lazy reads, disposable caches | Tiered tables read the database on demand through a hot tier (DATABASE.md §7); durable views live in the database (§8, S26) |
| Fast, lazy recovery | The flushed version plus the WAL tail; the views catch up from the WAL's written rows instead of re-reading tables (S26): a Kafka broker with 300 000 records was ready in 0.9 s |
| A filesystem seam | Every file the store touches goes through the `Vfs` trait (open, append, `sync_data`, `pread`, rename, list, lock); the simulator's `SimFs` crashes at every point |
| Deterministic replay | A node's ticks are deterministic given their inputs; `blossom trace` replays them |
| Clients that come and go | Client roles, tokens that resume an identity, the replay buffer and offline queue; and since S27 the link over plain HTTP requests (CLIENTS.md §3a), which needs no held connection |

What it does not have, and serverless needs:

- **A store that is not a local directory**: the WAL appends to a file and syncs it; the manifest is replaced by a
  rename; the lock is `flock`; blobs are files. Object storage has none of append, rename or lock.
- **A single writer without a lock file**: `StoreLock` holds the directory; nothing fences a second process on
  another machine.
- **A node that is not a process**: the runtime is a long-running server with listeners, a tick loop, timers driven by
  the wall clock, peers over TCP, and in-memory state about clients (the replay buffer, HTTP sessions).
- **An entry point that is not `blossom run`**: no API to open a store in-process, feed it inputs and query it.

## 3. A node as a library (the SQLite shape)

**Blossom as an embedded database**: a crate (`blossom-embed`) and a C ABI over it, so a program's state is a
directory an application opens, the way SQLite's is a file.

```rust
let db = blossom::open("todos.blossom", &program)?;   // recovers: flushed version + WAL tail (S24–S26)
db.input("add", row!["buy milk"])?;                    // queued for the next tick
let out = db.step()?;                                  // one tick: deterministic, durable (WAL synced) before it returns
for row in out.observed("todo") { … }                  // the tick's outputs: observed relations, host writes, sends
let open = db.query("open(t) = todo(_, t, false)")?;   // a read-only Datalog query at the last released tick (S23)
db.close()?;                                           // flushes; or just drop it: recovery covers a crash
```

- **What it is**: the node (`blossom-node`) with a driver that is a function call instead of a tick loop: no
  listeners, no threads of its own (the flush can run inline, or on a thread the embedder allows). It is the
  `ManualDriver` the tests already use, made a supported API: open with recovery, offer inputs, run a tick, read its
  released effects, query, flush, close.
- **Time**: the embedder passes `now` to `step` (as the simulator does), or uses the system clock. Timers fire at the
  next `step` past their time; `db.next_timer()` says when the embedder should call it.
- **Sends**: a program with several roles is several databases; their sends come out of `step` and the embedder
  delivers them (in-process, or over whatever transport it likes). The client link of CLIENTS.md is one such
  transport, already built.
- **Single writer**: the store's lock (as SQLite's file lock), so two processes cannot open one directory.
- **Why first**: it is useful on its own (an app with a Datalog database and no server, a CLI tool, tests), and every
  serverless shape below is this library plus a different store or a different wake-up.

**Litestream for Blossom**: the store's files are already the right shape to replicate (append-only WAL segments,
immutable SSTs, a manifest naming them). A sidecar, or the library itself, uploads sealed WAL segments and each new
SST and manifest to a bucket; a restore downloads the newest manifest, its SSTs and the WAL after it. That is backup
and read replicas for the library with no change to the commit path.

## 4. A store on a bucket

The store behind the `Vfs` today assumes a filesystem. Object storage gets its own layer, at the level the store
actually needs (whole objects, conditional creation, listing), not a filesystem emulation.

### 4.1 The layout

```
<bucket>/<deployment>/<node>/
  manifest/<seq>          the tree's manifest: SSTs, flushed version, writer epoch, reservations (§4.4)
  wal/<seq>               a batch of tick records (one object per commit), seq contiguous from the manifest's floor
  sst/<id>.sst            immutable, as today
  blob/<hash>             immutable blobs (content-addressed, so a duplicate PUT is harmless)
  clients/<seq>           the client registry, as a manifest-like sequence
```

Every object is written once (`If-None-Match: *`); nothing is renamed or appended. The current manifest is the
highest `manifest/<seq>`; the WAL is every `wal/<seq>` at or above the manifest's `wal_floor`.

### 4.2 One writer: epochs and fencing (SlateDB's scheme)

- **Open**: read the newest manifest; write `manifest/<n+1>` with `writer_epoch + 1` (conditional: if it exists,
  another writer opened first: read again, retry once, then refuse). Then write an empty **fence** object at the
  next WAL seq. From here the writer owns the next WAL seq.
- **A zombie** (an earlier invocation still running, a function frozen and thawed): its next WAL PUT targets a seq
  that now exists (the fence or the new writer's), the conditional PUT fails with 412, and it stops before releasing
  anything: its tick was never durable, so Invariant B says nothing of it left. Determinism does not save us here
  (a zombie's inputs differ), the fence does.
- **The manifest** is also changed by flushes and compactions: each is a conditional PUT of the next seq carrying the
  same epoch; a conflict means re-read and retry (a compactor elsewhere), or stop (a higher epoch).
- No lease, no clock, no lock service: the bucket is the only coordination.

### 4.3 The commit path

- A node runs ticks as now; their records accumulate. A **commit** is one `wal/<seq>` PUT of every record since the
  last, then the release of those ticks' effects (Invariant B, unchanged: "synced" now means "the PUT returned").
- The interval is the knob §1.2.3 describes: per request in a function (latency = one PUT), or every N ms under load
  (cost bounded by the interval). One process holding several nodes (§6.3) batches their records into one object.
- **Latency budget per durable request**: S3 Express One Zone ≈ 5–10 ms; S3 Standard 50–100 ms; GCS similar to
  Standard. A program that acknowledges clients pays it per acknowledgement; one that does not (reads, idempotent
  requests) pays nothing extra.
- **A faster log, later** (§1.2.1, Neon's and Durable Objects' answer): the WAL behind a trait, with a second
  implementation that replicates records to a small quorum of log nodes (we have Raft written in Blossom; a log node
  could be one), and ships sealed batches to the bucket in the background. The bucket stays the truth for SSTs.

### 4.4 Flush, compaction, reservations

- **Flush**: the memtable becomes `sst/<id>.sst` (a PUT), then `manifest/<seq+1>` names it and raises `wal_floor`
  past the WAL objects it covers (whose deletion follows, as the WAL's truncation does today: the record of the
  flushed tick stays, for the views' catch-up, S26).
- **Compaction**: as today; the writer can do it inline after a flush, or a separate compactor with its own epoch can
  (the manifest CAS keeps them apart).
- **Reservations** (`META` today: a range of tick numbers and of time reserved ahead, so a restarted node never
  reuses a tick or goes back in time) move into the manifest. Each open reserves a fresh range; a function that runs
  one request per instance spends a range per invocation, so the tick step shrinks (ticks are 64-bit: 2^16 per
  invocation is still ~2^48 invocations).

### 4.5 Reads

- The tiered engine reads the database through `ColdTables` (DATABASE.md §7); on a bucket that is block reads of SSTs
  (a ranged GET) through the block cache, then a local disk cache when the platform has one (a function's `/tmp`, a
  Cloud Run instance's memory), both disposable.
- **Cold start**: GET the newest manifest (a LIST then a GET), LIST the WAL from its floor, GET those objects, open.
  The views catch up from the WAL's written rows (S26), so the first tick reads only what it needs. Three or four
  round trips: 15–40 ms on S3 Express, 150–400 ms on Standard, plus the function's own start. A warm instance (the
  platform reuses it) skips all of it but the manifest check (one GET with `If-None-Match` on the ETag it has).

### 4.6 Testing it

The bucket gets a simulated implementation like `SimFs`: conditional PUTs, LIST consistency, latency, and crash points
between any two operations; and a "zombie" fault (a second writer replaying an old open). The properties are the ones
we already check, plus: no tick released by a fenced writer is ever visible, and every acknowledged tick survives any
interleaving of two writers.

## 5. A node in a function

A function invocation is an embedded node (§3) on a bucket store (§4), opened, stepped and closed. What changes is
everything that assumed a process outlives a request.

### 5.1 One invocation per node at a time

Two concurrent invocations for one node would fence each other (correct, but each kills the other's work). So the
platform must route a node's requests through one invocation at a time:

- **Lambda**: one function per node with reserved concurrency 1, or an SQS FIFO queue with the node as the message
  group, which serializes delivery per node;
- **Cloud Run**: `max-instances = 1` per node service, with concurrency inside the instance (one process, one node);
- **Durable Objects**: given (§6.1).

Under that routing a fence fires only on a real fault (a frozen instance thawing late), which is what it is for.

### 5.2 Inputs that arrive while nobody is running

- **HTTP clients**: a request *is* an invocation. The S27 HTTP link maps directly: `open`, `send` and `recv` are
  requests; a `send` runs a tick and is answered after its commit. What does not map is the long-polled `recv` and
  the replay buffer, which live in the node's memory today. In a function they go to the store: each tick's sends to a
  member are appended to the member's outbox (part of the tick's record, so durable with it), and a `recv` reads the
  outbox after the member's acknowledged seq. The replay buffer becomes durable, and `resumed` is true across
  restarts: an improvement for the long-running node too.
- **Streams** (raw TCP, the Kafka broker): do not fit a function (no socket outlives the request). A protocol served
  this way needs an always-on front, which is §6.2's "warm node" or a Durable Object.
- **Peers** (multi-node programs, Raft): a node cannot hold TCP connections to sleeping peers. Messages go to the
  receiver's **inbox** in the bucket (`inbox/<node>/<sender>/<seq>`, one object per sender's batch, conditional so a
  retry is idempotent) and wake it (§5.3). Channels are lossy already (LANGUAGE §8), so delivery may be delayed or
  duplicated (dropped by seq, as the link does) without changing a program's meaning. It is slow (a PUT and a wake per
  hop), so a chatty protocol like Raft in functions is possible but not fast; §6 is better for it.

### 5.3 Time and timers while asleep

- **The clock**: each invocation anchors at max(wall clock, the time reserved by the last one), as `SystemClock`
  anchors at the boot instant today, so time never goes backwards across invocations on different machines.
- **Timers**: a sleeping node's timers do not fire; Aurora's "skipped jobs" is what happens if nothing wakes it. At
  each commit the node writes its **next wake** (the earliest pending timer) into the manifest, and a scheduler
  (EventBridge Scheduler, Cloud Scheduler, a cron, a Durable Object alarm) invokes it then. A timer that fires late
  fires late, which programs already allow (timers are physical, LANGUAGE §15.2); one that must not slip needs a node
  that does not sleep.
- **Leases and liveness** (a Raft leader's heartbeats, failure detectors) assume a node that is running. A program
  that leans on them is for §6, not for functions; the checker could say so (a program whose timers fire faster than
  a wake can be scheduled).

### 5.4 What a request costs

Cold: the platform's start (for a Rust function, tens of milliseconds by common report; not measured here) + recovery
(§4.5) + one tick + one commit PUT. Warm: one tick
+ one PUT (S3 Express ≈ 6 ms). A read-only request (a query, a `recv` with nothing durable to do) skips the PUT.

## 6. The other shapes

### 6.1 A node as a Durable Object

Durable Objects give, per name, what §5.1–§5.3 build: one instance, an input gate (one event at a time: a tick), an
output gate (effects after durability: Invariant B), alarms (timers), storage, and hibernating WebSockets (the client
link, both transports). A Blossom node maps onto one almost directly:

- the engine compiles to wasm already (the page's member engine, S22);
- the store is the object's storage: the `ColdTables` and the WAL as tables in its SQLite, or the `Vfs` over it;
- each tick is one event; the alarm is the next timer; peers are other objects, addressed by node name;
- clients connect to the object over the link (WebSocket with hibernation, or HTTP).

It is the least work for the most of "serverless", at the price of one platform.

### 6.2 A warm node that sleeps

Between a function and a server: a container platform that scales to zero (Cloud Run, Fly Machines with auto-stop),
one instance per node, a volume or the bucket store. It keeps sockets, streams and timers while it runs, and pays a
cold start (recovery is §4.5) when it wakes. For a program with streams (Kafka) this is the serverless that fits.

### 6.3 Many nodes in one process

Turso's and WarpStream's cost lesson (§1.1): a process that hosts many nodes (tenants, or the members of one
deployment) batches their WAL records into one PUT per interval, and the per-node cost of object storage falls by the
number of nodes. The embedded library (§3) makes a node a value, so a host of many is a map of them.

## 7. The plan

In order, each a slice of its own with its tests and benchmarks; each is useful when it lands.

1. **S28: Blossom as a library.** `blossom-embed`: open (recovery), input, step (durable before it returns), the
   tick's effects, query at the last released tick, flush, close; time passed in or from the system clock;
   `next_timer`. The `ManualDriver` becomes this API. An example application with no server; the node's own
   runtime rebuilt on it, so there is one way to run a node. The durable client outbox (§5.2) lands here too: it
   helps every shape.
2. **S29: the store on a bucket.** An `ObjectStore` trait (GET, ranged GET, PUT if absent, PUT if match, LIST,
   DELETE) with a local-directory implementation (for development), an S3 one (also R2 and MinIO, which speak its
   API), and a simulated one with crash points and latency. The WAL as objects with group commit, manifests with
   epochs and fencing, SSTs and blobs as objects, reservations in the manifest. Recovery from a bucket; the zombie
   writer in the simulator. Benchmarks against S3 Express and Standard: commit latency, cold start, cost per 1000
   ticks.
3. **S30: a node in a function.** An adapter that runs the library on a bucket per invocation: the HTTP link's
   requests as invocations (durable outbox, `recv` without a long poll when there is nothing to wait for), the next
   wake written at each commit and a scheduler recipe, peer inboxes. A Lambda deployment of the shared TodoMVC
   (`examples/web/todos_shared.bls`) with its page on a static host, end to end, and its numbers.
4. **Then, as needed**: a Durable Objects host (§6.1); a replicated low-latency WAL (§4.3); a host of many nodes
   batching their commits (§6.3).

Two choices in this plan are the user's and are asked separately: whether S28 (the library) comes first or S29 (the
bucket store) does, and which object store the first implementation and benchmarks target.
