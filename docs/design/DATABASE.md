# The database behind a node (S23)

The user (2026-10-07): "what would it take to get us a real proper persisted database instead of just a write-ahead
log so that we could really have a datalog database behind all of this that can do datalog queries". Chosen
(question tool): queries and on-disk storage designed together as one slice, after S22; our own LSM tree over our
VFS, so the crash simulation keeps covering it.

## 1. What the database is

A node's database is its program's **durable relations**, as of its **released ticks**. That is exactly what the
node promises survives a crash (ARCHITECTURE §5.6, Invariant R), so a query never sees a row a crash could take
back, and never a row of a tick that has not been released. Tick-local and non-durable relations are not in it: they
are the program's working state, rebuilt after a restart.

Every row carries the tick that inserted it and, once deleted, the tick that deleted it. A query reads the database
**as of** a tick `T`: the rows present after tick `T` was released. `T` defaults to the latest released tick; any
tick inside the history the node keeps (§4) can be asked for.

## 2. Storage: an LSM tree of versioned keys

`blossom-store`'s `lsm` module is a byte-level, versioned LSM tree over the `Vfs` (so `SimFs` crash tests reach it):

- **Entries** are `(key, version, op)`: `key` bytes, `version` the tick, `op` put or delete. Entries order by key
  ascending, then version descending. A read as of `T` takes, per key, the first entry with `version ≤ T`; the key is
  present when that entry is a put.
- **The memtable** holds the entries of released ticks not yet flushed, in that order.
- **SSTables** (`db/sst/<id>.sst`) are immutable sorted runs: data blocks of entries, each with its CRC32C; an index
  of each block's first key, offset and length; a footer with the entry count, the version range and its own CRC.
  A reader keeps the index in memory and reads blocks with `pread`.
- **The manifest** (`db/MANIFEST`, written atomically with a BLAKE3 checksum, like a checkpoint's) lists the live
  SSTables and the tick and WAL position the flushed entries cover. A flush writes the SSTable (to a temporary name,
  synced, renamed, the directory synced), then the manifest. An SSTable no manifest names is a crashed flush's, and
  opening deletes it.
- **Compaction** is size-tiered: when four or more SSTables are within a factor of two of each other in size, they
  merge into one. A merge keeps every version newer than the history horizon (§4) and, per key, the newest version at
  or below it; a merge that includes the oldest SSTable also drops deletes at or below the horizon (nothing older can
  resurrect the key).

## 3. Rows as keys

A durable relation's row is the key `tag ++ ordered(columns) ++ codec(row) ++ len`:

- `tag`: 8 bytes of BLAKE3 over the relation's name and schema hash;
- `ordered(columns)`: each column's value in declaration order, in an order-preserving encoding
  (`blossom-node::keycode`): within a column's type, bytes order as values do for `bool`, the integers, `f64`
  (totalOrder), strings and bytes (escaped, so no encoding is a prefix of another), `Duration`, `Instant`, nodes (by
  name), and tuples, structs, enums, `Vec` and `Option` of those; other values (sets, maps, lattice values, …) are
  encoded whole in the canonical codec, escaped: equal values, equal bytes, in no meaningful order;
- `codec(row)`: the row in the durable tuple codec (`Node` values by name), which is what reading a key decodes, and
  its length as a big-endian `u32`.

So rows agreeing on their leading columns share a prefix (a lookup binding them is a prefix scan), keys order as rows
do column by column (a comparison on the next column is a range scan), and a scan of the relation is a scan of its
tag. The database's manifest records this key format (1); a database written in another is rebuilt from the
recovered rows when its node opens it.

## 4. How the node keeps it

- **Feeding.** Each released tick's durable delta (now part of `ReleasedTick`) is applied to the database at the
  tick's number, on the engine thread, after the tick's WAL record is synced. So the database only ever holds
  released ticks.
- **Flushing** happens on a database thread when the memtable passes 4 MiB, and compaction after a flush.
- **Recovery.** The manifest names the tick its SSTables cover; the WAL records after it are applied again
  (the WAL keeps them: its truncation waits for both the checkpoint and the database flush).
- **History.** `storage.history_ticks` (default 65 536) is how far back an as-of query may go; older versions are
  merged away by compaction.
- **Checkpoints stay, for now.** The engine still holds every relation in memory, and recovers it from the
  checkpoint chain. The database is a second, queryable copy of the durable rows. The engine reading durable
  relations from the database through a cache (so a node's durable state may outgrow its memory, and checkpoints
  retire) is §7.

## 5. Queries

A query is a view over the program's durable relations, written as a view is without its keyword:

```
blossom query --deploy d.toml --node s --admin 127.0.0.1:9900 'open(t) = todos(_, _, _, _, t, false, true)'
blossom query --deploy d.toml --node s --admin 127.0.0.1:9900 --as-of 812 'big(k) = store(k, v), v.len() > 2'
blossom query --deploy d.toml --node s --store data/s 'all(k, v) = store(k, v)'
```

- **Compiling.** The CLI adds `view <query>;` at the node's role to the deployed program's root file and compiles
  it; `ValidatedProgram::query` keeps the view's rules and the rules of every view they read, and turns each durable
  relation read into an input. A query that reads anything else (but static relations) is refused: only durable
  relations are in the database. The node never compiles: it gets this small IR program (`QueryRequest`, postcard).
- **Evaluating.** The oracle evaluates the kept rules, as the node at the tick asked, over the durable relations' rows
  as of that tick. A relation read only by atoms is read by prefix when every atom binds its leading columns to the
  same constants; a relation read by one atom, by a range when that atom's rule compares the next column with
  constants (`>`, `>=`, `<`, `<=`, `==`, either way round, the tightest bounds winning); by its tag otherwise. The
  rules still check every comparison, so reading only those rows is exact. (One difference remains: a row outside
  the range never reaches the query's rules, so an expression that would have raised a runtime error on it, like a
  division by zero, does not.)
- **Where.** Live, against a running node: `blossom run --admin ADDR` serves `POST /query` on its own listener; the
  answer is JSON (the tick read, the columns, the rows, each value as Blossom writes it). Relations are matched by
  name and schema hash: a query compiled against another version of the program is refused. The admin plane has no
  authentication yet (DIST-066), so `--admin` is refused outside `insecure-dev`. Offline: `blossom query --store DIR`
  takes the store's lock (a running node's store is refused), opens the database without changing a file, and
  applies the WAL records after its tables in memory: the state the node's next recovery would boot with.

