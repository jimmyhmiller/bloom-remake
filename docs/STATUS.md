# bloom-remake: status (2026-09-29)

Blossom is a Datalog-family language for distributed systems, in the line of Dedalus and Bloom, with lineage-driven
fault injection (LDFI) and an incremental engine. It is written in Rust. This page covers what works today, how it is
checked, and what is left. Everything described here is merged on `main`.

## The flagship goal is met

The goal was a linearizable key-value store, replicated with Raft, written in Blossom, running as three real processes
over TCP, and compared honestly with etcd.

| Criterion | Status | Evidence |
|---|---|---|
| LDFI finds the seeded bugs in Molly's Raft cases and certifies the fixed version | Done (S4) | `examples/e12_raft.bls` gives the BENCH-137a–h verdicts; `tests/integration/tests/bls_raft_ldfi.rs` |
| Every client history is linearizable under kill -9 and partitions | Done (S4, hardened in the S4 and S5 reviews) | `crates/blossom-cli/tests/it/raft3.rs` (3 processes); `tests/integration/tests/raft_kv.rs` (cluster simulator) |
| The engine agrees with the reference oracle on the whole corpus | Done (S5), for every case that runs | `xtask corpus --check --engine`; `engine_examples.rs`; `engine_planner.rs` |
| A fair comparison with etcd | Done (S5) | `docs/plan/notes/etcd-comparison.md`; `scripts/bench-raft-vs-etcd.sh` |

The Raft KV itself is `examples/e11_raft_kv.bls`, about 300 lines of Blossom.

### The etcd comparison in one table

Both systems run on the same machine, with the same client, both at their leader, and `F_FULLFSYNC` on every commit.
Every history was checked linearizable. The figures are the range over repeated 20 s trials.

| Clients | Blossom (`crc`, one fsync per commit) | etcd 3.6.5 |
|---|---|---|
| 1 | 70–78 ops/s, 12.7 ms p50 | 188–190 ops/s, 1.7 ms p50 |
| 16 | 581–636 ops/s, 24–26 ms p50 | 429–490 ops/s, 31 ms p50 |
| 64 | 1,698–1,765 ops/s, 34 ms p50 | 1,653–1,784 ops/s, 34–36 ms p50 |

- **One client:** etcd is about 2.5x faster, because e11 sends every read through the Raft log and etcd's reads use
  ReadIndex.
- **16 clients:** Blossom is 1.2–1.5x faster.
- **64 clients:** the two are even, but etcd's p99 is steadier.
- **Default durability:** Blossom's `strict` certification is about 4.3x slower than `crc`.

## Delivery so far

| Slice | Merged | What it delivered |
|---|---|---|
| 1 Molly parity | `bcd228f` | The `.ded` frontend, the reference oracle, the synchronous-round simulator, provenance, and SAT-based LDFI. Molly's programs give their published verdicts |
| 2 The Blossom language | `ec60d6b` | The `.bls` frontend (parse, resolve, typecheck, classify, lower) on the same IR; lattices; specs; LDFI on Blossom |
| 3 Real processes | `7fd2c94` | The sans-IO node, the runtime (threads, TCP, sessions, group commit), and durable tables on the WAL. `blossom run` serves e01, which survives kill -9 |
| 4 Raft | `48664d8` | The e11 Raft KV and the e12 Molly Raft; params, `argmin!`/`argmax!`, `index!()`, `rand_range`, `majority`, `bootstrap fresh`. The cluster simulator gets safety observers and a nemesis |
| 5 The fast engine | `b69c573` | The incremental engine, the default executor, cost-based joins, crc certification, and the etcd comparison |

Each slice ended with an adversarial review whose findings were fixed before the merge. The notes for each slice are in
`docs/plan/notes/S3.md`, `S4.md` and `S5.md`.

## What works

### Languages
- **Molly's `.ded` dialect** runs unchanged.
- **Blossom (`.bls`):**
  - modules, roles, and relation kinds: `table`, `scratch`, `static`, `input`, `output`, `channel`, `durable`, and
    `view`;
  - handlers (`on`/`while`), joins, stratified negation, recursion, and aggregates (`count`, `sum`, `min`, `max`,
    `collect`, …);
  - `let`/`where` and `if`, `send … to`, `upsert`/`delete`, and resolution policies;
  - built-in lattices (`LMax`, `LMin`, `LSet`, `LPoint`, …), bootstrap and facts, invariants, deploy-time params,
    `index!()`, choice and order filters, `rand_range`, `majority`, and integer casts;
  - specs for LDFI.
