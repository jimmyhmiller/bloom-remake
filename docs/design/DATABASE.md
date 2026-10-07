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

The database is the node's durable state (since S24; before, it was a copy beside a checkpoint chain).

- **Feeding.** Each released tick's durable delta (part of `ReleasedTick`) is applied to the database at the tick's
  number by the driver, after the tick's WAL record is synced. So the database only ever holds released ticks.
- **Flushing.** The manual driver flushes when the memtable passes its size, or when asked (the simulator asks at
  random). The runtime's engine asks its database thread for a flush when the memtable passes its size or the WAL
  has grown `storage.checkpoint_wal_bytes` since the last; compaction follows each flush.
- **The WAL behind it.** After a flush, the blobs logged in the records the tables cover become files
  (`FileWal::covered`), and the older WAL segments the tables wholly cover go (`FileWal::truncation`, whose token
  only a `Flushed` from the tree can make). Blob collection anchors to flushes: the blobs no recovery from the tables
  and no running rule can reach are deleted.
- **Recovery** (`blossom-node::recovery`, the same for the runtime, the manual driver and the simulator) opens the
  database, takes its rows as of the tick its tables cover, and applies the WAL records after that tick to both those
  rows and the database. A store from before the database recovers from its checkpoint chain and the WAL after it
  once: the database starts from those rows (its history begins there), is flushed, and the checkpoints go.
- **History.** `storage.history_ticks` (default 65 536) is how far back an as-of query may go; older versions are
  merged away by compaction.
- **The engine** still loads the recovered rows into memory and keeps every relation there; reading durable relations
  from the database instead is §7.

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

## 7. Next: the engine on the database (S24)

Done in S24's first step: the database is the durable state of every driver (item 10 below), checkpoints are retired
(item 8, except that the engine still loads the recovered rows), and the simulator's crashes reach the database.
Done in its second: the node keeps no copy of the released rows (admission reads the database), and a set table
carried by its frame has no next-state copy (item 2: its `Next` store holds only the other next-state rules'
support, and the tick's change to the next state is computed from the stores the frame reads). The rest:

Today the engine holds every relation in memory (`blossom-engine::store::Store`: rows with support counts, indexes
built on use) and loads the durable ones from the recovered rows at boot; the database holds them again on disk.
Moving durable relations onto the database, so a node's durable state may outgrow its memory:

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
10. **One durability path for the runtime and the simulator.** The simulator's cluster recovers nodes through
    `blossom-node`'s `ManualDriver` and checkpoints over `SimFs`, as the runtime does through `recovery::open`. The
    database lives in `blossom-runtime` today, so the simulator never exercises it. Before checkpoints retire, the
    database moves into the shared path (its tree and feeding into `blossom-node`'s recovery and drivers, its
    flushes driven deterministically under simulation), so every crash the simulator explores reaches it.

**How S24 builds items 1–7 (step 2c).**

- *Store.* `Lsm::amend` writes keys at the newest applied version (a derived keyspace built between two ticks: it
  claims no tick). `Lsm::scan_page` reads a range a bounded page at a time, so no scan holds a whole table.
- *Derived keyspaces in the database.* An index of relation `r` on columns `C` is the keyspace
  `itag(r, C) ++ keycode(row[C]) ++ codec(row) ++ len`; the blobs of `r` the keyspace `btag(r) ++ blob ++ codec(row)
  ++ len`. Each has a definition key (under a keyspace of its own); `Database::apply` and `bootstrap` keep every
  defined keyspace with the rows they write. The blob keyspaces are defined at open (built from the rows when a
  database from before them has rows); an index is defined the first time a probe needs it, and built from the rows
  at the applied version in the same `amend` as its definition (a crash loses both or neither; the build holds the
  new keys in memory, once). A definition made before any version is applied is written with the first.
- *The engine's cold side* is a trait (`blossom_engine::ColdTables`: the newest version, the tables, contains, a
  probe by columns with an optional range on one more, count; all as of a version) the node's `Database`
  implements. A probe on a leading run of the declared columns (and a range on the next) reads the relation's own
  keys; any other reads an index, and only as of the newest version.
- *Tiered tables.* After `Engine::reset_on(volatile, cold)`, a durable set table carried by its frame and written by
  no rule but its next state's keeps in memory only: the carry's changes the database may not hold yet (`overlay`,
  each row's newest carried membership with the tick that set it), support other than the carry (a program fact's),
  and the tick's change. A row is carried as the overlay says, else as the database says at its newest version: an
  overlay entry the database has caught up with agrees with it (no later tick changed the row, or the entry would be
  newer), and is dropped at a tick's start. The store takes a tick's change to its next state at the end of that
  tick, before the node can release the tick to the database, so every read is at the database's newest version;
  the change shows as the next tick's. Its first tick after the reset shows every row as new (the rules' stores
  start empty after any reset, and derive from all of the table), reading the table whole once, and its next state
  is compared with what the database held (as the baseline does for the other relations). Tables the engine does
  not tier (lattice tables, sealed and resolved ones, those a rule writes) are loaded from the database.
- *The hot tier.* A row's key is the whole row, so a table updated in place leaves a delete and a new key per
  update under the same prefix, and the history the tree keeps (`storage.history_ticks`) leaves them there: a probe
  of the tree walks them. Each tiered table keeps the answers to its recent probes (the rows with given values in
  given columns, sorted; whether a row is there) in a hot tier bounded in rows (`EngineConfig::hot_rows`, 16384 by
  default), kept equal to the database's newest version: an overlay entry the database catches up with is applied
  to them, not dropped, so a probe asked again never reads the tree. A range probe on the column after a leading
  run is cut from its prefix's kept rows by binary search; a prefix too large to keep is known as such, and its
  ranges read the tree. The overlay is indexed by the columns probes ask for, so a probe's correction costs the
  overlay's rows with its values. The Kafka produce benchmark (three brokers, 1 KiB records) went from 724 records/s
  without the hot tier to within about 10% of the in-memory engine.
- *Blobs.* The executor answers only for rows beyond the carried state (`Executor::holds_blob`), and a tiered table
  holds only carried rows once a tick has ended: it answers for none. The node counts the carried rows' blobs from
  the blob keyspaces at boot, then from each tick's change.
- *The node* boots on the database (`Boot::database`): recovery replays the WAL into it and builds an image only to
  start a database the store did not have. `Executor::reset_on` hands the executor the database: the engine tiers
  what it can; an executor of whole instances (the oracle, a recording) reads every table.
- *Checking.* `Backend::Checked` runs the engine and the oracle side by side and fails the first tick whose outputs
  differ; `BLOSSOM_EVALUATOR=checked` makes every simulated cluster run it.

Still in memory: lattice and sealed durable tables; the views over durable tables (rebuilt from all of a table at
the first tick after a boot); the node's per-blob counts.

Tests: every suite and the corpus run on the tiered stores (the engine and the oracle still agree on every corpus
program); a node whose durable state is several times its cache; kill -9 and the crash simulation over the
database as the only durable state.
