# Foreign protocols in Blossom: byte streams, functions, bytes, the standard library, blobs

This document covers the generic language and runtime additions that let a Blossom program speak an existing wire
protocol itself: Kafka first, and HTTP, Redis or anything else later. None of it is protocol-specific. It follows
decisions K2 and K3 in `docs/design/KAFKA.md`:
- no protocol-specific host code;
- the only platform additions are a generic byte-stream primitive and pure `extern fn` standard-library functions.

When a section here settles, it moves into `docs/design/LANGUAGE.md` (surface) and `docs/design/ARCHITECTURE.md`
(runtime), with feature ids. Until then this file is the design of record for S6 and S7.

## 1. Byte streams (new; feature ids are assigned when this moves into LANGUAGE.md)

A `stream` is a TCP endpoint the program owns at the byte level. The runtime moves bytes; the program does all the
framing and parsing.

### 1.1 Surface

```blossom
stream kafka: listen;            // accepts connections; its address comes from deploy.toml
stream upstream: connect;        // opens outbound connections on request
```

A stream declaration introduces:

| Relation | Kind | Meaning |
|---|---|---|
| `kafka.opened(c: Conn, peer: String, at: Instant)` | event | a connection was accepted (for `connect`: established) |
| `kafka.data(c: Conn, seq: u64, bytes: Bytes)` | event | the next chunk of bytes read from `c`; `seq` counts chunks from 0 per connection |
| `kafka.closed(c: Conn, reason: String)` | event | the peer closed, a read or write failed, or the program closed `c` |
| `send kafka.write(c: Conn, seq: u64, parts: Vec<Part>)` | channel to the host | bytes to send on `c`; the runtime writes in `seq` order per connection |
| `send kafka.close(c: Conn)` | channel to the host | close `c` after the writes already sent |
| `send upstream.dial(req: u64, addr: String)` | channel to the host | `connect` streams only: open a connection; it is reported as `upstream.opened` or `upstream.failed(req, reason)` |

- `Conn` is a built-in opaque type, like `Session`. It is unique across incarnations: it carries the incarnation, so a
  restarted node never confuses an old connection with a new one.
- `Part` is `enum Part { Bytes(Bytes), Blob(Blob, u64, u64) }`: literal bytes, or a range of a stored blob (§5). A
  blob part is sent without passing its bytes through the engine.

### 1.2 Semantics

- **Chunks are arbitrary.** A chunk is whatever one read returned; it has no message boundaries. Programs reassemble
  bytes themselves, for example with a volatile table holding each connection's unconsumed bytes.
- **Order within a tick.** Chunks are delivered as events at the tick after they are read, in `seq` order. Several
  chunks of one connection can arrive in the same tick; their order is `seq`, not the canonical row order.
- **Write order.** Writes are ordered per connection by `seq`, which the program assigns: strictly increasing and
  contiguous per connection, starting at 0.
  - The runtime writes them in order across ticks.
  - A gap holds back later writes until the missing one is sent.
  - A duplicate or out-of-range `seq` closes the connection with a located runtime error, never silently.
- **Durability, as for egress (Invariant B).** A tick's writes are released only after that tick's durable writes
  are synced. So a protocol acknowledgement written in a tick that made data durable is never sent before the data is
  durable.
- **Crashes.** Every connection closes at a crash; clients see a reset. Connection state lives in volatile relations,
  so a restart begins with no connections.
- **Backpressure.** Each connection has a byte budget per tick (`max_stream_bytes`, from the deployment).
  - Beyond it the runtime stops reading that connection until the next tick.
  - A connection whose unsent writes exceed a limit is closed, with a counter.
- **Deployment.** A node's `[[node]]` entry maps stream names to addresses: `streams = { kafka = "0.0.0.0:9092" }`.
  A `listen` stream without an address is a configuration error: the node refuses to start.

### 1.2a As built (S6 item 4)

