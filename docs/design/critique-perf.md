# Performance critique of `ARCHITECTURE.md` (first draft, 2026-09-27)

Reviewer lens: **performance**. Can this engine become the fastest Datalog implementation for *many small incremental
ticks + networking + lattices*? The review covers value encoding, storage, index selection, joins, allocation per tick,
cache behaviour, interpreter dispatch, codegen, parallelism and the benchmark plan. It compares the draft with Soufflé,
DD/DDlog, DBSP, Ascent, Hydro/DFIR, FlowLog and Free Join.

Section numbers (§x.y) refer to `ARCHITECTURE.md` unless another document is named. R09 is
`docs/research/09-fast-datalog-engines.md` and R08 is `docs/research/08-hydro.md`.

## Summary

The foundations are right:

- `u64` word columns with order-preserving scalar encodings;
- Δ, Old, All and TickNew as epoch row ranges, never copied;
- per-rule maintenance regimes (Standing, Transient, Counted, Recompute);
- dirty-stratum scheduling;
- native constructs;
- Δ-driven index nested loops as the default join;
- Soufflé's k-version scheme;
- chain-cover index selection;
- one kernel library shared by the interpreter and codegen;
- provenance that compiles out when it is off;
- pipelined group commit;
- merge at the sender.

No existing engine combines all of these, so the draft can win this workload.

The gaps are in five places, and each costs more to fix after the kernels exist than now:

1. **Per-message constant factors.** The IR materializes a relation for every handler header, `if`/`for` block, view
   alternative and upsert scratch. Each materialized relation gets a row store and a deduplicating primary index. The
   network path also goes through `Value` and owned buffers. DFIR does one hash update per message where this design
   does four or five.
2. **Tail latency.** Several O(state) operations run synchronously inside a tick: hash-table growth, whole-segment
   compaction, eager geometric run merges, interner mark-compact, and `clear()` on large tables. The targets are
   stated at p50 only.
3. **Update-heavy state.** A dead row stays in the hash-index chains until the whole segment is compacted, and every
   upsert appends a new row. Hot keys, the common case in KVS and Raft, make probes degrade linearly.