- Diagnostics use stable codes (`BLS0xxx`). An unimplemented feature is always a hard error, BLS0908, never a silent
  default.

### Execution
- **The reference oracle** (`blossom-oracle`) is the naive, stateless definition of the semantics.
- **The engine** (`blossom-engine`) is incremental and indexed.
  - It keeps counted support across ticks and computes delta queries per changed dependency.
  - Lattice cells settle lazily.
  - Joins run in a greedy cost-based order, with range probes and checks hoisted as early as their inputs allow.
  - It agrees with the oracle exactly, including runtime errors.
  - Its work per tick depends on what changed, not on the size of the state; for the Raft KV that stays flat as the
    log grows.
- **Nodes** (`blossom-node`) are sans-IO:
  - invariants R and B (release after durability);
  - tick and time reservations;
  - recovery from a checkpoint plus WAL replay;
  - `recovered()` semantics.
- **The runtime** (`blossom-runtime`) has an engine thread, a committer doing group commit, a checkpoint thread, TCP
  peers with per-node dial overrides, and client sessions.
- **The store** (`blossom-store`) is a WAL with segments, checkpoints and META.
  - It has two tail certifications: `strict` (four fsyncs, so corruption can always be told from a torn tail) and
    `crc` (one fsync).
  - Every crash point of its crash-point tests keeps every acknowledged write.

