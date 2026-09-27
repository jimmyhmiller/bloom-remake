# 07 — BOOM Analytics (Hadoop/HDFS in Overlog), and Consensus (Paxos / Raft) in Declarative Languages

Research report for the bloom-remake implementers. Cluster: large systems built in BOOM-family languages
(Overlog / Dedalus / Bloom / Hydro), with a deep dive into how Paxos and Raft have been (and should be)
expressed in a Dedalus-style model.

Everything marked **VERBATIM** is copied from a primary source (paper or source code) and cited. Everything
marked **SKETCH (ours)** is our own design proposal and is *not* from the literature. When a source could
not be accessed, this report says so explicitly.

---

## 0. Sources actually read (and what could not be read)

Read in full (PDF text extracted locally):

| Source | Where |
|---|---|
| Alvaro, Condie, Conway, Elmeleegy, Hellerstein, Sears. *BOOM Analytics: Exploring Data-Centric, Declarative Programming for the Cloud.* EuroSys 2010 (14 pp.) | https://www.neilconway.org/docs/booma_eurosys2010.pdf (the copy at https://dsf.berkeley.edu/papers/eurosys10-boom.pdf downloaded as a truncated 6-page file) |
| Same authors. *BOOM: Data-Centric Programming in the Datacenter.* Tech. Report UCB/EECS-2009-113 (earlier, longer on some points: LATE rules, failover numbers, Chukwa-like logging) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2009/EECS-2009-113.pdf |
| Alvaro, Condie, Conway, Hellerstein, Sears. *I Do Declare: Consensus in a Logic Language.* NetDB 2009 / SIGOPS OSR 43(4) | https://dsf.berkeley.edu/papers/netdb09-idodeclare.pdf |
| **The original Overlog Paxos source code** (`prepare.olg`, `propose.olg`, `election.olg`, tests, and `jol.jar` containing JOL's own Overlog system programs). The Bitbucket repo is gone; recovered from the Internet Archive tarball of `bitbucket.org/neilconway/overlog-paxos` (commit `7ba2c54468af`), linked from http://db.cs.berkeley.edu/netdb-09/ | `https://web.archive.org/web/20200621141401id_/https://bitbucket.org/neilconway/overlog-paxos/get/tip.tar.bz2` |
| Chu, Panchapakesan, Laddad, Katahanas, Liu, Shivakumar, Crooks, Hellerstein, Howard. *Optimizing Distributed Protocols with Query Rewrites.* SIGMOD 2024 (PACMMOD 2(1)); tech report incl. appendices | https://arxiv.org/pdf/2404.01593 |
| autocomp repo: the Dedalus (Hydroflow `datalog!`) MultiPaxos, CompPaxos, ScalablePaxos, 2PC, voting, PBFT programs used in that paper | https://github.com/rithvikp/autocomp |
| bud-sandbox: BFS (Bloom port of BOOM-FS), Paxos WIP, voting, heartbeat, KVS, ordering libraries | https://github.com/bloom-lang/bud-sandbox |
| Molly examples (Dedalus): `raft/*.ded`, `paxos_synod.ded`, `commit/2pc.ded`, `util/timeout_svc.ded`, … | https://github.com/palvaro/molly (src/test/resources/examples_ft) |
| Hydro repo (current HEAD `9e2a120`, 2026-09-24): `hydro_test/src/cluster/{raft,paxos,compartmentalized_paxos,paxos_with_client,two_pc,map_reduce}.rs`, `hydro_std/src/quorum.rs`, `kv_replica` | https://github.com/hydro-project/hydro |
| Two Bud (Bloom) Raft student implementations (CS194, Spring 2013, advised by Ongaro) | https://github.com/noeleo/raft , https://github.com/amidvidy/whitewater |
| Ongaro. *Consensus: Bridging Theory and Practice.* PhD dissertation, Stanford 2014 | https://github.com/ongardie/dissertation (stanford.pdf) |
| Kirsch & Amir. *Paxos for System Builders.* JHU CNDS-2008-2 (the leader-election algorithm BOOM used) | Wayback copy of http://www.cnds.jhu.edu/pub/papers/cnds-2008-2.pdf |
| van Renesse. *Paxos Made Moderately Complex* (cornell.edu version; older than the 2015 ACM CSUR version) | https://www.cs.cornell.edu/home/rvr/Paxos/paxos.pdf |
| Chandra, Griesemer, Redstone. *Paxos Made Live.* PODC 2007 | https://www.cs.utexas.edu/users/lorenzo/corsi/cs380d/papers/paper2-1.pdf |
| Zaharia et al. *Improving MapReduce Performance in Heterogeneous Environments* (LATE). OSDI 2008 | https://www.usenix.org/legacy/event/osdi08/tech/full_papers/zaharia/zaharia.pdf |
| hydro-optimize (automatic decoupling/partitioning with an ILP solver) README + file list | https://github.com/hydro-project/hydro-optimize |

Could **not** access in full:

* *Bigger, not Badder: Safely Scaling BFT Protocols* (Chu, Liu, Crooks, Hellerstein, Howard; PaPoC 2024) — only the abstract (ACM DL https://dl.acm.org/doi/10.1145/3642976.3653033). Its code lives in the autocomp repo (`autopbft_critical_path`, `pbft_critical_path`), which I saw listed but did not analyze.
* The BOOM Analytics source (http://db.cs.berkeley.edu/eurosys-2010) — not archived. So BOOM-FS / BOOM-MR Overlog code is known **only** through the rules printed in the paper/TR.
* Szekely & Torres, *A Paxon evaluation of P2* (klinewoods.com) — not accessed.
* The Ongaro raft-dev post "bug in single-server membership changes" (2015) — seen only through search-result summaries (https://groups.google.com/g/raft-dev/c/t4xj6dJTP6E). The fix is stated below as reported there.
* The ACM CSUR 2015 version of *Paxos Made Moderately Complex* (with `slot_in`/`slot_out`/`WINDOW`) — the WINDOW mechanism is described below from secondary summaries (paxos.systems) and flagged as such.

---

## 1. Executive summary for implementers

1. **BOOM Analytics proved the model scales to real systems**: HDFS-compatible BOOM-FS (469 lines of Overlog + 1,431 Java vs ~21,700 Java in HDFS), Hadoop-compatible BOOM-MR scheduler (55 rules / 396 lines of Overlog + 1,269 Java replacing ~6,573 lines of Hadoop), a Paxos-replicated hot-standby NameNode (~50 rules / ~400 lines), a hash-partitioned NameNode (8 developer-hours), and metaprogrammed tracing/coverage. Performance matched Hadoop 18.1 on a 101-node EC2 cluster.
2. **The runtime features BOOM actually leaned on** (and which we must have): per-table durability with an atomic durable commit at the end of each timestep (Stasis); transient "scratch" tables; primary keys with replace-on-key-conflict ("update"); `delete` rules; stratified negation (`notin`); aggregates including `count/min/max/sum/avg/set/percentile<p,X>`; physical *and* logical timers / `periodic`; `@` location specifiers for one-hop messaging; host-language objects in tuples, host-language method calls, UDF aggregates, table functions, and event listeners on inserts/deletes; a `die` relation that turns invariant violations into exceptions; and **programs represented as data in catalog tables** so rewrites (tracing, coverage) are themselves rules.
3. **What hurt**: Overlog's update/aggregate semantics were never formalized (bugs came from ambiguity — Dedalus was the fix); they avoided multi-location rule bodies (only used `@` for unidirectional messaging) because failure semantics were unclear; join-by-repeated-variable syntax was hard to read; translating state-machine-described optimizations (Multi-Paxos, leader election) made the code "hybrid" and mechanistic; liveness (timeouts/backoff) could only be expressed mechanistically, not declaratively.
4. **Paxos in declarative languages exists at three maturity levels** we can port verbatim as tests: the 2009 Overlog Paxos (Kirsch–Amir-style views, ARU, global history; recovered source below), the Molly Dedalus Synod (84 lines), and the 2024 Dedalus MultiPaxos in Hydroflow's `datalog!` syntax (complete leader + acceptor, with p1b log reconciliation, hole filling with no-ops, stable-leader heartbeats). SIGMOD'24 then shows **correct-by-construction rewrites** (decoupling, partitioning, partial partitioning with sealing) that scale Paxos 3×, 2PC 5×.
5. **Raft in declarative languages is under-served**: two unfinished 2013 Bud student projects (one has a real commit-index bug), an incomplete Molly sketch, and Hydro's current Raft — which **deliberately abandoned a decomposed dataflow design** because splitting vote state and log state across asynchronously connected components produced a *simulator-reproducible committed-entry truncation bug*; they now run a pure sequential `raft_step` inside one tick. This is the single most important design lesson for us: **Raft's vote decision and its log/ack decisions must read and write the same state atomically, in one per-node timestep, under a canonical serialization of the tick's batch.**
6. **A full Raft needs far more than election + replication** (dissertation): durable term/vote/log with persist-before-reply, randomized timers, commit-only-current-term rule, no-op on election, single-server membership changes (plus the 2015 fix), joint consensus, learners/catch-up rounds, leader removal, disruptive-server protection, PreVote, leadership transfer (TimeoutNow), CheckQuorum-style step-down, snapshots + InstallSnapshot, client sessions for exactly-once, ReadIndex and lease reads, batching/pipelining/parallel leader disk write.

---

## 2. Overlog as BOOM used it (the JOL dialect)

### 2.1 Timestep model (EuroSys §2, Figure 2)

> "When Overlog tuples arrive at a node either through rule evaluation or external events, they are handled in an atomic local Datalog 'timestep.' Within a timestep, each node sees only locally-stored tuples. … Each timestep consists of three phases … In the first phase, inbound events are converted into tuple insertions and deletions on the local table partitions. The second phase interprets the local rules and tuples according to traditional Datalog semantics, executing the rules to a 'fixpoint' … In the third phase, updates to local state are atomically made durable, and outbound events (network messages, Java callback invocations) are emitted." — VERBATIM, EuroSys 2010 §2

Implementer consequences:

* **Durability barrier before send.** Phase 3 orders "make durable" before/atomically-with "emit". Consensus protocols depend on this (a vote or an ack must not leave the node before the vote/log is on disk). Our runtime must fsync the durable deltas of a timestep before releasing that timestep's outbound messages (group commit).
* **Concurrency = serial timesteps.** "Concurrent requests to the NameNode are handled in a serial fashion by JOL." The TR states "there is no explicit synchronization logic in any of the BOOM Analytics code, and we view this as a clear victory for the programming model" (TR §7.1).
* Durable state was committed "as an atomic transaction at the end of each fixpoint" using Stasis; "JOL allows durability to be specified on a per-table basis" — durable tables vs "scratch tables … emptied at the end of each fixpoint" (EuroSys §3.1).

### 2.2 Concrete Overlog syntax (from the recovered JOL sources)

All of the following are taken from the recovered `overlog-paxos` sources or `jol.jar` (lightly condensed: some comments and line breaks removed, `...` marks elided body literals; the `//` annotations on the right are ours):

```
program paxos_prepare;                      // module/namespace
import java.lang.System;                    // host-language import
define(local_aru, keys(0), {                // table declaration: name, primary key columns, column types
  String, // host
  Integer // local ARU value
});
define(prepare, { String, String, Integer, Integer });   // no keys(...) given
public                                      // rule visible to other programs
local_aru(Me, 0) :- paxos::self(Me), start(), notin global_history(Me, _, _, _);
prepare(@Him, Me, View, Aru) :- leader::leader(@Me, Leader, View), paxos::parliament(@Me, Him),
    local_aru(@Me, Aru), Leader == Me;      // @ = location specifier; head at another node = send
delete prepare_oklist(Master, View, SeqNo, Update, Type, Len, Agent) :-
    prepare_oklist(Master, View, SeqNo, Update, Type, Len, Agent),
    global_history(Master, SeqNo, _, Update);           // deletion rule
datalist_length(Agent, Aru, count<SeqNo>) :- datalist(Agent, _, _, Aru, SeqNo, _, _);   // aggregate in head
top_q(Master, min<Id>) :- q(Master, _, _, Id);
timer(t, physical, 25, infinity, 0);        // timer(name, physical|logical, period_ms, count, delay)
timer(beta, logical, 500, 1, 500);
duty_cycle(Me) :- paxos::self(Me), t();     // joining a timer relation = periodic firing
do_election(Me, Id, Me) :- progress_timer(Me, Start, Duration), seconds#insert(), ...;  // #insert = delta/event of insertions this step
Start := System.currentTimeMillis();        // assignment with host-language call
progress_timer(Me, Start, NewDuration) :- preinstall(Me, _), progress_timer(Me, _, Duration),
    Start := System.currentTimeMillis()
{ NewDuration := Duration * 2; };           // host-code block attached to a rule
watch(parliament, ae);                      // tracing directive (modifier letters)
progress_timer_start(1000L);                // fact
```

From `jol.jar` (`compile.olg`, `runtime.olg`, `network.olg`) — VERBATIM excerpts showing more syntax:

```
runtime::priority("compile", new TableName("global", "rule"), 0);                 // facts can construct host objects
dependency(ProgramName, Head.name(), Body.name()) :- config(ProgramName, Object), rule(ProgramName, Rule),
	predicate(ProgramName, Rule, 0, _, Head), predicate(ProgramName, Rule, Pos, _, Body),
	Head.name() != Body.name(), Pos > 0, ProgramName != "compile";            // method calls on tuple fields
facts(Program, Name, tupleset<Tuple>) :- config(Time, Program, Object), fact(Program, Name, Tuple);   // collection aggregate
evaluator
evaluation(Time, Program, Name, Insertions, Deletions) :- evaluator(execute(Time, Program, Query, Name, Insertions, Deletions));  // named rule ("evaluator") + table function call
receive(ScheduleTime, Msg) :- network::buffer#insert(Protocol, Direction, Location, Msg), global::clock(_, Time),
	Protocol == "network", Direction == "receive", ScheduleTime := Time + 1;
Program := ((NetworkMessage) Msg).program();                                       // host casts
```

Other features documented in the papers (not all visible in the recovered code):

* `keys(...)` — primary key. Inserting a tuple whose key collides **replaces** the old one; this is how Overlog "updates" state. The 2PC example relies on it: "The DDL for transaction (not shown) specifies that the first two columns are a primary key" (I Do Declare, Fig. 1) and the `tick` counter "The first two columns of tick are a primary key" (Fig. 2).
* `periodic` relation "configured to produce new tuples at every tick of a wall-clock timer" (EuroSys §3.2); `timer(ticker, 1000ms);` in I Do Declare Fig. 2.
* `die` relation: "when tuples are inserted into the die relation, a Java event listener is triggered that throws an exception" (EuroSys §6.1).
* Java extensibility: "Java classes as abstract data types, allowing Java objects to be stored in fields of tuples, and Java methods to be invoked on those fields… Java-based aggregation functions… Java table functions: Java iterators producing tuples, which can be referenced in Overlog rules as ordinary relations" (EuroSys §2.1). TR §3.1.3: "We employed them all: table functions for producing tuples from Java, Java objects and methods within tuples, Java aggregation functions, and Java event listeners that listen for insertions and deletions of tuples into tables."
* Aggregates used in BOOM: `count<>`, `min<>`, `max<>`, `sum<>`, `avg<>`, `set<>` (e.g. `compute_chunk_locs(ChunkId, set<NodeAddr>)`), `percentile<0.25, PRate>` (LATE), `tupleset<>` (JOL internals). Ternary expressions `(ParentPath = "/" ? "" : "/")` and string `+`.
* Materialization control: "The materialization of each view can be changed via simple Overlog table definition statements without altering the semantics of the program" (EuroSys §3.1).

### 2.3 JOL's own compiler is Overlog over a catalog (metaprogramming substrate)

`jol.jar` contains `compile.olg`, `stratachecker.olg`, `runtime.olg`, `network.olg`, `tcp.olg`, `debug.olg`, `grappa.olg`. The catalog tables referenced there (VERBATIM names/arity):
`program(Program, Owner, Object)`, `rule(Program, Rule, Public, Async, Delete, Object)`,
`predicate(Program, Rule, Position, Event, Object)` (position 0 = head), `selection(Program, Rule, Position, Object)`,
`assignment(Program, Rule, Position, Object)`, `query(Program, Rule, Public, Async, Delete, Event, Input, Output, Object)`,
`watches(Program, Tablename, Modifier, Operator)`, `fact(Program, TableName, Tuple)`, `runtime::priority(Program, TableName, Strata)`, `clock(Location, Time)`.

Stratification is itself computed by rules (VERBATIM from `compile.olg`):

```
/* Transitive closure over the dependency graph. */
dependency(ProgramName, Ancestor, Child) :-
	dependency(ProgramName, Ancestor, Descendant),
	dependency(ProgramName, Descendant, Child),
	Ancestor != Child;
/* Bubble up predicates in the stratum chain. */
priorityUpdate(ProgramName, Head, max<Priority>) :-
	runtime::priority(ProgramName, Head, HPriority),
	runtime::priority(ProgramName, Body, BPriority),
	dependency(ProgramName, Head, Body),
	notin dependency(ProgramName, Body, Head),
	HPriority <= BPriority,
	Priority := BPriority + 1;
```

and checked by rules (`stratachecker.olg`): an error is raised when a `notin` child or an aggregation's dependency is not in a strictly lower stratum than the head. Even the network layer and the per-timestep scheduler (insertion/deletion queues per stratum; deletions run only when in a lower stratum than all current insertions) are Overlog rules. This is the Evita Raced lineage ("each Overlog program is compiled into a representation that is captured in rows of tables. Program testing, optimization and rewriting can be written concisely as metaprograms", EuroSys §2.1).

### 2.4 Distributed-programming idioms (I Do Declare §2.1, §3.5, Figs. 5–7)

These are the "library layer" BOOM authors found themselves thinking in; our standard library should provide them:

| Idiom | Construction (paper's words, paraphrased where marked) |
|---|---|
| **Multicast** | "composing the messaging primitive … with a join against a relation containing the membership list" |
| **Sequence** | "a single-row relation whose attribute values change over time … defined by a base rule that initializes the counter attribute of interest, and an inductive rule that increments this attribute" |
| **Timeout** | sequence + timer relation counts ticks since an event (Fig. 2) |
| **Roll call** | coordinator multicast + peer unicast response |
| **Barrier** | "A count aggregate over a table containing network messages … synchronization is achieved when the count is high enough" |
| **Voting** | "a roll call with a selection at the peer … and a barrier at the coordinator" |
| **Choice** | "an exemplary aggregate function like min in combination with selection … selects a particular tuple from a set" |
| **Atomic dequeue** | choice + "a conditional delete rule against the base relation"; "useful as a flow control mechanism, to ensure that at most one tuple enters the prepare phase dataflow at a time" (Fig. 5) |
| **GC** | "rules that explicitly delete tuples that are no longer needed" |

Figure 7 (VERBATIM table, counts of rules by pattern/idiom in their Paxos):

| Rule pattern | Idiom | Prepare | Propose | Election |
|---|---|---|---|---|
| All | | 13 | 13 | 19 |
| Messages | Multicast | 1 | 2 | 2 |
| | Other | 1 | 1 | 0 |
| State update | Sequence | 2 | 2 | 3 |
| | GC | 1 | 3 | 2 |
| | Other | 0 | 1 | 6 |
| Aggregation | Barrier | 1 | 1 | 1 |
| | Choice | 2 | 1 | 0 |
| | Other | 5 | 2 | 3 |
| Timer | Timeout | 0 | 0 | 2 |

Safety vs. liveness finding (I Do Declare §4): "our Paxos implementation encodes safety properties declaratively, and liveness properties mechanistically." Liveness needs "reasoning about timeout, retries, and potential livelock cycles between dueling proposers"; "Overlog lacks the ability to directly specify liveness properties as such."

---

## 3. BOOM-FS (HDFS in Overlog)

### 3.1 Architecture

HDFS/GFS model: one NameNode holds metadata; files are split into 64 MB chunks replicated 3× on DataNodes; DataNodes heartbeat their chunk lists; NameNode declares a DataNode dead after a timeout and re-replicates its chunks; only read and append are supported (EuroSys §3). BOOM-FS kept the **control path in Overlog** and the **data path in Java**: "we implemented the simple high-bandwidth data path 'by hand' in Java, concentrating our Overlog code on the trickier control-path logic."

### 3.2 Relations (EuroSys Table 1, VERBATIM; the paper underlines key columns but the underline is lost in extraction)

| Name | Description | Relevant attributes |
|---|---|---|
| file | Files | fileid, parentfileid, name, isDir |
| fqpath | Fully-qualified pathnames | path, fileid |
| fchunk | Chunks per file | chunkid, fileid |
| datanode | DataNode heartbeats | nodeAddr, lastHeartbeatTime |
| hb_chunk | Chunk heartbeats | nodeAddr, chunkid, length |

Chunk ordering: "relations are unordered. Currently, we assign chunk IDs in a monotonically increasing fashion and only support append operations, so clients can determine a file's chunk order by sorting chunk IDs" (footnote 2). → Our language needs an ordered-id generator per node (sequence idiom) or a built-in monotone id source.

Durability: Table 1 relations are durable; request-processing scratch tables are transient. DataNodes also maintain a relation of locally stored chunks "populated by periodically invoking a table function defined in Java that walks the appropriate directory".

### 3.3 Rules printed in the papers (VERBATIM)

Fully-qualified paths (EuroSys Fig. 3):

```
// fqpath: Fully-qualified paths.
// Base case: root directory has null parent
fqpath(Path, FileId) :-
    file(FileId, FParentId, _, true),
    FParentId = null, Path = "/";
fqpath(Path, FileId) :-
    file(FileId, FParentId, FName, _),
    fqpath(ParentPath, FParentId),
    // Do not add extra slash if parent is root dir
    PathSep = (ParentPath = "/" ? "" : "/"),
    Path = ParentPath + PathSep + FName;
```

"when a file representing a directory is removed, all fqpath tuples that describe child paths of that directory are automatically removed (because they can no longer be derived…)" and "we configured the fqpath relation to be cached after it is computed. Overlog will automatically update fqpath when file is changed, using standard relational view maintenance logic" → **we need incremental view maintenance with deletions (DRed or counting) for recursive views**, and a per-view materialize/recompute switch.

Chunk-location request (TR Fig. 2):

```
// The set of nodes holding each chunk
compute_chunk_locs(ChunkId, set<NodeAddr>) :-
    hb_chunk(NodeAddr, ChunkId, _);
// Chunk exists => return success and set of nodes
response(@Src, RequestId, true, NodeSet) :-
    request(@Master, RequestId, Src,
        "ChunkLocations", ChunkId),
    compute_chunk_locs(ChunkId, NodeSet);
// Chunk does not exist => return failure
response(@Src, RequestId, false, null) :-
    request(@Master, RequestId, Src,
        "ChunkLocations", ChunkId),
    notin hb_chunk(_, ChunkId, _);
```

### 3.4 Protocols

* **Metadata protocol**: "For each command … there is a single rule at the client (stating that a new request tuple should be 'stored' at the NameNode). There are typically two corresponding rules at the NameNode: one to specify the result tuple that should be stored at the client, and another to handle errors." Mutating requests additionally derive changes to metadata relations.
* **Heartbeat protocol**: request/response but clocked by the `periodic` relation. NameNode removes DataNodes (and their `hb_chunk` rows) after a configurable silence.
* **Control messages**: "If the number of replicas of a chunk drops below the configured replication factor … the NameNode sends a message to a DataNode that stores the chunk, asking it to send a copy of the chunk to another DataNode."
* **Data protocol**: "When an Overlog rule deduces that a chunk must be transferred from host X to Y, an output event is triggered at X. A Java event handler at X listens for these output events and uses a simple but efficient data transfer protocol…" → our runtime needs **output-event callbacks** (Bud: `register_callback`) and a way to stream bulk bytes outside the tuple engine.

### 3.5 Numbers and lessons

* Code: HDFS ~21,700 Java / 0 Overlog; BOOM-FS 1,431 Java / 469 Overlog (EuroSys Table 2). DataNode = 414 Java lines; Hadoop-API shim +400 Java lines.
* Effort: "After four person-months of work" (EuroSys §3.3; the TR says "After two months of work"); durability "about a day"; client APIs "an additional week".
* Not implemented: permissions, web UI, proactive rebalancing.
* Lesson: "For NameNode-local invariants (e.g., ensuring that the fqpath relation is consistent with the file relation), Overlog gave us confidence… However, Overlog was less useful for describing invariants that require the coordination of multiple nodes (e.g., ensuring that the replication factor of each chunk is satisfied)… distributed Overlog rules induce asynchrony across nodes; hence, such rules must describe protocols to enforce distributed invariants, not the invariants themselves." (EuroSys §3.3)

### 3.6 BFS: the Bloom (Bud) port of BOOM-FS (bud-sandbox/bfs)

Module structure (from `bfs/README`): `FSMaster` (metadata ops over a key-value store, flat namespace keyed by full path; directories are arrays of child names), `ChunkedFSMaster` (new chunk ids, file→chunks, chunk→nodes), `HBMaster` (heartbeats, chunk cache), `BFSDatanode`, `BFSDataProtocol` (Ruby, not Bloom), `BFSClient` (hybrid Ruby/Bloom), `BFSMasterGlue`, `BFSBackgroundTasks` (re-replication). Config (VERBATIM `bfs_config.rb`): `REP_FACTOR = 2`, `CHUNKSIZE = 100000`, `MASTER_DUTY_CYCLE = 1`, `CLIENT_RETRIES=15`, `READ_RETRIES=15`; heartbeat `HB_EXPIRE = 4.0`, `periodic :hb_timer, 2`.

Interfaces (VERBATIM):

```ruby
module FSProtocol
  state do
    interface input, :fsls, [:reqid, :path]
    interface input, :fscreate, [] => [:reqid, :name, :path, :data]
    interface input, :fsmkdir, [] => [:reqid, :name, :path]
    interface input, :fsrm, [] => [:reqid, :name, :path]
    interface output, :fsret, [:reqid, :status, :data]
  end
end
module ChunkedFSProtocol
  include FSProtocol
  state do
    interface :input, :fschunklist, [:reqid, :file]
    interface :input, :fschunklocations, [:reqid, :chunkid]
    interface :input, :fsaddchunk, [:reqid, :file]
  end
end
module BFSClientMasterProtocol
  state do
    channel :request_msg, [:@master, :source, :reqid, :rtype, :args]
    channel :response_msg, [:@source, :master, :reqid, :status, :response]
  end
end
```

Re-replication background task (VERBATIM `background.rb`, shows `argagg`, `group`, `choose`, `count`, `periodic`, output interface as callback):

```ruby
  bloom :replication do
    cc_demand <= (bg_timer * chunk_cache_alive).rights
    cc_demand <= (bg_timer * last_heartbeat).pairs {|b, h| [h.peer, nil, nil]}
    chunk_cnts_chunk <= cc_demand.group([cc_demand.chunkid], count(cc_demand.node))
    chunk_cnts_host <= cc_demand.group([cc_demand.node], count(cc_demand.chunkid))
    lowchunks <= chunk_cnts_chunk { |c| [c.chunkid] if c.replicas < REP_FACTOR and !c.chunkid.nil?}
    # nodes in possession of such chunks
    source <= (cc_demand * lowchunks).pairs(:chunkid => :chunkid) {|a, b| [a.chunkid, a.node]}
    # nodes not in possession of such chunks, and their fill factor
    candidate_nodes <= (chunk_cnts_host * lowchunks).pairs do |c, p|
      unless chunk_cache_alive.map{|a| a.node if a.chunkid == p.chunkid}.include? c.host
        [p.chunkid, c.host, c.chunks]
      end
    end
    best_dest <= candidate_nodes.argagg(:min, [candidate_nodes.chunkid], candidate_nodes.chunks)
    chosen_dest <= best_dest.group([best_dest.chunkid], choose(best_dest.host))
    best_src <= source.group([source.chunkid], choose(source.host))
    copy_chunk <= (chosen_dest * best_src).pairs(:chunkid => :chunkid) do |d, s|
      [d.chunkid, s.host, d.host]
    end
  end
```

Heartbeat master (VERBATIM `hb_master.rb` core):

```ruby
    chunk_cache_alive <+ (master_duty_cycle * chunk_cache * last_heartbeat).combos(chunk_cache.node => last_heartbeat.peer) do |l, c, h|
      c if (l.val.to_f - h.time) < OLD
    end
    chunk_cache_alive <- (master_duty_cycle * chunk_cache_alive).rights
    hb_ack <~ heartbeat do |l|
      [l.sender, l.pload[0]] unless l.pload[1] == [nil]
    end
    available <= last_heartbeat.group(nil, accum(last_heartbeat.peer))
```

Heartbeat agent keeps the latest heartbeat per peer with `argagg(:max, …)` and expires entries older than `HB_EXPIRE` with a deletion rule (`heartbeat_log <- to_del`). Datanodes send only chunk ids the master hasn't acked (`server_knows`), a delta-heartbeat optimization.

Observations for us: BFS leans on Ruby blocks inside rules (e.g. `chunk_cache_alive.map{…}.include?` — an embedded non-monotone subquery), `argagg`, `accum`, `choose`, `<+-` (upsert), and `bootstrap` blocks. The Ruby-in-rule escape hatch hides negation from the stratifier; **our language should make such anti-joins first-class so the analyzer sees them**.

Tests in bud-sandbox (`test/tc_bfs.rb`, `test/tc_e2e_bfs.rb`): `test_directorystuff1`, `test_fsmaster`, `test_rms`, `test_many_datanodes`, and an end-to-end test that appends `/usr/share/dict/words` and compares MD5 of the read-back file.

---

## 4. The Availability rev: Paxos in Overlog

### 4.1 Evolution (EuroSys §4, TR §4)

* Basic Paxos: "22 Overlog rules in 53 lines of code, corresponding nearly line-for-line with the invariants from Lamport's original paper … Since our entire implementation fit on a single screen, we were able to visually confirm its faithfulness."
* Multi-Paxos + liveness module + catch-up (+ "optimizations to reduce message complexity", TR): "caused our implementation to swell to 50 rules in roughly 400 lines of code … these enhancements made our code considerably more difficult to check for correctness."
* "Our Paxos implementation constituted roughly 400 lines of code and required six person-weeks of development time. Adding Paxos support to BOOM-FS took two person-days and required making mechanical changes to ten BOOM-FS rules … We suspect that the rule modifications required to add Paxos support could be performed as an automatic rewrite." (EuroSys §4.3) → **candidate compiler feature: "replicate this component via consensus" as a program rewrite.**
* Leader election followed Kirsch & Amir, "in 19 Overlog rules (the original specification required 31 lines of pseudocode)" (I Do Declare §3.4).
* The Multi-Paxos phase-1-skip optimization is "naturally expressed in a state machine model as a pair of transition rules for the same input … we frequently found it easier … to model the state as a relation with a single row, allow certain rules to fire only in certain states, and explicitly describe the transitions" (TR §4.3). The resulting rules "have a hybrid feel".

### 4.2 BOOM-FS integration

"All state-altering actions are represented in the revised BOOM-FS as Paxos decrees, which are passed into the Paxos logic via a single Overlog rule that intercepts tentative actions and places them into a table that is joined with Paxos rules. Each action is considered complete at a given site when it is 'read back' from the Paxos log (i.e., when it becomes visible in a join with a table representing the local copy of that log). A sequence number field in the Paxos log table captures the globally-accepted order of actions on all replicas." (EuroSys §4.2). Durability: "Lamport's description of Paxos explicitly distinguishes between transient and durable state. Our implementation already divided this state into separate relations, so we simply marked the appropriate relations as durable." (TR §4.2)

The recovered glue (VERBATIM `glue.olg`, "glue for BFS"):

```
public
queueing::enq(Master, Decree, From, Id) :-
  paxos_global::decreeRequest(Master, Decree, From),
  Id := Runtime.idgen();

public
paxos_global::requestStatus(Master, Client, Decree, Instance, "passed") :-
  paxos_prepare::global_history(Master, Instance, Client, Decree);
```

and `queueing.olg` forwards enqueued decrees to the current leader: `paxos_propose::q(@Leader, Update, Requestor, Id) :- enq(@Me, Update, Requestor, Id), leader::leader(@Me, Leader, View);`

### 4.3 The recovered Overlog Paxos, annotated

State (Kirsch–Amir vocabulary): `local_aru` ("all received up to", next sequence number), `global_history(Agent, SeqNo, Requestor, Update)` (the decided log, keys (0,1)), `accept(Agent, Master, View, SeqNo, Update)`, `datalist` (prepare-OK contents), `leader::last_attempted`, `leader::last_installed`, `leader::progress_timer`.

**Phase 1 (prepare.olg)** — VERBATIM key rules:

```
public
prepare(@Him, Me, View, Aru) :-
    leader::leader(@Me, Leader, View),
    paxos::parliament(@Me, Him),
    local_aru(@Me, Aru),
    Leader == Me;

datalist(Agent, Master, View, Aru, SeqNo, Update, Type) :-
    global_history(Agent, SeqNo, _, Update),
    datalist(Agent, Master, View, Aru, -1, _, "Bottom"),
    SeqNo >= Aru,
    Type := "Ordered";

public
datalist(Agent, Master, View, Aru, SeqNo, Update, Type) :-
    accept(Agent, M, OldView, SeqNo, Update),
    datalist(Agent, Master, View, Aru, -1, _, "Bottom"),
    SeqNo >= Aru,
    Type := "Proposed";

// Rather than using explicit negation, we insert a dummy
// tuple into datalist to represent "nothing".
datalist(Agent, Master, View, Aru, -1, "none", "Bottom") :-
    prepare(Agent, Master, View, Aru),
    leader::last_installed(Agent, LastView),
    LastView == View;

prepare_oklist(@Master, View, SeqNo, Update, Type, Len, Agent) :-
    datalist_length(@Agent, Aru, Len),
    datalist(@Agent, Master, View, Aru, SeqNo, Update, Type);

// only count agents who have sent prepare_ok messages towards
// the quorum if we're received the whole set of messages from
// that agent.
prepare_ok_cnt(Master, View, count<Agent>) :-
    prepare_oklist_cnt(Master, View, Agent, Cnt, Cnt2),
    Cnt == Cnt2;

quorum(Master, View) :-
    paxos::priestCnt(Master, PCnt),
    leader::leader(Master, Leader, View),
    Master == Leader,
    prepare_ok_cnt(Master, View, RCnt),
    RCnt > (PCnt / 2);
```

Two design points worth copying: (a) **a multi-tuple reply is "sealed" by shipping its length with every tuple** (`Len`), and the receiver only counts an agent once `count == Len` — exactly the "sealing" construct the 2024 rewrites paper later formalizes (§10.4); (b) a sentinel `"Bottom"` tuple stands in for "empty".

**Phase 2 (propose.olg)** — VERBATIM key rules:

```
max_proposal(Master, SeqNo, max<View>) :-
    paxos_prepare::prepare_oklist(Master, View, SeqNo, _, "Proposed", _, _);

// Constrained update
send_propose(@Agent, Master, MyView, Aru, Update) :-
    duty_cycle(@Master),
    paxos::parliament(@Master, Agent),
    max_proposal(@Master, SeqNo, View),
    paxos_prepare::quorum(@Master, MyView),
    paxos_prepare::local_aru(@Master, Aru),
    paxos_prepare::prepare_oklist(@Master, View, SeqNo, Update, "Proposed", _, _);

// dequeue on the duty cycle only, not on deltas to local_aru, etc.
timer(t, physical, 25, infinity, 0);
duty_cycle(Me) :- paxos::self(Me), t();

// Unconstrained update
send_propose(@Agent, Master, View, Aru, Update) :-
    duty_cycle(@Master),
    paxos::parliament(@Master, Agent),
    notin paxos_prepare::prepare_oklist(@Master, View, _, _, "Proposed", _, _),
    paxos_prepare::quorum(@Master, View),
    leader::last_installed(@Master, View),
    paxos_prepare::local_aru(@Master, Aru),
    q(@Master, Update, R, Id),
    top_q(@Master, Id);

top_q(Master, min<Id>) :- q(Master, _, _, Id);
delete q(Me, Update, Sender, Id) :- q(Me, Update, Sender, Id), globally_ordered(Me, _, _, Update);

paxos_prepare::accept(@Other, Agent, View, SeqNo, Update) :-
    send_propose(@Agent, _, View, SeqNo, Update),
    notin paxos_prepare::global_history(@Agent, SeqNo, _, Update),
    paxos::parliament(@Agent, Other, _);

delete paxos_prepare::accept(Agent, Master, View, SeqNo, Update) :-
    paxos_prepare::accept(Agent, Master, View, SeqNo, Update),
    paxos_prepare::accept(Agent, _, View2, SeqNo, _),
    View2 > View;

accept_cnt(Me, View, SeqNo, count<Agent>) :- paxos_prepare::accept(Me, Agent, View, SeqNo, _);

globally_ordered(Me, View, SeqNo, Update) :-
    accept_cnt(Me, View, SeqNo, Cnt),
    paxos::priestCnt(Me, PCnt),
    Cnt > (PCnt / 2),
    send_propose(Me, _, View, SeqNo, Update);

hmax(Agent, max<SeqNo>) :- paxos_prepare::global_history(Agent, SeqNo, _, _);
paxos_prepare::local_aru(Agent, SeqNo + 1) :- hmax(Agent, SeqNo);
paxos_prepare::global_history(Agent, SeqNo, Requestor, Update) :-
    globally_ordered(Agent, _, SeqNo, Update), Requestor := "?";
```

Notes: this is the "each acceptor broadcasts accepts to all" (learner-at-every-agent) variant: every agent counts `accept`s for (View, SeqNo) and decides locally. The unconstrained proposal is the **choice + atomic dequeue** idiom (`top_q` = `min<Id>`, delete once ordered). Uses keyed `local_aru` (keys(0)) as an updatable single-row sequence. Proposals are paced by a 25 ms physical timer (`duty_cycle`) — explicitly to avoid firing on every delta.

**Leader election (election.olg)** — VERBATIM excerpts (Kirsch–Amir views; leader of view = originator; progress timer doubling; `vc_proof` periodic proof messages):

```
/* safety-critical logic is (perhaps inappropriately) 
   defined here: do_election carries with it the new View number.
*/
do_election(Me, Id, Me) :-
    progress_timer(Me, Start, Duration),
    seconds#insert(),
    last_attempted(Me, Last),
    paxos::priestCnt(Me, Cnt),
    Id := Last + Cnt,
    Start != -1,
    (System.currentTimeMillis() - Start) > Duration;

/* if progress timer isn't set, and View > Last */
do_election(Me, View, Originator) :-
    view_change(Me, View, Other, Originator),
    last_attempted(Me, Last),
    Me != Other,
    View > Last;
    /* the progress_timer rule doesn't seem to work.  relaxing it; may 
       cause livelock issues ... */

view_change(@Other, Id, Me, Originator) :-
    do_election(@Me, Id, Originator),
    paxos::parliament(@Me, Other, _);
vc_cnt(Me, View, count<Other>) :- view_change(Me, View, Other, _);
preinstall(Me, View) :- vc_cnt(Me, View, Cnt), paxos::priestCnt(Me, Total), Cnt > (Total / 2);
leader(Me, Originator, View) :- preinstall(Me, View), view_change(Me, View, _, Originator);
progress_timer(Me, Start, NewDuration) :-
    preinstall(Me, _), progress_timer(Me, _, Duration), Start := System.currentTimeMillis()
{ NewDuration := Duration * 2; };
```

View numbers are made unique per host by stepping `Last + Cnt` (host-id-offset arithmetic). The comment in `prepare.olg` states the safety dependency explicitly: "The common wisdom says that the prepare phase of Paxos enforces all the safety guarantees … This is not true in this implementation, because the leader election module produces our View number. The following safety constraints must hold over leader::leader(_, _, View): 1. it must be unique to this host. 2. it must be monotonically increasing." — i.e., **ballot uniqueness is a cross-module invariant; our design should make ballots a first-class lattice type `(round, node_id)` so uniqueness holds by construction.**

The Kirsch–Amir algorithm it follows (PfSB Fig. 6): on Progress_Timer expiry `Shift_to_Leader_Election(Last_Attempted+1)`; jump to a higher attempted view only if "the server must already suspect the current leader" and "Progress Timer must not already be set"; preinstall on ⌊N/2⌋+1 matching View_Change messages, then double the progress timer; leader of view i is the server with `id ≡ i mod N`; periodic VC_Proof lets a server join an already-installed view. State variables: `Last_Attempted`, `Last_Installed`, `VC[]`, `Prepare`, `Prepare_oks[]`, `Local_Aru`, `Last_Proposed`, `Global_History[]` (Proposal, Accepts[], Globally_Ordered_Update per seq), `Progress_Timer`, `Update_Timer`, `Update_Queue`, `Last_Executed[]`, `Last_Enqueued[]`, `Pending_Updates[]` (PfSB Fig. 2).

**Built-in test assertions (assertions.olg)** — VERBATIM shape; these are ideal end-to-end tests for our port:

```
lt("fail") :-
    la(M, O),
    paxos_prepare::global_history(M, S, R, A),
    paxos_prepare::global_history(M, S, R2, A2),
    A != A2
{ System.out.println("Local Conflict! ..."); };

lt("fail") :-
    cen_q(C, L, V), cen_q(C, L2, V), L != L2
{ System.out.println("two quorums for the same view " ...); };

lt("fail") :-
    cen(C, O, M, S, A), cen(C, O2, M2, S, A2), M != M2, A != A2
{ System.out.println("Distributed Conflict! ..."); };

lt("succeed") :- ccnt(C, Cnt), Cnt > 199;
```

(`cen`/`cen_q` ship every replica's history/quorum facts to a central checker at `tcp:localhost:7001` — a distributed watchdog.)

### 4.4 Measured failover (TR Appendix D, Table 5 VERBATIM)

Wordcount on 5 GB, 20 EC2 nodes:

| # NameNodes | Failure condition | Avg completion (s) | Std dev |
|---|---|---|---|
| 1 | None | 101.89 | 12.12 |
| 3 | None | 102.70 | 9.53 |
| 3 | Backup | 100.10 | 9.94 |
| 3 | Primary | 148.47 | 13.94 |

"in the absence of failure replication has negligible performance impact". They also used metaprogrammed tracing to check that "the message complexity of our Paxos code, both at steady state and under churn … matched the specification".

### 4.5 2PC in Overlog (I Do Declare Figs. 1–2, VERBATIM) — test program

```
/* Count number of peers */
peer_cnt(Coordinator, count<Peer>) :-
    peers(Coordinator, Peer);
/* Count number of "yes" votes */
yes_cnt(Coordinator, TxnId, count<Peer>) :-
    vote(Coordinator, TxnId, Peer, Vote),
    Vote == "yes";
/* Prepare => Commit if unanimous */
transaction(Coordinator, TxnId, "commit") :-
    peer_cnt(Coordinator, NumPeers),
    yes_cnt(Coordinator, TxnId, NumYes),
    transaction(Coordinator, TxnId, State),
    NumPeers == NumYes, State == "prepare";
/* Prepare => Abort if any "no" votes */
transaction(Coordinator, TxnId, "abort") :-
    vote(Coordinator, TxnId, _, Vote),
    transaction(Coordinator, TxnId, State),
    Vote == "no", State == "prepare";
/* All peers know transaction state */
transaction(@Peer, TxnId, State) :-
    peers(@Coordinator, Peer),
    transaction(@Coordinator, TxnId, State);
```

```
/* Declare a timer that fires once per second */
timer(ticker, 1000ms);
/* Start counter when TxnId is in "prepare" state */
tick(Coordinator, TxnId, Count) :-
    transaction(Coordinator, TxnId, State),
    State == "prepare",
    Count := 0;
/* Increment counter every second */
tick(Coordinator, TxnId, NewCount) :-
    ticker(),
    tick(Coordinator, TxnId, Count),
    NewCount := Count + 1;
/* If not committed after 10 sec, abort TxnId */
transaction(Coordinator, TxnId, "abort") :-
    tick(Coordinator, TxnId, Count),
    transaction(Coordinator, TxnId, State),
    Count > 10, State == "prepare";
```

Note these rules *overwrite* `transaction` via its (0,1) primary key while also reading it — only coherent under a "next state" semantics. Dedalus later made this explicit (`@next`). Our language must reject or clearly define same-timestep self-overwrites.

The Paxos acceptor promise rule (Fig. 3, VERBATIM):

```
promise(@Master, View, OldView, OldUpdate, Agent) :-
    prepare(@Agent, View, Update, Master),
    prev_vote(@Agent, OldView, OldUpdate),
    View >= OldView;
```

and quorum (Fig. 4, VERBATIM):

```
agent_cnt(Master, count<Agent>) :-
    parliament(Master, Agent);
promise_cnt(Master, View, count<Agent>) :-
    promise(Master, View, Agent, _);
quorum(Master, View) :-
    agent_cnt(Master, NumAgents),
    promise_cnt(Master, View, NumVotes),
    NumVotes > (NumAgents / 2);
```

---

## 5. The Scalability rev (partitioned NameNode)

"it involved adding a 'partition' column to various tables to split them across nodes … each NameNode-partition can be deployed either as a single node or a Paxos group." Partitioning: "based on the hash of the fully-qualified pathname of each file." Client library broadcasts directory listing and directory creation to all partitions; "Although the resulting directory creation implementation is not atomic, it is idempotent; recreating a partially-created directory will restore the system to a consistent state." No atomic cross-partition rename ("would involve the atomic transfer of state between independent Paxos groups … we have previously built a two-phase commit protocol in Overlog"). Effort: 8 hours (2 on client partitioning/broadcast). EuroSys §5.

For us: partitioning should be declarable (a partition key per relation + a routing function), and composable with replication (a partition = a consensus group). 2PC-over-Paxos-groups (cross-shard rename) is a natural flagship test that BOOM skipped.

---

## 6. The Monitoring rev (invariants, tracing via metaprogramming)

* **Invariants**: `die` relation → Java exception (§6.1). "A watchdog rule describes a query over system state that must never hold: such a rule is both a specification of an invariant and a check that enforces it." 12 rules / 60 lines of assertions, ≤8 person-hours. The paper also mentions watchdogs like "the number of messages sent by a protocol like Paxos matches the specification."
* **Tracing by rewriting** (§6.2, VERBATIM):

```
quorum(@Master, Round) :-
    priestCnt(@Master, Pcnt),
    lastPromiseCnt(@Master, Round, Vcnt),
    Vcnt > (Pcnt / 2);
```
"might have an associated tracing rule:"
```
trace_r1(@Master, Round, RuleHead, Tstamp) :-
    priestCnt(@Master, Pcnt),
    lastPromiseCnt(@Master, Round, Vcnt),
    Vcnt > (Pcnt / 2),
    RuleHead = "quorum",
    Tstamp = System.currentTimeMillis();
```
"The resulting program passes no more than twice as much data through the system… Using the metaprogramming approach of Evita Raced, we were able to automate this task via a trace rewriting program written in Overlog, involving the meta-tables of rules and terms … Network traces fall out of this approach naturally: any dataflow transition that results in network communication is flagged in the generated head predicate." Finer detail by "tapping" each body predicate.
* **Code coverage**: "less than a day" to build; traced unit tests and "reported statistics on the 'firings' of rules … and the counts of tuples deduced into tables"; found dead rules. Size: 15 rules / 64 lines of Overlog + 280 lines of Java UI (EuroSys); TR: "5 Overlog rules that are evaluated by every participating node, and 12 summary rules that are run at a centralized location".
* **Chukwa-style log collection** (TR §6.3 only): Java modules read `/proc` and Hadoop logs as tuples; "Windowing, aggregation and buffering are carried out in Overlog"; agent+collector logic runs inside the NameNode's JOL.

Implementer consequences: rules/predicates must be reflected into queryable catalog relations *at runtime*; the engine must support installing new rules at runtime (hot program modification) and per-rule firing counters; trace output must be able to target a remote location.

---

## 7. BOOM-MR (Hadoop JobTracker scheduling in Overlog)

### 7.1 State (EuroSys Table 3, VERBATIM)

| Name | Description | Relevant attributes |
|---|---|---|
| job | Job definitions | jobid, priority, submit_time, status, jobConf |
| task | Task definitions | jobid, taskid, type, partition, status |
| taskAttempt | Task attempts | jobid, taskid, attemptid, progress, state, phase, tracker, input_loc, start, finish |
| taskTracker | TaskTracker definitions | name, hostname, state, map_count, reduce_count, max_map, max_reduce |

`jobConf` is an opaque Java object; "we pass the JobConf object into a custom Java table function that manufactures task tuples for the job" (TR §3.1.3). Reduce attempts have phases copy/sort/reduce. "A scheduling policy is simply a set of rules that join against the taskTracker relation to find TaskTrackers with unassigned slots, and schedules tasks by inserting tuples into taskAttempt."

### 7.2 Policies

* Hadoop FCFS: 9 rules (96 lines). LATE: +5 rules (30 lines) + modifying 2 existing rules (speculation candidate selection and tracker choice). Hadoop's LATE patch: "over 800 lines of Java" (EuroSys) / TR Table 2: Hadoop patch 2102 lines across 17 files vs BOOM-MR 82 lines across 2 files.
* Hadoop default speculation (LATE paper §2.2 and TR App. C): speculate a task whose progress score is below its category average minus 0.2 after running ≥1 minute; progress score = input fraction for maps; reduces have three phases each counting 1/3.
* LATE (Zaharia OSDI'08 §4, VERBATIM summary): "If a node asks for a new task and there are fewer than SpeculativeCap speculative tasks running: – Ignore the request if the node's total progress is below SlowNodeThreshold. – Rank currently running tasks that are not currently being speculated by estimated time left. – Launch a copy of the highest-ranked task with progress rate below SlowTaskThreshold." ProgressRate = ProgressScore/T; time left = (1 − ProgressScore)/ProgressRate; SpeculativeCap = 10% of slots; thresholds = 25th percentiles; wait 1 minute before evaluating.

The BOOM Overlog statistics for LATE (TR Fig. 8, VERBATIM):

```
// Compute progress rate per task
taskPR(JobId, TaskId, Type, ProgressRate) :-
    task(JobId, TaskId, Type, _, _, _, Status),
    Status.state() != FAILED,
    Time = Status.finish() > 0 ?
        Status.finish() : currentTimeMillis(),
    ProgressRate = Status.progress() /
        (Time - Status.start());
// For each job, compute 25th pctile rate across tasks
taskPRList(JobId, Type, percentile<0.25, PRate>) :-
    taskPR(JobId, TaskId, Type, PRate);
// Compute progress rate per tracker
trackerPR(Tracker, JobId, Type, avg<PRate>) :-
    task(JobId, TaskId, Type, _),
    taskAttempt(JobId, TaskId, _, Progress, State,
        Phase, Tracker, Start, Finish),
    State != FAILED,
    Time = Finish > 0 ? Finish : currentTimeMillis(),
    PRate = Progress / (Time - Start);
// For each job, compute 25th pctile rate across trackers
trackerPRList(JobId, Type, percentile<0.25, AvgPRate>) :-
    trackerPR(_, JobId, Type, AvgPRate);
// Compute available map/reduce slots
speculativeCap(sum<MapSlots>, sum<ReduceSlots>) :-
    taskTracker(_, _, _, _, _, _,
        MapCount, ReduceCount,
        MaxMap, MaxReduce),
    MapSlots = MaxMap - MapCount,
    ReduceSlots = MaxReduce - ReduceCount;
```

Note: `currentTimeMillis()` inside a rule body — a non-deterministic, time-varying built-in. Our language must model "now" as an input relation per timestep (Dedalus style) so replay/simulation is deterministic.

### 7.3 Evaluation and effort

* 101-node EC2 cluster (1 master "high-CPU extra large", 100 slaves "high-CPU medium", 2 map + 2 reduce slots each); wordcount on 30 GB, 481 maps; 100 reduces for the baseline comparison and 400 reduces (two waves) for the LATE experiment; 6 artificially loaded straggler nodes. "the LATE implementation in BOOM Analytics handles stragglers much more effectively than the FCFS policy ported from Hadoop" (Fig. 4).
* Performance (Fig. 5): BOOM-MR over HDFS "nearly identical to Hadoop 18.1"; over BOOM-FS "slightly slower than HDFS, but remains competitive". Maps complete in 3 waves (2×100 slots). "We observed that system load averages were much lower with Hadoop than with BOOM Analytics."
* Effort: initial BOOM-MR one person-month + two person-months debugging/tuning; 55 rules / 396 lines Overlog + 1,269 Java; "based on Hadoop version 18.1" (the paper's wording); "we estimate that we removed 6,573 lines from Hadoop (out of 88,864)".
* Lesson: "scheduling can be decomposed into two tasks: monitoring the state of a system and applying policies for how to react to changes to that state. Monitoring is well-handled by Overlog … statistics … are naturally realized as aggregate functions, and JOL took care of automatically updating those statistics as new messages from TaskTrackers arrived."

### 7.4 Implications for our "modern Hadoop successor" (SKETCH, ours)

BOOM-MR's architecture generalizes: cluster state as relations (jobs/stages/tasks/attempts/workers/slots), scheduling policies as swappable rule sets over incrementally maintained aggregates (LATE's percentiles, delay-scheduling locality waits [Zaharia EuroSys'10 is BOOM ref 40]), data plane outside the rule engine. For a successor we additionally need: incremental aggregates including percentiles/quantile sketches, time as input, per-policy modules with an interposition point (BOOM: "a form of encapsulation could be achieved by constraining the points in the dataflow at which interposition is allowed to occur"), and HA of the master via the same Paxos/Raft rewrite used for BOOM-FS.

---

## 8. Experience and lessons (EuroSys §9, TR §7) — mapped to design requirements

| BOOM lesson (quoted/paraphrased) | Requirement for bloom-remake |
|---|---|
| "Everything is data … even parsed code" | Catalog tables for programs/rules; runtime rule install/uninstall |
| Partitioning was "a textbook exercise" once state was in relations | Declarative partitioning + routing; FD analysis (see §10.4) |
| Interposition via rerouting dataflow "pipes" enabled LATE and Paxos insertion | Module system with typed input/output interfaces (Bloom `interface`), rewrite hooks |
| "Many of the bugs we encountered were due to ambiguities in the language semantics, particularly with regard to state update and aggregate functions" | Dedalus semantics: all mutation via `@next`, stratified aggregation, explicit async |
| "we did not utilize arbitrary distributed queries (i.e., rules with two or more distinct location specifiers in their body terms) … unsure of the semantics … in the event of node failures" | Only allow location-local bodies; cross-node only via async heads (Dedalus restriction) |
| Join-by-repeated-variables "hard to write, and especially hard to read" | Offer named-field / SQL-like join syntax (Bloom's `pairs(:a => :b)`) in addition to positional |
| Liveness via timers is "mechanistic" | First-class timers, timeouts, backoff; plus model checking/fault injection for liveness |
| Hybrid state-machine rules for Paxos optimizations | Provide a sanctioned "sequential step / fold" construct or a state-machine sugar that compiles to rules |
| "modest performance of the current JOL interpreter"; planned C kernel | Compiled dataflow (Hydro-style) in Rust |
| "we have not to date seriously dealt with the idea of a single JOL runtime hosting multiple programs" | Multi-program runtime with namespaces (JOL had `program X;` + `::`) |

---

## 9. Consensus in declarative languages after BOOM

### 9.1 Molly's Dedalus Paxos Synod (`paxos_synod.ded`, VERBATIM excerpt)

```
include "util/timeout_svc.ded";
timer_svc(A, M, 3) :- proposal(A, M);

nodes(A, N, I)@next :- nodes(A, N, I);
seed(A, S)@next :- seed(A, S), notin update_seed(A);
seed(A, S+C)@next :- seed(A, S), update_seed(A), agent_cnt(A, C);

prepare(B, A, S, M)@async :- proposal(A, M), seed(A, S), nodes(A, B, _);
update_seed(A) :- proposal(A, _);

redo(A, M) :- timeout(A, M), notin accepted(A, _, M);
prepare(B, A, S, M)@async :- redo(A, M), seed(A, S), nodes(A, B, _);
timer_svc(A,M,3) :- redo(A, M);
update_seed(A) :- redo(A, M);

response_log(C, A, S, O, Os) :- prepare_response(C, A, S, O, Os);
response_log(C, A, S, O, M)@next :- response_log(C, A, S, O, M);
response_cnt(C, S, count<I>) :- response_log(C, A, S, O, Os), nodes(C, A, I);
best(C, S, max<Os>) :- response_log(C, A, S, O, Os);
what(C, I) :- nodes(C, _, I);
agent_cnt(C, count<I>) :- what(C, I);

accept(A, S, O)@async :- agent_cnt(C, Cnt1), response_cnt(C, S, Cnt2),
                         response_log(C, _, S, O, Os), best(C, S, Os), nodes(C, A, _), Os != 1, Cnt2 > Cnt1 / 2;
accept(A, S, P)@async :- agent_cnt(C, Cnt1), response_cnt(C, S, Cnt2), response_log(C, _, S, O, Os), 
                         best(C, S, Os), my_proposal(C, P), nodes(C, A, _), Os == 1, Cnt2 > Cnt1 / 2;

// acceptor
dominated(A, S) :- prepare(A, _, S, _), prepare_log(A, S2, _), S2 > S;
can_respond(A, C, S, M) :- prepare(A, C, S, M), notin dominated(A, S);
prepare_response(C, A, S, O, Os)@async :- can_respond(A, C, S, M), accepted(A, Os, O), highest_accepted(A, Os);
prepare_response(C, A, S, "anything", 1)@async :- can_respond(A, C, S, M), notin accepted(A, _, _);

highest_accepted(A, max<S>) :- accepted(A, S, _);
accepted(A, S, M) :- accept(A, S, M);
accepted(A, S, M)@next :- accepted(A, S, M);
prepare_log(A, S, M) :- prepare(A, _, S, M);
prepare_log(A, S, M)@next :- prepare_log(A, S, M);
...
disagree(M) :- important(_, M), important(_, N), M != N;
bad(A) :- important(A, "peter"), important(_, "foobar");
good("yay") :- important(A,M), notin bad(A);
```

and the reusable logical timeout service (VERBATIM `util/timeout_svc.ded`):

```
timer_state(H, I, T-1)@next :- timer_svc(H, I, T);
timer_state(H, I, T-1)@next :- timer_state(H, I, T), notin timer_cancel(H, I), T > 1;
timeout(H, I) :- timer_state(H, I, 1);
```

Takeaways: ballots are made unique with the "seed + agent count" trick (like BOOM's `Last + Cnt`); **timeouts in Molly are logical (counted in timesteps)** so that fault-injection search is finite and deterministic. Our runtime must support both logical-tick timers (for simulation/verification) and physical timers (for deployment), ideally with the same source program.

### 9.2 bud-sandbox Paxos and voting

`paxos/README`: "The paxos implementation is a work in progress. Leader election is (mostly) done, prepare phase is getting there, propose phase is nonexistent. — palvaro". It re-uses Kirsch–Amir names (`local_aru`, `datalist`, `global_history`, `last_installed`) on top of a reusable voting library (VERBATIM `voting/voting.rb`, majority variant):

```ruby
module VoteInterface
  state do
    channel :ballot, [:@peer, :master, :ident] => [:content]
    channel :vote, [:@master, :peer, :ident] => [:response, :content]
  end
end
module MajorityVotingMaster
  include VotingMaster
  bloom :summary do
    victor <= (vote_status * member_cnt * vote_cnt).combos(vote_status.ident => vote_cnt.ident) do |s, m, v|
      if s.response == "in flight" and v.cnt > m.cnt / 2
        [v.ident, s.content, v.response, v.content]
      end
    end
    vote_status <+ victor
    vote_status <- victor {|v| [v.ident, v.content, 'in flight', nil] }
  end
end
```

(`vote_cnt <= votes_rcvd.group([votes_rcvd.ident, votes_rcvd.response], count(votes_rcvd.peer), accum(votes_rcvd.content))`.) The voting/multicast/membership/nonce/serializer libraries are the Bloom rendition of I Do Declare's idioms.

### 9.3 Dedalus MultiPaxos in Hydroflow `datalog!` (autocomp `rust/examples/multipaxos`) — VERBATIM rules

Syntax of this dialect (as used in the file): `:-` same-tick, `:+` next tick, `:~` async; `rel@addr(...)` in the head selects destination; `!` negation; `count(x)`, `max(x)`, `index()` (per-tick enumeration); `.input`, `.output`, `.async`, `.persist` declarations; host-language (Rust) expressions in backticks bind relations to sources/sinks.

Leader (proposer), rules only (debug outputs omitted):

```
p1b(a, l, i, n, mi, mn) :- p1bU(a, l, i, n, mi, mn)
p1b(a, l, i, n, mi, mn) :+ p1b(a, l, i, n, mi, mn)
p1bLog(a, p, s, pi, pn, i, n) :- p1bLogU(a, p, s, pi, pn, i, n)
p1bLog(a, p, s, pi, pn, i, n) :+ p1bLog(a, p, s, pi, pn, i, n) # drop all p1bLogs if slot s is committed
p2b(a, p, s, i, n, mi, mn) :- p2bU(a, p, s, i, n, mi, mn)
p2b(a, p, s, i, n, mi, mn) :+ p2b(a, p, s, i, n, mi, mn), !allCommit(_, s) # drop all p2bs if slot s is committed
receivedBallots(i, n) :+ receivedBallots(i, n)
iAmLeader(i, n) :- iAmLeaderU(i, n)
iAmLeader(i, n) :+ iAmLeader(i, n), !iAmLeaderCheckTimeout() # clear iAmLeader periodically (like LRU clocks)

# Initialize
ballot(zero) :- startBallot(zero)

######################## stable leader election
RelevantP1bs(acceptorID, logSize) :- p1b(acceptorID, logSize, i, num, maxID, maxNum), id(i), ballot(num)
receivedBallots(id, num) :- iAmLeader(id, num)
receivedBallots(maxBallotID, maxBallotNum) :- p1b(acceptorID, logSize, i, num, maxBallotID, maxBallotNum)
receivedBallots(maxBallotID, maxBallotNum) :- p2b(acceptorID, payload, slot, ballotID, ballotNum, maxBallotID, maxBallotNum)
MaxReceivedBallotNum(max(num)) :- receivedBallots(id, num)
MaxReceivedBallot(max(id), num) :- MaxReceivedBallotNum(num), receivedBallots(id, num)
HasLargestBallot() :- MaxReceivedBallot(maxId, maxNum), id(i), ballot(num), (num > maxNum)
HasLargestBallot() :- MaxReceivedBallot(maxId, maxNum), id(i), ballot(num), (num == maxNum), (i >= maxId)

# send heartbeat if we're the leader.
iAmLeaderU@pid(i, num) :~ iAmLeaderResendTimeout(), id(i), ballot(num), IsLeader(), proposers(pid), !id(pid) # don't send to self
LeaderExpired() :- iAmLeaderCheckTimeout(), !IsLeader(), !iAmLeader(i, n)

# Resend p1a if we waited a random amount of time (timeout) AND leader heartbeat timed out. Send NewBallot if it was just triggered (ballot is updated in t+1), otherwise send ballot.
p1a@a(i, i, num) :~ p1aTimeout(), LeaderExpired(), id(i), NewBallot(num), acceptors(a)
p1a@a(i, i, num) :~ p1aTimeout(), LeaderExpired(), id(i), ballot(num), !NewBallot(newNum), acceptors(a)

# ballot = max + 1. If anothe proposer sends iAmLeader, that contains its ballot, which updates our ballot (to be even higher), so we are no longer the leader (RelevantP1bs no longer relevant)
NewBallot(maxNum + 1) :- MaxReceivedBallot(maxId, maxNum), id(i), ballot(num), (maxNum >= num), (maxId != i)
ballot(num) :+ NewBallot(num)
ballot(num) :+ ballot(num), !NewBallot(newNum)

######################## reconcile p1b log with local log
RelevantP1bLogs(acceptorID, payload, slot, payloadBallotID, payloadBallotNum) :- p1bLog(acceptorID, payload, slot, payloadBallotID, payloadBallotNum, i, num), id(i), ballot(num)

# cannot send new p2as until all p1b acceptor logs are PROCESSED; otherwise might miss pre-existing entry
P1bLogFromAcceptor(acceptorID, count(slot)) :- RelevantP1bLogs(acceptorID, payload, slot, payloadBallotID, payloadBallotNum)
P1bAcceptorLogReceived(acceptorID) :- P1bLogFromAcceptor(acceptorID, logSize), RelevantP1bs(acceptorID, logSize)
P1bAcceptorLogReceived(acceptorID) :- RelevantP1bs(acceptorID, logSize), (logSize == 0)
P1bNumAcceptorsLogReceived(count(acceptorID)) :- P1bAcceptorLogReceived(acceptorID)
IsLeader() :- P1bNumAcceptorsLogReceived(c), quorum(size), (c >= size), HasLargestBallot()

P1bMatchingEntry(payload, slot, count(acceptorID), payloadBallotID, payloadBallotNum) :-  RelevantP1bLogs(acceptorID, payload, slot, payloadBallotID, payloadBallotNum)
# what was committed = store in local log. ...
CommittedLog(payload, slot) :- P1bMatchingEntry(payload, slot, c, payloadBallotID, payloadBallotNum), quorum(size), (c >= size)

# what was not committed = find max ballot, store in local log, resend
P1bLargestEntryBallotNum(slot, max(payloadBallotNum)) :- RelevantP1bLogs(acceptorID, payload, slot, payloadBallotID, payloadBallotNum)
P1bLargestEntryBallot(slot, max(payloadBallotID), payloadBallotNum) :- P1bLargestEntryBallotNum(slot, payloadBallotNum), RelevantP1bLogs(acceptorID, payload, slot, payloadBallotID, payloadBallotNum)
ResentLog(payload, slot) :- !nextSlot(s), IsLeader(), P1bLargestEntryBallot(slot, payloadBallotID, payloadBallotNum), P1bMatchingEntry(payload, slot, c, payloadBallotID, payloadBallotNum), !CommittedLog(otherPayload, slot)
p2a@a(i, payload, slot, i, num) :~ ResentLog(payload, slot), id(i), ballot(num), acceptors(a)

# hole filling: if a slot is not in ResentEntries or proposedLog but it's smaller than max, then propose noop. ...
ProposedSlots(slot) :- startSlot(slot)
ProposedSlots(slot) :- CommittedLog(payload, slot)
ProposedSlots(slot) :- ResentLog(payload, slot)
MaxProposedSlot(max(slot)) :- ProposedSlots(slot)
PrevSlots(s) :- MaxProposedSlot(maxSlot), less_than(s, maxSlot)
FilledHoles(no, s) :- !nextSlot(s2), IsLeader(), noop(no), !ProposedSlots(s), PrevSlots(s)
p2a@a(i, no, slot, i, num) :~ FilledHoles(no, slot), id(i), ballot(num), acceptors(a)

# To assign values sequential slots after reconciling p1bs, start at max+1
nextSlot(s+1) :+ !nextSlot(s2), IsLeader(), MaxProposedSlot(s)

######################## send p2as
IndexedPayloads(payload, index()) :- clientIn(payload), nextSlot(s), IsLeader()
p2a@a(i, payload, (slot + offset), i, num) :~ IndexedPayloads(payload, offset), nextSlot(slot), id(i), ballot(num), acceptors(a)
NumPayloads(count(payload)) :- clientIn(payload)
nextSlot(s+num) :+ NumPayloads(num), nextSlot(s)
nextSlot(s) :+ !NumPayloads(num), nextSlot(s), IsLeader()

######################## process p2bs
CountMatchingP2bs(payload, slot, count(acceptorID), i, num) :- p2b(acceptorID, payload, slot, i, num, payloadBallotID, payloadBallotNum)
allCommit(payload, slot) :- CountMatchingP2bs(payload, slot, c, i, num), fullQuorum(c)
clientOut@r(payload, slot) :~ allCommit(payload, slot), replicas(r)
```

Acceptor (VERBATIM):

```
ballots(id, num) :+ ballots(id, num)
.persist log

ballots(id, num) :- p1a(pid, id, num)
MaxBallotNum(max(num)) :- ballots(id, num) 
MaxBallot(max(id), num) :- MaxBallotNum(num), ballots(id, num)
LogSize(count(slot)) :- p1a(_,_,_), log(p, slot, ballotID, ballotNum)
p1b@pid(i, size, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p1a(pid, ballotID, ballotNum), LogSize(size), MaxBallot(maxBallotID, maxBallotNum), id(i)
p1b@pid(i, 0, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p1a(pid, ballotID, ballotNum), !LogSize(size), MaxBallot(maxBallotID, maxBallotNum), id(i)

LogEntryMaxBallotNum(slot, max(ballotNum)) :- p1a(_,_,_), log(p, slot, ballotID, ballotNum)
LogEntryMaxBallot(slot, max(ballotID), ballotNum) :- p1a(_,_,_), LogEntryMaxBallotNum(slot, ballotNum), log(p, slot, ballotID, ballotNum)

# send back entire log 
p1bLog@pid(i, payload, slot, payloadBallotID, payloadBallotNum, ballotID, ballotNum) :~ p1a(pid, ballotID, ballotNum), log(payload, slot, payloadBallotID, payloadBallotNum), LogEntryMaxBallot(slot, payloadBallotID, payloadBallotNum), id(i)

log(payload, slot, ballotID, ballotNum) :- p2a(pid, payload, slot, ballotID, ballotNum), MaxBallot(ballotID, ballotNum)
p2b@pid(i, payload, slot, ballotID, ballotNum, maxBallotID, maxBallotNum) :~ p2a(pid, payload, slot, ballotID, ballotNum), id(i), MaxBallot(maxBallotID, maxBallotNum)
```

Notable engineering in this program (and things our language must support to write it):
* **Two-level argmax for a lexicographic ballot** (`MaxReceivedBallotNum` then `MaxReceivedBallot(max(id), num)`) — we should instead provide ballots as a lexicographic-pair lattice (`lmax<(num, id)>`) so this is one aggregate.
* **Sealing of a multi-message reply**: p1b carries `logSize`; the leader counts `p1bLog` per acceptor and only treats the acceptor's log as received when counts match — the same trick as BOOM's `Len`.
* **Per-tick batch indexing** `index()` to assign consecutive slots to all client requests arriving in one tick.
* The persistence discipline: inputs arrive in `xU` ("unpersisted") relations, copied into persisted aliases; GC via `!allCommit(_, s)` on the persistence rule.
* `IsLeader()` is a nullary derived relation — an idiom for boolean state.
* Commit here waits for `fullQuorum` (2f+1) acks in this benchmark variant (the `quorum(f+1)` commit rule is commented out in the source).

### 9.4 Optimizing Distributed Protocols with Query Rewrites (SIGMOD 2024)

Dedalus conventions the paper assumes (§2.3): every IDB relation carries location `L` and time `T` as its last two attributes; all body literals share the same `l,t`; three rule kinds — **synchronous** (head time = body time, same location), **sequential** (head time = t+1, same location), **asynchronous** (different head location and time chosen by the built-in non-deterministic `delay` relation, constrained so arrival time t' > t). Persistence rules `r(…, l, t') :- r(…, l, t), t'=t+1`. Library functions are infinite EDB relations usable only with bound inputs.

Correctness notion (§2.5): the optimized program is correct if every run "generates the same output facts with the same timestamps as some run of P" (history equivalence à la linearizability), under asynchronous network + general omission failures of up to f nodes (a failure of any decoupled sub-node = partial failure of the original node).

Rewrites (preconditions → mechanism), §3–4 and Appendix:

| Rewrite | Precondition | Mechanism |
|---|---|---|
| Mutually independent decoupling | C1, C2 reference disjoint relations, neither references the other's outputs | Add a `forward(l', l'')` redirection EDB to rules whose heads C2 references |
| Monotonic decoupling | C1 independent of C2 and C2 monotonic (sufficient: inputs persisted, no negation/aggregation; relaxed in App. A.2) | Redirection + add persistence rules in C2 for C1→C2 relations (CALM) |
| Functional decoupling | C2 has no aggregation/negation and each rule body has ≤1 IDB relation | Redirection only (stateless) |
| Asymmetric decoupling (App. A.5) | C2 monotonic but C2 independent of C1 (feedback allowed, e.g., p2b proxy notifying proposer of higher ballot) | batching + acknowledgement |
| State-machine decoupling (App. A.4; "cut from the paper") | C2 "behaves like a state machine" (existence / no-change dependencies on inputs) | C1 tags facts with its time T1 and batch counts; C2 processes batches in order |
| Partitioning with co-hashing | a distribution policy D that co-locates facts sharing join/group/antijoin keys in every rule (parallel disjoint correctness, Def. 4.1) | route inputs by D |
| Partitioning with dependencies | functional dependencies (FD) and co-partition dependencies (CD) make a D exist | same |
| Partial partitioning | the non-partitionable part C1 is rarely written | replicate C1's relations to all partitions; buffer other inputs until a replicated write is known received by all (needs commit/consensus) |
| Partitioning with sealing (App. B.4) | a batched multi-fact message must be re-assembled across partitions | desugar `out(seal<r>, …)` into count/received/sealed relations; partitions send per-partition counts and receiver sums them |

Sealing desugaring (App. B.4.1, VERBATIM):

```
# Component 𝐶.
rCount(count<...>,a,l,t) :− r(...,l,t), s(a,l,t)
outCount(c,a,l',t') :− rCount(c,a,l,t), s(a,l,t), dest(l',l,t),
delay((c,a,l,t,l'),t')
out(...,a,l',t') :− r(...,l,t), s(a,l,t), dest(l',l,t), delay((...,a,l,t,l'),t')
# Component 𝐶 ′ .
outReceived(count<...>,a,l,t) :− out(...,a,l,t)
sealed(a,l,t) :− outReceived(c,a,l,t), outCount(c,a,l,t)
u(...,a,l,t) :− out(...,a,l,t), sealed(a,l,t)
# Only persist until sealed.
out(...,a,l,t') :− out(...,a,l,t), !sealed(a,l,t), t'=t+1
outCount(c,a,l,t') :− outCount(c,a,l,t), !sealed(a,l,t), t'=t+1
```

Results (§5): BaseVoting 100k → ScalableVoting 250k cmd/s (26 machines); Base2PC 30k → Scalable2PC 160k (46 machines; 2PC with presumed abort, disk log+flush at each step); BasePaxos 50k → ScalablePaxos 150k (29 machines: 2 proposers, 3 p2a proxy leaders and 3 p2b proxy leaders per proposer, 1 coordinator + 3 partitions per acceptor, 3 replicas). Paxos roles in their implementation: f+1 proposers, 2f+1 acceptors; "Each acceptor stores the highest ballot it has received and rejects or accepts payloads into its log based on whether its local ballot is less than or equal to the leader's." Dedalus-on-Hydroflow BasePaxos (50k) beat Whittaker's Scala BasePaxos (25k) on identical hardware; Dedalus CompPaxos 160k vs Scala CompPaxos 130k measured (150k reported). 1-machine-limited comparison at 20 machines: ScalablePaxos 130k.

What rule-driven rewrites **cannot** reproduce (§5.3): CompPaxos's shared proxy leaders across proposers; nacks instead of relaying p2bs; batching, thriftiness, flexible quorums; and **uncoordinated partitioned acceptors whose ballots diverge** — Appendix C gives a concrete non-linearizable (but still safe) CompPaxos execution: a proposer's p1b merge can "read" a later write ("bar") without an earlier one ("foo"); safe only because such a proposer necessarily fails phase 1. Lesson: our optimizer should stay with local, history-preserving rewrites unless a protocol-specific proof is supplied.

Follow-ups: *Bigger, not Badder* (PaPoC'24) adapts decoupling/partitioning to BFT (PBFT 5× on the critical path) by modeling Byzantine nodes (a "Borgesian simulator") — abstract only. **hydro-optimize** (https://github.com/hydro-project/hydro-optimize) automates decoupling + partitioning of Hydro programs using network-cost calibration and a Gurobi ILP to choose rewrites (files: `decouple_analysis.rs`, `partition_ilp_analysis.rs`, `partial_partitioner.rs`, `reduce_pushdown.rs`, `repair.rs`, …). The author's site lists "DistOptimize: Automatic Optimization of Distributed Protocols" as under submission.

### 9.5 Hydro's Paxos (`hydro_test/src/cluster/paxos.rs`, 972 lines) and CompPaxos

Structure (current Hydro, Rust-embedded dataflow with explicit `tick`s and `nondet!` annotations):
* `Ballot { num: u32, proposer_id }` with lexicographic `Ord`.
* `PaxosConfig { f, i_am_leader_send_timeout, i_am_leader_check_timeout, i_am_leader_check_timeout_delay_multiplier }` — leader heartbeats ("I am leader"), expiry check, and **staggered** election triggers (delay = proposer id × multiplier).
* Proposer ballot: if a received max ballot exceeds ours, next ballot num = received.num + 1; "has largest ballot" = received max ≤ ours.
* Acceptor phase 1: `a_max_ballot = max over all p1a ballots across ticks`; reply `Ok(log)` iff p1a ballot == max ballot else `Err(max_ballot)`.
* Quorum collection `collect_quorum_with_response(responses, min=f+1, max=2f+1)` (hydro_std) — persists responses until either min successes or all max responses arrive; reports failures (higher ballots) to preempt.
* `recommit_after_leader_election`: for each slot in the p1b logs keep the highest-ballot value and count matching values; slots with count > f are known committed (skip); slots ≤ max checkpoint skipped; **holes between checkpoint+1 and max slot are filled with `None` (no-op)**; new payloads indexed from max_slot+1.
* Acceptor phase 2: accept p2a iff `p2a.ballot >= max_ballot`; per-slot `reduce_watermark(a_checkpoint, keep higher ballot)` — i.e., **log GC below a checkpoint watermark supplied by replicas**.
* Replicas (`kv_replica`): buffer out-of-order slots, apply the contiguous prefix, emit checkpoints every `checkpoint_frequency` slots back to acceptors.
* Documented non-determinism: "when the leader is changing, payloads may be non-deterministically dropped" — clients must retry.
* CompPaxos (`compartmentalized_paxos.rs`): proxy leaders chosen by slot; acceptors in a grid (rows = write quorums for p2a, columns = read quorums for p1b; Flexible Paxos); `acceptor_retry_timeout` to resend to a different write quorum.

---

## 10. Raft in declarative languages

### 10.1 Bud student implementations (Spring 2013, CS194 Hellerstein/Alvaro, Ongaro advising)

**noeleo/raft** (README): "We believe leader election and log replication are working properly, but recovery has not been tested. We also have not implemented dynamic membership." Decomposed into modules (VERBATIM interfaces): `ServerState` (`set_state`, `set_term`, `set_leader`, `reset_timer` → `alarm`; states ranked `['leader','candidate','follower']` and resolved per tick with `argagg(:min …)`; term only increases via `argagg(:max …)`), `Logger` (`add_log [term, entry, replace_index]`, `commit_logs_before`, `remove_logs_after`, `remove_uncommitted_logs`, outputs `status [last_index, last_term, last_committed]`, `committed_logs`), `VoteCounter` (races won by majority), `SnoozeTimer` (`periodic :timer, 0.1`; `timer_state <+- set_alarm {|a| [Time.new.to_f, a.time_out]}`), `StateMachine`. Vote granting (VERBATIM):

```ruby
    # can only be potential candidate if the candidate's log is at least as complete as our local log
    potential_candidates <= (vote_request * logger.status).pairs do |v, l|
      condition_1 = (v.last_log_term > l.last_term)
      condition_2 = (v.last_log_term == l.last_term and v.last_log_index >= l.last_index)
      v if condition_1 or condition_2
    end
    voted_for_in_current_step <= potential_candidates.argagg(:choose, [], :from) {|v| [v.from]}
    # grant the vote if we haven't voted for anyone else OR if this is the server we already voted for
    vote_response <~ (vote_request * voted_for_in_current_step * st.current_term).combos do |r, v, t|
      grant_vote = (r.from == v.candidate and not voted_for_in_current_term.exists?) or voted_for_in_current_term.include?([r.from])
      [r.from, ip_port, t.term, grant_vote]
    end
    voted_for <+ (voted_for_in_current_step * st.current_term).pairs do |v, t|
      [t.term, v.candidate] if not voted_for_in_current_term.exists?
    end
```

Note the per-tick **`argagg(:choose …)` to pick one candidate among simultaneous requests** — the intra-timestep serialization problem made explicit. Deviations from Raft visible in the code: on timeout it calls `remove_uncommitted_logs` (not in Raft); one entry per AppendEntries; commit counting by an index-keyed vote race without the current-term restriction; `voted_for` is an in-memory `table` updated with `<+` (next tick) while the vote reply leaves with `<~` in the same tick, and nothing is written to stable storage at all (README: "recovery has not been tested") — **no durability, hence no persist-before-reply barrier**.

**amidvidy/whitewater** (README: "a few issues that could potentially cause data loss or corruption … we do not recommend the use of whitewater in production"). Interesting because it uses **Bloom^L lattices** for Raft state (VERBATIM `serverstate.rb`):

```ruby
  state do
    lmax :term
    lmax :term_voted
    table :members, [:host]
    table :role, [] => [:role]
  end
  bloom :update do
    term <= update_term { |t| Bud::MaxLattice.new t.term }
    term_voted <= update_max_term_voted { |t| Bud::MaxLattice.new t.max_term }
    role <+- update_role { |r| r if ServerStateImpl::ROLES.include? r }
  end
```

and `lmax :max_index_committed` for the commit index. Its follower rule `max_index_committed <= append_entry_buffer do |as| Bud::MaxLattice.new(as.commit_index) end` adopts the leader's commit index **without capping it at the index of the last new entry** (Raft Fig. 3.1: `commitIndex = min(leaderCommit, index of last new entry)`), and because failed AppendEntries are also routed into `append_entry_buffer` (`append_entry_buffer <+ append_entry_failure do |aef| [..., aef.commit_index, false] end`) the commit index is raised **even when the consistency check failed**. A follower holding a stale uncommitted suffix from an old term (e.g. indices 5..7) that receives a *rejected* AppendEntries from a new leader whose commit index is 10 will then apply those stale entries (`to_commit` selects log entries with `index <= max_index_committed`) — a State Machine Safety violation. (The lattice itself was not the problem; the missing guard and cap were.) Also the author comments "this is extremely inefficient (must compute the cross product of all log entries…)" for building AppendEntries via `(next_indices * log * log * current_term).combos` — **we need indexed lookups by key (log[i]) and range scans (log[next..]) as first-class operations**.

### 10.2 Molly's Raft sketch (`examples_ft/raft/{raft,election,clock,raft_edb,raft_assert}.ded`)

Incomplete ("// a stub till I figure out commit indexes", "// need to do client retries, obvs."), but instructive Dedalus patterns (VERBATIM):

```
role(N, R)@next :- role(N, R), notin role_change(N, _);
role_x(N, max<I>) :- role_change(N, R), rank(N, R, I);
role(N, R)@next :- role_x(N, I), rank(N, R, I);          // rank: F=1, C=2, L=3 resolves conflicting transitions
term(N, T)@next :- term(N, T), notin stall(N, T);
term(N, T)@next :- new_term(N, T);
new_term(N, T+1) :- term(N, T), stall(N, T);
stall(Node, Term)@next :- lclock(Node, "Localtime", Term, Time), last_append(Node, Term, Last),
            current_term(Node, Term), notin role(Node, "L"), Time - Last > 1;
winner(Node, Term, min<Id>) :- request_vote(Node, Term, Candidate, _, _), member(Node, Candidate, Id);
vote(Candidate, Node, Term, "T")@async :- accept_vote(Node, Candidate, Term);
yes_vote_cnt(Node, Term, count<Id>) :- vote_log(Node, Member, Term, "T"), member(Node, Member, Id);
role_change(N, "L") :- yes_vote_cnt(N, _, Cnt1), member_cnt(N, Cnt2), Cnt1 > Cnt2 / 2;
safe(Leader, Term, Idx + 1) :- ack_cnt(Leader, Term, Idx, Cnt1), member_cnt(Leader, Cnt2), Cnt1 > Cnt2 / 2;
lclock(Host, Type, Id, 0) :- lclock_register(Host, Type, Id);
lclock(Host, Type, Id, Time + 1)@next :- lclock(Host, Type, Id, Time), notin lclock_unreg(Host, Id);
```

Assertions (VERBATIM `raft_assert.ded`):

```
bad(N1, N2, "disagree") :- log(N1, Idx, _, _, Entry), log(N2, Idx, _, _, Entry2), Entry != Entry2, notin crash(_, N2, _), notin crash(_, N1, _);
bad(N1, N2, "two leaders") :- leader(_, T, N1), leader(_, T, N2), N1 != N2;
```

Patterns to adopt: **role as a keyed single-row relation with a rank-based conflict resolution aggregate**; **logical clocks as per-key tick counters** (`lclock`); **per-term min-id `winner` to choose one vote**; global `bad/good` invariants evaluated over all nodes' histories (Molly's verification harness). Gaps: it never implements the §5.4.1 up-to-date check correctly (compares `log_term` to the candidate's *term*), no current-term commit restriction, no persistence semantics.

### 10.3 Hydro's Raft (`hydro_test/src/cluster/raft.rs`, 2,656 lines incl. tests) — the key lesson

Design statement (VERBATIM module docs):

> "The protocol logic is a pure, sequential step function over one unified per-member state (see [`raft_step`]), hosted in a single dataflow tick — deliberately *not* decomposed into separately-ticking election/replication components, because RAFT's safety proofs interlock vote and log decisions through shared state"

> "Keeping `term` / `voted_for` / `log` in one struct, mutated by one sequential step function ([`raft_step`]), is what makes RAFT's safety argument hold: the decision to grant a vote and the decision to acknowledge log entries are read-modify-writes against the *same* state, so no schedule can interleave them against stale views of each other. (A previous design split this state across two dataflow components with asynchronous feedback, which was a real, simulator-reproducible safety bug: committed entries could be truncated.)"

> "Message processing is order-insensitive at the batch level: the batch is sorted into a canonical order first, so the outcome is a function of the batch multiset (required for deterministic simulation)."

State (`RaftServerState`): `term`, `voted_for`, `role`, `votes: HashSet`, `heartbeat_seen` (suppresses the next election), `known_leader`, `log: Vec<LogEntry{message, term_received, index}>`, `commit_index`, `emitted_index` (exactly-once emission of committed entries), `next_index`, `match_index` (both reset on each leadership acquisition). Inputs per tick: election-timer-fired, heartbeat-timer-fired, client requests (order fixed by `assume_ordering`, declared non-determinism), and all intra-cluster messages over **one** channel (`RaftRpc = RequestVote | RequestVoteResponse | AppendEntries | AppendEntriesReply`). Canonical sort key: `(kind, term, …)` then sender.

Step semantics (condensed from code): (a) `observe_term`: higher term ⇒ follower, clear `voted_for`/`votes`/`known_leader`; (b) RequestVote: stale ⇒ ignore; grant iff `(last_log_term, last_log_index) >= mine` and (`voted_for` is None or == sender); (c) RequestVoteResponse: count if candidate, majority ⇒ become leader (`next_index = len+1`, `match_index = 0`); (d) AppendEntries: stale ⇒ reply failure with our term; **assert no other leader in same term**; set `heartbeat_seen`; candidate ⇒ follower; log-matching check on `prev_log_index/prev_log_term`; append with truncate-on-conflict and skip-if-present, **asserting truncation never touches committed entries**; commit ⇐ `min(leader_commit, prev_log_index + entries.len())`; ack with `match_index`; (e) AppendEntriesReply: stale-term acks ignored ("the follower's log may have been truncated since, so it must not count toward this term"); success ⇒ monotone max of match/next; failure ⇒ `next_index -= 1` (≥1); client requests: leader appends, others redirect with leader hint; election timer: leader ignores; if `heartbeat_seen` consume it, else `term += 1`, candidate, vote self, broadcast RequestVote (single-node cluster wins immediately); leader commit: scan down from last index for an entry of **the current term** with a majority of `match_index >= N` (§5.4.2), iterating the fixed member list for determinism; heartbeat timer: send each follower `log[next-1..]` plus `leader_commit`; emit newly committed entries in order.

Not implemented in Hydro's Raft (by inspection): persistence/restart, snapshots, membership changes, PreVote, leadership transfer, read index/leases, client sessions. Channel fault model is caller-chosen: sim uses `TCP.fail_stop()`; deployments `TCP.lossy_delayed_forever()`.

Tests present (names VERBATIM) — reuse as our Raft test suite (§17): `even_cluster_simultaneous_candidates_exactly_one_leader_per_term`, `heartbeats_converge_leader_views`, `vote_and_ack_decisions_interlock`, `leader_steps_down_on_higher_term_reply`, `stale_log_candidate_is_refused`, `leader_replicates_and_commits_requests`, `non_leader_redirects_requests`, `leader_without_quorum_commits_nothing`, `new_leader_overwrites_conflicting_uncommitted_entries`, `previous_term_entries_commit_only_transitively`, `composed_raft_elects_replicates_and_suppresses`, `concurrent_elections_never_fork_the_committed_log`, `fully_concurrent_run_never_forks_the_committed_log`.

---

## 11. Mapping Raft onto a Dedalus / Bloom^L model (design analysis)

### 11.1 The central semantic hazard: a timestep sees a *batch*

In Dedalus, all facts that arrive at a node for timestep t are visible simultaneously; deductive rules run to fixpoint over the snapshot of state at t; mutations land at t+1; messages leave asynchronously. Raft's pseudocode is per-message sequential. Therefore every Raft rule set must define a **serialization of the tick's batch** and all rules must be consistent with it. Hydro abandoned a *multi-component* design (vote logic and log logic in different components exchanging state asynchronously): the unsafe schedule is a voter granting a vote in term T′ using a log view that does not include entries it has already acknowledged to the leader of term T (or vice versa), which breaks Leader Completeness and can truncate a committed entry.

Rules that keep a single-node, single-timestep formulation safe (our analysis):

1. **All Raft state of a node lives in one component/location and is read at the same timestep.** Never decouple Raft's vote state from its log state (the SIGMOD'24 preconditions agree: Raft's server is neither monotone nor functional; only state-machine decoupling with ordered batching would be legal).
2. **Term first.** Compute `eff_term = max(current_term, max term in this tick's messages)` (a monotone `lmax` join). Treat every message with `term < eff_term` as stale (reject/ignore). This corresponds to the serialization "process the highest-term message first".
3. **At most one vote per term per tick.** Among up-to-date requesters for `eff_term`, pick one deterministically (`argmin` on node id, or `choose` with a seeded tie-break) unless `voted_for(eff_term, C)` already exists (then only re-grant to C). This is I Do Declare's *choice* idiom; noeleo used `argagg(:choose)`, Molly used `min<Id>`.
4. **Votes and acks in the same tick must be judged against a consistent log.** Case analysis: (a) AE(T) and RV(T′>T) in one tick → AE is stale by rule 2; no ack is sent; the vote reads the pre-tick log: equivalent to "RV then AE". (b) AE(T) and RV(T) in the same tick for the same term T: a leader already exists for T, and a voter may grant its (single) T-vote to a different candidate only if it had not voted — that candidate cannot gain a majority in T because the leader already holds one; safe. So the formulation is safe **provided** rule 2 and rule 3 hold and all reads are from the same pre-tick snapshot. When in doubt, use the fallback in (5).
5. **Fallback: serialize with an atomic dequeue.** Admit at most one state-changing Raft message per tick (I Do Declare Fig. 5 `top_of_queue`/`delete` idiom; bud-sandbox `ordering/serializer.rb`), buffering the rest with `@next`. Correct by construction, lower throughput. Our language should also provide a **sanctioned sequential-fold operator** (fold over the tick's batch sorted by a canonical key, like Hydro's `raft_step`) so that performance-critical protocols can be written as a proven step function *inside* a declarative program, with the canonical ordering enforced by the compiler (the fold input must be sorted; the fold's non-commutativity is then acceptable because it depends only on the multiset).

### 11.2 State classification

| Raft variable (dissertation Fig. 3.1) | Persistence | Monotone? | Suggested representation |
|---|---|---|---|
| `currentTerm` | **durable**, persist before replying | yes | `lmax<u64>` |
| `votedFor` | **durable**, per term | no (local first-writer-wins per term) | keyed table `voted_for[term] -> node`; write-once per key with an assertion |
| `log[]` | **durable**, entries persisted before counted/acked | no (conflicting suffix truncation) | keyed table `log[index] -> (term, entry)` with deletion; range scans |
| `commitIndex` | volatile (may reset to 0 on restart, §3.8) | yes (within a run) | `lmax<u64>` |
| `lastApplied` | volatile (durable if state machine is persistent, §3.8) | yes | `lmax<u64>` |
| `nextIndex[]` | volatile leader state | no (decrements) | keyed table with upsert |
| `matchIndex[]` | volatile leader state | yes within a term | `lmap<(term, follower), lmax>` |
| votes received | volatile | yes | `lmap<term, lset<node>>`; `size ≥ majority` is a monotone threshold (CALM-friendly) |
| role | volatile | no | keyed single row + rank-based resolution (Molly) |
| known leader | volatile | per term: at most one | `leader_of[term] -> node` + invariant |
| election deadline | volatile | no | `last_heard` timestamp vs. `now` input |
| configuration | derived from log (latest config entry, committed or not, §4.1) | no (can roll back when log truncated) | derived view `argmax index` over config entries |
| snapshot (state, lastIncludedIndex/Term, config) | durable | yes (index only increases) | blob table + `lmax` index |
| client sessions | part of replicated state machine | no | table `session[client] -> (last_seq, response)` |

### 11.3 SKETCH (ours): core Raft rules in Dedalus-with-lattices pseudo-syntax

Conventions: `@next` = inductive, `@async` = message, `notin` = stratified negation, `lmax`/`lset` = Bloom^L lattices, `now(N, Ms)` = per-tick clock input, `random_timeout(N, D)` = seeded non-deterministic input. `durable` relations must be fsynced before the tick's `@async` facts are released (§12).

```
// ---------- term handling
msg_term(N, T) :- request_vote(N, T, _, _, _).
msg_term(N, T) :- vote_reply(N, T, _, _).
msg_term(N, T) :- append_entries(N, T, _, _, _, _, _).
msg_term(N, T) :- append_reply(N, T, _, _, _).
eff_term(N) <= current_term(N) ⊔ max<T> msg_term(N, T)            // lmax join
stepped_down(N) :- msg_term(N, T), current_term(N, C), T > C.
current_term(N)@next <= eff_term(N) ⊔ (eff_term(N) + 1 if start_election(N))

// ---------- election timer (physical time as input; logical in simulation)
heard_valid_leader(N) :- append_entries(N, T, L, _, _, _, _), eff_term(N) == T.
last_heard(N, Ms)@next :- heard_valid_leader(N), now(N, Ms).
last_heard(N, Ms)@next :- vote_granted_this_tick(N), now(N, Ms).
last_heard(N, Ms)@next :- last_heard(N, Ms), notin heard_valid_leader(N), notin vote_granted_this_tick(N).
start_election(N) :- now(N, Ms), last_heard(N, H), deadline(N, D), Ms - H > D, notin role(N, "leader"),
                     notin heard_valid_leader(N), notin stepped_down_to_higher_leader(N).
deadline(N, D)@next :- start_election(N), random_timeout(N, D).     // [T, 2T]

// ---------- voting (voter side)
up_to_date(N, C) :- request_vote(N, T, C, LLI, LLT), eff_term(N) == T, last_log(N, MyI, MyT),
                    (LLT > MyT ; (LLT == MyT, LLI >= MyI)).
may_vote(N, C) :- up_to_date(N, C), eff_term(N) == T, notin voted_for(N, T, _),
                  notin leader_recently_heard(N).                  // disruptive-server guard, §4.2.3 (unless RV.transfer flag)
may_vote(N, C) :- up_to_date(N, C), eff_term(N) == T, voted_for(N, T, C).
grant(N, T, min<C>) :- may_vote(N, C), eff_term(N) == T.            // CHOICE: at most one per tick
voted_for(N, T, C)@next :- grant(N, T, C).                          // durable
voted_for(N, T, C)@next :- voted_for(N, T, C).
vote_reply(C, T, N, true)@async :- grant(N, T, C).                  // released only after voted_for is durable

// ---------- candidate side
votes(C, T) <= lset<N> :- vote_reply(C, T, N, true), eff_term(C) == T, role(C, "candidate").
won(C, T) :- |votes(C, T)| + 1 >= majority(C), eff_term(C) == T.     // monotone threshold on a set lattice
role(C, "leader")@next :- won(C, T), notin stepped_down(C).
log(C, I+1, T, noop)@next :- won(C, T), last_log(C, I, _).           // no-op at start of term (§6.4 / §3.6.2)

// ---------- follower: AppendEntries
ae_ok(N, L, PI, Es, LC) :- append_entries(N, T, L, PI, PT, Es, LC), eff_term(N) == T,
                           (PI == 0 ; log(N, PI, PT, _)).
ae_bad(N, L, T)          :- append_entries(N, T, L, PI, PT, _, _), eff_term(N) == T, PI != 0, notin log(N, PI, PT, _).
conflict(N, I) :- ae_ok(N, _, _, Es, _), entry(Es, I, ET, _), log(N, I, LT, _), ET != LT.
first_conflict(N, min<I>) :- conflict(N, I).
truncate(N, I) :- first_conflict(N, F), log(N, I, _, _), I >= F.
log(N, I, T, X)@next :- log(N, I, T, X), notin truncate(N, I).
log(N, I, T, X)@next :- ae_ok(N, _, _, Es, _), entry(Es, I, T, X), notin log_same(N, I, T).
die("truncating committed entry") :- truncate(N, I), commit_index(N) >= I.     // Hydro's assertion
commit_index(N) <= min(LC, PI + len(Es)) :- ae_ok(N, _, PI, Es, LC).          // lmax; capped (whitewater bug)
append_reply(L, eff_term, N, true, PI + len(Es))@async :- ae_ok(N, L, PI, Es, _).
append_reply(L, eff_term, N, false, hint)@async :- ae_bad(N, L, _).            // hint: conflict term & first index (§3.5)

// ---------- leader: replication and commit
match(L, T, F) <= lmax<M> :- append_reply(L, T, F, true, M), eff_term(L) == T, role(L, "leader").
next_index(L, F, M+1)@next :- append_reply(L, T, F, true, M), ...                 // upsert, monotone per success
next_index(L, F, max(1, X-1))@next :- append_reply(L, T, F, false, _), next_index(L, F, X), ...
replicas_at(L, I) <= count<F> :- log(L, I, T, _), current_term(L) == T, match(L, T, F) >= I.
commit_index(L) <= max<I> :- replicas_at(L, I) + 1 >= majority(L).             // only current-term entries (§3.6.2)
append_entries(F, T, L, NI-1, term_at(NI-1), log[NI..NI+batch], commit_index(L))@async :-
    heartbeat_tick(L), role(L, "leader"), current_term(L) == T, follower(L, F), next_index(L, F, NI).

// ---------- apply (ordered fold)
to_apply(N, I, X) :- log(N, I, _, X), last_applied(N) < I, I <= commit_index(N).
state(N)@next = fold_ordered(state(N), to_apply(N, I, X) order by I)          // requires an ordered-fold operator
last_applied(N) <= max<I> :- to_apply(N, I, _).
```

Everything in this sketch that is non-monotone (voted_for choice, log truncation, next_index decrement, role) is local to one node, which is fine for correctness but means the Raft server must not be decoupled or partitioned by an optimizer (§9.4 preconditions fail) — only its *monotone periphery* (e.g., broadcasting AppendEntries = functional decoupling; counting acks = monotone decoupling, like Scalable Paxos's p2a/p2b proxies) can be offloaded.

### 11.4 Timers and time

* Election timeout randomized per server in a range (dissertation: typically 10–500 ms; timing requirement `broadcastTime ≪ electionTimeout ≪ MTBF`, §3.9). Heartbeat period ≪ election timeout.
* Needs: periodic sources (`periodic`/`timer(… physical …)`), a `now()` input per tick, seeded randomness as an input relation, and **logical timers** (Molly `timeout_svc`, `lclock`) for simulation and lineage-driven fault injection.
* Liveness helpers: Kirsch–Amir's progress-timer doubling; Hydro Paxos's staggered checks; PreVote (§9.6).

### 11.5 Persistence and durability

Dissertation §3.8: "each server persists its current term and vote … Each server also persists new log entries before they are counted towards the entries' commitment"; commit index may be volatile; persistent state machines must also persist `lastApplied`; a server that loses persistent state must rejoin with a new identity via a membership change. Engine requirements: per-relation `durable` storage class; WAL + fsync at the end of each timestep *before* the timestep's async outputs are released (JOL phase 3 / Stasis); recovery = reload durable relations then resume at a fresh timestep; group commit across ticks for throughput; §10.2.1 optimization — "writing to the leader's disk in parallel" with sending AppendEntries (the leader counts itself toward the majority only once its own write completes). autocomp's 2PC models this with an explicit completion relation: `logVoteComplete(client, id, p) :+ voteToParticipant(client, id, p)` then `voteFromParticipant@addr(...) :~ logVoteComplete(...)` (VERBATIM).

### 11.6 Client interaction (dissertation ch. 6)

* Find leader / redirect with leader hint (Hydro `redirected` output); a leader steps down if an election timeout elapses without a successful heartbeat round to a majority (§6.2); followers discard leader identity on term change.
* **Linearizable writes**: client id + serial numbers; the state machine keeps a session per client with latest serial and response; concurrent requests per client via the "lowest unacknowledged sequence number" watermark; **deterministic session expiry** using leader timestamps stored in log entries; `RegisterClient` RPC; unknown-session commands return an error (§6.3).
* **Read-only queries via ReadIndex** (§6.4): (1) leader must have committed an entry of its current term (the no-op); (2) `readIndex = commitIndex`; (3) a heartbeat round acknowledged by a majority (amortizable over many reads); (4) wait `lastApplied ≥ readIndex`; (5) execute. Followers can serve reads by asking the leader for a readIndex. In rules: `read_ok(R) :- read(R, RI), acks(R) >= majority, last_applied >= RI` — the acks count and `last_applied ≥ RI` are monotone thresholds.
* **Lease reads** (§6.4.1): after majority heartbeat acks at time `start`, lease valid until `start + electionTimeout / clockDriftBound`; must expire the lease before leadership transfer; requires bounded clock drift.

### 11.7 Log compaction (dissertation ch. 5)

Each server snapshots independently (committed prefix only); store `lastIncludedIndex`, `lastIncludedTerm`, and the latest configuration as of that index, then discard the log prefix and older snapshots; the snapshot must include client-session state. Leader sends `InstallSnapshot` (chunked, in order; each chunk resets the follower's election timer) when it has discarded the entry needed for `nextIndex`. Follower: if the snapshot contains information beyond its log, discard the entire log; if it describes a prefix, delete only covered entries. Snapshot concurrently via copy-on-write (immutable data structures or fork); when to snapshot = size threshold vs. log size; incremental approaches (log cleaning, LSM) as alternatives (§5.2–5.3). Engine needs: blob-valued tuples or out-of-band streaming, bulk deletion of a range, "snapshot of a relation as of timestep t" (persistent/immutable collections make this cheap), and the prefix-GC idiom (Hydro's `reduce_watermark(checkpoint, …)`; PMMC §4.2 acceptor GC once all replicas learned).

### 11.8 Membership changes (dissertation ch. 4)

* **Single-server changes** (§4.1): config is a log entry; each server uses the latest config in its log whether or not committed; one change at a time (next change only after C_new commits); servers accept AppendEntries and grant votes to servers not in their config.
* **2015 correction** (raft-dev "bug in single-server membership changes", via search summary): a leader must not begin replicating a new configuration entry while an older configuration entry might still commit; the fix is that **a newly elected leader must commit an entry in its current term (the no-op) before appending a configuration change**.
* **Catch-up of new servers** (§4.2.1): add as non-voting learner first; replicate in rounds; after a fixed number of rounds (e.g. 10), add if the last round took less than an election timeout, else abort; followers can return log length on rejection to speed nextIndex convergence.
* **Removing the leader** (§4.2.2): either transfer leadership first, or leader manages C_new without counting itself and steps down once C_new commits.
* **Disruptive servers** (§4.2.3): "if a server receives a RequestVote request within the minimum election timeout of hearing from a current leader, it does not update its term or grant its vote"; leadership-transfer RequestVotes carry a flag to bypass this.
* **Joint consensus** (§4.3): C_old,new entry; agreement requires separate majorities of both; then C_new.
* In rules: `quorum_ok(X) :- |acks(X) ∩ C_old| > |C_old|/2, |acks(X) ∩ C_new| > |C_new|/2` — needs set-intersection/cardinality on lattices and a derived "effective configuration" view that can roll back on truncation.

### 11.9 Other extensions

* **Leadership transfer** (§3.10): stop accepting client requests, bring target up to date, send `TimeoutNow`; abort after ~an election timeout.
* **PreVote** (§9.6): increment term only after a majority says it would vote (log up to date and they haven't heard from a leader within the baseline timeout).
* **Log-mismatch acceleration** (§3.5): reply with conflicting term and first index of that term, or binary search.
* **Performance** (ch. 10): batching and pipelining of AppendEntries; parallel leader disk write.

---

## 12. Full feature lists

### 12.1 Full Raft (Ongaro 2014 dissertation)

1. Roles follower/candidate/leader; terms; RequestVote and AppendEntries RPCs (§3.3–3.5).
2. Election: randomized timeouts, self-vote, majority wins, heartbeats suppress elections, one vote per term (§3.4).
3. Election restriction: candidate log at least as up to date (last term, then last index) (§3.6.1).
4. Log matching: prevLogIndex/prevLogTerm check; truncate conflicting suffix only on real conflict; never truncate in the leader (Leader Append-Only) (§3.5).
5. Commit rule: leader commits only current-term entries by counting; earlier entries commit indirectly (§3.6.2, Fig. 3.7).
6. Follower commitIndex = min(leaderCommit, last new entry).
7. Apply committed entries in order exactly once; State Machine Safety (§3.6.3).
8. Step down on higher term in any message; stale-term handling.
9. Persistence of currentTerm, votedFor, log before replying (§3.8); restart recovery.
10. Timing requirement and parameter guidance (§3.9); PreVote (§9.6).
11. Leadership transfer with TimeoutNow (§3.10).
12. Membership: single-server changes, learners/catch-up rounds, leader removal, disruptive-server protection, joint consensus (ch. 4); 2015 fix.
13. Log compaction: snapshots, InstallSnapshot (chunked), concurrent snapshotting, when to snapshot, disk-based state machines, log cleaning/LSM alternatives, snapshots in the log / leader-based (ch. 5).
14. Client interaction: finding the cluster, redirect/proxy, leader step-down without majority heartbeat, sessions for linearizability, session expiry, RegisterClient, ReadIndex reads (incl. follower reads), lease reads (ch. 6).
15. Implementation/perf: parallel leader disk write, batching, pipelining (ch. 10).
16. Correctness: formal TLA+ spec and proof exist for basic Raft (ch. 8, App. B) — useful as a reference model for checking our implementation's traces.

### 12.2 Full Multi-Paxos (Lamport; Kirsch–Amir; PMMC; Paxos Made Live; Hydro/autocomp)

1. Ballots totally ordered `(round, leader_id)`; uniqueness per leader.
2. Acceptor: promised ballot, accepted pvalues per slot, persisted; p1b includes accepted values (state reduction: only the max-ballot pvalue per slot, PMMC §4.1).
3. Phase 1 once per leadership over all slots (or over slots above ARU/checkpoint); "sealed" multi-message p1b.
4. Leader recovery: per slot choose the value with highest ballot (pmax); fill holes with no-ops; resume slot numbering at max+1.
5. Phase 2 per slot: p2a/p2b; majority (or flexible read/write quorum) commit; preemption on higher ballot (nack).
6. Stable-leader liveness: heartbeats ("I am leader"), expiry, staggered or backoff-based re-election (Kirsch–Amir progress timer doubling; view-change VC/VC_Proof).
7. Replicas/learners: apply decided commands in slot order; buffer out-of-order; exactly-once via client id + timestamp (`Last_Executed[]`, PfSB) and respond.
8. Catch-up / reconciliation of lagging replicas (PfSB "reconciliation"; PML "catch-up").
9. Log compaction: checkpoints/snapshots; acceptor GC once all replicas have executed (PMMC §4.2; Hydro checkpoint watermark); snapshots unsynchronized across replicas (PML §5.5).
10. Reconfiguration: via the log (PMMC CSUR version: a configuration decided at slot s takes effect at s+WINDOW — described here from paxos.systems, not from the PDF I read); PML §5.4 notes gaps in the literature it had to fill.
11. Read-only commands: leases (PMMC §4.4; PML §5.2 master leases), or through the log.
12. Epoch numbers to detect master changes across a sequence of operations (PML §5.3).
13. Disk corruption handling: rejoin as non-voting until caught up (PML "Handling disk corruption").
14. Batching, pipelining, flow control (PfSB), thriftiness, proxy leaders (compartmentalization), partitioned acceptors, grid quorums (Hydro/autocomp CompPaxos).
15. Testing modes: safety mode and liveness mode with fault injection; runtime consistency checks (PML §6.2–6.3).

---

## 13. Requirements for our engine derived from this cluster

* Timestep = (ingest events) → (stratified fixpoint) → (atomic durable commit of durable relations' next-state) → (emit messages / callbacks). Messages derived at t must not be released before t's durable writes are fsynced (group commit allowed).
* Storage classes: `table` (persistent in memory), `durable table` (WAL+fsync), `scratch` (per tick), `channel` (async, location-addressed), `interface input/output` (module boundary), `periodic`/`timer` (physical and logical), `lattice` relations (lmax, lmin, lset, lmap, lbool, lexicographic pair).
* Keys with upsert semantics (declared keys; last-writer within a tick must be resolved deterministically or rejected at compile time).
* Deletion (`<-` / `delete`) at t+1; range deletion (log truncation / prefix GC).
* Aggregates: count, sum, min, max, avg, set/accum, argmin/argmax (`argagg`), choose (seeded), percentile/quantile, and **ordered fold** over a sorted batch (for state machine application and optional step functions); `index()` per-tick enumeration.
* Stratified negation with compile-time stratification check (JOL did this *in Overlog*).
* Host-language extensibility: opaque host values in tuples, UDFs, UDAs, table functions, output callbacks for bulk data paths (BOOM-FS data protocol, BOOM-MR JobConf).
* Metaprogramming: rules/predicates/programs as catalog relations; runtime rule install; per-rule firing counters; rule rewriting for tracing and coverage; `die`/assert relations mapped to hard errors.
* Location model: only single-location bodies; heads may target a different location (async). Partition declarations and routing for sharding (hash of key); replication of a component via consensus as a rewrite.
* Deterministic simulation: time, randomness, and network delivery as inputs; canonical ordering of batches; logical timers — needed for Molly-style LDFI and Hydro-style simulator fuzzing.

---

## 14. MUST-IMPLEMENT CHECKLIST

Language/runtime features:

1. **Three-phase timestep** — ingest → fixpoint → atomic durable commit → emit; messages from tick t released only after t's durable deltas are fsynced. (EuroSys 2010 §2 Fig. 2; Raft diss. §3.8)
2. **Per-relation durability classes** (durable / in-memory persistent / scratch). (EuroSys §3.1 "durability … per-table basis"; Bud `table`/`scratch`/`sync`)
3. **Primary keys with upsert** (key collision replaces) and compile-time detection of same-tick conflicting writes. (I Do Declare Figs. 1–2; JOL `define(…, keys(…), …)`)
4. **Deletion rules** applied at t+1, including range deletion for log truncation and prefix GC. (JOL `delete`; Bloom `<-`; Raft §3.5, ch. 5)
5. **Stratified negation (`notin`) with a stratification checker.** (JOL `stratachecker.olg`)
6. **Aggregates**: count, sum, min, max, avg, set/accum, argmin/argmax, choose, percentile<p,X>. (Overlog Paxos; TR Fig. 8; BFS)
7. **Ordered fold / sequential step over a canonically sorted batch** (apply log in index order; optional Raft step). (Hydro `raft_step`; kv_replica)
8. **Per-tick enumeration `index()`** for assigning consecutive slots to a batch. (autocomp MultiPaxos `IndexedPayloads`)
9. **Physical timers/periodics and logical timers** from the same source program. (JOL `timer(…, physical|logical, …)`; Molly `timeout_svc.ded`, `lclock`)
10. **Time and randomness as per-tick input relations** (no `currentTimeMillis()` inside rules without modeling). (TR Fig. 8 uses it; Dedalus/Molly)
11. **Location specifiers restricted to heads (async send); single-location bodies.** (EuroSys §9.2; SIGMOD'24 §2.3)
12. **Lattice types incl. lexicographic pair (ballot), lmax, lset, lmap, lbool; monotone threshold tests.** (whitewater `lmax`; Paxos ballot `(num,id)`)
13. **Sealing construct** for multi-tuple messages (count shipped with message, receiver waits for count). (Overlog Paxos `Len`; autocomp p1b `logSize`; SIGMOD'24 App. B.4)
14. **Choice + atomic dequeue / serializer idiom** in the standard library. (I Do Declare Fig. 5; bud-sandbox `ordering/serializer.rb`)
15. **Standard library idioms**: multicast, roll call, barrier, voting (all/majority), sequence, timeout, heartbeat/failure detector, nonce, reliable delivery. (I Do Declare §2.1; bud-sandbox)
16. **Host-language extensibility**: opaque values in tuples, UDFs, UDAs, table functions, output-event callbacks. (EuroSys §2.1; TR §3.1.3)
17. **Catalog/metaprogramming**: programs, rules, predicates as queryable relations; runtime rule install/uninstall. (JOL `compile.olg`, `runtime.olg`; EuroSys §6.2)
18. **Trace rewriting and code coverage** as metaprograms (per-rule firing counts, tuple counts, network-flagged traces). (EuroSys §6.2–6.3)
19. **`die`/assert relations** that raise hard errors; distributed watchdogs that ship facts to a checker. (EuroSys §6.1; overlog-paxos `assertions.olg`)
20. **Incremental view maintenance of recursive views with deletions**, with per-view materialization control. (EuroSys §3.1 fqpath)
21. **Indexed point lookups and range scans** on keyed relations (log[i], log[i..]). (whitewater comment on cross-product inefficiency)
22. **Declarative partitioning** (partition key + routing), composable with consensus replication. (EuroSys §5)
23. **Correct-by-construction rewrites**: mutually independent / monotonic / functional decoupling, co-hashing and FD/CD partitioning, partial partitioning, sealing — with the stated preconditions. (SIGMOD'24 §3–4, App. A–B)
24. **Deterministic simulation / fault injection hooks** (message delay/loss/crash as inputs; canonical batch order). (Hydro sim tests; Molly)

Protocol features (flagship programs):

25. **2PC** with timeout-abort and presumed-abort logging. (I Do Declare Figs. 1–2; SIGMOD'24 §5.2)
26. **Multi-Paxos** complete: ballots, p1 with log return, pmax recovery, hole filling with no-ops, p2 with preemption, stable leader heartbeats, replicas applying in order, checkpoint-driven acceptor GC. (autocomp multipaxos; Hydro paxos.rs; PMMC)
27. **Kirsch–Amir leader election** variant (views, progress timer doubling, VC_Proof) as an alternative election module. (overlog-paxos `election.olg`; PfSB Fig. 6)
28. **Raft core**: terms, elections with §5.4.1 restriction, log matching with conflict-only truncation, current-term commit rule, capped follower commit, in-order apply. (Diss. ch. 3)
29. **Raft durability**: term/vote/log persisted before replies; restart recovery. (Diss. §3.8)
30. **Raft single-node-state rule**: vote and log decisions in one component, one tick, canonical serialization; never decoupled. (Hydro raft.rs design notes)
31. **Raft membership**: single-server changes + no-op-before-config fix + learners/catch-up + leader removal + disruptive-server guard + joint consensus. (Diss. ch. 4; raft-dev 2015)
32. **Raft snapshots + InstallSnapshot (chunked)**, snapshot includes sessions and config. (Diss. ch. 5)
33. **Raft client semantics**: redirect, sessions/exactly-once with deterministic expiry, RegisterClient, ReadIndex, lease reads, leader step-down without majority heartbeat. (Diss. ch. 6)
34. **Raft extensions**: PreVote, TimeoutNow leadership transfer, fast nextIndex backtracking, batching/pipelining, parallel leader fsync. (Diss. §3.5, §3.10, §9.6, ch. 10)
35. **BOOM-FS equivalent**: file/fqpath/fchunk/datanode/hb_chunk schema, metadata RPCs, heartbeats with expiry, re-replication below replication factor, out-of-band data path. (EuroSys §3; bud-sandbox bfs)
36. **HA NameNode via consensus log** ("decrees" intercepted into the log; actions complete when read back). (EuroSys §4.2)
37. **Partitioned NameNode** by hash(fqpath) with idempotent broadcast mkdir/ls. (EuroSys §5)
38. **BOOM-MR equivalent**: job/task/taskAttempt/taskTracker schema, FCFS + LATE policies as rule sets over incremental statistics. (EuroSys §7; TR Fig. 8; LATE §4)

---

## 15. TEST PROGRAMS (end-to-end), with expected behavior

| # | Program | Source | Expected behavior / assertions |
|---|---|---|---|
| T1 | 2PC coordinator (Fig. 1) + timeout abort (Fig. 2) | I Do Declare | Commit iff all peers vote yes; abort on any "no"; if not committed after >10 ticks in "prepare", abort; all peers learn final state via multicast. |
| T2 | 2PC variants from Molly (`commit/2pc.ded`, `2pc_timeout.ded`, `2pc_ctp.ded`, `3pc.ded` + asserts) | Molly repo | 2PC: under coordinator crash, agents block (liveness violation found by fault injection); 3PC variants behave per their `*_assert.ded` files. |
| T3 | Scalable2PC / ScalableVoting (autocomp `twopc`, `autotwopc`, `voting`, `autovoting`) | SIGMOD'24 | Rewritten program's client-visible outputs match base program's; throughput scales with partitions (reference: 30k→160k, 100k→250k cmd/s on their GCP setup). |
| T4 | Paxos Synod (`paxos_synod.ded`) | Molly | `disagree(M)` never derivable; `good("yay")` holds in failure-free runs; with injected omissions, safety still holds (liveness may not). |
| T5 | NetDB'09 Overlog Multi-Paxos (prepare/propose/election) ported verbatim to our syntax, driven by `insertions.olg` workload | recovered overlog-paxos | `lt("fail")` never derived: no "Local Conflict", no "Distributed Conflict", no "two quorums for the same view"; `lt("succeed")` after 200 globally ordered decrees. |
| T6 | Dedalus MultiPaxos leader+acceptor (autocomp `multipaxos`) | autocomp | Replicas receive `clientOut(payload, slot)` for every accepted request, each slot once, same payload on all replicas; after killing the leader, a new leader reconciles p1b logs, re-proposes uncommitted values, fills holes with no-ops, continues from max+1. |
| T7 | ScalablePaxos / CompPaxos (autocomp `automultipaxos`, `comppaxos`) and Hydro `compartmentalized_paxos` | SIGMOD'24; Hydro | Same committed sequence as T6; throughput scaling; CompPaxos may exhibit the App. C non-linearizable p1b merge but never an unsafe decision. |
| T8 | Hydro Paxos + kv_replica with checkpoints | Hydro | Acceptor logs GC'd below checkpoint; replicas apply contiguous prefixes only; payloads may be dropped during leader change and must be retried by clients. |
| T9 | Raft core suite (port each Hydro test): exactly one leader per term with simultaneous candidates in a 4-node cluster; heartbeats converge leader views; vote/ack interlock; step-down on higher-term reply; stale-log candidate refused; replicate & commit; redirect when not leader; no commit without quorum; new leader overwrites conflicting *uncommitted* entries; previous-term entries commit only transitively; composed elect/replicate/suppress; concurrent elections never fork the committed log; fully concurrent run never forks | Hydro raft.rs tests | As named; plus global invariants from Molly `raft_assert.ded`: never `bad(_, _, "two leaders")`, never `bad(_, _, "disagree")` among non-crashed nodes. |
| T10 | Raft paper scenarios | Diss. Fig. 3.7 (commit of previous-term entry), Fig. 4.2 (direct multi-server config change unsafe), Fig. 4.6 (removing the leader of a 2-server cluster), Fig. 4.7 (disruptive removed server), §6.3 (duplicate lock acquisition without sessions), §6.4 (stale read from partitioned leader) | Our implementation must avoid each anomaly: never commit a previous-term entry by counting; reject multi-server changes unless joint consensus; removed leader steps down only after C_new commits; RequestVote ignored within min election timeout of hearing a leader; sessions dedupe; ReadIndex prevents stale reads. |
| T11 | Single-server membership-change bug scenario (2015) | raft-dev | Without the "commit a current-term entry before a config change" rule, a fault-injection search should find two leaders/diverging configs; with it, none. |
| T12 | Raft persistence/restart | Diss. §3.8 | Crash and restart any subset of servers at arbitrary ticks: no server votes twice in a term; no committed entry lost; commit index recovers. Also verify messages are never released before their durable writes (inject crash between fsync and send). |
| T13 | Raft snapshots | Diss. ch. 5 | Lagging follower receives InstallSnapshot, discards/retains log correctly (prefix vs superseding), resumes AppendEntries; state identical to leader after catch-up. |
| T14 | Raft linearizable KV with sessions + ReadIndex + leases | Diss. ch. 6 | Porcupine/Knossos-style linearizability check over client histories under partitions, leader changes, and duplicated client retries. |
| T15 | Bud Raft ports (noeleo/raft, whitewater) as *negative* tests | GitHub | The analyzer/tests should flag: missing durability barrier (noeleo), uncapped follower commit (whitewater) — a fault-injection run should produce a State Machine Safety violation for whitewater's rule. |
| T16 | BFS (bud-sandbox `bfs/*`) with `tc_bfs.rb`/`tc_e2e_bfs.rb` scenarios | bud-sandbox | mkdir/create/ls/rm semantics (rm of non-empty dir fails; missing parent fails); append+read round-trip MD5 equal; with REP_FACTOR replicas; killing a datanode triggers `copy_chunk` re-replication; heartbeats expire after HB_EXPIRE. |
| T17 | BOOM-FS HA: NameNode as a 3-replica consensus group | EuroSys §4; TR Table 5 | No metadata loss on primary failure; job completes; overhead of replication negligible without failures (reference: 101.89 s vs 102.70 s; primary failure 148.47 s). |
| T18 | Partitioned NameNode | EuroSys §5 | Files routed by hash(fqpath); `ls` merges across partitions; interrupted `mkdir` re-run converges; (extension) cross-partition rename via 2PC over Paxos groups is atomic. |
| T19 | BOOM-MR scheduler: FCFS and LATE on wordcount with stragglers | EuroSys §7.3; TR App. C | LATE policy speculates stragglers (≤ SpeculativeCap = 10% slots, no speculation on nodes below 25th-percentile progress); reduce-task completion CDF tail shorter than FCFS. |
| T20 | Monitoring metaprograms | EuroSys §6 | Trace-rewrite every rule of T5/T6; coverage report lists never-fired rules; message counts per decree at steady state match the protocol's expected message complexity. |

---

## 16. Source URLs

* BOOM Analytics (EuroSys 2010): https://www.neilconway.org/docs/booma_eurosys2010.pdf ; https://dsf.berkeley.edu/papers/eurosys10-boom.pdf ; https://dl.acm.org/doi/10.1145/1755913.1755937
* BOOM TR UCB/EECS-2009-113: https://www2.eecs.berkeley.edu/Pubs/TechRpts/2009/EECS-2009-113.pdf
* I Do Declare (NetDB 2009): https://dsf.berkeley.edu/papers/netdb09-idodeclare.pdf ; source index page (archived): http://web.archive.org/web/20160914024014/http://db.cs.berkeley.edu/netdb-09/
* Recovered Overlog Paxos tarball: https://web.archive.org/web/20200621141401id_/https://bitbucket.org/neilconway/overlog-paxos/get/tip.tar.bz2
* bud-sandbox: https://github.com/bloom-lang/bud-sandbox
* Molly: https://github.com/palvaro/molly
* Optimizing Distributed Protocols with Query Rewrites: https://arxiv.org/abs/2404.01593 ; https://dl.acm.org/doi/10.1145/3639257
* autocomp: https://github.com/rithvikp/autocomp
* Bigger, not Badder (abstract only): https://dl.acm.org/doi/10.1145/3642976.3653033
* hydro-optimize: https://github.com/hydro-project/hydro-optimize
* Hydro: https://github.com/hydro-project/hydro (hydro_test/src/cluster/raft.rs, paxos.rs, compartmentalized_paxos.rs; hydro_std/src/quorum.rs)
* Bud Raft: https://github.com/noeleo/raft ; https://github.com/amidvidy/whitewater
* Ongaro dissertation: https://github.com/ongardie/dissertation
* raft-dev single-server membership bug: https://groups.google.com/g/raft-dev/c/t4xj6dJTP6E
* Kirsch & Amir, Paxos for System Builders: http://www.cnds.jhu.edu/pub/papers/cnds-2008-2.pdf (via Wayback)
* Paxos Made Moderately Complex: https://www.cs.cornell.edu/home/rvr/Paxos/paxos.pdf ; https://paxos.systems/
* Paxos Made Live: https://www.cs.utexas.edu/users/lorenzo/corsi/cs380d/papers/paper2-1.pdf
* LATE (OSDI 2008): https://www.usenix.org/legacy/event/osdi08/tech/full_papers/zaharia/zaharia.pdf
