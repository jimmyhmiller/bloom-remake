# Critique of ARCHITECTURE.md: production quality and code health

Reviewer lens: trait boundaries, the crate DAG and compile times, error handling, the no-silent-stub policy, the
testing strategy, durability, security, observability, operability, and whether independent agents can build crates
in parallel against these interfaces. Semantics, planning and performance are reviewed only where they touch that lens.

Reviewed: `docs/design/ARCHITECTURE.md` (first draft, 2026-09-27), read in full, checked against `docs/DECISIONS.md`,
FEATURES.md (SEM-07x, DIST-02x/03x/04x/06x, TEST-010/011/103–108, LANG-181/183) and LANGUAGE.md §16 and §20.
Section numbers below (§n) refer to ARCHITECTURE.md unless marked otherwise.

## Verdict

Most of the structure is right and should stay:

- the sans-IO node shared by the runtime and the simulator (ARCH-03);
- an oracle that shares no code with the engine (ARCH-16);
- construct expansions as the normative meaning, with plan perturbation checking every fast path against them
  (ARCH-02, §11.3);
- determinism lints (ARCH-19);
- the pure admission function (§5.8);
- SAT and SMT behind traits, with a differential exhaustive backend (§8.6, §9.3);
- a machine-checked layer DAG (ARCH-01);
- an explicit `Unimplemented` error (§12.1).

The draft is not yet ready for implementation, and certainly not for parallel implementation, for four reasons:

1. **The crate DAG has hidden cycles.** Several types sit in crates that their consumers are not allowed to depend on,
   so the table cannot be built as written.
2. **The durability path has correctness bugs.** They are the kind that pass every test described and then lose or
   refuse data after an ordinary power failure or an upgrade.
3. **The durability code is not simulated.** The one subsystem most likely to have bugs is the one that ARCH-03's
   rule ("the code that simulation tests is the code that ships") does not cover.
4. **The fault policy is dangerous in production.** One well-typed client message can halt a server for good.

The nine must-fix items below cover these. Items are ordered by priority within each tier.

| Id | Tier | Area | One line |
|---|---|---|---|
| MF-1 | must | crate DAG | Semantic cycles and misplaced crates: trace↔sim/node/engine, std at L5, codegen `Builder`, spec planning in sim, `CompiledProgram` in node, prov↔engine, testkit generators |
| MF-2 | must | parallel work | No interface freeze or type ownership. The codegen↔engine ABI (`ExecCtx`) is unspecified. No conformance suites or fixtures. |
| MF-3 | must | durability | The release frontier is wrong in the driver sketch and sits outside the sans-IO node. fsync failure is unspecified. `Durability` serializes checkpoints behind the WAL. |
| MF-4 | must | durability | WAL framing reports false "corruption" after a normal crash and accepts stale records. Migration recovery order corrupts or loses data. |
| MF-5 | must | durability/ops | No data-dir lock or identity. An empty or wrong data dir silently runs `bootstrap fresh`, which breaks Raft-class safety. |
| MF-6 | must | testing | The simulator runs `MemDurability` only: no VFS, no torn writes, no async fsync completion. |
| MF-7 | must | security/ops | ARCH-20 plus remotely triggerable hard errors let one message halt a server, or crash-loop it under `Restart`. |
| MF-8 | must | replay/oracle | Traces miss service results, sessions, directory updates and cluster version. Extern fns are unavailable to the oracle. The quarantine file is not replayable. |
| MF-9 | must | CI/stubs | "Corpus cases are never skipped" contradicts "every milestone ends green". A strict status ratchet is needed. |
| S-1…S-37 | should | all | See the should-fix section |
| C-1…C-15 | consider | all | See the consider section |

---

## Must-fix

### MF-1. The crate DAG has semantic cycles and misplaced crates

`xtask check-layers` checks `cargo metadata` edges. The table (§1.2) and the mermaid graph (§1.3) agree with each
other for L0–L4; I checked mechanically that every table edge is in the mermaid graph's transitive closure. The
graph omits the L5 crates std, blossom, bench, lsp and testkit. But the *types* the
document places in each crate need edges the table forbids. Cargo will reject these, or agents will add edges ad hoc.

| # | Where | Conflict |
|---|---|---|
| a | §6.4 `TraceEvent` in `blossom-trace` (deps: core, ir) | Uses `Decision` and `MsgId` (defined in sim, §6.2), `TickTrigger` (node, §5.1), `Reject` (node ingress, §5.8) and `ChoiceEntry` (engine/oracle). Sim, node and engine all depend on trace, so this is a cycle. |
| b | §1.2 `blossom-std` is L5 (deps: core, kernel) | Its `.bls` sources are needed by `core::SourceDb`/`blossom-front`/`blossom-driver` (L1, §13.1) and by `blossom_codegen::Builder` (L2, §10.1). Its host fns must produce the engine's `HostFns` (§4.7), and std does not depend on the engine. |
| c | §10.1 `blossom_codegen::Builder` "runs `blossom-driver::CompileSession`" | Codegen's deps are core, ir, plan and schema. It cannot reach the driver or the frontend. |
| d | §6.5 "A spec program is **planned** and run on an ordinary `Engine`" inside `blossom-sim` | Sim is forbidden to depend on `plan` (§1.3). `CompiledProgram.specs` holds only IR (§5.1). |
| e | §5.1 `CompiledProgram`/`CompiledRole`/`GeneratedProgram` live in `blossom-node` and are "what the compiler (blossom-driver) hands to a runtime" | Driver (L1) cannot name node (L3) types. `CompiledRole.acl: AclTable` is "the ANA-105 result", but node may not depend on analysis. |
| f | §1.2 `blossom-prov` (deps: core, ir) builds graphs "from Tier C logs and Tier B annotations"; Tier B queries are "compiled as subroutines over the same kernels" (§4.9) | The `FiringLog` format belongs to the engine, and the kernels to the kernel crate. Prov depends on neither. |
| g | §1.2 facade `blossom` depends on driver, node and runtime; §1.3 says "a generated binary must not link the compiler" | Generated system crates that use the facade link the whole compiler. |
| h | §11.1 "proptest in each crate, generators from `blossom-testkit::gen`"; testkit depends on "nearly all" | A dev-dependency from, say, `blossom-kernel` on testkit, which itself depends on `blossom-kernel`, builds **two copies** of the kernel in the test binary. That produces "expected `blossom_kernel::X`, found `blossom_kernel::X`" errors the first time a generated value crosses crates. |

**Change.**