4. **Long-running memory.** Every string and byte value, transient ones included, is hash-consed into an interner that
   has no reclamation at P0. The P0 flagships (the KVS values, the Raft log's `cmd: Bytes`) will reach the hard cap
   and abort.
5. **Forks and snapshots.** These are paid for by the running engine: an `Arc::make_mut` on every append, and O(state)
   copies of the tail chunks and hash tables after every snapshot. LDFI takes a snapshot every tick.

The benchmark plan also cannot establish "fastest" for this workload. The strongest competitor (DFIR) and the maintained
incremental engine (DBSP) are not live baselines, there is no lattice suite and no tail-latency suite, and one P0 target
from FEATURES.md was weakened without being flagged.

### Where the draft stands against each engine

| Engine | What makes it fast | Draft | Gap → item |
|---|---|---|---|
| Soufflé | 32-bit domain; specialized B-trees with operation hints; chain cover; pipelined index nested loops; RAM transforms; outer-loop parallelism | chain cover yes; pipelining yes; RAM transforms P1; **u64 only**; parallelism P1 | S3, S2, S8 |
| DD / DDlog | shared arrangements; **amortized ("fueled") spine merges**; frontier compaction; full incrementality including recursion with deletion | delta queries yes; epochs yes; frontier yes; **eager merges**; recursive deletion falls back to Recompute (FBF is P1) | MF-2 |
| DBSP (Feldera) | Z-set algebra; incremental distinct; batch-at-a-time operators over one integrated state | Counted regime matches it | S11 |
| Ascent | codegen by macro; hash indexes; in-place lattice `join_mut`; BYODS | matches | — |
| Hydro / DFIR | compiled push/pull pipelines with **nothing materialized**; no dedup unless asked; arena buffers; inline DAG codegen (+35–50% on Paxos, R08 §4.6); one thread per process | **headers materialized and deduplicated**; ENG-006 not placed at all | MF-1, MF-5, S5, C10 |
| FlowLog | structural JST planning; SIP; subplan sharing; Boolean-specialized diffs; data-parallel dedup | JST P0; SIP and sharing P1 | S8, C2 |
| Free Join | GHT/COLT lazy tries; vectorized batch probes | single-level lazy hash only; vectorized probing P1 | S1, C2 |

---

## Must-fix

Each item states the problem with evidence, why it matters, and the proposed change.

### MF-1. Generated tick-local relations are materialized and deduplicated for every message

**Problem.**

- LANGUAGE §8.1 lowers every handler header to a materialized scratch relation `H$when`, and every nested `if`/`for`
  to another one. Upsert lowers to a keyed scratch `r$ups` plus `r$del` (LANGUAGE §8.2).
- §3.8 gives *every* relation a primary dedup index. §3.6 lists `HandlerHeader`, `Block` and `ViewAlternatives` as
  "provenance-only groupings (never replaced)".
- §3.7 step 8 fuses only *within* a rule (ENG-083). The codegen example in §10.2 says the rule is "fused with its
  header", but no planner step does that fusion.
- ENG-006 (in-out trees, arena buffers, static topological schedule) is not mentioned anywhere in the document.

**Cost.** Take one `put` in `examples/e01_kvs.bls`:

1. insert into the `put` receive side (hash dedup);
2. the Choose native;
3. insert into `apply_put$when` (hash dedup);
4. the `store$ups` staging insert;
5. insert into `ack_put$when` (hash dedup);
6. the outbox insert, deduplicated by (dest, channel, tuple).

That is roughly four hash inserts, three row appends and three rescans beyond the essential work (the choice group,
the store upsert and the send). At 20–40 ns per in-cache hash insert, this overhead alone is 100–150 ns per message,
about the whole per-message budget of a 50k–100k cmd/s leader. DFIR does one hash update and one send.

**Change.** Add a normative planner pass `fuse_tick_local` that runs before regimes are assigned.

1. **Inline** a tick-local IDB relation R into each consumer when all of these hold:
   - R is non-recursive;
   - R is not an interface, output, durable, subscribed or `#[materialize]` relation;
   - R is not read by an operator that needs R complete or R's multiplicity (a native, an aggregate, a `Neg` inside
     R's own stratum).

   LANGUAGE §8.1 already states that inlining `H$when` is observationally identical and that "engines may do so". A
   choice in the header stays in its own generated relations, as LANGUAGE requires.
2. **Materialize as a buffer, not a relation**, when R has k > 1 consumers and its body is more than a probe or two.
   The buffer is an arena `Vec` of rows with no primary index. It feeds its consumers through a new `Op::Tee`, which
   is the push fan-out of DFIR's in-out trees.
3. **Elide deduplication when every consumer is idempotent.** A relation needs a dedup index only if some consumer is
   multiplicity-sensitive: `count`, `sum` or `avg` without set semantics, a Z-set head, `seq!`, `index!`,
   `fold_ordered`, or a Tier C firing log in the backward slice. Inserts into deduplicating relations, staging, the
   outbox, lattice merges, `Exists`, min/max/bool aggregates and priority choice are all idempotent.
   - Channel receive sides qualify: CR-02's per-tick set semantics is preserved because every observable sink
     deduplicates.
   - This is FlowLog's Boolean specialization (R09 §5.4) applied to tick-local data.
4. **Place ENG-006.** An acyclic, tick-local region of the plan executes as one pipeline per tick in topological
   order: pull fan-in, then a pivot, then push fan-out, over arena buffers that are reset at tick start.
5. **Keep the checks.** The oracle still evaluates the expansion. `PlanLimits::fuse = false` is added to plan
   perturbation (§11.3). Tier C still sees the header bindings, because inlining keeps every variable.

**Target.** A put in e01 costs at most two hash operations plus the choice.

### MF-2. O(state) maintenance runs inside ticks; there is no tail-latency discipline

**Problem.** Six operations can stall a single tick for time proportional to state, not to the tick's work:

| Operation | Where | Cost when it fires |
|---|---|---|
| hashbrown growth (primary, hash indexes, group tables, interner) | §4.1, §4.3, §4.6 | rehashes the whole table: tens of ms at 10^7 entries |
| segment compaction "when dead rows exceed live rows" | §4.2 | rebuilds the segment and re-points every index |
| sorted runs "merge while last.len() <= 2·new.len()" | §4.3 | datafrog's *eager* merge: at every power of two the whole run is re-merged in one epoch |
| interner mark-compact (P1) | §4.1 | marks from every stored column |
| hashbrown `clear()` on a transient table | §4.2 "truncate keeps capacity" | writes every control byte, so one burst makes every later tick pay O(peak capacity) |
| first write after a snapshot | §4.2 | copies flat hash tables (see MF-6) |

Taken together, these contradict "latency stays flat as state grows" (§4.12), which is stated at p50 only. Protocols
live at p99: a 100 ms stall exceeds Raft's 150–300 ms election window with the heartbeat's margin.

**Change.** Add a new decision to §0.1:

> **ARCH-21 (tail-latency discipline).** No operation inside a tick may cost more than O(tick work + fuel), where fuel
> is a per-tick budget proportional to the tick's work. Everything larger is incremental.

The mechanisms:

- **Incremental rehash** for every long-lived hash structure. On growth, allocate the new table and migrate a bounded
  number of buckets per insert, plus a fuel budget at the end of each tick. Probes consult both tables during the
  migration. This follows Redis's incremental rehash, and `griddle` 0.6 is prior art over hashbrown. Build it on
  `hashbrown::HashTable` so the raw API stays available.
- **Fueled LSM merges, after DD's spine** (Shared Arrangements, VLDB'20 §4). Each epoch end performs merge work
  proportional to the new batch on the merges in progress. Readers see an unfinished merge as its input runs.
