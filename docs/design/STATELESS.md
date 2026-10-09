# Stateless hosting: a deployment on external storage

The user (2026-10-09): "we need an easy way to let blossom work with an external data store like sql. We need
adapters for letting this happen. Also on blob storage like s3. I want to be able to deploy blossom to a function as a
service setup and have it just work. The proof will be a keyed chat setup that can work with and without websockets
that can be hosted on a stateless http server with external storage."

Their choices (2026-10-09):

- **Storage shape: blobs now, tables next.** A node's store (its WAL and database files, laid over keys and values by
  `KvFs`, DURABLE-OBJECTS.md way A) is what the adapters keep. Durable relations as real SQL tables (way B,
  `ColdTables` over SQL) is a later sub-slice (§12).
- **Adapters:** Postgres and SQLite, each its own; and S3 (MinIO locally).
- **Host:** a generic stateless server, `blossom serve`, run as several copies behind any load balancer.
- **WebSockets:** the server accepts them as well as the plain-request link; whether a platform passes them through
  is the platform's business.

## 1. What "stateless" means here

A `blossom serve` process keeps nothing that matters between requests. Any instance answers any request; an instance
may be killed at any moment, and another started, with no loss of anything a page was told. Everything durable is in
the **state store**: the nodes' stores, the pages' links, the timers, the messages between nodes.

An instance does keep caches (a compiled program, nodes it ran recently, blobs it read), each checked against the
store before it is used, so a cache is never the source of truth.

## 2. Objects

The unit of state is an **object**, as on Durable Objects (KEYED.md §4): `node/NAME` runs the deployment's node
`NAME`, `member/ROLE/KEY` the keyed member of `ROLE` named `KEY`, and `registry` mints pages' tokens. An object is
created by the first request for it. Each object's state is a set of entries (key → bytes) and a **version**, which
every commit raises by one.

A request is served by running its object's node for the length of the request: load it, deliver what the request
carries, run its ticks until it is quiescent (`ManualDriver`, each tick durable before the next), commit what they
wrote, and only then answer. This is `ObjectNode`, the node Durable Objects run, without the single live instance a
Durable Object guarantees: two instances may run the same object at once, and the store decides which commit wins.

## 3. The state store

`blossom_statestore::StateStore` is the one seam between a host and its storage:

```rust
pub trait StateStore: Send + Sync {
    /// The object's entries and version (0 and none: never committed).
    fn load(&self, object: &str) -> Result<Snapshot, StoreError>;
    /// The object's version, without its entries: how a cached node checks it is current.
    fn version(&self, object: &str) -> Result<u64, StoreError>;
    /// Applies `writes` and raises the version to `expected + 1`, if the version is still `expected`; else changes
    /// nothing and says which version is there.
    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StoreError>;
    /// Returns when the object's version is past `since`, or at `deadline`: the version then.
    fn wait(&self, object: &str, since: u64, deadline: Instant) -> Result<u64, StoreError>;
    /// Asks for the object to be woken at `at` (milliseconds since the epoch); a later ask for an earlier time wins.
    fn schedule(&self, object: &str, at: u64) -> Result<(), StoreError>;
    /// The objects due at `now` (at most `limit`), each with the time it was due.
    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StoreError>;
    /// Forgets the object's wake at `at` (only if it is still that one).
    fn unschedule(&self, object: &str, at: u64) -> Result<(), StoreError>;
    /// Unversioned records outside any object, last write wins: pages' presence (§6.4).
    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StoreError>;
    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn delete_side(&self, key: &str) -> Result<(), StoreError>;
}
```

**What a commit promises.** A commit is atomic (all its writes or none) and conditional (it applies only at the
version it names). Two commits at the same version: at most one succeeds. A commit that returned succeeded is durable.
A commit whose outcome is unknown (the connection failed after it was sent) is reported as an error, never as a
conflict or a success; the caller treats the object as unknown and loads it again.

**Waking.** `schedule` happens before the commit that needs it (so a crash between the two leaves an extra wake,
never a lost one); `due` and `unschedule` let any instance find the objects whose time came. A spurious wake costs a
load and nothing else: a node with no timer due runs no tick and commits nothing.