### Checking
- **Simulators:**
  - the synchronous-round simulator, with omissions and crashes (Molly's crash view);
  - the cluster simulator: real nodes on a simulated filesystem, a lossy network, and a nemesis with crashes (some
    between append and sync), downtime, splits and one-way cuts;
  - Raft safety observers and a scripted fault API for directed scenarios.
- **LDFI** (`blossom-ldfi`, `blossom-prov`, `blossom-sat`): provenance graphs, a SAT encoding, minimal falsifiers, and
  exhaustive certification when the search space is small enough.
- **Linearizability:** a WGL checker with per-key partitioning (`blossom-sim::linearize`).
- **Golden corpus** under `tests/corpus/`, with a status ratchet.

  | Area | Engine agrees | Not yet runnable | Failing |
  |---|---|---|---|
  | core | 189 | 54 | 0 |
  | async | 35 | 141 | 0 |
  | lattices | 47 | 74 | 0 |
  | lprov | 26 | 38 | 5 (don't compile on the oracle either) |
  | net | 3 | 3 | 0 |
  | ldfi (Molly) | 92 (verdicts; the engine also agrees on the programs) | 0 | 4 |

  The 4 ldfi failures are run-count targets that need unbuilt search reductions, and Flux 22/21/1, which needs
  proofs.

### Commands that work
- `blossom sim`
- `blossom ldfi`
- `blossom run` (`--evaluator engine|oracle`)
- `blossom node init`
- `blossom-kv load` / `blossom-kv etcd`
- `xtask corpus`
- `xtask check-codes` / `xtask check-layers`

## What is left

### The Raft KV and runtime
- **Reads go through the log.** e11 writes every get into the Raft log; this is the one-client latency gap with etcd.
  ReadIndex or lease reads would fix it.
- **Out-of-order entries are dropped.** A follower drops an entry that arrives before its predecessor, and the next
  heartbeat repairs it. TCP delivers in order between peers, but the simulator reorders messages, so simulated
  throughput collapses under heavy load.
- **No log compaction.** The log grows without bound. Checkpoints encode the whole durable image, on the engine thread,
  every 256 MiB of WAL.
- **Only plaintext transport.** mTLS is not implemented (DIST-060), so deployments need `insecure-dev`.
- **The benchmark scope** is one machine, one SSD, uniform keys, small values, and no failures during the measured
  runs.

### Language features not yet implemented
These are all BLS0908 today, listed roughly by how many corpus cases they block:
- functions (LANG-180);
- user-defined lattices and impl blocks (LANG-135);
- several choices in one body (LANG-116);
- soft tables (LANG-048);
- named generic arguments (LANG-021);
- `sealed by` and `exactly_once` clauses (LANG-020), and sealed tables (LANG-049);
- lattice folds in expressions (LANG-123) and `Lex` (LANG-124);
- `final` tests (LANG-212);
- keys on channels and inputs (SEM-050);
- `top!` in bodies (LANG-108);
- `index!` with `by`/`per`;
- `majority` over a relation (LANG-113).

Three examples need some of these: e05 lattices, e06 wordcount, and e08 failure detector.

### Missing subsystems
- **The analysis backend.** Static analyses such as confluence and CALM certificates, finality, and the lint codes. It
  blocks 104 corpus cases, most of the "not yet runnable" column.
- **The seeded asynchronous simulator** (TEST-001). It blocks 43 cases.
- **Restart faults in the synchronous harness.**
- **Corpus cases with extra sources or spec files.**
- **Verification:** `blossom verify` (BMC, SMT, inductive invariants) is a stub; `blossom-smt` exists but isn't wired
  up. Flux 22/21/1 is waiting on this.
- **LDFI search reductions** needed to meet Molly's published run counts (BENCH-130q, BENCH-136g/i).
- **Code generation and the word-level kernel.** `blossom-codegen`, `blossom-kernel`, `blossom-plan` and
  `blossom-rewrite` are empty crates; the engine is value-level.
- **CLI stubs:** `check`, `build`, `fmt`, `plan`, `explain`, `deploy`, `node status`, `config`, `trace`, `why`/`whynot`,
  `verify`, `compat`/`release`, `store`, `self-check`, `repl`, `upgrade`, `admin`, `lsp`, and `completions`. Each
  exits with a clear "not implemented" message (exit 7).
- **Placeholder crates:** `blossom-lsp`, `blossom-trace`, `blossom-schema` and `blossom-testkit`.

### Known spec gaps and small issues
- **Open spec questions.** `docs/plan/BUGS.md` has 26 open rows, mostly questions for LANGUAGE/PLAN found while
  writing the corpus, not code defects.
- **Several errors in one tick.** Which error is reported is unspecified. The oracle and the engine can report
  different codes, though always at the same node and tick.
- **`sum!` cost.** It refolds its group on every change, O(group); `count!`, `min!` and `max!` are O(log group).

## The next goal: a Kafka-compatible broker in Blossom

Decided 2026-09-29. Stock Kafka clients (`kcat`, the Java client 3.7+/4.0, franz-go) connect to a broker cluster
written entirely in Blossom, with no Rust translation layer. The platform gains only generic additions: byte streams,
functions, byte primitives, an `extern fn` standard library for checksums and codecs, and blobs.

| Document | What it holds |
|---|---|
| `docs/design/KAFKA.md` | the goal, decisions K1–K7, the architecture, the test strategy, and how to resume |
| `docs/design/FOREIGN-PROTOCOLS.md` | the generic language and runtime additions |
| `docs/design/SLICES.md` | slices 6–10 |
| `docs/plan/notes/S6.md` | the first slice's checklist and "resume here" |

## Where things are

| What | Where |
|---|---|
| The Raft KV / Molly Raft / election | `examples/e11_raft_kv.bls`, `examples/e12_raft.bls`, `examples/e03_raft_election.bls` |
| Language reference | `docs/design/LANGUAGE.md` |
| Architecture and plan | `docs/design/ARCHITECTURE.md`, `docs/design/SLICES.md` |
| Slice notes and reviews | `docs/plan/notes/S3.md`, `S4.md`, `S5.md` |
| The etcd comparison | `docs/plan/notes/etcd-comparison.md`, `scripts/bench-raft-vs-etcd.sh` |
| Engine | `crates/blossom-engine/src/{engine,rule,store,expr}.rs` |
| Reference oracle | `crates/blossom-oracle/` |
| Raft tests | `tests/integration/tests/raft_kv.rs`, `crates/blossom-cli/tests/it/raft3.rs` |
| Engine differential tests | `tests/integration/tests/engine_{planner,examples}.rs`, `xtask corpus --check --engine` |