- **Chunk-granular incremental compaction.** Keep a dead counter per chunk. When a chunk is more than half dead and
  all its deaths are older than the frontier, rewrite that chunk only, and re-point its rows through the posting lists
  of MF-3 / S2. Budget the work per tick.
- **Interner reclamation without a global mark** (MF-4).
- **Capacity policy for tick-local tables.** Shrink a table when its peak over the last N ticks is below one eighth
  of its capacity.
- **Optional capacity hints.** Add `#[capacity(n)]` or a deployment key that pre-sizes relations, so known-large
  state never grows in production.

Add p99, p999 and maximum tick latency to §4.12, and a 24-hour churn soak to §4.13 (targets in MF-7).

### MF-3. Deaths stay in index chains, and upsert means death plus append; hot keys degrade linearly

**Problem.**

- §4.2: "Re-inserting a dead tuple appends a new row and repoints the primary index". Compaction starts only when dead
  rows exceed live rows *across the segment*.
- §4.3: a hash index is intrusive chains (`heads` + `next: Vec<RowRef>`).
- §4.4: `probe<D: Deaths>` skips dead rows while it walks.
- Nothing removes a dead row from a secondary chain before the whole segment is compacted.

**Cost.** Take a store of 10^6 keys where one hot key is updated 10^5 times.

- The hot key's chain holds 10^5 dead entries until 10^6 deaths have accumulated. Every probe of that key walks them
  all, at about two cache misses per step.
- The persistent segment grows by one row per update, doubling memory before compaction fires.

The same pattern appears in Raft (`deadline key()` upserted on every reset, `current_term` resolve, `leader_of`), in
every soft-state table, and in the KVS itself. Update-heavy keyed state is *the* protocol workload.

**Change.**

1. **Separate current-state indexes from history.** The indexes that rule evaluation reads (sources `All`, `Old`,
   `TickNew`) contain only rows alive at the current epoch.
   - A death applied at the tick boundary (`apply_staged`) unlinks the row from every secondary index and from the
     primary, in O(1) expected time. This needs per-key posting lists with a position back-pointer and swap-remove
     (S2) instead of intrusive chains.
   - History (ENG-029 as-of reads, Tier B, LDFI) is served from the row store plus birth and death stamps. Lazily
     built *history indexes* (COLT) exist only while an as-of reader exists, which is the only time dead rows matter.
2. **Update keyed relations in place under `HistoryPolicy::CurrentOnly`.** An upsert that replaces the payload of an
   existing key overwrites the payload words in place and appends `(epoch, row, old payload)` to the relation's change
   log. This is the mechanism lattice cells already use (§3.3).
   - Transient consumers read the current state.
   - Counted consumers get ZDelta {−old, +new} from the change log.
   - Rows do not grow and chains do not grow.
   - When history is required (LDFI, as-of readers), fall back to death plus append.
   - As a consequence, "birth is implicit from position" becomes "birth from position, updates from the change log".
     Snapshots must version these overwrites (MF-6).

**Target.** A KVS with 10^6 keys under Zipf 0.99 and 100% updates keeps flat p50 and p99 latency, and flat memory,
over a 24-hour run.

### MF-4. The interner grows without bound at P0, and interns every transient payload

**Problem.**

- §4.1 interns `String`, `Bytes`, records and collections. Reclamation is P1. Until then there is a hard cap that
  aborts the node.
- e01 has `key: String, val: Bytes`, and e03 has `log(idx, term, cmd: Bytes)`.
- Every string or bytes field of every message is hashed (xxh3), probed and copied into the arena at ingest, even when
  a filter drops the message on the next line.

**Cost.** At 10k puts/s with 100-byte values, the interner grows by about 3.6 GB per hour, so a P0 flagship aborts
within hours. Hash-consing buys O(1) equality, but only for values that are *compared*. Payload-only columns (never a
join key, a probe key or an operand of `==`) gain nothing from it.

**Change.** Add ARCH-22 (value representation):

1. **Choose the representation per column** in `ColEnc`, at plan time.
   - `Interned` (hash-consed) for columns that take part in equality: join and probe keys, and key columns of keyed
     relations.
   - `Blob` for payload-only columns. The word is a handle to an arena value with a cached fingerprint. Equality is
     fingerprint equality followed by a byte compare, and the hash is the fingerprint.
   - Dedup of a set relation that contains blob columns uses fingerprint equality plus a byte compare on a match,
     which is O(1) expected.
2. **Tick arena.** Values created by ingest or by expressions go into a bump arena that is reset at tick start. A
   value is promoted, copied once, when it is inserted into a long-lived segment, staging, the lattice heap or the
   outbox. Promotion may change a blob's handle, because blobs have no identity.
