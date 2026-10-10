# Durable relations as SQL tables

**Status (2026-10-09):** built on branch `slice-sql`: `KeyTree` (§1), the tables and `TableStore` on Postgres,
SQLite and memory (§2, §5), the SQL tree and stateless objects on it (§4). With the keyed chat on Postgres:

```sql
set search_path = <the store's schema>;
select member, who, k, text from log order by at;
--  member |       who       | k |         text
-- --------+-----------------+---+----------------------
--  lunch  | Browser#0@rooms | 0 | anyone up for tacos?
--  lunch  | Browser#1@rooms | 0 | yes! 12:30?
--  dinner | Browser#2@rooms | 0 | pasta tonight
```

The user (2026-10-09), after stateless hosting (STATELESS.md): durable relations as real Postgres and SQLite tables,
"so state is queryable with SQL and a request reads only what it touches". Their choices:

- **History and current rows.** Each durable relation is a history table (each row with the ticks it was present
  from and until), and a SQL view of its current rows under the relation's own name: `SELECT * FROM log` just works,
  and Blossom's as-of reads keep working.
- **Shared tables.** One table per relation for every object of the deployment, with `node` and `member` columns:
  `SELECT * FROM log WHERE member = 'lunch'`, or across every room at once.
- **Native types where exact**, JSON for structured values.
- **Stateless hosts first.** `blossom serve` on Postgres or SQLite keeps its objects' durable relations in tables;
  `blossom run` keeps its LSM; the S3 store keeps its blobs.

## 1. Where the tables go in

A node's database (DATABASE.md, `blossom_node::database::Database`) is a **versioned set of keys**: every durable
row is a key (the relation's tag and the row's order-preserving encoding, `DurableCodec::row_key`), present from the
tick that inserted it until the tick that deleted it; derived keyspaces (indexes, durable views, blobs, definitions)
are keys too. The database does everything else (codecs, indexes, views, probes, images) over that set, through a
dozen operations of its LSM.

So the tables go in under the database, not instead of it: `blossom_store::KeyTree` is those operations, the LSM is
one implementation, and **`SqlTree`** (blossom-runtime) is another, over the state store's tables. The database, the
engine, recovery and the drivers are unchanged, and so is everything that tests them against models.

## 2. The tables

For each durable relation (its tag `T`, its name `R`):

```sql
create table r_R_T (                -- the history table
    node     text not null,         -- the deployment node (a keyed member's host)
    member   text not null,         -- the keyed member's key; '' for a node's own rows
    key      bytea not null,        -- the row's encoding (DurableCodec::row_key without the tag): identity and order
    from_tick bigint not null,      -- present from this tick ...
    to_tick   bigint,               -- ... until this one (null: still present)
    c1 ..., c2 ..., …               -- the row's columns, typed (§3), for SQL readers
    primary key (node, member, key, from_tick)
);
create index on r_R_T (node, member, key) where to_tick is null;
create view R as select node, member, c1, c2, … from r_R_T where to_tick is null;
```