- **One chunk per connection per tick.** The runtime delivers at most one `data` event per connection per tick:
  everything read from it since the previous tick, up to the budget. This decision was taken because `fold!`
  aggregates do not exist yet, and so that a program needs no in-tick reassembly. `seq` still counts chunks, so a
  program can check it.
- **Event order across ticks.**
  - A connection's `opened` is in an earlier tick than any of its `data`.
  - Its `closed` is in a later tick than its last `data`.
  - So state a program emits in `opened` is current when the data arrives, and state it updates with `upsert` (at
    t+1) is current for the next chunk.
- **A connect stream's `opened`** carries the dial request it answers: `opened(c: Conn, req: u64, peer: String,
  at: Instant)`. `failed(req, reason)` reports a dial that did not connect.
- **`Part`** is a built-in enum with one variant for now, `Part::Bytes(b)`. `Part::Blob(…)` comes with blobs (§5).
  Adding a variant does not break programs that build `Part::Bytes`.
- **Surface.**
  - A stream's relations are named like an instance's interface: `s.opened`, `s.data`, `s.closed`, `s.failed` are
    read.
  - `s.write`, `s.close`, `s.dial` are written with `send` and no `to`. Reading them is BLS0203; writing an event is
    BLS0400.
- **IR.**
  - `Program.streams` holds each `StreamDecl`, naming its relations.
  - The events are `RelClass::Event(EventSource::Stream(e))`.
  - The requests are `RelClass::HostOut(op)`, written only by async rules and never read.
  - Each tick's requests leave as `TickOutput::host` and, in the node, are released with the tick's other output
    after its durable writes are synced.

### 1.3 IR, oracle and engine

- **IR.** Streams lower to event relations of a new source (`EventSource::Stream { stream, part }`) and to async heads
  addressed to the host, like services (LANGUAGE §16.4).
- **Evaluators.** Neither the oracle nor the engine knows about sockets. They see events and produce writes, as for
  sessions.

### 1.4 Simulation

- **Cluster simulator.** Streams become simulated byte pipes between programs, or between a program and a Rust test
  client.
  - Chunks are split at random byte boundaries, which tests reassembly.
  - The nemesis can drop connections.
  - Everything is deterministic under the seed.
- **Synchronous simulator (`blossom sim`).** It takes scripted chunks as inputs.

## 2. Functions (LANG-180, specified in LANGUAGE §16.1, not yet implemented)

Protocol code is written as pure functions. LANGUAGE §16.1 defines them:
- total, with no recursion;
- `let`s and a final expression;
- closures only as arguments to the built-in combinators (`map`, `filter`, `fold`, …).

S6 implements them in the frontend (resolve, typecheck, lower to IR pure functions), the oracle and the engine. The
oracle and the engine evaluate function bodies independently, as they do everything else.

### 2.1 Additions this work needs

| Addition | Why |
|---|---|
| **Ranges.** `range(lo: u64, hi: u64)` is iterable by the combinators without building a vector: `range(0, n).fold(init, \|acc, i\| …)` | Decoding a length-prefixed array is a fold over `range(0, n)`, carrying the decoded prefix and a cursor. Totality holds: `n` is a value and the fold is bounded |
| **Recoverable failure.** `Option` and a built-in `Result<T, String>`, with `?`-free combinators (`and_then`, `map`, `unwrap_or`) | A decoder must never call `error(…)` on client input: that aborts the tick (BLSR010) and crashes the node. Decoders return `Option`/`Result`; malformed input becomes a protocol error or a closed connection |
| **Structs and enums in functions** | Typed requests and responses (already in the type system) |
| **Memoization.** A function call is memoized per input per tick, as `extern fn`s are (§16.2) | Decoding a frame referenced by several rules runs once |

### 2.2 Style for protocol code

- **A cursor.** A decoding cursor is a value: `struct Cur { buf: Bytes, pos: u64 }`. Every primitive read returns
  `Option<(T, Cur)>`.
- **Composition.** Composite decoders chain these in `let`s. Arrays fold over `range(0, n)`.
- **Encoders.** They build `Bytes` by concatenation. The frame size is computed from the finished body, never guessed.
- **Where the functions live.** One module per API version (a `mod` per file). Shared primitives go in a protocol
  library written in Blossom, not in the standard library.

## 3. Byte primitives (Appendix B additions)

These are pure built-ins, implemented independently in the oracle and the engine, with differential tests between
them.

| Built-in | Result |
|---|---|
| `b.u8_at(pos)`, `i8_at`, `u16_be_at`, `i16_be_at`, `u32_be_at`, `i32_be_at`, `u64_be_at`, `i64_be_at` | `Option<T>`: `None` past the end |
| `b.uvarint_at(pos)`, `b.varint_at(pos)` (zigzag) | `Option<(value, next_pos)>`: `None` on truncation or overlong encoding |
| `Bytes::from_u8(x)`, `from_u16_be`, `from_i16_be`, `from_u32_be`, `from_i32_be`, `from_u64_be`, `from_i64_be` | `Bytes` |
| `Bytes::uvarint(x)`, `Bytes::varint(x)` | `Bytes` |
| `Bytes::empty()`, `b.concat(c)` (exists), `b.slice(lo, hi)` (exists), `Bytes::join(v: Vec<Bytes>)` | `Bytes` |
| `b.put_u32_be(pos, x)` and friends | `Option<Bytes>`: a copy with the bytes at `pos` replaced (used to patch a Kafka batch header) |
| `s.to_utf8()`, `b.from_utf8()` (exists) | `Bytes`, `Option<String>` |

Integer reads are exact: a value that doesn't fit its type is an error, never a silent wrap.

As built (S6 item 2; LANGUAGE Appendix B has the list):
- The 8-bit forms have no `_be`: `u8_at`, `i8_at`, `put_u8`, `put_i8`, `from_u8`, `from_i8`.
- Varint reads return the value and the position after it.
- A non-minimal varint (`80 00`) decodes. `None` means truncated, longer than 10 bytes, or not fitting a `u64`.
  Protocol code range-checks Kafka's 32-bit varints itself.
- Both evaluators implement every primitive independently (`blossom-oracle/src/library.rs`,
  `blossom-engine/src/func.rs`).

## 4. The `extern fn` standard library (LANGUAGE §16.2)

`extern fn` binds a declaration to a registered Rust function (in `blossom-std-host`).
- It is declared pure and must be deterministic.
- It is memoized per input per tick.
- Both evaluators call the same function: it is a leaf computation, not evaluation logic.

The standard library holds generic functions only. Nothing protocol-specific goes here (decision K3).

| Function | Notes |
|---|---|
| `std::checksum::crc32c(b: Bytes) -> u32` | Castagnoli, as Kafka's record batches and many other formats use |
| `std::checksum::crc32(b: Bytes) -> u32` | IEEE |
| `std::compress::gzip_decompress(b) -> Option<Bytes>`, `gzip_compress(b, level: u8) -> Bytes` | and the same for `snappy`, `lz4` (the frame format), `zstd` |
| `std::hash::sha256`, `std::hash::blake3` | as in LANGUAGE §16.2 |

As built (S6 item 3):
- **Paths and signatures.** Paths are `blossom_std::…`, as in LANGUAGE §16.2. The catalog, with exact signatures,
  is `blossom_value::STD_EXTERNS`:
  - `crc32c`, `crc32`;
  - `gzip_compress(b, level: u8)`, then `snappy_compress`, `lz4_compress` and `zstd_compress` of `b`;
  - `*_decompress(b, max: u64) -> Option<Bytes>` for each codec;
  - `sha256`, `blake3`.
- **Formats.**
  - Snappy is the raw block format. Kafka's xerial framing is protocol code, written in Blossom on top of it.
  - LZ4 is the frame format.
  - gzip and zstd decode concatenated members and frames.
- **Implementations are pure Rust** (flate2 with miniz_oxide, snap, lz4_flex, ruzstd, crc32c, crc32fast, sha2,
  blake3). Compression is deterministic.
- **Where each check happens.**
  - *Compile time:* the frontend checks each `extern fn` against the catalog (BLS0216).
  - *Load time:* the oracle and the engine bind every declared extern against the registry they are given, with its
    signature (`EvalError::Externs`). `blossom run`, `blossom sim`, `blossom ldfi` and the corpus runner give them
    `blossom_std_host::registry()`, which a test holds equal to the catalog.
- **A host failure** (for example a gzip level above 9) aborts the tick with BLSR010.
- **Memoization per input per tick** is not built yet. It is only an optimization, because the functions are pure.

Two rules for these functions:
- **Bounded output.** Decompression takes a maximum output size, and returns `None` past it or on malformed input. It
  never panics or aborts.
- **Build-time checks.** The library is enumerated at build time. An `extern fn` that names an unregistered function
  is a compile error, not a runtime surprise.

## 5. Blobs (LANGUAGE §16.6, LANG-028)

Large byte payloads live outside the engine. A `Blob` is an immutable byte object identified by its content: a BLAKE3
hash plus a length (`Value::Blob` already exists).

- **Creating.** `std::blob::of(b: Bytes) -> Blob` is pure: the handle is a function of the bytes. The runtime stores
  the bytes, and a tick that references a new blob in a durable row makes it durable before that tick's WAL record
  syncs. So recovery never finds a row whose blob is missing.
- **Reading.**
  - `blob.len()`.
  - `std::blob::read(b: Blob, lo, hi) -> Bytes`, for the rare case the program needs the bytes (validation, patching
    a header).
  - A stream write sends a blob range directly (`Part::Blob`).
- **Collection.** Blobs referenced by no durable row are deleted after a checkpoint.
- **Simulation.** The blob store sits on `SimFs`, crash fates included.

For Kafka, a partition log row is `(offset, count, max_timestamp, producer…, batch: Blob)`. Fetch responses are
composed of encoded headers (`Part::Bytes`) and batch ranges (`Part::Blob`).

## 6. Storage that follows change (store work, S7)

Two changes to the store.

- **Incremental checkpoints.** A checkpoint writes the delta since the previous one, chained, with background
  compaction. Today every checkpoint encodes the whole durable image. A broker's durable metadata (offsets, handles,
  producer state) grows with retained data, so full checkpoints would grow with it, and today they stall the engine
  thread.
- **Deletion at scale.** Retention deletes many rows at once. Deletions must cost in proportion to the rows deleted,
  including the index maintenance in the engine's stores.

## 7. Crate by crate

| Crate | Work |
|---|---|
| `blossom-syntax` | `stream` items; function bodies (closures in combinators, ranges) where the grammar lacks them |
| `blossom-front` | resolve and typecheck functions, `Conn`, `Part`, `Result`, the byte built-ins, `extern fn`; lower streams to event sources and host channels |
| `blossom-ir` | stream event sources; pure functions (exist in the IR); new `BuiltinFn`s; validator rules |
| `blossom-oracle`, `blossom-engine` | evaluate functions, ranges and byte built-ins independently; call `extern fn`s through a registry |
| `blossom-node` | stream events and writes as inputs and outputs; write ordering; stream writes released with the tick |
| `blossom-runtime` | listeners and dialers, a reader per connection, a writer honouring `seq` order and release after sync, budgets and counters |
| `blossom-sim` | simulated byte pipes, random chunking, connection drops; stream-aware cluster simulator clients |
| `blossom-store` | blob store; incremental checkpoints (S7) |
| `blossom-std-host` | the `extern fn` registry and the standard library in §4 |

Every addition gets corpus cases (functions, bytes and streams) run on both evaluators, and every durability claim
(stream writes after sync, blob durability) gets a mutation-checked test.