3. **Refcounted hash-consing (P0) for `Interned`.**
   - Each entry counts its *long-lived* references: rows in the persistent, standing, carried and weighted segments,
     staging, lattice objects, the outbox, and parent records.
   - The count goes up on a long-lived insert and down on physical removal: compaction, overwrite, or freeing a
     lattice object.
   - Transient rows hold uncounted references.
   - Entries created during a tick, and entries whose count fell to 0 during the tick, are freed at tick end if their
     count is still 0.
   - Freed ids go on a LIFO free list, which is deterministic under replay. Reuse is deferred by epoch while a snapshot
     that could reference the id is alive (MF-6).
   - No global mark is needed and no id ever moves, so word-ordered sorted indexes stay valid.
   - The cost is O(1) per interned column per long-lived insert, the same order as index maintenance.
4. Delete the P1 sliding mark-compact from §4.1. The hard cap stays, as a safety net rather than the plan.

**Target.** e01 and e03 run for 24 hours at 10k ops/s with `interner_bytes` bounded by live state.

### MF-5. The tick and network APIs allocate by construction, and data goes through `Value`

**Problem.** Owned per-tick outputs appear at every layer:

- the engine: `Engine::run_tick -> TickOutput`, which holds `Vec<Violation>`, `Outbox`, `SubscriptionDeltas`,
  `ChoiceLog`, `DurableDelta` and `TimerChanges` (§4.7);
- the node: `Node::run_tick -> TickEffects { wal: Option<WalRecord>, trace: TickTrace }`, and
  `Released { frames: Vec<(NodeId, OutFrame)>, …, stdout: Vec<String> }` (§5.1);
- transport and ingress: `Transport::send(frames: Vec<OutFrame>)` (§5.3), and `NodeEvent::Deliver(AdmittedBatch)`
  per frame;
- the value path: `ValueStore::intern(&mut self, ty, v: &Value)` is the only way in besides `intern_record`, so
  ingest decodes to `Value` first.

The zero-allocation test (§11.7) runs the *engine* only, so it can pass while every production tick allocates in the
node, the wire codec and the runtime.

**Change.**

- **Engine.** `run_tick(&mut self, input) -> Result<TickOutputRef<'_>, TickError>` returns borrowed views into
  engine-owned buffers that are recycled at the next tick. An equivalent form is `run_tick(&mut self, input, out:
  &mut TickOutputBuf)`. The same pattern applies to `Node::run_tick` and to `committed`.
- **Egress.** The node owns reusable per-(dest, channel) encode buffers and encodes in place at tick end, after merge
  at the sender. `Transport::send(&self, to, batch: FrameBatch)` takes a pooled buffer that the writer task returns to
  the pool. A bounded pool whose exhaustion is backpressure (DIST-008).
- **Ingress.** The pure `admit` validates the frame without decoding values. It returns
  `AdmittedBatch { bytes: Bytes, offsets: PooledVec<u32> }`, a refcounted slice of the read buffer.
  `Engine::ingest` decodes *directly into words*:
  - a varint becomes a direct word;
  - `str`/`bytes` go through `intern_bytes(ty, &[u8])` (a probe with the slice, copying only on a miss) or into the
    blob arena (MF-4);
  - records go through a bottom-up `RecordBuilder`.

  `Value` is reserved for the oracle, the REPL, the dynamic host API and dumps. Add `intern_bytes`, `intern_str` and
  `RecordBuilder` to `ValueStore`.
- **Metrics and tracing.** Metric handles are registered once per node, so no label hashing happens per tick.
  Histograms record into a node-local `hdrhistogram` that is flushed on scrape. `tick` and `stratum` spans are
  `TRACE` or `DEBUG` only (§12.2 does not give their level).
- **Test.** Extend the counting-allocator test to the whole node loop: `ManualDriver` + `MemTransport` +
  `MemDurability`, frames in and out, codec included, 1,000 steady-state ticks, zero allocations. Growth of
  long-lived state is measured separately.

### MF-6. Snapshots and forks tax the running engine

**Problem.** The storage design makes the live engine pay for sharing:

- §4.2: the tail chunk is "mutable via `Arc::make_mut` (COW after a fork)". Mutable tails and flat hash tables "are
  copied on first write after a fork. That is O(state) per mutated relation per fork."
- §4.5: the lattice heap uses "Arc chunks: COW on fork". Its `LatObj` values own hash sets, so a chunk copy is a deep
  clone.
- Snapshots are frequent. §8.5 keeps a `WorldSnapshot` every tick for LDFI resume, §6.2's exhaustive scheduler
  backtracks through snapshots, and §5.6 checkpoints are snapshots.

**Cost.**

- `Arc::make_mut` performs a compare-exchange and a release store on *every append*, even when no snapshot exists.
- After each snapshot, the first append to each relation copies its tail chunk: 4096 rows × words × 8 B, which is
  128 KiB at four words.
- The first write to each hash index after a snapshot copies the whole table.
- With a snapshot every tick, every tick is O(Σ touched index sizes). The fork optimization becomes a slowdown at any
  non-toy state size.

**Change.**