A client role's durable tables live in its pages, never at a node: they have no table. A typed column named like a
system column (`node`, `member`, `key`, `from_tick`, `to_tick`) is renamed with a trailing `_` (`key_`); a name
that is not an identifier has its other characters as `_` (`Server.votes` is `Server_votes`). A **durable view** (DATABASE.md §8: a view
the engine keeps in the database) has a table too, `v_NAME_D` (`D` from its definition's hash), and its current-rows
view: the database tells the tree its keyspace when it opens it (`KeyTree::view_keyspace`). The view's support counts
stay internal. A view whose definition changes gets a new table, as a relation whose schema changes does; a view the
database starts again in a new generation (a program that added a durable view) keeps its table, and the object's rows
of the older generation are closed in the same commit, so the table shows the current generation's rows only.

`T` (16 hex digits of the relation's tag) changes with the relation's schema, as its keyspace does in the LSM: a
relation whose schema changed starts as a new, empty table (DATABASE.md), and the view `R` follows the deployed
program. The derived keyspaces, which only the database reads, share one table:

```sql
create table blossom_keys (node text, member text, key bytea, from_tick bigint, to_tick bigint,
                           primary key (node, member, key, from_tick));
```

A deployment's tables live in the store's schema (Postgres) or file (SQLite), with the object tables of STATELESS.md.
A `blossom_tables` catalog records each table's relation, tag and columns, and the deployment they belong to: a store
serves one deployment, and a second one is refused.

## 3. Types

| Blossom | Postgres | SQLite |
|---|---|---|
| `bool` | `boolean` | `integer` 0/1 |
| `i8`…`i64`, `Duration`, `Instant` (nanoseconds) | `bigint` | `integer` |
| `u8`…`u32` | `bigint` | `integer` |
| `u64`, `u128`, `i128`, `Mod<N>` | `numeric` | `text` (decimal) |
| `f64` | `double precision` | `real` |
| `String`, `Principal` | `text` | `text` |
| `Bytes` | `bytea` | `blob` |
| `Node<R>` | `text`, as Blossom writes it: `rooms`, `Browser#3@rooms`, `Room:"lunch"` | `text` |
| `Blob` | `text` (`HASH:LEN`) | `text` |
| tuples, structs, enums, `Option`, collections, `Session` | `jsonb` | `text` (JSON) |

The JSON of a structured value is deterministic (fields in declaration order, enums as `{"variant": N, "fields":
[…]}`). The typed columns are for SQL readers: the database itself reads only `key`, so identity, order and equality
are Blossom's, never SQL's collation or JSON's.

## 4. Reads and writes

**Reads** are the tree's: a scan of a key range as of a tick is

```sql
select key from r_R_T where node = $1 and member = $2 and key >= $lo and key < $hi
  and from_tick <= $at and (to_tick is null or to_tick > $at) order by key
```

and a membership test the same for one key. A scan that crosses relations (none of the database's do) is refused.

**Writes.** Within a request, the ticks the node releases are applied to the tree's **pending** changes, in memory
(reads merge them, so the node reads its own request's ticks); nothing goes to SQL until the request commits. The
commit (STATELESS.md §5) then carries, in one transaction with the object's entries and the version check, each
pending key's change as a row: an insert opens a row (`from_tick = t`), a delete closes the open one (`to_tick = t`).
A commit that loses changes nothing, so no other request ever sees a row of a tick that did not commit.

The WAL is not removed: on a SQL-backed object the database asks for a flush after every tick (its rows are as
durable as the request's commit), and the WAL truncates behind it within the request. An object's entries then hold
only its meta record, its links, sessions, hibernation and outbox: a request loads those, and reads rows only as its
probes need them.

**History** goes back as far as `storage.history_ticks`: the tree's floor, raised as the database raises it; rows
closed below the floor are deleted by the store's housekeeping (`StateStore::maintain`).

## 5. The store's part

`blossom_statestore::TableStore`, which the Postgres and SQLite stores implement (and `MemStore`, for deterministic
tests):

- `ensure_tables(deployment, tables)`: creates the history tables, the internal table, the views and the catalog,
  under an advisory lock (instances start together);
- `scan(table, node, member, lo, hi, at)` and `contains(…)`: the reads of §4;
- `commit_rows(object, expected, writes, rows)`: `StateStore::commit` with row changes in the same transaction.

It works in terms of tables, keys, ticks and typed SQL values (`SqlValue`), knowing nothing of Blossom's values.

## 6. Tests

- The tree against a model: `KeyTree` conformance (versioned puts and deletes, scans and gets as of every version,
  a floor), run on the LSM, on an in-memory tree, and on `SqlTree` over each `TableStore`.
- The database's own tests on a `SqlTree` over `MemStore`.
- The engine checked against the oracle (`BLOSSOM_EVALUATOR=checked`) on SQL-backed objects.
- The keyed chat of STATELESS.md on SQLite and Postgres tables, and `select * from log where member = 'lunch'`
  showing its lines.

## 7. Out of scope

`blossom run` on SQL tables; S3 tables (its objects keep their blobs); blobs as SQL rows (they stay files in the
object's entries); changing a typed column's SQL type when a program's type changes (a changed schema is a new
table).