- (a) Give the trace crate the observation vocabulary. `blossom-trace` owns `SchedDecision` (today's `Decision`),
  `MsgId`, `TickTrigger`, `DropReason`, `RejectReason`, `FaultSchedule` and `NodeDesc`. The engine and the oracle
  both produce `ChoiceEntry`, `ViolationRecord` and `TickDigests`, so those move to a new `blossom-ir::obs` module.
  Node's `Reject` wraps `RejectReason`. Sim, ldfi and verify import `FaultSchedule` from trace.
- (b) Split std in two:
  - `blossom-std-src` (L1, no deps) exports `STD_SOURCES`, and the driver loads it into `SourceDb`.
  - `blossom-std-host` (L3) holds the Rust `extern fn` implementations, against the Value-level `ExternFn` trait
    proposed in MF-8, so it needs only `core` plus `sha2`.
- (c) Add `blossom-build` (L5, deps: driver, codegen): the `build.rs` API and the regeneration cache. `blossom-codegen`
  becomes a pure function from `(PhysicalProgram, SchemaCatalog)` to a `TokenStream`. It can then also drop its
  dependency on the planner; `ir::plan` is enough.
- (d, e) The driver outputs a compiler-side `CompileOutput`, defined in `blossom-ir`/`blossom-schema`. It holds the
  guarded IR; per-role projections and `PhysicalProgram`s; **planned spec programs** (`SpecPlan`);
  `SchemaCatalog`; `AclTable`, which moves to `blossom-schema` and is filled by ANA-105; and certificates.
  `blossom-node` provides `CompiledProgram::from_output(CompileOutput, ExecutorKind)`.
- (f) The Tier C record types and `FiringLog` move to `blossom-kernel::prov`, next to `ProvenanceSink` and
  `FiringRecord`, and `blossom-prov` depends on kernel (an L2→L2 edge). Tier B proof search runs against an
  `EngineSnapshot` through a small read trait (`ProvRead`) that the engine implements and prov defines.
- (g) The facade gets features `compiler` (driver) and `runtime` (node and runtime, the default). `check-layers` also
  checks that no `systems/*` crate has a normal (non-build) dependency path to `blossom-driver`.
- (h) Each crate exposes its own generators behind an `arbitrary` feature (`blossom-core/arbitrary`,
  `blossom-ir/arbitrary`, `blossom-wire/arbitrary`, …), as `blossom-lattice/laws` already does. Testkit only
  re-exports them and composes programs. `check-layers` includes dev-dependencies and rejects any dev-dependency
  cycle.
- Make the layer table machine-readable (`xtask/layers.toml`). `check-layers` reads it, and the doc's table and
  mermaid graph are generated from it, so the three cannot drift apart.

Edit: §1.2, §1.3, §1.5, §4.7, §5.1, §6.4, §6.5, §10.1, §11.1, §13.1.

### MF-2. No interface freeze, type ownership or seam contracts for parallel agents

The doc says signatures are "interface sketches" that "do not fix every derive or lifetime" (§0 conventions). That is
reasonable for a design, but about 40 cross-crate types are named without an owning crate or a definition:

- engine types: `Delivery`, `HostRow`, `TimerFire`, `Outbox`, `OutboxBuilder`, `DurableDelta`, `DurableImage`,
  `SubscriptionDeltas`, `ChoiceLog`, `TickDigests`, `Staging`, `Store`, `ProvRuntime`;
- node and store types: `WalRecord`, `Recovery`, `RecoveredImage`, `DurableSnapshot`, `TickTrace`, `AdmittedBatch`,
  `OutFrame`, `ConnInfo`, `Quotas`, `IngressSink`;
- shared types: `HostFns`, `SchemaCatalog`, `AclTable`, and more.

Recovery alone has three differently named image types in three crates (`Recovery`, `RecoveredImage`,
`DurableImage`), with no stated conversion. Two agents building engine and node in parallel will each invent these
types, and they will not agree.

The largest unspecified seam is **generated code ↔ engine**. `ExecCtx` (§4.7) exposes `pub` fields of engine
internals (`Store`, `Staging`, `OutboxBuilder`, `EpochClock`, `ProvRuntime`). The codegen example (§10.2) indexes
`cx.store[self.msg]` and calls `cx.outbox.push::<2>` and `kernel::scan::<Transient, NoDeaths, 4>`. As written, every
refactor of engine internals breaks codegen, and the engine and codegen agents will collide at M5.

The build order (§14.2) is also strictly sequential, even though half the crates have no dependency on each other
(smt, the SAT backends, wire, trace, syntax/front and kernel).

**Change.**

1. **The M0 deliverable is a compiling skeleton of the whole workspace.** Every crate exists. Every public type and
   trait named in this document compiles with its final owning crate, and every body is `unimplemented_feature!`.
   Add an appendix, "Type ownership", listing each cross-crate type with its crate and module (MF-1 gives most rows).
   After M0, changing a public item of `blossom-core`, `blossom-ir` (including `ir::plan`), `blossom-schema` or
   `blossom-trace` requires three things: an ARCHITECTURE.md amendment, a DECISIONS.md line, and a bump of that
   crate's `API_VERSION` constant.
2. **Specify a narrow execution ABI.** `blossom-engine::abi` is the only engine surface generated code may name. It
   contains cursor constructors, `insert`/`merge`/`stage`/`outbox_push`/`prov_firing` entry points,
   `RelHandle`/`IndexHandle` newtypes resolved in `make`, and `ExecCtx` with private fields and methods. The kernel's
   generic entry points are re-exported through `abi`. `blossom_kernel::API_VERSION` becomes `blossom_engine::abi::VERSION`
   and covers both. `xtask check-codegen-abi` parses generated code with `syn` and rejects any path outside
   `blossom_engine::abi`, `blossom_wire::abi` and `core`.
3. **Put a conformance suite next to each trait**, so an implementer can prove conformance without the rest of the
   system:
   - `blossom_store::conformance::wal_suite::<W: WalWriter>()`, run on `FileWal` over `SimFs` and the real FS
     (MF-6), and on `MemWal`;
   - `blossom_node::conformance::transport_suite`, run on `MemTransport`, `TcpTlsTransport` and `QuicTransport`;
   - `sat_suite`, which checks every `SatSolver` against `ExhaustiveSolver`;
   - `smt_suite`, which runs z3 and cvc5 on a fixed set of scripts;
   - `executor_suite`, which checks any `PlanExecutor` against the interpreter tick by tick;
   - `scheduler_suite`, which checks replay equivalence.
4. **Fixture programs built without the frontend.** `blossom-ir::fixtures` (feature `fixtures`) builds 20–30 small
   programs directly with `IrBuilder`: TC, a counter, a keyed table with deletes, choose, a lattice cell, a channel
   ping-pong, an upsert conflict, and so on. The planner, engine and oracle agents can then start at M0+ε instead of
   waiting for M1.
5. **Make `blossom-node` generic over an `Evaluator` trait**
   (`run_tick`, `load_durable`, `snapshot`/`fork`, `state_digest`, `wants_tick`). The engine implements it, and a
   testkit adapter implements it over the oracle. Node and sim (M3) can then be built and tested before the engine
   (M2) is finished, and sim-on-oracle versus sim-on-engine becomes one more differential check.
6. **Error-code registry.** LANGUAGE.md §20 is the source of truth for `BLSnnnn`. Add the `BLSRnnn` table there too;
   today the codes exist only in the `TickError` enum. Mirror both in `blossom-core::codes` as a `const` table. A test
   checks that every code the source uses is registered, and that no code is used by two variants with different
   meanings. Allocate code ranges per crate so parallel agents cannot collide.
7. **Replace the sequential M-list with tracks plus integration milestones.** After M0 (skeleton) and M1a (core, lattice,
   ir, fixtures), these can proceed in parallel:
   - A: plan, kernel, engine;
   - B: syntax, front;
   - C: oracle;
   - D: wire, store, SimFs;
   - E: trace, then node and sim over the oracle evaluator;
   - F: smt;
   - G: SAT backends.

   The milestones M2–M5 become the points where tracks integrate.

Edit: §0 conventions, §4.7, §10.2, §12.1, §14.2, plus a new appendix.

### MF-3. Group commit: the release frontier, fsync failure, and the `Durability` trait shape

Three linked defects, all in §5.1, §5.2 and §5.6.

**(a) The release frontier.** The node-task sketch (§5.2) does this for a tick with no WAL record:
`None => commit_tx.send(fx.tick) // still ordered behind earlier ticks`. `Node::committed(upto)` "releases every
parked tick ≤ t" (§5.1). Take tick 5, which has a WAL record in flight, and tick 6, which is trivial. The node
receives `done.tick = 6`, calls `committed(6)`, and releases tick 5's outbox **before tick 5's fsync completes**. That
violates SEM-072 and DIST-020. The comment claims an ordering that nothing enforces. The invariant also lives in the
tokio driver, outside the sans-IO node, so the simulator never exercises it.

**Change (a).** Move the rule into `Node` and state it as an invariant:

```rust
impl Node {
    /// The driver reports: every WAL record this node produced with tick ≤ `upto` is durable.
    pub fn wal_synced(&mut self, upto: Tick) -> Released;
    /// An append or fsync failed. The node becomes Faulted. Nothing parked is ever released (see (b)).
    pub fn wal_failed(&mut self, err: DurabilityFailure) -> NodeFault;
}
// Invariant R (checked with debug_assert and property-tested in sim):
//   tick t is released  ⇔  every tick t' ≤ t that produced a WalRecord has been reported synced.
// Trivial ticks send no message to the driver. The node releases them as soon as the frontier passes them.
```

**(b) fsync failure.** `sync() -> Result<Lsn, StoreError>` has no stated policy. After a failed `fsync`, Linux may
already have dropped the dirty pages and marked them clean (the 2018 PostgreSQL "fsyncgate" problem), so retrying
the call can report success for data that is gone. The engine has also moved past the failed tick, since ticks t+1…
were computed while the fsync was in flight.

**Change (b).** Any `append` or `sync` error (EIO, ENOSPC, EDQUOT) is fatal to the incarnation:

- The WAL writer is poisoned and never retried. `wal_failed` faults the node, and every parked tick is discarded.
- Recovery starts from what is on disk, and it opens a new segment. It never appends after a failed one.
- The failure is recorded as `durability_failures_total{kind}`, and the process exit code is "runtime fault" (S-19).

Crash-recovery semantics make this legal: the unsynced ticks were never released.

**(c) The trait shape.** The design wants checkpoints encoded on a background thread while ticks and the committer
keep running (ARCH-10, §5.6, DIST-022). But `Durability::checkpoint(&mut self, …)` takes the same `&mut self` as
`append` and `sync`. Either the committer blocks for the whole checkpoint encode, or an implementer adds a mutex
around the WAL. Recovery is also a method on an already-built backend, so nothing says what state the object is in
before `recover` runs.

**Change (c).** Split the trait by owner:

```rust
pub fn open_node_store(fs: Arc<dyn Vfs>, dir: &Path, id: &StoreIdentity, mode: OpenMode)
    -> Result<OpenedStore, StoreError>;                 // OpenMode::{Existing, InitFresh} (MF-5)
pub struct OpenedStore { pub recovered: Recovered, pub wal: Box<dyn WalWriter>,
                         pub checkpoints: Box<dyn CheckpointWriter>, pub meta: MetaStore, pub lock: StoreLock }
pub trait WalWriter: Send {                              // owned by the committer thread
    fn append(&mut self, rec: &WalRecord) -> Result<Lsn, StoreError>;   // no durability promise
    fn sync(&mut self) -> Result<Lsn, StoreError>;                       // Err ⇒ poisoned for good
}
pub trait CheckpointWriter: Send {                      // owned by the checkpoint thread
    fn write(&mut self, snap: DurableSnapshot, covers: SyncedTick) -> Result<CheckpointId, StoreError>;
    fn install(&mut self, id: CheckpointId) -> Result<TruncateToken, StoreError>;   // CURRENT swap + dir fsync
}
// The committer calls `wal.truncate_through(token)` between batches. Truncation can therefore never race an append.
```

`SyncedTick` can be built only from a tick the node has reported synced, which makes S-2's "checkpoint only committed
state" a type-level fact.

Edit: ARCH-10, §5.1, §5.2, §5.6, and the §15 row on pipelined commit.

### MF-4. WAL framing and recovery rules

**(a) False corruption after an ordinary crash.** §5.6 says: "A bad record followed by valid ones is corruption.
Refuse to start." The committer appends several records and then fsyncs once. Before the fsync returns, the kernel
may write the dirty pages back in any order. After a power loss, record r₂ (page P₁) can be missing while r₃ (page P₂)
survives. None of them was acknowledged, but recovery sees a bad record followed by a valid one and **refuses to start
after a normal power failure**. `MemDurability::crash()` discards a clean suffix (§5.6, §6.3), so the simulator never
produces this pattern.

**(b) Stale records.** The record header `[len][crc32c][tick][kind][payload]` carries no LSN, segment identity or
incarnation, and the doc does not say whether the CRC covers `len`. A preallocated, recycled or partially overwritten
segment can therefore present an old record with a valid CRC after a torn one. Recovery would then either refuse
wrongly or replay a stale delta.

**(c) Migration recovery order.** §5.6 step 1 migrates the **checkpoint** (v6 to v7) and writes a new checkpoint.
Step 2 then "replay[s] the WAL after the checkpoint LSN". The WAL records after the old checkpoint are v6-encoded,
and their segment headers carry v6 hashes. If the new checkpoint keeps the old LSN, v6 records are applied to v7
relations. If it takes the WAL-end LSN, those ticks are silently lost. Either way this is data corruption on upgrade.
TEST-103 would catch it only if its migration fixtures happen to include a non-empty WAL tail.

**Change.**

```
segment header := magic "BLSW" format:u16 store_uuid:[16] segment_seq:u64 incarnation:(restarts:u64, boot_nonce:u64)
                  catalog (DIST-081 table) header_crc:u32
record         := len:u32 crc:u32 lsn:u64 batch:u64 tick:u64 kind:u8 payload
                  crc = crc32c(len ‖ lsn ‖ batch ‖ tick ‖ kind ‖ payload); lsn = byte offset in the log stream
```

- **Invariant B.** The committer never has writes of batch k+1 in flight until `sync` for batch k has returned. The
  loop in §5.2 already works this way; the invariant makes it binding.
- **Recovery scan.** Stop at the first record that is invalid: bad CRC, wrong `lsn` for its position, or wrong
  `store_uuid`/`segment_seq`.
  - Let b be the batch of the last valid record before that point. Scan to the end of the segment.
  - If any valid record there has `batch > b`, batch b was synced before the damage happened. That is **corruption**:
    refuse, naming the file and offset.
  - Otherwise the damage is a **torn tail**. Truncate at the first invalid record, fsync, and continue. Valid records
    of batch b after the tear are dropped; they were never acknowledged.
- **Every incarnation starts a new segment**, so a torn tail can only be at the end of the previous incarnation's last
  segment.
- **Recovery order with migrations:** load the checkpoint (old version) → replay the WAL, decoding each segment against
  its own header's catalog → run the migration chain over the full recovered old state → write a new-version
  checkpoint that covers the WAL end → start a new-version segment → then boot. A crash anywhere in this sequence
  leaves the old checkpoint and WAL intact (TEST-103).
- **Checkpoint integrity.** Each `rel-<id>.dat` is checksummed in `MANIFEST` (BLAKE3 over the file). The whole
  sequence is fsynced in order: files → dir → MANIFEST → dir → CURRENT tmp → rename → dir.

Edit: §5.6 (record format, damage rules, recovery steps), §11.8 (the recovery fuzz target also takes structured crash
images from `SimFs`).

### MF-5. The data directory has no identity and no lock, and an empty directory boots fresh

A node is started with `recovered: Option<Recovery>` (§5.1), and `bootstrap fresh` runs "only when no durable state
was recovered" (DECISIONS.md). Three operator mistakes follow:

- a wrong `data_dir` path;
- an unmounted volume;
- a node started on a new host with the same name.

Each gives an empty directory, so the node boots fresh with the same identity. For Raft, that is a node that has
forgotten its term and vote, which breaks safety. R07 is explicit: "a server that loses persistent state must rejoin
with a new identity via a membership change". There is also no lock file. Two processes pointed at one directory, for
example a supervisor restart racing a stuck process, interleave WAL appends.

**Change.**

- **`blossom deploy init`** creates each node's directory with an identity record in `META`: `store_uuid` (random),
  `deployment_id`, `program_id`, `node_name`, `principal` and `format`.
- **`blossom run` opens with `OpenMode::Existing` by default.**
  - A missing or empty directory is a refusal (exit 5, S-19) with the message "no durable state for node s1 at
    /var/lib/…; if this node is new or has been re-provisioned under a new identity, run `blossom node init` or pass
    `--init-fresh`".
  - A directory whose identity does not match the deployment spec is also a refusal.
- **`LOCK`** is taken with an exclusive `flock` through the VFS and held for the life of the process. If it is already
  held, startup is refused and the holder's pid is named.
- `NodeId` numbering by sorted `(role, name)` (§5.9) must be recorded in `META`, and checked at open. Durable rows
  contain `Node` words (§4.1), so a spec edit that renumbers nodes would silently re-point durable references. Either
  refuse a renumbering, or store `Node` columns durably as names and re-map them at load.

Edit: §5.6 (META, layout), §5.9, §12.4 (config validation), and the CLI table (S-36).

### MF-6. The simulator does not exercise the real durability code

ARCH-03 promises that "the code that simulation tests is the code that ships", but every simulated node uses
`MemDurability` (§6.1). The code most likely to be wrong never runs in simulation:

- segment files and headers, CRCs, torn-tail handling and truncation;
- `CURRENT`/rename sequencing and directory fsyncs;
- checkpoint install, and WAL truncation racing appends;
- migration-on-recovery.

The scheduler also has no event for "an fsync completed" (`Decision`, §6.2), so the pipelined window central to
ARCH-10 (tick t+1 computing, t unsynced, several ticks parked) is never explored. The §15 row "MemDurability injects
crashes at every append and fsync boundary" overstates what is actually tested.

**Change.**

- `blossom-store` writes `FileDurability` (`FileWal`, `FileCheckpoints`, `MetaStore`) against a `Vfs` trait. `RealFs`
  uses `std::fs`. `SimFs` is a deterministic in-memory filesystem with the POSIX crash model:
  - Every write is volatile until `sync_data`/`sync_all` on the file (and `sync_dir` for creates and renames).
  - At a crash, each unsynced write independently survives, is lost, or is torn at sector (512 B) granularity. The
    choice is drawn from a PRF stream keyed `("fs", node, file, offset)`.
  - Renames and creates are lost unless the directory was synced.
  - Injectable failures: `EIO` on `sync` (after which the unsynced pages are gone, as on Linux), `ENOSPC` on append,
    and short writes.
- `SimNode` uses `FileDurability<SimFs>`. `MemDurability` stays only as a fast option for LDFI and BMC, where
  durability is not under test.
- The committer becomes a simulated actor. Add `SchedDecision::SyncComplete { node, upto }` and
  `SchedDecision::SyncFail { node }`, so the scheduler decides when fsyncs finish relative to ticks, crashes and
  deliveries. The swarm profile includes fsync latency and failure rates.
- Checks, in addition to TEST-103:
  - after every simulated crash, the recovered durable state equals an uninterrupted run up to the last **released**
    tick, or a later synced tick;
  - no released message depends on a tick that was lost;
  - recovery never refuses a crash image that `SimFs` produced without an injected media fault;
  - recovery always refuses an image with a media fault injected into synced data.
- `xtask crashcheck` enumerates every durable syscall of a scripted run and crashes at each one. This is ALICE-style
  exhaustive crash-point testing over `SimFs`.

Edit: §5.6, §6.1–6.3, §11.1, §11.8, §14.2 (M3 exit criteria), §15.

### MF-7. One message can halt a server: ARCH-20 needs poison isolation and resource admission

ARCH-20 maps every tick error to a crash, with `Halt` as the default (§5.1). Several hard errors are directly
triggered by input that a peer or an external client controls:

- BLSR001, when two clients supply tuples that share a key;
- BLSR004, arithmetic on client-supplied integers;
- BLSR005, duplicate map keys;
- BLSR010, a user `error(…)` on a bad input;
- BLSR009;
- the interner hard cap (§4.1), which any client can reach by sending distinct strings, since reclamation is P1.

Under `Halt`, one well-typed message halts a server permanently. Under `Restart { backoff }`, a level-triggered or
retrying sender redelivers the message after recovery, and the node crash-loops. Each iteration also writes a new
`quarantine/<tick>.blsq`, with no bound on disk use. ARCH-20's own argument, that crashes are inside the fault model,
is correct but incomplete: it proves the *program's* guarantees survive, not the *service's* availability.

**Change.**

1. **Poison isolation, as an omission.** Rejection is an omission (SEM-090), and batch composition is a scheduling
   choice (CR-02), so the runtime may legally drop a single poisonous input.
   - After a tick fault whose batch held ingress rows, the node restarts from durable state in **probation**: for the
     next K ingress rows, or until a time window ends, each ingress row gets a tick of its own.
   - A singleton tick that faults identifies its row. The row's `(channel, principal, tuple fingerprint)` goes to a
     durable, bounded, expiring deny-list in `META`.
   - Admission rejects matches with a new DIST-063 reason, `poison`. The rejection is counted, audited, and visible
     to LDFI as an omission (TEST-105).
   - Faults with no ingress rows in the batch (timers, bootstrap, program bugs) skip probation. They go straight to a
     circuit breaker.
2. **Circuit breaker.** `NodePolicy { on_tick_error: Restart { backoff, max_restarts: 5 per 10 min } }`. When the
   breaker trips, the node Halts. Quarantine files are deduplicated by batch digest and capped in bytes (oldest
   deleted first, with a counter).
3. **Defaults by context:** `Restart` with probation for `blossom run` and the runtime; `Halt` for the simulator, tests
   and `ManualDriver`. `Halt` stays the semantic model; the policy only decides when to invoke it.
4. **Resource errors are admission decisions, not tick faults.** Interned bytes and rows added per principal per
   window are metered in `Quotas` at admission, from the decoded frame's size. Once the budget is exhausted the reason
   is `rate_limit`, so the node never reaches the global hard cap from ingress. The global cap stays for
   program-internal growth.
5. **Process-level behaviour.**
   - The `blossom` binary builds with `panic = "abort"`, so a panic is crash-stop and the supervisor restarts the
     process into the recovery path.
   - Embedders get `catch_unwind` at the node-task boundary, which maps a panic to `Faulted(Internal)`. Never let a
     panicked tokio task disappear silently (`JoinError::is_panic`).
   - A halted node in a single-node process exits with the runtime-fault code (S-19).

Edit: ARCH-20, §5.1, §5.8 (reasons, `Quotas`), §4.1 (cap), §12.3 (metrics), §12.4 (policy config).

### MF-8. Replay and oracle completeness: trace events, host functions and the quarantine claim

**(a) Trace events.** `NodeEvent` (§5.1) has these variants: `Deliver`, `Host`, `Timer`, `Service`, `Session` and
`Directory`. `TickInput` (§4.7) also carries `cluster_version` and service results. `TraceEvent` (§6.4) has no events
for service results, session open/close, directory updates or `ClusterVersion` samples. "Minimal holds inputs and
decisions, which is enough to replay" is therefore false for any program with a `service`, sessions, dynamic
membership or `cluster_version` (LANG-264, which the IR marks "sampled per tick, recorded").

**(b) Host functions.** LANGUAGE §16.2's own example `extern table fn lines(path) = "blossom_std::io::lines"` reads
files, so its output depends on the world. It is neither recorded nor declared as an input. `extern fn`s are
"declared pure" (LANG-181), but nothing checks that. The oracle (§11.2) has no way to call host functions at all:
`Oracle::new(program, strat, deploy)` takes no registry, and `HostFns` lives in the engine, which the oracle may not
depend on. Any corpus program that calls `std`'s extern fns cannot run on the oracle backend.

**(c) Quarantine.** §5.1 promises that `quarantine/<tick>.blsq` is "a replayable trace fragment" holding the batch,
the seeds and the error. A tick cannot be replayed without the engine state it started from. That state includes
non-durable relations, which exist only in the memory of the faulted process. The claim is false as stated. The node
is also sans-IO, so it cannot "write" the file.

**Change.**

- Add `TraceEvent` variants for `ServiceResult { node, service, call, tuple }`,
  `Session { node, session, principal, open }`, `Directory { node, update }` and
  `ClusterVersion { node, tick, version }`. Rule: every `NodeEvent` and every `TickInput` field that is not
  recomputable has a `TraceEvent`, and a unit test enumerates both enums to enforce it.
- Split `HostFns` (see S-20):
  - `ExternRegistry` is Value-level `ExternFn` implementations defined in `blossom-core`, so the oracle, the
    interpreter (through a word adapter), codegen and `blossom-std-host` all share them. `Oracle::new` takes
    `&ExternRegistry`.
  - `HostServices` (services and output handlers) belongs to node and runtime.
- `extern table fn` is pure by default. A world-reading table function must be declared as an input source
  (`#[source]`). Its rows then enter through `HostInput`, are recorded, and are replayable.
- In simulation and `--paranoid` builds, a memo miss calls each `extern fn` twice and compares the results. A
  mismatch is `BLSR010` naming the function: purity is checked, not assumed.
- Quarantine becomes an effect: `NodeFault { quarantine: QuarantineRecord, .. }`, which the driver writes with mode
  0600. The record holds the batch, seeds, error, pre-tick state digest, and dumps of the relations the error names.
  Exact replay is available when the operator enables `record = "minimal"`: an on-disk input log since the
  incarnation began, rotated with a bound. The doc states that without it, a quarantine file is a report, not a
  reproducer.

Edit: §4.7, §5.1, §6.4, §11.2, §13 (LANGUAGE §16.2 cross-reference), TEST-010 mapping.

### MF-9. "Never skipped" contradicts "each milestone ends green"

§11.4 says: "A case that exercises an unimplemented feature fails with the `Unimplemented { feature }` error. It is
never marked skipped." The `test` CI job runs the corpus (§11.9), and "each milestone ends green on CI" (§14.2). Until
M8, most of the corpus exercises P1 features, so CI is red at every milestone. Agents will respond by deleting cases
or adding ad-hoc `#[ignore]`s, which is exactly the silent skip the user forbade.

**Change.** Add a strict status ratchet to the corpus manifest:

```toml
status = "unimplemented"            # "pass" (default) | "unimplemented" | "known-failure"
unimplemented = ["ENG-084"]         # the exact FeatureIds the run must fail with
until = "M8"                        # the milestone that must flip it to "pass"
# known-failure additionally requires: issue = "…", and is rejected on main after `until`
```

The runner's rules:

- `pass` must pass.
- `unimplemented` must fail with `Unimplemented` whose feature is in the list. Any other error, and any *success*,
  fails the test ("stale status: update manifest"), so the ratchet only tightens.
- A case whose `until` milestone is reached fails regardless of its status.
- `xtask corpus --status` reports per FEATURES id and feeds PLAN.md.

Use the same mechanism for the Molly parity table and the systems suites. CI stays green, nothing is hidden, and every
gap is named by its feature id.

Edit: §11.4, §11.5, §11.9, §14.2.

---

## Should-fix

### Durability and runtime

**S-1. Tick and `now` should be monotone across incarnations.** Recovery boots at "`last_tick + 1`" (§5.6), where
`last_tick` is the last WAL record. Ticks with no durable delta leave no record, so after a crash their numbers are
reused, even though they released messages. The same `(node, send_tick)` then names two different messages (SEM-073
keys omissions this way), and `(self, $tick)` stops being unique, which programs will use as an id. `SystemClock`
re-anchors on the wall clock at every start (§5.5), so `now` can also go backwards across incarnations and upset soft
state and `LMax<Instant>` births.

*Change:* reserve ticks in `META`. Before using tick t > reserved, write `reserved = t + 65536` (one fsync per 65k
ticks), and boot at `reserved + 1`. Record `now` in every WAL record and in `META`, and boot with
`now = max(wall, last_now + 1ns)`.

**S-2. Checkpoint discipline.**

- Snapshot only a tick at or before the synced frontier (MF-3's `SyncedTick`), and label the checkpoint with that
  tick's LSN.
- `DurableSnapshot` shares only row chunks, lattice objects and **interner chunks**. It never shares indexes, since
  §4.2 says forked flat hash tables are copied on first write, which would make every checkpoint cost an O(state) copy
  on the next tick.
- Specify that interner arenas are `Arc`-chunked, so a background encoder can read a frozen prefix.
- P1 interner compaction (§4.1) must wait until no checkpoint, fork or snapshot pins the old id space. Otherwise the
  background encoder or an LDFI fork decodes dangling ids.

**S-3. Graceful shutdown.** Specify how SIGTERM and `stop()` proceed:

1. stop admitting new ingress, finish the current tick, and stop scheduling ticks;
2. wait for the committer to sync every parked tick, then release those ticks;
3. flush the transport with a deadline, and close connections with a GOAWAY frame type;
4. optionally take a final checkpoint, and write `clean_shutdown` to `META`;
5. exit 0.

A second SIGTERM, or the deadline expiring, exits immediately. That is crash semantics, so it is legal.

**S-4. Output commit covers every externally visible effect.** `Released` (§5.1) has frames, egress, callbacks,
service calls and stdout, but no subscription deltas, while `TickOutput` has them. Put subscription deltas in
`Released`. Also state that `sync_do`'s result, output-handler calls and subscription deliveries are post-commit. A
host must never observe state that a crash can take back.

**S-5. Ticks must not run on tokio worker threads.** The node task (§5.2) runs up to `MAX_INFLIGHT_TICKS` (64)
synchronous `run_tick`s inside a `select!` loop. A long fixpoint then blocks a worker thread and starves transport
I/O, timers and the other nodes in the process. Give each node its own OS thread (engines are single-threaded
anyway), and keep I/O in tokio, connected by channels. Add a `tick_duration_seconds` alert threshold and a
`long_tick` warning that names the slowest stratum.

**S-6. Detect duplicate node instances.** Two live processes holding the same certificate (a restored VM, or a
fat-fingered deployment) both pass identity binding. Peers should remember the highest `(restarts, boot_nonce)` seen
per node. A HELLO from an older incarnation is rejected (`reason = stale_incarnation`). A second concurrent
connection claiming the same node with a different nonce raises `duplicate_node` in the audit log and a metric.

**S-7. Store tooling for operators.** "Refuse to start and never skip it" (§5.6) is correct, but the operator then
needs tools:

- `blossom store inspect` (headers, catalog, LSN ranges, incarnations);
- `blossom store verify` (all CRCs and checksums, offline);
- `blossom store dump --rel`;
- `blossom store backup`/`restore`, which snapshot through a checkpoint;
- `blossom store truncate --at-lsn N --accept-data-loss`, which is loud, audited, and writes a marker into `META`.

### Security

**S-8. Verify the peer's identity on outbound connections too.** §5.8 binds identity when a connection is
*accepted* (a HELLO claiming N must come from `principal_of(N)`). It says nothing about the dialer checking the
listener. rustls's default server verifier checks a DNS name or IP SAN, which SPIFFE SVIDs usually lack. The
predictable result is a connection failure, followed by someone adding a `dangerous()` verifier. Specify a custom
`ServerCertVerifier` that builds the chain to the peer CA with webpki and then requires the single URI SAN to equal
`principal_of(N)` for the node being dialed. The client verifier requires a URI SAN too. For QUIC (P1), disable 0-RTT,
because replayed early data would be a duplicated message, and duplication is outside the delivery model (§0.2).

**S-9. The seed is a secret.** DIST-033 stores the deployment seed "in config", and §5.9 puts it in `deploy.toml`,
which will be committed. The seed makes every `$rand`, `choose` and priority outcome predictable, which is a real
concern for randomized leader choice or load balancing. It also derives the hash-table key that §4.1 calls
"unpredictable to remote clients".

*Change:*

- Keep the seed in a separate secrets file (mode 0600, rejected if group- or world-readable) or an env var, and never
  in the spec.
- Derive the hash-table key from the per-incarnation boot nonce (OS entropy), not the seed. Hash order is never
  observable (ARCH-05), so determinism does not need a fixed key, and the nonce is recorded for replay anyway.

**S-10. Decoder resource limits.** §5.4 specifies `max_frame`, but not:

- a nesting-depth limit for `nested`, `variant` and lattice payloads (recursion);
- that `count` must be checked against the remaining bytes before any allocation;
- a limit on tuples per batch;
- per-frame and per-principal interner growth (MF-7).

Put these in `WireLimits`, apply them identically in the generic and generated codecs, and fuzz with them.

**S-11. The client listener at P0.** §5.8 says the client listener takes "a client certificate or a token". Tokens are
P2 (DIST-067), and sessions are P1. At P0, specify client certificates only. A `token` configuration key fails
validation with `Unimplemented { feature: "DIST-067" }`. It is never accepted and then ignored.

**S-12. Ops surface hygiene.**

- The Prometheus exporter serves plaintext, unauthenticated HTTP, on all interfaces unless configured. Bind it to
  loopback by default, and offer mTLS on an ops listener.
- The audit log is a `tracing` target (§12.2). `RUST_LOG=warn` silently discards it, so give it a dedicated sink that
  log filtering does not affect.
- `.blstrace`, `.blsq` and firing logs hold payloads: write them with mode 0600, and say so in the docs.
- Runtime error reports (BLSR001's tuples) and logs should redact payload columns under a `redact` config setting,
  consistent with DIST-069.

**S-13. Choose the rustls crypto provider now.** rustls needs an explicit provider (`aws-lc-rs` or `ring`). Check its
license expression against the `deny.toml` allowlist before M5. `aws-lc-sys` has historically carried an `OpenSSL`
license term that the allowlist does not contain. Record the choice in DEPENDENCIES.md.

### Error handling and the stub policy

**S-14. Report `Unimplemented` at compile time and load time, not mid-tick.** Today a P1 native or regime that is not
built fails inside `run_tick`, perhaps hours into production, and crash-stops the node. Instead:

- The planner refuses to emit a plan that needs an unbuilt feature.
- `Engine::new` checks the plan against an executor capability table, and `Builder`/`blossom check` report it.
- Give it a user-facing diagnostic: "BLS0908: not implemented in this build: ENG-084 (leapfrog join), needed by rule
  raft::…". Add it to LANGUAGE §20.
- The runtime `Unimplemented` stays as the backstop.

**S-15. The `panic-on-bug` switch cannot work as specified.** §12.1 says the feature is "on in the test profile", but
Cargo features cannot be enabled per profile. Use `cfg(debug_assertions)` or a runtime `BLOSSOM_PANIC_ON_BUG=1`.
Also:

- Set `overflow-checks = true` in the release profile. Otherwise internal arithmetic (epochs, counters, weights before
  the checked path) wraps in release and panics in debug, and the two builds diverge.
- Add `clippy::unreachable`, `clippy::panic_in_result_fn` and `clippy::string_slice` to the denied set. `assert!` in
  library code should be `bug!`.

**S-16. Size of `TickError`.** It carries `Row`s and `Derivation`s inline, and `clippy::result_large_err` will fail
the `-D warnings` job. Use `TickError(Box<TickErrorKind>)`, and do the same for `NodeFault` and `IrError`. Row values
in errors go through the redaction policy (S-12).

**S-17. `ValueStore` signatures versus the hard cap.** §4.1 promises that the interner cap "aborts with a clear
error". But `intern_record` returns a bare `Word`, and `to_value`/`resolve_str` are infallible, even though a word of
the wrong type is possible after a bad plan. Make them `Result<_, ValueError>`, or have them take typed handles
(`StrWord`).

**S-18. Keep the node sans-IO.** Quarantine writing (MF-8) and quota refill (`&mut Quotas` inside the "pure" `admit`)
must take `now` as an argument and return effects. Add a CI grep: `blossom-node` must not import `std::fs`,
`std::net`, `std::time::Instant` or `std::thread`.

**S-19. Exit codes.** The draft has 1 = user error, 2 = verification failure, 3 = internal. clap exits with **2** on a
usage error, which collides with "verification failure". Proposed table:

| Code | Meaning |
|---|---|
| 0 | ok |
| 1 | program or user error (diagnostics) |
| 2 | clap usage error |
| 3 | verification failed |
| 4 | internal error (bug) |
| 5 | refused to start: storage identity, corruption, config (supervisors should *not* restart) |
| 6 | runtime fault or halted node (supervisors *should* restart) |
| 7 | unimplemented feature |

Document the table in `blossom --help`.

### Trait seams

**S-20. Split `HostFns`.** §4.7 puts `extern fn`s, table functions, services and output handlers in one registry that
the engine holds. Services are async and handlers run after commit (step 6). Neither belongs in the tick. The split:

- **`ExternRegistry`** (pure, Value-level, in `blossom-core`; MF-8): used by the engine, the oracle and codegen.
- **`HostServices`** (services, handlers, subscriptions; in `blossom-node`): used by the node and the runtime.

Load-time binding checks stay as they are in both.

**S-21. Small seam fixes.**

- `Transport::shutdown(&self) -> impl Future` makes the trait non-object-safe, but the runtime picks TCP, QUIC or
  plain TCP from config. Return `Pin<Box<dyn Future + Send>>`.
- Add an `Entropy` trait for the boot nonce and backoff jitter, beside `Clock`, so the simulator controls both.
- Name the home of the metrics sink trait (node) and of `Clock` (node).

**S-22. Validation carried in the types.** `Program` has only `pub` fields. "Immutable once built" (§2) and "every
rewrite re-validates" are conventions, not guarantees.

- Introduce `ValidatedProgram` (a private field wrapping `Arc<Program>`). Only `IrBuilder::finish` and
  `validate(Program)` construct it, and the planner, oracle and engine accept only it.
- `PhysicalProgram` decoded from bytes (§2.10, §5.1) goes through `PhysicalProgram::validate` (every id in bounds,
  widths consistent, `kernel_api`), so corrupt or stale bytes cannot turn into an index panic in the engine.
- Every postcard artifact (IR, plan, certificates, traces, caches) starts with a magic number and a format version.
  Postcard is not self-describing, so a version skew otherwise decodes as garbage, not as an error.

**S-23. `Engine::fork` needs an executor.** `Engine::fork(snap)` returns an `Engine`, whose `exec` is a
`Box<dyn PlanExecutor>` with `&mut self` state (shadow trees with resolved handles). Add
`PlanExecutor::rebind(&self, store: &Store) -> Box<dyn PlanExecutor>`, or keep a `make` factory in the snapshot.

**S-24. Split `blossom-core`.** Everything depends on it, and it holds unrelated concerns: indices, symbols,
`SourceDb` and diagnostics (frontend); the type table, the Value model, canonical order, `Word`/`ValueStore`,
encodings, fingerprints and the PRF (evaluation); BLAKE3 digests; and graph algorithms. Any edit rebuilds the world,
and three tracks (MF-2) would edit it at once. Split it into:

- `blossom-base`: idx, `Symbol`, `Span`/`SourceDb`, `Diagnostic`, errors, `FeatureId`, graph algorithms;
- `blossom-value`: types, `Value`, canonical order, encodings, fingerprint, PRF, digests, `ValueStore`, `ExternFn`.

Freeze both at M0.

### Testing

**S-25. Make the oracle independent of the stratifier.** `Oracle::new` takes `Arc<Stratification>` from
`blossom-analysis` (§11.2), the same one the planner uses. A stratification bug, such as an order that reads a
relation before its last writer, is then shared by the oracle and the engine and never detected. The oracle should
compute its own strata with a deliberately naive algorithm: repeatedly pick every relation whose negative
dependencies are all complete. A test asserts that the result is a valid linearization of `blossom-analysis`'s
strata.

**S-26. Enforce determinism, not just review it.** ARCH-19 bans `std::collections::HashMap` and tells people to "use
`hashbrown` with our deterministic hasher". But `hashbrown::HashMap::new()` uses `DefaultHashBuilder`, which in
current hashbrown is a randomly seeded foldhash. The ban is bypassed by the recommended crate.

*Change:*

- Disallow `hashbrown::{HashMap, HashSet}`, `hashbrown::DefaultHashBuilder`, `foldhash::*::RandomState` and
  `std::collections::hash_map::RandomState` as types.
- Provide `blossom_value::{DetMap, DetSet}` aliases, whose hasher is keyed from the boot nonce or a fixed key.
  `hashbrown::HashTable` with an explicit hash stays allowed.
- Add a CI check that runs the same simulation seed in **two separate processes** (different ASLR) and on both
  platforms, and compares every `TickEnd` digest. Address-dependent hashing leaks show up only this way.

**S-27. Pin down IR digest canonicalization.** "Rules are sorted by label, relations by name" (§2.10). The digest
must be invariant under renumbering every `IndexVec`. That includes `TypeId`s from a hash-consed table, whose order
depends on insertion order, and `ConstId`, `VarId` and `RuleId` references inside constructs. Specify the canonical
relabeling algorithm. Add a validator rule V9 (rule labels are unique) and a property test: a random permutation of
every id space leaves the digest unchanged.

**S-28. Benchmark gating that will not flap.** A 5% wall-clock gate (§4.13) on shared CI machines will fail at
random, and flapping gates get disabled. Gate on instruction and allocation counts (cachegrind/`iai-callgrind` on
Linux, and the counting allocator). Report wall time and percentiles for sign-off on the dedicated machine.

**S-29. CI for feature combinations and time budgets.**

- Run `cargo hack --each-feature` on crates with features (`sat-cadical`, `sat-batsat`, `quic`, `interp`,
  `parallel`, `fixtures`, `arbitrary`), since feature-gated code rots.
- Use `cargo nextest` with per-test timeouts, so a hung simulation fails instead of stalling CI.
- Tier the jobs: the PR tier must finish in 20 minutes or less, and full LDFI/BMC system suites move to the merge
  queue or nightly. Running them on every PR (§11.9) will not scale past M7.

**S-30. External solvers: no silent skips.** For tests that need z3, cvc5 or clingo:

- In CI (`BLOSSOM_REQUIRE_SOLVERS=1`), a missing binary is a hard failure.
- Locally, the test is reported as "not run: z3 not found (set BLOSSOM_Z3)" through libtest-mimic's ignore reason.
  It is never a pass.
- Find binaries through `$BLOSSOM_Z3`, then `PATH`, not the hard-coded `/opt/homebrew/bin/z3` (§9.3), which breaks
  on Linux CI.

**S-31. Golden fixtures beyond storage.** TEST-106 covers checkpoints and WAL. The wire codec and the trace format
are also custom and versioned, so commit byte-exact golden encodings of a canonical tuple set per released version,
plus one `.blstrace` per version, and require every new binary to decode them, or to refuse them with the documented
error.

### Observability and operability

**S-32. Node health at P0.** ARCH-20 says faults are "surfaced by the admin API", but the admin plane is P1 (§5.8).
At P0, provide:

- a `node_state{state=running|probation|halted|faulted}` gauge;
- `/healthz` (process alive) and `/readyz` (recovered, commit pipeline healthy, not halted) on the ops listener
  (S-12);
- `blossom node status` reading from that listener.

**S-33. Metrics the table lacks.**

- recovery: `recovery_duration_seconds`, `wal_replay_records`;
- storage: `wal_bytes`, `wal_segments`, `durability_failures_total{kind}`;
- faults: `quarantine_files`, `quarantine_bytes`, `restarts_total`, `incarnation`;
- progress: `last_released_tick`, `last_synced_tick`;
- load and transport: `ingress_batch_rows`, `tick_rate`, `peer_connected{peer}`, `handshake_duration_seconds`;
- capacity: headroom against the interner cap and quotas;
- `stratum_duration_seconds{stratum}` under `--stratum-timing`.

**S-34. Metric cardinality rules.** The `relation` label should cover only user-origin relations; the generated
`$`-relations can run to hundreds. `rule` labels stay opt-in. Cap label sets and document the caps.

**S-35. Name collision: `blossom explain`.** It means provenance `explain`/`whynot` in §1.2's CLI list, and
diagnostic-code explanation in LANGUAGE §20. Rename one of them: `blossom why`/`whynot` for provenance, and
`blossom explain BLSnnnn` for codes.

**S-36. One CLI inventory.** Subcommands are scattered through the draft, and several are missing from the §1.2 list:
`self-check`, `sim replay`, `deploy init`, `deploy local`, `config explain`, `upgrade`, and (from this critique)
`store *` and `node init`/`status`. Give one table with the subcommand, its crate, P level, milestone and exit codes.
`blossom --version` prints the compiler version, the ABI version, `ENCODING_VERSION`, `PRF_VERSION`, the storage
format and the trace format.

**S-37. Configuration.**

- Give the configuration files a schema version (`format = 1`).
- Keep secrets (seed, keys) separate from the spec (S-9).
- `blossom deploy init` writes per-node directories and identities (MF-5).
- List every `BLOSSOM_*` variable in one table.
- `NodeConfig` exposes `max_batch_frames`/`max_batch_bytes`, `MAX_INFLIGHT_TICKS` (also bounded in bytes), the
  probation and breaker parameters, and quarantine caps. Each has a documented default.

---

## Consider

- **C-1.** On Apple targets, std's `File::sync_data`/`sync_all` already issue `F_FULLFSYNC` (check against the pinned
  toolchain's `sys/.../fs.rs`). If so, `blossom-store` needs no `rustix` and no `unsafe` for syncing. Avoid `memmap2`
  for recovery reads, since a truncated file raises SIGBUS in safe code; use `pread` through the `Vfs`.
- **C-2.** Interpreter width specializations for 1..=16, times `Deaths` and prov variants, multiply compile time.
  Start with a measured set (for example 1–4, 6, 8, and the slice fallback), and add widths only when benchmarks show
  a gain.
- **C-3.** `build.rs` runs the whole compiler as a build dependency, which Cargo compiles unoptimized by default. Set
  `[profile.*.build-override] opt-level = 2` for `blossom-*` crates, or large programs will take minutes to codegen.
  Resolver 2 also builds shared crates twice (once for build deps, once for normal deps); keep their feature sets
  identical to limit that.
- **C-4.** Use one integration-test binary per crate (`tests/it/main.rs`) to cut link time, because 30 crates times
  several test binaries times a heavy dependency graph adds up.
- **C-5.** Run nightly mutation testing (`cargo-mutants`) on kernel, engine natives, wire and store. It shows whether
  the differential suites actually catch seeded bugs. The corpus size alone does not.
- **C-6.** Test `TcpTlsTransport` against an in-process fault proxy (delay, reset, half-open, slow reader), so the
  real reconnect, backoff and queue-drop code is exercised. The simulator bypasses it by design.
- **C-7.** No distributed tracing is needed. Put `(node, incarnation, tick)` on every log span, and
  `(from, send_tick)` on delivery events; that correlates logs across nodes for free.
- **C-8.** The `.ded` frontend does not pre-check everything the validator checks. Let each frontend declare whether
  `IrError`s from `finish` are frontend bugs (internal) or user errors, which are then rendered as diagnostics.
- **C-9.** Preallocate WAL segments (`fallocate`) so appends do not change the file size and `fdatasync` stays cheap
  on Linux. MF-4's per-record LSN makes preallocated zeros safe.
- **C-10.** Release engineering: `cargo auditable` builds, an SBOM, reproducible-build checks in `xtask release`, and
  signed artifacts.
- **C-11.** At startup, refuse private-key files that are group- or world-readable.
- **C-12.** Generate the §1.2 table and the §1.3 graph from `xtask/layers.toml` (MF-1).
- **C-13.** A program that stages a change every tick (a counter) spins a core at 100%. Offer `NodeConfig::min_tick_interval`
  (default 0: no pacing), which is purely a scheduling choice, and expose `tick_rate`.
- **C-14.** Run the zero-allocation test (§11.7) with the production `tracing` subscriber configuration. Per-tick
  spans at `INFO` or `DEBUG` allocate if a subscriber records them.
- **C-15.** Replace `Location::caller()` in `unimplemented_feature!` with `file!()`/`line!()`/`column!()`. The meaning
  is the same, and it does not depend on `#[track_caller]` subtleties inside macro expansions.

---

## Appendix: proposed ownership of the types the draft leaves homeless

| Type(s) | Proposed crate::module | Why there |
|---|---|---|
| `Word`, `Value`, `ValueStore`, `ExternFn`, `ExternRegistry`, `DetMap` | `blossom-value` (split from core, S-24) | Needed by the oracle, engine, wire and std-host |
| `ChoiceEntry`, `ViolationRecord`, `TickDigests` | `blossom-ir::obs` | Produced by both the engine and the oracle, which must not share engine code |
| `SchedDecision`, `MsgId`, `TickTrigger`, `DropReason`, `RejectReason`, `FaultSchedule`, `NodeDesc` | `blossom-trace` | Trace events name them. Sim, node, ldfi and verify depend on trace. |
| `SchemaCatalog`, `AclTable` | `blossom-schema` | Produced by the compiler, consumed by node and wire without analysis |
| `CompileOutput`, `SpecPlan` | `blossom-ir` (+ schema) | The driver's output; no node types |
| `CompiledProgram`, `CompiledRole`, `GeneratedProgram`, `Evaluator` | `blossom-node` | Built from `CompileOutput` plus an `ExecutorKind` |
| `FiringLog`, Tier C record types | `blossom-kernel::prov` | Next to `ProvenanceSink`; read by `blossom-prov` |
| `abi::*` (`ExecCtx`, handles, entry points) | `blossom-engine::abi` | The only surface generated code may use (MF-2) |
| `TickInput`, `TickOutput`, `DurableDelta`, `Outbox`, `DurableImage` | `blossom-engine` | Engine I/O. The node encodes the outbox and durable delta to bytes **inside** `run_tick` handling, so no `Word` outlives the tick (interner compaction, S-2). |
| `WalRecord`, `Recovered`, `DurableSnapshot`, `Vfs`, `WalWriter`, `CheckpointWriter`, `MetaStore`, `StoreIdentity` | `blossom-store` | Durability (MF-3, MF-5, MF-6) |
| `Transport`, `IngressSink`, `AdmittedBatch`, `ConnInfo`, `Quotas`, `Clock`, `Entropy`, `HostServices`, `QuarantineRecord` | `blossom-node` | Sans-IO interfaces the drivers implement |
| `OutFrame`, `WireLimits`, `abi::*` for generated codecs | `blossom-wire` | Codec |
| `Builder`, the regeneration cache | `blossom-build` (new, L5) | Needs both the driver and codegen (MF-1c) |
| `STD_SOURCES` / host fns | `blossom-std-src` (L1) / `blossom-std-host` (L3) | MF-1b |