## 6. Tests

- The LSM against a model (a map of versioned rows): random puts, deletes, flushes, compactions and as-of reads, on
  `SimFs`, with a crash at every write and sync of a flush or compaction: reopening gives the model's state as of the
  manifest's tick, and the WAL replay restores the rest.
- The node: the database agrees with the released image after every tick of a run; kill -9 and restart; as-of reads
  of earlier ticks.
- Queries: the shared TodoMVC and the KV store, live and offline, prefix and full scans, as-of, refusals.

## 7. Next: the engine on the database (S24, planned)

Today the engine holds every relation in memory (`blossom-engine::store::Store`: rows with support counts, indexes
built on use), a table twice (its `Main` store, the rows now, and its `Next` store, the rows the next tick starts
from, whose change is applied to `Main` at the next tick's start), and recovers durable ones from the checkpoint
chain; the database is a third copy. Moving durable
relations onto the database, so a node's durable state may outgrow its memory and checkpoints retire:

1. **A tiered store for each durable table.** `base`: the table as of the previous tick, which is the database at the
   released tick plus an in-memory overlay of the computed-but-unreleased ticks' deltas. `hot`: the counting store of
   only the rows whose support this tick is more than their carry (a cold row's support is exactly 1, its carry, and
   is not stored). A row moves into `hot` when a derivation or a deletion touches it, and back out when its support
   returns to its carry at the end of the tick.
2. **No next-state store for durable tables.** The frame (`FramePlan`, already incremental: only the rows that
   changed this tick move) yields the tick's delta directly; the delta is the WAL record, joins the overlay, and goes
   to the database when the tick is released. `retract_all` and other whole-store operations never apply to tables.
3. **Reads.** `contains(row)`: `hot`, then the overlay, then a point lookup in the database (`Lsm::get`, a key's
   newest entry at or below the version). `present()`: a merge of the three, ordered by key. `old()` (the table at
   the start of the tick) is `base` itself.
4. **Indexes as keyspaces.** A plan's probes on a key prefix are prefix scans. For every other column set a plan
   probes (known when the engine plans the program), the database keeps a secondary keyspace
   `tag' ++ cols ++ row`, written with every apply. Probes merge the keyspace's scan with `hot` and the overlay.
5. **Lattice tables** keep their merged cells in the database and their live contributions in `hot` (a cell's carried
   row is one contribution).
6. **Blob counts** (FOREIGN-PROTOCOLS §5) move to a keyspace of their own, kept with every apply.
7. **A block cache**: decoded SSTable blocks in a bounded LRU (`storage.cache_bytes`), so a node's working set stays
   in memory and the rest is read on demand.
8. **Recovery and checkpoints.** A node boots on its database (the WAL after the database's flushed tick applied
   to it); it loads no image. Checkpoints retire: the WAL truncates behind the database alone, and blob collection
   anchors to its flushes.
9. **Range probes** of plans become range scans of the order-preserving keys (§3).

Tests: every suite and the corpus run on the tiered stores (the engine and the oracle still agree on every corpus
program); a node whose durable state is several times its cache; kill -9 and the crash simulation over the
database as the only durable state.