**Conformance.** `blossom_statestore::conformance` is one suite every adapter runs: atomicity, the version check,
concurrent committers from several threads (exactly one winner per version, every winner's writes visible, no lost
or torn write), `wait` woken by another handle's commit, schedules, side records, large values (several MiB), many
keys, keys that sort strangely (prefixes, UTF-8, `/`), and a reload through a second handle that shares nothing in
memory with the first. A memory store (`MemStore`) passes it too and serves the deterministic tests (§10).

## 4. The adapters

All three are crates of their own, so a deployment links only the client libraries it uses.

### 4.1 SQLite (`blossom-statestore-sqlite`)

One database file, shared by every instance on one machine (WAL journal mode, a busy timeout).

```sql
create table objects (object text primary key, version integer not null);
create table entries (object text not null, key text not null, value blob not null, primary key (object, key));
create table wakes   (object text primary key, at integer not null);
create index wakes_at on wakes (at);
create table side    (key text primary key, value blob not null);
```

A commit is one `BEGIN IMMEDIATE` transaction: check the version, apply the writes, raise the version. `wait` polls
the version (a read of one row) every 20 ms, and returns at once for a commit made through the same handle.
SQLite suits one machine: several processes behind a local proxy, or a single instance that restarts.

### 4.2 Postgres (`blossom-statestore-postgres`)

The same tables (`bytea` for blobs, `bigint` for numbers), in a schema the URL names. A commit is one transaction:
`UPDATE objects SET version = version + 1 WHERE object = $1 AND version = $2` (an insert for version 0, refused when
the row exists), the writes, then `NOTIFY` on the deployment's channel with the object and its new version. Each
instance keeps one connection `LISTEN`ing and wakes its waiters; the notification is a hint, and `wait` re-reads the
version before it returns, so a lost notification costs latency (a poll every second backs it up), never a missed
change. Connections are pooled; TLS is supported (rustls), required when the URL says `sslmode=require`.

### 4.3 S3 (`blossom-statestore-s3`)

S3 has no transaction across keys, so an object's state is a **manifest** plus **blobs**:

- `PREFIX/o/OBJECT/head`: the manifest: the version, and each entry's key with either its value (values of at most
  1 KiB are inline) or the name of the blob that holds it;
- `PREFIX/o/OBJECT/b/HASH`: a blob, named by the BLAKE3 of its bytes, written once (`If-None-Match: *`), never
  changed.

A commit uploads the blobs the new manifest names that are not there yet, then writes the manifest with `If-Match:
ETAG` (the ETag of the manifest it loaded; `If-None-Match: *` for version 0). S3 answers `412 Precondition Failed`
when another commit came first: a conflict. This is the conditional write S3 (since 2024), R2, GCS and MinIO support.
Blobs are immutable, so an instance caches them by name without ever checking them again; a load reads the manifest
and only the blobs it has not seen.

Blobs no manifest names any more (a value overwritten, a commit that lost) are deleted by a collector that lists an
object's blobs and deletes those the current manifest does not name and that are older than a grace period (10
minutes by default), so a reader that loaded an older manifest a moment ago still finds its blobs.

Wakes are objects too (`PREFIX/w/AT/OBJECT`, `AT` zero-padded so a listing is in time order), as are side records
(`PREFIX/s/KEY`). `wait` polls the manifest with `If-None-Match` on its ETag (a `304` costs nothing to transfer),
every 100 ms by default.