1. **Append-only chunks with stable addresses and no COW.** A row is immutable once written. A chunk has fixed
   capacity, the single owner appends past the published length, and a snapshot records `(chunk, len)` and never
   reads past `len`.
   - Implement it as `UnsafeCell<[MaybeUninit<u64>]>` with a single-writer invariant, tested under Miri. This is the
     `boxcar` / append-only-vec pattern.
   - A snapshot is then a copy of the chunk-pointer vector plus the lengths. Nothing is copied later.
2. **Deaths are written in place.** They are already epoch-versioned (`alive(death, at)`, §4.10). Store them as
   `AtomicU64` with `Relaxed` ordering, which compiles to a plain load or store on arm64 and x86, and let each snapshot
   read at its own epoch.
3. **Snapshots never include indexes.** `EngineSnapshot` = rows + deaths + cells + lattice objects + native state +
   interner length. `fork` and `restore` rebuild eager indexes, lazily per relation on first probe. That costs O(state)
   once per restore instead of once per tick. The live engine owns its indexes exclusively, with no refcount checks.
   The P1 HAMT index variant behind `fork_heavy` becomes unnecessary.
4. **Cells and lattice objects.** Inline cell columns use chunk-level COW. They are words only, so a copy is 32 KiB
   per chunk. Object lattices are individually `Arc`-ed and copied at object granularity. Large sets and maps under
   `fork_heavy` use a persistent HAMT (`imbl`).
5. **Payload overwrites** (MF-3) use the same cell-chunk COW.

**Target.** A snapshot costs at most 1 µs + 20 ns per relation. The running engine pays nothing when no snapshot
exists, and nothing after a snapshot beyond cell-chunk COW.

### MF-7. The targets and the benchmark plan cannot establish "fastest", and one P0 target was weakened silently

**Problem.**

- **A silent weakening.** FEATURES BENCH-200 (P0) says "at least as fast as compiled Soufflé at 4 threads, ≤ 2× its
  memory". §4.13 makes the P0 target "within 1.5× of compiled Soufflé `-j1`" and moves the 4-thread parity to P1.
  §0.2 is supposed to list every refinement, and this one is not listed.
- **Targets that are p50 only and physically inconsistent.** §4.12 targets "p50 ≤ 2 µs (generated) for 1–100
  messages per tick at 10^6 state". A 100-message tick with about three key probes per message makes around 300 probes
  into roughly 50 MB of rows and indexes, which is beyond the LLC. At 1–2 DRAM misses of about 100 ns each, that is
  30–60 µs serial. Only memory-level parallelism (S1) reaches single-digit microseconds.
- **The wrong baselines.** Hydro/DFIR, the direct competitor for small ticks, networking and lattices, is P1 and is
  compared only against 2024 published numbers. Yet `dfir_rs` 0.17 is on crates.io and the SIGMOD'24 protocol
  artifacts are Rust. DBSP (`dbsp` 0.356 on crates.io, maintained by Feldera) is missing, and DD alone stands in for
  incremental engines.
- **Missing suites.** There is no lattice suite, no update-heavy suite, no tail-latency suite and no soak test.
- **A noisy gate.** The regression gate is a 5% wall-clock change on nightly CI VMs, and the noise on those VMs is
  already 5–10%.

**Change.**

- List the BENCH-200 refinement in §0.2 with its rationale. Replace §4.12 with the targets below, and §4.13 with the
  benchmark plan below.
- Make the DFIR same-machine comparison **P0**.

---

## Should-fix

### S1. Batched, prefetching probes in both backends (P0)

Use group prefetching or AMAC-style interleaving: Chen et al. ICDE'04, Kocberber et al. VLDB'15, Menon et al.
VLDB'17.

- **Interpreter.** For each `Probe` over a `BindBatch`, hash all keys, prefetch the bucket groups, probe, prefetch the
  rows, then compare.
- **Codegen.** Today it emits row-at-a-time loops (§10.2), which have no memory-level parallelism. For large state the
  interpreter could then beat generated code. Codegen should emit
  `kernel::probe_batch::<W, K>(ix, keys, &mut out)` whenever the driving source can exceed about 8 rows, and keep
  scalar loops for sources known to be singletons (nullary, key-bound).
- **ENG-085.** Move vectorized probing from P1 to P0, and lower its threshold from 4096 to the batch size.

### S2. Index layout: reuse the primary, inline keys, 32-bit refs, posting lists

- **Reuse the primary.** A search whose equality set is exactly the relation key uses the primary index. Today §3.8
  adds a separate `Hash` index for it, which doubles maintenance on the most common protocol probe, lookup by key.
- **Inline keys.** For keys of at most 2 words, store the key words in the entry, `(u32 row, [u64; K])`, so a probe
  touches one cache line instead of a bucket plus a row. For set relations of arity ≤ 2, the primary entry *is* the
  tuple.