Requests are signed with AWS Signature Version 4, written here (checked against AWS's published examples) over a
small HTTP client with rustls; the endpoint, region, bucket, prefix and path-style addressing come from the URL, the
credentials from the environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`).

### 4.4 Store URLs

`blossom serve --store URL`:

- `sqlite:PATH` (relative to the working directory);
- `postgres://USER:PASS@HOST:PORT/DB?schema=NAME&sslmode=disable|require`;
- `s3://BUCKET/PREFIX?endpoint=http://127.0.0.1:9000&region=us-east-1&path_style=true`;
- `memory:` (one process; tests and trying things).

## 5. A request

1. **Find the node.** The instance's cache holds, per object, a running `ObjectNode` and the version it is at. If
   the store's version is the cached one, the cached node is current; else it is dropped and the object loaded
   (entries into a `JournalKv`, the node recovered from them as on any restart).
2. **Run.** The request's frames go to the node; its ticks run until it is quiescent. Every write lands in the
   journal; nothing leaves.
3. **Commit** the journal at the version the node started from, with the object's next wake scheduled first.
4. **On success**, the cache keeps the node at the new version, and the request's answer and the node's messages
   to other objects (§8) go out: after the commit, as Invariant R requires.
5. **On a conflict**, the node and its output are thrown away (the node ran ahead of a state that does not exist),
   the object is loaded at the version that won, and the request runs again from step 2. A request is retried at most
   16 times, with jittered backoff, then answered `503` (the page reconnects and resumes; nothing it was told is
   lost).
6. **On an error** (the store failed, or the outcome of the commit is unknown), the node is dropped from the cache and
   the request is answered `503`.

A request that changes nothing (a receive with nothing new, a wake with no timer due) commits nothing.

Two instances racing on one object make progress: each conflict means another commit succeeded. Retrying is safe
because a node's input is the request alone: a page's frames carry their own numbers, so running the same `MSG` again
against the state that won delivers it once (the link drops a batch it took, CLIENTS.md §3).

## 6. Links without a connection

On `blossom run` and on Durable Objects, a page's link lives in the node's memory: the numbering, the replay buffer of
what the node sent, and the connection. Here they are part of the object's committed state, so any instance can
continue any page's link.

### 6.1 The link's state

`MemberLinks` keeps, per page: its role, the next number of a batch to it, the replay buffer of batches it has not
acknowledged, what was lost from it, and the highest number of its batches taken. At the end of a request every
message the node was given has been taken by a released tick (the node is quiescent), so nothing waits for an
acknowledgement and the state is complete; it is written to the object's entries (`L/<page id>`) in the same commit
as the ticks' writes. A request that finds messages still waiting for their acknowledgement is a fault, not a state
to persist.

### 6.2 Sessions

A **session** stands for a connection (CLIENTS.md §3a): what a `connected` opens and a `disconnected` closes. Its
record (`S/<session>`, in the object's entries) holds the page it is for, the page's `WELCOME`, and the time it was
opened. The session's id is the object's name and 128 random bits (`ROLE/KEY~HEX`), so a request names its object
without a lookup, and only the page holding the id can use the session.

### 6.3 What a receive answers

A receive does not take frames from a queue in memory: it computes them from the state, starting from a **cursor**
the page sends back each time (`Blossom-Cursor`, opaque to the page): whether the session's `WELCOME` was delivered,
the number of the last batch delivered, and the last acknowledgement delivered. The answer is the `WELCOME` (if not
delivered yet), the replay buffer's batches after the cursor, and an `ACK` with the highest number taken (if it
moved), and the next cursor. Nothing is written for a receive, so receives never conflict.

A receive with nothing to answer waits (`StateStore::wait`) for the object's next commit, up to 25 s, then answers
empty, as on `blossom run`. A lost answer is the connection's loss; the page opens a new session and resumes from
the replay buffer, as after any loss.

The page changes in two places: the open request names the member (`?member=KEY`, as the WebSocket's URL does), and
the receive loop sends the cursor it was last given (`?cursor=`). Both are ignored by `blossom run`.

### 6.4 When a session ends

As on `blossom run` (CLIENTS.md §3a): the page closes it, the member opens another, the page does not take what the
node sends (the replay buffer's bound), or no request reached it for 30 s. The last needs a clock that does not live
in an instance: each request a session makes records the time in a side record (`presence/OBJECT/SESSION`, no
version, last write wins, so receives still never conflict), and the object schedules a wake at the session's
deadline. At the wake, a session whose presence is older than 30 s ends: the node hears `disconnected`. A session
seen since is checked again at its new deadline.

### 6.5 WebSockets

An instance that accepts a page's WebSocket holds the socket and nothing else: each frame the page sends is a request
(§5); the instance pushes what a receive would answer, waiting on the object's commits; and it records the session's
presence every 10 s while the socket is open. When the socket closes, the instance ends the session (a request). When
the instance dies, the socket closes, the presence stops, and the session ends at its deadline; the page reconnects,
to any instance, and resumes.

## 7. Tokens

Pages' tokens are signed with the deployment's seed (`BLOSSOM_SEED`, the same secret on every instance;
KEYED.md §4), so any instance checks one without the store. Their serials come from the `registry` object: a commit
that raises its counter, retried on a conflict like any request, so no two pages get one serial.

## 8. Messages between objects

A node's sends to another node or member (`Output::Send`) are written to the sender's **outbox** in the same commit
as the tick that sent them (`X/<seq>`, numbered per sender). After the commit, the instance delivers each to its
target (a request on the target object, §5), which records the highest number it took from that sender
(`R/<sender>`) in the same commit as the delivery and drops a number it already took. Then the sender's outbox is
trimmed (a commit; a conflict there is harmless, the next request trims it). An outbox that is not empty schedules
the sender a wake 1 s ahead, so an instance that dies between the two commits leaves messages a sweeper delivers.
Delivery is exactly once and in order per sender and target: stronger than a channel promises (LANGUAGE §8).

## 9. Timers and the sweeper

An object's next wake is the earliest of its node's next timer, its sessions' deadlines and its outbox's retry
(§6.4, §8), scheduled before each commit. Each `blossom serve` instance runs a sweeper (every 250 ms by default):
`due`, then a wake request on each object due, then `unschedule`. Several sweepers may wake one object at once; the
version check makes that harmless. A platform that freezes idle instances (a function) calls `POST /blossom/wake`
from a scheduler instead (`--sweep off`), which runs one sweep and answers what it woke.

## 10. Testing

- **Conformance** (§3) on `MemStore`, SQLite (a temporary file), Postgres (a cluster `scripts/test-services.sh`
  starts with `pg_ctl` under the scratch directory) and S3 (MinIO in Docker, the same script). A test that needs a
  service it cannot reach fails, naming the variable to set or the script to run; it never passes by skipping.
- **Signature V4**: AWS's documented examples, byte for byte.
- **Races, deterministically**: a test host runs several instances over one `MemStore` that can stall a commit,
  fail it, or report it lost, under seeds, with pages driving the chat; the oracle is that every page sees every line
  once and the program's tables equal those of one instance run alone.
- **The proof** (Playwright, `tests/web/stateless.spec.mjs`): the keyed chat on three `blossom serve` instances behind
  a round-robin proxy (`scripts/rr-proxy.mjs`), for each of SQLite, Postgres and MinIO, and each of the plain-request
  and WebSocket links: pages in two rooms chat, an instance is killed and another started mid-chat, every page keeps
  every line exactly once and the rooms stay apart; a page closed for good leaves the room's list at its deadline.

## 11. `blossom serve`

```sh
BLOSSOM_SEED=$(openssl rand -hex 16) \
  blossom serve --deploy examples/web/keyed_chat.deploy.toml --store sqlite:.data/chat.db --web 127.0.0.1:8081
```

It compiles the deployment's program once at start, then serves the page (`--web-root`), `app.json`, the client
parts, `/blossom/token`, the link both ways (`/blossom/http/…`, `/blossom/link`), and `/blossom/wake`. Its options:
`--store`, `--web`, `--web-root`, `--sweep MS|off`, `--cache N` (nodes kept per instance). The deployment's nodes
need no addresses: their objects are reached through the store. A program whose nodes use streams or answer external
sessions is refused at start, naming what an object cannot do yet.

## 12. Sub-slices

1. **The store and its adapters**: the trait, `MemStore`, the conformance suite; SQLite; Postgres; S3 with SigV4 and
   the collector. Gate: conformance green on all four.
2. **Durable links**: `MemberLinks` and sessions in an object's entries; the cursor receive; presence and deadlines;
   the page's two changes. Gate: an `ObjectNode` reloaded between every request keeps a page's link.
3. **`blossom serve`**: requests, the cache, conflicts and retries, tokens, both links, the sweeper, the outbox.
   Gate: the deterministic race tests.
4. **The proof**: `examples/web/keyed_chat.bls` and the Playwright matrix.
5. **Next** (its own slice): durable relations as SQL tables (`ColdTables` over Postgres and SQLite), so state is
   queryable and a request reads only what it touches.

## 13. Out of scope

Streams and external sessions at objects (refused, as on Durable Objects); loading an object incrementally (a load
reads all its entries; a room's store is small, and the cache saves most loads); a socket gateway for platforms that
hold sockets for functions.