- **Smaller entries.** `RowRef` becomes a `u32` scoped to its segment, instead of a `u64` with a 3-bit tag. Drop the
  stored 64-bit hash: hashbrown keeps 7 bits in its control bytes, and the full hash is recomputed from the words on
  resize.
- **Posting lists** instead of intrusive `next` chains. Use `SmallVec<[u32; 2]>`, spilling to a pooled arena. That is
  one miss per key instead of one per row, O(1) removal (MF-3), and sequential row prefetch.
- **Correct the memory arithmetic in §4.12.** The layout as drafted costs about 26 B per row for the primary
  (16-byte entries plus a control byte, at hashbrown's average load), plus about 8 B per row and 26 B per distinct key
  for each hash index. That is not "~12 bytes/row per index". A binary relation with one secondary index comes to
  about 50 B per row, against about 20 B in Soufflé.

### S3. Per-relation 32-bit lanes (ENG-020, silently dropped)

ENG-020 says "packed to u32 where the type allows". ARCH-05 makes every column a `u64` and does not list the change in
§0.2.

- Add `RowShape { WORDS, LANE: U32 | U64 }`. A relation whose columns all fit in 32 bits uses 4-byte lanes. Such
  columns are u32/i32, bool, C-like enums, `NodeId` and interned ids, which are already `u32` (§4.1).
- This covers nearly every BENCH-200 relation. It halves scan, merge and sorted-run bandwidth, and it is the main
  lever for the "≤ 2× Soufflé memory" target, since Soufflé's default domain is 32-bit.

### S4. Lattice representations chosen per type

- **Dense small domains.** `LSet<Node<R>>` and sets over other dense domains of at most 64 values (enums, small `Mod`)
  become an **inline bitmask word**.
  - join is OR, `size` is popcount, `majority(s, R)` is a popcount comparison, and Atomize iterates the bits;
  - clusters of up to 256 nodes use an inline `[u64; k]`.

  Quorum state in Raft, Paxos and 2PC (e03 `votes`) becomes one word, with no heap object and no hashing.
- **Small sets.** Sets of at most 8 elements are an inline sorted small-vec with linear merge, promoted to a hash set
  past the threshold.
- **Cached summaries.** Keep `size` and sums in the object, so thresholds are re-evaluated in O(1).
- **Interpreter fast path.** Inline lattice kinds get dedicated shadow-tree operators with no `LatticeOps` vtable,
  matching codegen (§4.5 promises this for codegen only).
- **Maps.** Map lattices whose values are inline lattices store words, not the 16-byte `LatSlot` enum.

### S5. Engine thread placement

- **Dedicated threads.** Run each node's engine on a dedicated OS thread (thread-per-core in multi-node processes), not
  on a work-stealing tokio task, which can migrate between cores and lose L1/L2 state.
- **I/O on its own runtime.** Keep I/O and TLS on the multi-thread runtime, connected by SPSC rings (`rtrb`).
- **Optional busy-polling.** Offer bounded spin-before-park for latency-critical deployments.

### S6. Separate tick-local dedup from the long-lived primary

§3.4 says the primary index spans every segment. Truncating a transient segment then has to delete each transient row
from a large table, which is a random access per row. That contradicts the comment in §3.9, "O(tick data), no per-row
work".

- Give every relation with a transient segment its own tick-local dedup table.
- A transient insert first probes the long-lived primary, read-only and only for mixed relations, and then inserts
  into the tick-local table.
- Truncation clears the small, cache-resident table, subject to MF-2's shrink policy for tick-local tables.
- MF-1 removes most of these tables entirely.

### S7. Key layout in the kernel shape traits

ARCH-04 promises kernels generic over "key layout", but §4.10 lists only `RowShape`, `Deaths`, `LatShape` and `Prov`.

- Add `KeyShape` as const-generic column positions, so generated code unrolls the key gather, hash and compare.
- The interpreter pre-instantiates the common shapes (1–3 key columns at positions 0–3) and keeps a dynamic fallback.

### S8. Batch-mode head insert for large Δ

Per-tuple hash insert-if-absent dominates batch Datalog:

- Soufflé's profile is 45% inserts and 35% membership tests;
- GPUlog measured 77.8% of the time in deduplication at 32 threads (R09 §4.4).

When a version's output passes a threshold, switch modes: buffer the output, radix-sort it by key words, deduplicate,
then probe the primary in sorted order or merge-difference it against a sorted run. RecStep's DSD cost model decides
between one-phase and two-phase difference. ENG-102's parallel path needs this anyway.

### S9. Digest maintenance behind a shape trait

The incremental state digest (§4.11) adds, for every insert, death and merge, a hash per column plus a 128-bit add.
Only the simulator, BMC and replay need it.

- Add `DigestSink { const ENABLED: bool }`, following `ProvenanceSink`.
- It is off in production unless a feature asks for it.

### S10. Specify the aliasing contract for append-while-scan

Recursive and self-feeding rules append to relations they are reading. Say how that is done in Rust:

- reads are epoch-bounded views over stable-address chunks (MF-6);
- appends go only to the tail;
- secondary indexes catch up at epoch end, using the existing `built_upto` watermark;
- only the primary index and lattice cells are mutated in the middle of a pipeline.

Without this contract, the natural safe-Rust implementation either buffers every derived tuple, which costs a copy
and a second dedup, or reallocates `Vec<Arc<Chunk>>` under live iterators.

### S11. Counted regime: one integrated state, versioned reads

§3.4 applies `D_j` to `I_j` "right after version j", and says that therefore "no multiversion read is ever needed".
That breaks down when two Counted rules in different strata read the same Shrinking input. The second rule's `ZOld`
has already been mutated by the first, unless every rule keeps its own integrated copy, which multiplies memory by
the number of consumers.

- Use the epoch machinery instead: `ZOld` is the state as of the tick-start epoch, and `ZNew` is the current state.
- Rows touched this tick carry `(w_old, w_new)` in the `ZBatch`.
- One integrated state per relation then serves every consumer.

### S12. Canonical-order indexes and sort keys for strings and bytes

Sorted indexes over interned columns follow intern-id order (§4.1). As a result:

- range predicates on strings cannot use a sorted index. That covers object-store prefix listings (FLAG-125–127) and
  `DeleteRange { prefix }` (DIST-024);
- `index!`, `top!`, `collect!` and subscriptions sort through `cmp_canonical`, which touches random interner entries
  on every comparison.

The fix:

- Cache a `sort_prefix: u64` in each interner entry: the first 8 bytes, big-endian.
- Add `IndexKind::Sorted { order: Canonical }`, whose run keys are `(sort_prefix, id)`. A full compare happens only
  when two prefixes tie.
- Use `sort_prefix` in every canonical sort.

### S13. Bound the epoch boundary tables

Each `RowStore` gains one `EpochIndex` entry per epoch in which it received appends. That is unbounded for a
relation appended every tick, such as a Raft log.

- For `CurrentOnly` relations, collapse the boundaries older than the frontier into one.
- More generally, keep only the boundaries some reader can still ask for.

### S14. The benchmark infrastructure details of MF-7

These are listed in "Proposed benchmark plan" below. They are should-fix in their details, but MF-7 requires the
baselines and the suites to exist.

---

## Consider

- **C1. Inline short strings.** A string of up to 7 bytes is packed into the word with a tag bit. The form is
  canonical, so word equality still holds, and short keys skip the interner entirely.
- **C2. Real Free Join nodes.** The Op set has no multi-subatom node, so "convert to Free Join nodes" (§3.7 step 4)
  is nominal. Add `Op::Node { cover, probes }`, which probes every subatom for a batch of cover tuples before
  iterating, which is vectorized Free Join. Add multi-level COLT tries for batch workloads.
- **C3. Profile-guided plans for services.** Export firing counters (ENG-115) and Δ/full statistics from production
  and feed them into the next codegen build (`blossom build --profile`). Soufflé's LOPSTR'22 optimizer gained 12× over
  untuned orders this way.
- **C4. Wire.** Do not sort BATCH tuples canonically unless DIST-006 requires it for the batch kind, because digests
  are order-independent. Skip sender-side dedup for small non-lattice batches. Make the per-channel specialized codecs
  P0.
- **C5. A faster simulator.** When both ends share a schema, pass decoded word batches through a per-world
  fingerprint-to-id translation instead of encoding and decoding each frame. Run the full codec on a sampled fraction
  of worlds and on every replay. This trades some fidelity (ARCH-03) for node-ticks per second.
- **C6. Adaptive tick batching under saturation.** A linger of at most N µs or M frames amortizes fixed costs and
  level-triggered resends (§3.5), bounded by a latency setting.
- **C7. WAL encoding.** Word-image WAL records would encode faster, but they break the principle that messages and the
  WAL share one codec. Revisit only if profiles show WAL encoding; fsync dominates 2PC.
- **C8. Linux-only fsync sign-off.** State that fsync-bound benchmarks (2PC) are signed off on Linux only, because
  macOS `F_FULLFSYNC` costs milliseconds.
- **C9. Tiny-tick fast path.** Skip a version with an empty Δ range in O(1) (state this explicitly), and give the
  interpreter a single-tuple path that bypasses the batch machinery.
- **C10. Merge tiny strata in codegen.** Fuse consecutive tiny non-recursive strata into one function per wave, after
  DFIR's inline DAG, which gained 35–50% on Paxos. Keep `#[inline(never)]` for large strata, where compile time
  matters.

---

## Proposed targets (replace §4.12's micro-targets)

The numbers are proposals, to be confirmed or revised at the first M2 measurement. State size means rows across all
relations.

| Metric | Interpreter | Generated |
|---|---|---|
| Idle tick (no dirty stratum) | ≤ 300 ns | ≤ 100 ns |
| Fixed cost per dirty stratum | ≤ 60 ns | ≤ 20 ns |
| Marginal cost per message, state in cache (≤ 10^4 rows), 3 key-lookup strata | ≤ 400 ns | ≤ 150 ns |
| Marginal cost per message, state 10^6–10^7 | above + ≤ 1 DRAM miss per probe, with ≥ 4 misses in flight (S1) | same |
| p999 / p50 tick latency at 10^7 state under churn | ≤ 10 | ≤ 10 |
| Maximum tick latency over a 24 h soak at 10^7 state | ≤ 5 ms | ≤ 5 ms |
| `interner_bytes`, memory over a 24 h soak | bounded by live state (MF-4) | same |
| Memory per row, binary relation, 1 secondary index | ≤ 1.5× Soufflé | — |
| Snapshot | ≤ 1 µs + 20 ns per relation; zero cost to the running engine | same |
| Batch, P0, single-threaded | ≤ 1.0× Soufflé `-j1` on ≥ 2/3 of BENCH-200, ≤ 1.5× on all (listed in §0.2) | same |
| Batch, P1 | ≥ Soufflé `-j4`, ≤ 2× its memory (BENCH-200 as written) | same |
| Protocols | ≥ 1.0× DFIR on the same machine and transport (P0); ≥ BENCH-202 absolute numbers | same |

## Proposed benchmark plan (amend §4.13)

**Baselines, all P0 and all on the same machine.**

- `dfir_rs`: Voting, 2PC and Paxos from the SIGMOD'24 artifacts, over the same TCP/TLS transport.
- `dbsp` (Feldera): the primary incremental baseline.
- `differential-dataflow`: kept.
- Ascent: batch programs and lattice SSSP.
- Soufflé: kept.
- Hand-written Rust Voting, 2PC and Paxos over our transport. This is the floor, and it measures the abstraction tax.

**Suites.**

1. **Tick micro-benchmarks.** Idle ticks; 1 and 100 messages; state from 10^3 to 10^7, both in cache and out of
   cache; a per-phase breakdown (ingest/decode/intern, strata, temporal, outbox encode, WAL encode, fsync wait) in every
   report.
2. **Update-heavy KVS.** Zipf 0.99; mixed upsert, delete and get; a 24-hour soak; memory flatness; p99 and p999.
3. **Lattices.**
   - Ascent's `Dual` SSSP;
   - an Anna-style LWW and set-union KVS;
   - OR-set and 2P-set CRDT gossip with delta shipping (DIST-006);
   - quorum counting over `LSet<Node>`;
   - Bloom^L's non-morphism trap run as a benchmark as well as a test.
4. **Network.**
   - open-loop, constant-rate load (wrk2-style) with coordinated-omission-corrected HDR histograms;
   - at 50%, 80% and 95% of peak throughput;
   - TLS on;
   - the n2-standard-4 sign-off stays, on Linux only.
5. **Simulation and LDFI.** Node-ticks per second per core, with and without per-tick snapshots; LDFI time against
   Molly's published times.

**Gating.**

- Micro-suites are gated on instruction counts (`iai-callgrind` on Linux CI, since valgrind has no macOS arm64 port)
  with a 1–2% threshold.
- Macro suites are gated on wall-clock time on a dedicated bare-metal runner, with repeated runs and a statistical
  test.
- Allocation counts are exact.

## Edits to `ARCHITECTURE.md` implied by this critique

| Section | Edit |
|---|---|
| §0.1 | Add ARCH-21 (tail-latency discipline, MF-2) and ARCH-22 (per-column value representation plus a refcounted interner, MF-4). |
| §0.2 | List the refinements of BENCH-200 and ENG-020 (MF-7, S3). |
| §3.6–3.8 | Add the `fuse_tick_local` pass, dedup elision and `Op::Tee` (MF-1); reuse the primary index for key probes (S2). |
| §3.4 | Specify Counted reads through epochs (S11). |
| §4.1 | Blob columns, the tick arena, refcounted interning, `sort_prefix` (MF-4, S12, C1). |
| §4.2–4.3 | Stable-address chunks; live-only indexes; posting lists; in-place keyed update; incremental compaction, rehash and merges; tick-local dedup tables (MF-2, MF-3, MF-6, S2, S6, S13). |
| §4.4 | Batched prefetching probes at P0 (S1). |
| §4.5 | Bitmask and small-set lattices; per-object COW (S4, MF-6). |
| §4.7, §5.1, §5.3, §5.8 | Borrowed and pooled output APIs; decode-to-words ingest (MF-5). |
| §4.10 | `KeyShape`, `DigestSink` and `LANE` in the shape traits (S7, S9, S3). |
| §4.12–4.13 | Replace with the targets and benchmark plan above (MF-7). |
| §4.14, §5.2 | A dedicated engine thread and SPSC rings (S5). |
| §10.2 | Batch probe kernels in generated code; merging of tiny strata (S1, C10). |
| §11.7 | Zero-allocation test over the full node loop (MF-5). |
