# 06 — Molly / Lineage-Driven Fault Injection, Nemo, and Provenance

Research cluster report for **bloom-remake**. Audience: the implementers of the Rust engine, the
Dedalus-style language, and the verification/fault-injection tooling.

This report covers:

1. Lineage-driven fault injection (LDFI) and its prototype Molly: the model, the algorithm, the
   formal guarantees, the case studies and results, **and what the Molly source code actually does**
   (it differs from the paper in several places that matter).
2. LDFI in production (Netflix, SoCC 2016), the ACM Queue position paper, and the Elastic case study.
3. Provenance-based debugging: Nemo (CIDR 2019) and why-across-time provenance (SoCC 2018).
4. The provenance foundations we need: provenance semirings (PODS 2007), firing-graph rewrites
   (Köhler et al. 2012), distributed provenance (ExSPAN), negative (why-not) provenance (Wu et al.
   SIGCOMM 2014), and efficient provenance in a production Datalog engine (Soufflé, TOPLAS 2020).
5. Implications for our design, a **MUST-IMPLEMENT CHECKLIST**, and **TEST PROGRAMS** with expected
   results.

Conventions. "Paper" means the text of the cited publication. "Code" means the Molly source at
`github.com/palvaro/molly`, commit `a3a6d79` (Nov 4 2018), which I cloned and read. Sections
labelled **[Recommendation]** are my design suggestions for bloom-remake. They do not come from the
literature.

---

## 0. Sources read

| Source | URL | Status |
|---|---|---|
| Alvaro, Rosen, Hellerstein. *Lineage-driven Fault Injection.* SIGMOD 2015 | https://people.ucsc.edu/~palvaro/molly.pdf | Full text read, including Appendix B (algorithms and proofs) |
| Molly source (Scala), commit a3a6d79 | https://github.com/palvaro/molly | All of `src/main` read, plus the tests and the example `.ded` programs |
| Alvaro, Andrus, Sanden, Rosenthal, Basiri, Hochstein. *Automating Failure Testing Research at Internet Scale.* SoCC 2016 | https://people.ucsc.edu/~palvaro/socc16.pdf | Full text read |
| Alvaro, Tymon. *Abstracting the Geniuses Away from Failure Testing.* ACM Queue 15(5), 2017 (also CACM 61(1), 2018) | https://par.nsf.gov/servlets/purl/10053504 (NSF PAR copy); https://queue.acm.org/detail.cfm?id=3155114 | Read the NSF PDF. PDF extraction lost pp. 13 and 16. queue.acm.org returned 403. |
| Oldenburg, Zhu, Ramasubramanian, Alvaro. *Fixed It For You: Protocol Repair Using Lineage Graphs* (**Nemo**). CIDR 2019 | https://www.cidrdb.org/cidr2019/papers/p122-oldenburg-cidr19.pdf | Full text read |
| Nemo source (Go + Neo4j) | https://github.com/numbleroot/nemo | Main modules and case studies skimmed |
| Whittaker, Teodoropol, Alvaro, Hellerstein. *Debugging Distributed Systems with Why-Across-Time Provenance.* SoCC 2018 | https://mwhittaker.github.io/publications/wat_SOCC18.pdf | Sections 1–3 and the definitions read closely. The Watermelon implementation section was only skimmed. |
| Green, Karvounarakis, Tannen. *Provenance Semirings.* PODS 2007 | https://web.cs.ucdavis.edu/~green/papers/pods07.pdf | Definitions, datalog semantics, All-Trees and Monomial-Coefficient read |
| Zhao, Subotić, Scholz. *Debugging Large-scale Datalog: A Scalable Provenance Evaluation Strategy.* TOPLAS 42(2), 2020 (arXiv: *Provenance for Large-scale Datalog*) | https://arxiv.org/pdf/1907.05045 | Sections 3–4 read closely. I read the arXiv version, which may differ slightly from the TOPLAS text. |
| Köhler, Ludäscher, Smaragdakis. *Declarative Datalog Debugging for Mere Mortals.* Datalog 2.0, 2012 | https://yanniss.github.io/DeclarativeDebugging.pdf | Rewrite rules read |
| Wu, Zhao, Haeberlen, Zhou, Loo. *Diagnosing Missing Events in Distributed Systems with Negative Provenance.* SIGCOMM 2014 | https://haeberlen.cis.upenn.edu/papers/negative-provenance-sigcomm2014.pdf | Section 3 (graph model and QUERY algorithm) read |
| Zhou et al. *Efficient Querying and Maintenance of Network Provenance at Internet-Scale* (**ExSPAN**). SIGMOD 2010 | https://netdb.cis.upenn.edu/papers/netprov-sigmod10.pdf | Data model section read |
| Ramasubramanian et al. *Growing a Protocol.* HotCloud 2017 | https://www.usenix.org/system/files/conference/hotcloud17/hotcloud17-paper-ramasubramanian.pdf | Skimmed |
| Hydro deterministic simulation | https://hydro.run/ | Homepage only. The docs page I tried returned 404, so details are unverified. |
| Meiklejohn et al. *Service-Level Fault Injection Testing* (Filibuster). SoCC 2021 | https://christophermeiklejohn.com/publications/filibuster-socc-2021.pdf | Not read, only identified |

Not accessed or not read: Alvaro's PhD thesis; Chen et al. *Differential Provenance* (SIGCOMM 2016),
which I know only through its description in Nemo and Queue; Amsterdamer, Deutch, Tannen, *Provenance
for aggregate queries* (PODS 2011); Deutch et al., *Circuits for Datalog Provenance* (ICDT 2014),
which I only identified; Green's ICDT 2009 containment paper, whose semiring hierarchy I know from
secondary sources (the search results cite it). Anything in this report about those works is marked
as secondary.

---

## 1. Why this cluster matters for bloom-remake

LDFI is the verification story that fits Dedalus/Bloom most naturally, for three reasons:

* **Lineage is almost free in a rule language.** Every derived fact has explicit rule-firing
  provenance. Forward execution with provenance gives, for each good outcome, the complete set of
  alternative derivations. That set *is* the program's redundancy.
* **Faults are EDB deletions.** Message omission and crash failures become deletions from one
  distinguished input relation (`clock`). Molly's paper frames the question "which faults could
  prevent this outcome?" as a *how-to* provenance query over that relation (§6 of the paper). The
  Queue article frames it as materialized-view maintenance.
* **The guarantee is bounded but complete.** If Molly finishes without a counterexample, no admissible
  combination of faults, up to the bound (EOT, EFF, Crashes) and for the given inputs, violates the
  invariant (Theorem B.2). The only other ways to get a guarantee like this are exhaustive search or
  model checking.

What we have to build: a Dedalus evaluator that runs a **synchronous, deterministic, fault-injectable
simulation** with **complete rule/goal provenance**, a **hazard-analysis** stage that turns lineage
into a monotone Boolean formula over fault variables, a **SAT/hitting-set** engine, a driver for the
**forward/backward loop**, and **debugging views**: space-time diagrams, lineage graphs, and
Nemo-style provenance queries.

---

## 2. The LDFI model (SIGMOD 2015)

### 2.1 Synchronous execution model (paper §2.1)

* A general verifier would have to explore faults (loss, crashes) *and* nondeterministic ordering and
  timing. LDFI makes the **dual** of the usual simplification: *successfully delivered messages are
  received in a deterministic order*, and only failures are explored. Asynchronous Dedalus programs
  are evaluated in a **synchronous simulation**, using the synchronous Dedalus semantics of Interlandi
  et al., *Datalog in time and space, synchronously* (CEUR'13), which the paper cites.
* Cost: completeness in the fully asynchronous model is lost. Bugs that depend on reordering or delay
  (as opposed to loss) are out of scope (§7). For consensus algorithms that must tell delay from
  failure, the abstraction is "fundamentally incomplete". Molly validates Paxos *agreement* but says
  nothing about termination.
* Logical time is well defined because of the synchronous abstraction. In the code, every `@async`
  message sent at time `t` is delivered at exactly `t+1`, unless it is lost.

### 2.2 Failure model and failure specification (paper §2.1.1)

* Faults simulated: **permanent crash failures** (fail-stop), **message loss**, and **temporary
  network partitions** (modelled as bursts of loss). *Not* considered: Byzantine failures and
  crash-recovery, which involve both a loss window and loss of ephemeral state. Crash-recovery is
  listed as future work.
* **EOT** (end of time): executions have at most EOT global transitions, meaning rounds of message
  transmission and state-changing internal events.
* **EFF** (end of finite failures): the logical time after which message loss stops ("failure
  quiescence"), so the program gets a chance to recover. Molly enforces `EFF < EOT`. `EFF = 0`
  disables message loss entirely, giving the fail-stop model.
* **Crashes**: the maximum number of crash failures.
* **Fspec** = ⟨EOT, EFF, Crashes⟩, for example ⟨6, 4, 1⟩. A fault set is **admissible** iff it has no
  omissions after EFF and at most Crashes crashes.
* **Inconsistency to resolve.** The paper says loss is permitted at "times 1–4" for EFF=4 (§2.1.1) and
  that deletion happens "only if t ≤ EFF" (§4.1.1). Algorithm 2 uses `n.time < EFF`. The code uses
  strict `<`: `FailureSpec` requires `omissions.forall(_.time < eff)`, Z3 only creates a loss literal
  when `time < eff`, and the SAT4J path assumes `¬O` for `time ≥ eff`. The code's run-count estimate
  (`grossEstimate`) counts `eff` time steps of loss, which contradicts the strict rule. To reproduce
  Molly's test expectations, use **strict: omission allowed iff 1 ≤ sendTime < EFF**, and document
  the choice.
* **Parameter sweep** (§2.1.1). Start with EFF=0 and grow EOT until non-trivially-correct executions
  appear, meaning executions that send messages (vacuous executions send none). Then grow EFF until
  either (a) a violation is found, in which case grow EOT again so the protocol can recover, or (b)
  EFF = EOT−1, in which case grow both by 1. Stop at a user wall-clock bound. Report the minimal
  parameters that produce a counterexample, or the maximal parameters explored for bug-free programs.
  Users can override the sweep, for example to fix a fail-stop model. **The sweep is not in the code
  snapshot I read.** `SyncFTChecker` takes fixed `-t/-f/-c`.

### 2.3 Language requirements (paper §2.2)

LDFI needs three things from the language:

1. Clearly identified program outputs.
2. Fine-grained lineage covering the *uncertain* communication steps.
3. A runtime that lets the simulator **interpose on communication**, controlling loss and delivery
   timing.

Dedalus provides all three. Lineage comes from "simple, well-understood program rewrites" (Köhler et
al.). Dedalus also removes the distinction between events, persistent state and communication, which
makes redundancy easy to see.

### 2.4 Correctness properties: `pre` and `post` (paper §2.3)

* A program is fault-tolerant for an Fspec iff its correctness assertions hold for *all* admissible
  fault combinations.
* Invariants are implications **pre → post**. A violation is an execution where `pre` holds and
  `post` does not. An execution where `pre` does not hold is **vacuously correct**. Without the
  implication form there is always a trivial bad run (crash everything, drop everything).
* The user defines the relations `pre` and `post`, both of user-chosen arity, as views over program
  state. The paper says that if they are not defined, all persistent relations are treated as
  outcomes. **The code snapshot does not do this:** `Verifier` always reads the table `"post"`, and
  `"pre"` in `isGood`.
* Meta-outcomes should *abstract away* details that legitimately vary under faults, such as the
  number of ACKs, or which value consensus decided.
* Reliable-broadcast spec, paper Figure 3, verbatim:

```
missing_log(A, Pl) :- log(X, Pl), node(X, A), notin log(A, Pl) ;
pre(X, Pl) :- log(X, Pl), notin crash(_, X, _) ;
post(X, Pl) :- log(X, Pl), notin missing_log(_, Pl) ;
```

The code's version (`delivery/deliv_assert.ded`) is slightly different:

```
// someone has a log, but not me.
missing_log(A, Pl) :- log(X, Pl), node(X, A), notin log(A, Pl);//, notin crash(_, A, _);

pre(X, Pl) :- log(X, Pl), notin bcast(X, Pl)@1, notin crash(X, X, _);
post(X, Pl) :- log(X, Pl), notin missing_log(_, Pl);
```

Commit-protocol invariants (§5.1.1):

* **Agreement**: if an agent decides commit (abort), then all agents decide commit (abort).
* **Termination**: if a transaction is initiated, all agents decide either commit or abort.

Kafka durability (§5.1.3): if a write is acknowledged at the client, it is stored on a correct
(non-crashed) replica.

### 2.5 The programmer-versus-adversary game (paper §3)

Setup: a correctness spec, inputs, and a failure model are agreed. In each round the programmer
submits a program. The adversary watches an execution, then picks faults from the agreed set for the
next execution. If the spec is violated, the programmer loses the round. If the adversary runs out of
moves, the programmer wins. Both sides do best by reasoning over **lineage**. Rounds 1–3 and 5 use the
input `bcast(A, data)@1`, a fully connected `node` over {A,B,C}, EFF=2 and Crashes=1.

* **Round 1, simple-deliv (Figure 2), verbatim:**

```
log(Node, Pload) :- bcast(Node, Pload);
node(Node, Neighbor)@next :- node(Node, Neighbor);
log(Node, Pload)@next :- log(Node, Pload);
log(Node2, Pload)@async :- bcast(Node1, Pload),
                           node(Node1, Node2);
```

  `log(B,data)` has one support. Dropping `O(A,B,1)` falsifies it. The adversary wins.

* **Round 2, retry-deliv**: add `bcast(N, P)@next :- bcast(N, P);`. This gives redundancy in *time*
  but not in *space*: every support passes through A. Crashing A after it has reached C but not B
  wins. (Crashing A immediately is vacuous.)
* **Round 3, redun-deliv**: add `bcast(N, P)@next :- log(N, P);`. Every node relays forever, giving
  redundancy in space and time. The adversary has no moves. **The lineage of a single failure-free run
  proves there is no counterexample** (Figure 6).
* **Round 4, ack-deliv (Figure 5)**: retries stop once an ACK arrives. Verbatim:

```
ack(S, H, P)@next :- ack(S, H, P);
rbcast(Node2, Node1, Pload)@async :- log(Node1, Pload),
    node(Node1, Node2), notin ack(Node1, Node2, Pload);
ack(From, Host, Pl)@async :- rbcast(Host, From, Pl);
rbcast(A, A, P) :- bcast(A, P);
log(N, P) :- rbcast(N, _, P);
```

  The failure-free run shows redundancy in space but not in time. The adversary's hypothesis
  `O(A,B,1) ∧ (O(A,C,1) ∨ O(C,B,2))` makes the missing ACK trigger more retries, which is new support.
  The adversary keeps cutting edges, new edges keep appearing, and eventually it has no admissible
  moves. The programmer wins, **but only after several forward/backward iterations**. This is exactly
  why LDFI alternates.
* **Round 5, classic-deliv** (Birman et al.'s "on first receipt, relay to all, then deliver"). It is
  correct under fail-stop but not under omission. It has redundancy in space but not in time. Winning
  move: drop A→B at time 1 and C→A, C→B at time 2. (Figure 4c's caption gives the support falsifier
  `O(A,C,1) ∨ O(C,B,2)`.)

**The key lesson.** A good outcome needs enough *redundant supports*. The adversary's winning move is a
set of faults that hits every known support. Hypotheses built from one run are only potential
counterexamples, because the program may find new supports under faults (retry, failover).

---

## 3. Molly: architecture, algorithms, and code-level semantics

### 3.1 Pipeline (paper Figure 1 and §4, cross-checked against code)

```
inputs: Dedalus program files (+ includes), topology/EDB facts, pre/post, Fspec, node list
  │
  ├─ parse (DedalusParser) → include expansion
  ├─ rewrite: referenceClockRules → splitAggregateRules → addProvenanceRules
  │
  ├─ FORWARD: failure-free run (Fspec with eff=0, crashes=0)
  │     addClockFacts → inferTypes → C4 codegen → C4 evaluation (clock facts installed one timestep at a time)
  │     → "UltimateModel": every table, every timestep
  │
  ├─ BACKWARD ("hazard analysis"): ProvenanceReader builds rule/goal graphs for every post tuple at EOT
  │     → per-goal Boolean formula → SAT/SMT (Z3 default, SAT4J option) → all models
  │     → minimal sets → FailureSpecs (potential counterexamples)
  │
  └─ LOOP: for each unexplored FailureSpec: FORWARD run with those faults;
        if the invariant is violated → counterexample (stop unless --find-all-counterexamples)
        else BACKWARD on the new run, seeded with the current faults → push the new hypotheses
     stop when the worklist is empty ⇒ "certified" for this configuration
  │
  └─ report: runs.json, per-run space-time DOT/SVG, optional per-run provenance DOT/SVG, HTML index
```

Implementation components (code): the Datalog evaluator is **C4**
(`github.com/bloom-lang/c4`), called through JNR-FFI (`c4_make`, `c4_install_str`, `c4_dump_table`).
The solvers are **Z3** (default, `Config.solver = "z3"`) and **SAT4J** (`org.ow2.sat4j.core 2.3.5`,
`--solver sat4j`). An ILP option is stubbed out. The paper names neither evaluator nor solver; it
says only "off-the-shelf Datalog evaluator" and "SAT solver".

CLI (`SyncFTChecker`), from the code:

```
-t/--EOT <int> (default 3)   -f/--EFF <int> (default 2)   -c/--crashes <int> (default 0)
-N/--nodes a,b,c (required)  --solver z3|sat4j|ilp         --strategy sat|random|pcausal
--use-symmetry  --prov-diagrams  --disable-dot-rendering  --find-all-counterexamples
--negative-support   (NOTE: this flag *disables* negative support; the default is on. The help text says
                      "negative support is slow, but necessary for completeness")
<file>...  (Dedalus files are concatenated; includes resolve relative to the first file's directory)
```

README example: `SyncFTChecker simplog.ded deliv_assert.ded --EOT 4 --EFF 2 --nodes a,b,c --crashes 0 --prov-diagrams`
finds a counterexample.

### 3.2 The Dedalus dialect Molly accepts (from `DedalusParser.scala` and `ast/AST.scala`)

```
program    ::= clause*
clause     ::= include | rule | fact
include    ::= 'include' STRING ';'                  -- textual; path relative to the first input file's dir; recursive
fact       ::= predicate ';'                         -- MUST carry '@<int>' (the rewrite calls f.time.get)
rule       ::= predicate ':-' bodyTerm (',' bodyTerm)* ';'
bodyTerm   ::= predicate | expr                      -- expr = selection/qualifier, e.g. A != B, Cnt > N / 2
predicate  ::= ['notin'] IDENT '(' [atom (',' atom)*] ')' [timesuffix]
timesuffix ::= '@next' | '@async' | '@' INT          -- '@INT' in a body means "this relation at absolute time INT"
atom       ::= aggregate | expr | constant
aggregate  ::= IDENT '<' IDENT '>'                   -- head only: count<X>, max<X>, min<X> appear in the examples
expr       ::= constant OP (expr | constant)         -- right-nested, NO parentheses, NO precedence
OP         ::= '==' | '!=' | '+' | '-' | '/' | '*' | '<' | '>' | '<=' | '>='
constant   ::= STRING | INT | IDENT                  -- capitalised IDENT = variable; '_' = don't-care
IDENT      ::= [a-zA-Z0-9._?@]+     STRING ::= '"' [^"]* '"'     INT ::= [0-9]+
comments   ::= '//' to end of line | '/* ... */'
```

Semantics and conventions (README, tutorial, and code):

* **Location specifier.** The first column of every relation is a location and is typed `LOCATION`
  (the typer asserts it). The code takes a rule's location from **the first column of its first body
  predicate** (`Rule.locationSpecifier`).
* **Locality.** Per the tutorial, all body predicates must share the same location binding, and a
  head with a different location must be `@async`. **Molly does not enforce either rule.** Spec rules
  in the examples join across locations, for example Kafka's
  `good(D) :- ack("C", D, _), write(R, D, _), notin crash(R, R, _);`. The centralized simulator
  evaluates them anyway. Our implementation should enforce locality for protocol rules and allow
  global joins only in **spec rules**, which should be explicitly marked.
* **Temporal annotations.** No annotation means deductive: same timestep, same place. `@next` means
  the head holds at t+1 at the same node. `@async` means the head holds at the receiver at an unknown
  time; in Molly's synchronous model that is t+1 unless lost. A rule head may not carry `@INT`
  (`IllegalStateException`). Facts must carry `@INT`, and a fact holds *only* at that time unless a
  persistence rule carries it forward.
* **Persistence is explicit.** It is written as the frame rule `p(X..)@next :- p(X..);`. A relation
  without one is *ephemeral*: an event that is true at one timestep.
* **Negation.** `notin p(...)`. All variables in negated predicates must be bound by positive
  predicates (README). Head variables must be bound in the body.
* **Reserved meta-relations** (the typer hard-codes their types):
  * `clock(From: LOCATION, To: LOCATION, SendTime: INT, DeliveryTime: INT)` is generated by Molly.
    User programs reference it only through the rewrite. (The Raft example has its own `lclock`
    relation.)
  * `crash(Observer: LOCATION, Crashed: LOCATION, CrashTime: INT)`, which the rewrite extends to
    4-ary with a time column. See §3.4 for its **oracle semantics**.
  * `pre` and `post` are the invariant relations. Older examples use `good` and `bad`, which the
    current `Verifier` ignores.
* **Quirks not to copy.** The parser tries `<` before `<=` in the `OP` alternation, and no example
  uses `<=` or `>=`, so they may never parse (unverified). `expr` is right-associative, so
  `A - B - C` parses as `A - (B - C)`. There are no unary minus, no parentheses, and no string
  functions.

### 3.3 Type inference (`DedalusTyper.scala`)

* Types are `INT`, `STRING`, `LOCATION`, and `UNKNOWN`, which is an error at codegen.
* Algorithm: build a union-find over `(table, column)` references. Union columns that share a variable
  inside a rule, and columns whose variables appear together in a qualifier. Gather evidence: string
  literal → STRING, int literal → INT, aggregate or expression → INT, column 0 → LOCATION, plus fixed
  evidence for `clock` and `crash`. Where the evidence is {STRING, LOCATION}, LOCATION wins. Any other
  conflict is an error that prints source positions for each piece of evidence.
* Every occurrence of a predicate must have the same arity. `clock` and `crash` have arity 4 after
  the rewrite.

### 3.4 Program rewrites (paper §4.1; code `DedalusRewrites.scala` and `FailureSpec.scala`)

**Time column.** Every relation gets a last attribute `Time`. Reserved variables: `NRESERVED` is the
body time and `MRESERVED` is the async delivery time. From the code (`referenceClockRules`):

| Dedalus rule kind | Rewritten head | Guard appended to body |
|---|---|---|
| deductive `h :- b...` | `h(..., NRESERVED)` | `clock(Loc, Loc, NRESERVED, _)` (added only to ensure the time variable is bound by a positive subgoal, and to limit derivations) |
| inductive `h@next :- b...` | `h(..., NRESERVED+1)` | `clock(Loc, _, NRESERVED, _)` |
| async `h@async :- b...` | `h(..., MRESERVED)` | `clock(Loc, To, NRESERVED, MRESERVED)`, where `To` = the head's first column |
| body pred `p(...)@k` | `p(..., k)` (absolute time) | |
| other body preds | `p(..., NRESERVED)` | |
| fact `p(...)@k` | `p(..., k)` | |

`Loc` is the first column of the first body predicate.

The paper's presentation (Examples 1–3) is the same, except that its clock is 3-ary
`clock(From, To, SndTime)` and loss is described as *deleting* the clock record.

**Clock and crash EDB generation** (`FailureSpec.generateClockFacts`, code):

```
for from ∈ nodes, to ∈ nodes, t ∈ 1..EOT, unless ∃ crash(from, tc) with tc ≤ t:
    clock(from, to, t, (from ≠ to ∧ omitted(from,to,t)) ? NEVER : t+1)       -- NEVER = 99999
for node ∈ nodes, t ∈ 1..EOT:
    clock(node, node, t, t+1)                                                   -- NOT filtered by crashes!
for each crash c, node ∈ nodes, t ∈ 1..EOT:
    crash(node, c.node, c.time, t)
```

Consequences, several of them subtle:

1. **Lost messages get delivery time NEVER** instead of being deleted. The async head lands at time
   99999, outside the window. This keeps the send visible to the space-time diagram, which draws the
   message as "LOST". It is the same as deletion for everything that happens up to EOT.
2. **Omission granularity is the channel-timestep `(from, to, sendTime)`**, not the individual
   message. Dropping `O(a,b,t)` drops *every* async tuple, from every rule, that `a` sends to `b` at
   time `t`.
3. **Self-sends cannot be lost.** A `MessageLoss` requires `from != to`, and `(n,n,t)` clocks are never
   "important".
4. **Crash semantics in the code.** Crashed node `n` loses all *outgoing* channels from its crash
   time onward. It still has `clock(n,n,t,t+1)` for every t, so its deductive and `@next` rules keep
   firing. It still *receives* messages, because clocks are keyed on the sender. The paper says a
   crashed node "ceas[es] to send messages or make internal transitions". The code gives the same
   external behaviour, since a crashed node's continued local computation cannot influence anyone.
   Its local state can still leak into global spec rules, which is why the specs filter with
   `notin crash(X, X, _)`.
5. **`crash` is an omniscient oracle.** `crash(obs, n, tc, t)` exists at *every* time t in 1..EOT
   and at every observer, *including times before the crash*. It reveals the future. It is meant only
   for invariants: "correct process" = `notin crash(X, X, _)`. `heartbeat_assert.ded` even compares
   crash times: `dead_after(A,O) :- crash(A,O,N), tic(A,O,M), M > N;`. Letting protocol logic read
   `crash` would be cheating. **We must restrict `crash` to spec rules.**

**Aggregation split** (`splitAggregateRules`, and paper §4.1.2). A rule with an aggregate head, e.g.
`agg(X, count<Y>) :- a(X, Z), b(Z, Y)`, becomes

```
agg_vars(X, Y, Z, T) :- a(X,Z,T), b(Z,Y,T), clock(...)   -- records ALL variables (the "bindings" rule)
agg(X, count<Y>, T)  :- agg_vars(X, Y, _, T)              -- aggregates over the bindings; non-head vars → '_'
```

This keeps the extra captured bindings from changing the grouping (there is a regression test for
exactly this). The paper writes it as `r_bindings` / `r_prov`.

**Provenance ("firings") rewrite** (`addProvenanceRules`; paper §4.1.2 after Köhler et al.). For each
rewritten rule number `i` with head `h`, add a rule with the same body and head
`h_prov<i>(headcols..., extra..., Time)`. The paper says the firings relation "captures bindings of
all premise variables". The code records the head columns, then only the **bound variables**
(variables occurring ≥ 2 times across the body predicates and qualifiers, i.e. join variables) plus
expression variables, and finally the time column. Variables that occur once and are not in the head
are *not* recorded. They become `__WILDCARD__` in reconstructed subgoals, which makes the provenance
imprecise (the "missing fields are wildcards" test shows two enumerated derivations for one firing).
For async rules, both `NRESERVED` (send time) and `MRESERVED` (delivery time) end up recorded.

**Evaluation** (`C4Wrapper.run`). Rules and non-clock facts are installed first. Then the clock facts
are installed **one timestep at a time, in increasing time order**, and each install runs a C4
fixpoint ("to stratify the execution by time"). Since every rule is guarded by a clock atom for its
body time, rules for time t can only fire once the clock facts for t exist. At the end, every table is
dumped (all timesteps) into an `UltimateModel`. C4 codegen moves negated body predicates to the end of
each rule to work around a C4 bug (bloom-lang/c4 issue #1).

**Stratification caveat.** Appendix B assumes the submitted Dedalus program is stratifiable and argues
that the rewrite keeps it so. Molly's own `2pc.ded` has a **predicate-level negative cycle**
(`running ←¬ commit ←¬ missing_vote ← running`) that is broken only by `@next`. It is *temporally*
(locally) stratified, not statically stratified. **Our evaluator must stratify the deductive
(same-timestep) dependency graph only, treat `@next`/`@async` edges as crossing to a later timestep,
and run one stratified fixpoint per timestep.** That is the correct Dedalus semantics and is what
makes these programs legal.

### 3.5 Provenance extraction: rule/goal graphs (paper §4.2; code `ProvenanceReader.scala` and `DerivationTrees.scala`)

**Definitions (paper).** A derivation graph is a bipartite rule/goal graph. Goal nodes are facts. Rule
nodes are rule firings with particular bindings. Each goal has an edge to every firing that derived
it, and each firing has an edge to every premise it used. A single outcome can have several
derivations through the *same* rule with different premises. A derivation graph yields a finite
**forest of proof trees**. Each proof tree is an independent support, and losing *any* message it uses
falsifies it.

**Construction (code).** Goals are `post` tuples at EOT: `model.tableAtTime("post", eot)`.

```
buildDerivationTree(goal):                         -- memoised per GoalTuple
  if goal.negative and negativeSupport:
      causes := possibleCauses(goal)               -- conservative over-approximation, below
      return Goal(goal, causes.empty ? {} : { PhonyRule(children = causes.map(buildDerivationTree)) })
  if goal ∈ EDB facts (pattern match with wildcards): return leaf Goal(goal)
  firings := for each prov table of goal.table: rows whose first (arity-1) cols and last (time) col match goal
  if firings empty and goal was derived: error "Couldn't find rules to explain derivation"
  for each firing (rule, bindings):
      pos := positive body preds instantiated with bindings (unbound → WILDCARD)
      aggregate body preds (preds mentioning head aggregate vars):
           expand to ALL tuples of that table at time NRESERVED matching the pattern   -- aggregate support
      neg := negated body preds instantiated; assert that none of them exists in the model
      children := pos (+ neg marked negative, if negativeSupport)
      RuleNode(rule, children.map(buildDerivationTree))
```

Findings:

* **Aggregate provenance** is conjunctive over *all* contributing tuples of the group. Losing any
  contributor changes the aggregate value, so the aggregate tuple is falsified.
* **Negative subgoals** (Appendix B, Algorithm 2 line 17, and `possibleCauses` in the code). The
  derivation graph does not explain why a fact is absent. Molly offers three options: (1) ignore
  negated goals, which is fine for monotone programs and fast but incomplete; (2) surrogate tuples as
  in Wu et al.; (3) the default, a **conservative over-approximation**. The possible causes of `not n`
  are every fact `z` in the model such that relation(n) is statically reachable from relation(z) in
  the predicate dependency graph (excluding `clock`), with `time(z) ≤ time(n)`. The code uses strict
  `<`, or `=` only when the reachability path is purely deductive. If any cause is falsified, `n`
  might appear, so falsifying a cause falsifies the firing that needed `n` to be absent. The paper
  notes this can be narrowed to `z` from which n is reachable through an **odd number of negations**.
  The code computes the parity but leaves the filter commented out (`// FIXME`). Soundness does not
  suffer from over-approximation, because every hypothesis is re-executed.
* **Messages list** (`ProvenanceTableManager.messages`, used for diagrams and the solver). For each
  async rule's provenance table, every row gives `Message(table, from, to, sendTime=NRESERVED,
  receiveTime=MRESERVED)`. Rows with `sendTime == EOT` are dropped.
* **Proof-tree enumeration** (`enumerateDistinctDerivations`) takes the cross product over rule
  choices. It is exponential and exists only for tests and diagrams. The Z3 encoding works directly
  on the **DAG** with memoization.

### 3.6 Hazard analysis: the Boolean encoding (paper §4.3 and Algorithms 1–2; code solvers)

**Variables.** `O(from,to,t)` means the channel from→to lost its messages sent at time t.
`C(n,t)` means node n crashed at time t. The code also has `NeverCrashed(n)` as a helper.

**Paper, Algorithm 2 `clocks(n, G)`**, which returns a formula whose models are faults that could
prevent n:

```
clocks(n):
  if n is a goal:
    if n is a leaf:
      if relation(n) = clock:
          φ := (n.time < EFF) ? O(n.from, n.to, n.time) : false
          ψ := ⋁_{i=0..n.time} C(n.from, i)
          return φ ∨ ψ
      else return true      -- as printed; see the note below
    else if n is negative:
      C := { z ∈ G | relation(z) →+ relation(n) }
      return ⋁_{z∈C} clocks(z)
    else return ⋀_{(n,r)∈E} clocks(r)        -- falsify every alternative derivation
  else (n is a rule firing):
    return ⋁_{(n,g)∈E} clocks(g)             -- falsify any premise
```

**Note on the printed Algorithm 2.** It returns `true` for non-clock leaves (the comment says "Ignore
non-clock leaves"). Read literally, that would make every rule with an EDB premise trivially
falsifiable. The Z3 implementation returns **`false`** for a leaf without an important clock: an
unfalsifiable premise contributes nothing to the OR. That is the semantically correct choice.
**Implement `false`.**

**Worked example, verbatim (§4.3).** `(O(a,c,2) ∨ C(a,2) ∨ C(a,1)) ∧ (O(b,c,1) ∨ C(b,1))`: two proofs.
The first is falsified by losing a→c at time 2, or by `a` crashing at time 1 or 2. The second by
losing b→c at time 1, or by `b` crashing at time 1.

**Encoding details from the code (Z3Solver, the default).**

* `goal ↦ AND(rules.map(rule ↦ OR(subgoals.map(goal↦...))))`, memoised on the DAG.
* A leaf with `ownImportantClock = (from,to,t)` (a clock fact with from≠to, no wildcards) becomes
  `(t < EFF ? O(from,to,t) : false) ∨ ⋁_{t'=firstSend(from)..t} C(from,t')`. A leaf with no important
  clock becomes `false`.
* **First-send optimization.** `firstSend(n)` is the earliest time n sends any message in the run.
  Crashing before that is equivalent to crashing at it, so crash variables start at `firstSend(n)`
  (default 1).
* **Important nodes** are the senders of important clocks with `t < EOT`, plus any node crashed in the
  seed. Only these get crash variables. If there are none, the SAT call is skipped.
* **Crash constraints.** For each important node n: exactly one of
  `{C(n,t) | t ∈ firstSend(n)..EOT-1} ∪ seedCrashes ∪ {NeverCrashed(n)}`. Globally:
  `Σ_n NeverCrashed(n) ≥ |nodes| − maxCrashes`. This is encoded with pseudo-Boolean integer variables
  in Z3, and with `addExactly`/`addAtLeast` cardinality constraints in SAT4J. The code passes
  `failureSpec.nodes.map(NeverCrashed)` over *all* nodes, so non-important nodes have a free
  `NeverCrashed` variable that the at-least constraint can use.
* **Seed.** The faults already injected in the current run are asserted true (Z3) or passed as
  assumptions (SAT4J). New hypotheses are therefore *supersets* of the current fault set.
* **One problem per goal.** A separate SAT problem is solved for **each goal tuple**, and the
  potential counterexamples are the union of all solutions (paper §4.3; `Solver.solve` does
  `goals.flatMap`).
* **Model enumeration.** Z3: after each model, add a blocking clause negating the *complete*
  assignment, positive and negative literals, and continue until UNSAT. SAT4J: `ModelIterator`.
  Empty models (no faults) are dropped.
* **Minimization.** `SetUtils.minimalSets` removes every solution that is a superset of another,
  bucketing by size.
* **`solutionToFailureSpec`** drops omissions subsumed by crashes: sender crashed at t_c ≤ loss time,
  or receiver crash where `t_c + 1 ≥ loss time`. The receiver condition looks inverted relative to its
  comment. The unit test expects `MessageLoss(B,A,1)` to be removed when `A` crashes at 2. An empty
  solution maps to `None`.
* **SAT4J path.** Formula = `BooleanFormula(goal.booleanFormula).simplifyAll.flipPolarity`, then
  **CNF by naive distribution** (`convertToCNFAll`), which can blow up exponentially. Here
  `goal.booleanFormula` is the *positive* "derivable" formula (goal = own-clock ∧ ⋁rules; rule =
  ⋀subgoals), and `flipPolarity` swaps ∧/∨ without negating literals. Literals denote "loss of this
  clock", so the flip is a De Morgan dualization. `BFLiteral(None)` is simplified as an identity for
  both ∧ and ∨, which is wrong for ∨: `true ∨ x` should be `true`. The result is extra, but still
  sound, hypotheses. Each CNF clause of losses is then widened with the sender's crash variables. **Do
  not copy naive CNF distribution.** Use Tseitin or the hitting-set formulation (§10).
* **Pre-vacuity pruning.** Paper §4.3 describes an optimization that skips hypotheses that falsify a
  `post` record only by also falsifying the matching `pre` record. The code instead checks vacuity
  *after* running the hypothesis (`isGood`, §3.7).

### 3.7 The forward/backward driver and the oracle (code `Verifier.scala`; paper Algorithm 1)

**Paper, Algorithm 1 `LDFI(P, E, g)`.** R := provenance-enhanced rewrite of P; G := RGG(R, E, g);
φ := clocks(g, G). If φ is satisfiable, then for each model A of φ:
`D := {clock(f,t,l) ∈ E | A ⊨ O_{f,t,l} ∨ (A ⊨ C_{f,l'} ∧ l' < l)}`, and if `g ∉ P(E \ D)` yield D.

**Code.**

```
verify():
  M0 := run(failure-free spec)                        -- eff=0, crashes=0
  goals0 := derivation trees of post@EOT in M0
  Q := solve(Fspec, goals0, messages0, seed = ∅)      -- iterator of FailureSpecs
  emit Run(0, success, M0)
  doVerify(Q)

doVerify(Q):
  skip every f ∈ Q already in alreadyExplored        -- HashSet, or SymmetryAwareSet with --use-symmetry
  if Q exhausted: stop                                 -- ⇒ no counterexample for this configuration
  f := next(Q); M := run(f); mark f explored
  if isGood(M):
      new := solve(f, derivation trees of post@EOT in M, messages(M), seed = f.crashes ∪ f.omissions) \ {f}
      emit Run(success); doVerify(Q ++ new)
  else emit Run(failure)                               -- counterexample; the stream is cut here unless find-all
```

**The oracle `isGood(M)`** is the exact test used:

```
FFG   := post@EOT of the failure-free run
posts := post@EOT of M;  pres := pre@EOT of M
good  ⇔ posts == FFG  ∨  ∀ g ∈ (FFG \ posts): g ∉ pres
```

Consequences:

1. **`pre` and `post` must have the same schema and keys.** A violation needs a tuple that was in
   post in the failure-free run, is missing from post now, and *is* in pre now.
2. A post tuple that did not exist in the failure-free run (for example a different decision value)
   is **not** flagged. That is why specs key post on the transaction or payload, not on the decided
   value. See `2pc_assert.ded`.
3. Invariants are evaluated **only at EOT**. They must therefore be defined over persistent state, or
   derived at EOT.
4. There is a dead sanity check. `failureFreePre` is computed from `"post"` instead of `"pre"`, so the
   "post empty in failure-free run" guard never fires.

**Other strategies (code).**

* `random`: pick a random number of crashes (≤ max) at random times, and random omissions for
  `time ∈ 1..EFF-1`. This is the paper's random baseline (§5.2).
* `pcausal`: use "phony" derivation trees in which each goal depends on **every message received at
  its location before its time**. That is Lamport happens-before causality instead of data lineage,
  so it over-approximates. It is used as an ablation in `TableOfCorrectPrograms`. It is not described
  in the paper text, but it is the baseline that wat-provenance (§6.2) argues is too coarse.

**Symmetry reduction** (`symmetry/`, `--use-symmetry`). Candidate automorphisms are permutations of
the nodes that never appear as location literals in rule bodies. Keep those that map the stable EDB
(program facts) to itself. Two FailureSpecs are equivalent if one of those permutations maps one's
generated clock/crash facts onto the other's. Cache buckets are keyed by (histogram of crash times,
histogram of omission times, multiset of per-channel omission counts).

**Run-count estimate** (`FailureSpec.grossEstimate`, used for the "Combinations" column):

```
L = 2^(|nodes|-1)                         -- per-node, per-timestep outgoing-loss patterns
crashFree  = L^EFF
crashProne = Σ_{ct=1}^{EOT+1} L^{min(EFF, ct-1)}      -- ct = EOT+1 means "never crashes"
estimate   = C(|nodes|, maxCrashes) · crashFree^(|nodes|-maxCrashes) · crashProne^maxCrashes
```

### 3.8 Formal guarantees (paper Appendix B)

* **Fault set:** `D ⊆ {f ∈ E | relation(f) = clock}`. **Falsifier** of goal g: g ∈ P(E) but
  g ∉ P(E \ D). **Minimal** falsifier: no proper subset is a falsifier.
* **Soundness.** Every yielded D is re-executed (Algorithm 1, line 9), so reported counterexamples are
  real.
* **Lemma B.1.** For every minimal falsifier D of g there is a model A of `clocks(g, G)` such that for
  each f ∈ D, `A ⊨ O_{f.from,f.to,f.time}` or `A ⊨ C_{f.from,t}` for some t ≤ f.time. The proof goes by
  induction. The monotone (negation-free) structure lets models of the conjuncts be merged by
  unioning their true variables. The negative-goal case uses static reachability: EDB deletions can
  create facts only through negation.
* **Theorem B.2 (completeness).** For every minimal falsifier D of g ∈ P(E), there is a
  D' ∈ LDFI(P, E, g) with D ⊆ D' that is a falsifier.
* **The assumption behind completeness: internal determinism** (§7). If some execution produced a
  proof tree of an outcome, every execution with the same faults does too. Randomized protocols and
  anti-entropy can still have bugs found, but they cannot be certified.
* **Scope of the guarantee:** given inputs, topology, EOT, EFF, and Crashes, in the synchronous model.

### 3.9 Case studies and results (paper §5)

* **Commit protocols** (nodes a, b, C, d; C is the coordinator).
  * 2PC blocks when the coordinator crashes after prepare, violating termination (Figure 8).
  * 2PC-CTP (collaborative termination) still has blocking executions.
  * 3PC is nonblocking under fail-stop, but **with message loss the timeout-based failure detector is
    wrong**. Agents a and b roll forward to commit while the coordinator aborts, violating agreement
    (Figure 9, found at EOT=9, EFF=7, Crashes=1).
* **Kafka 0.8 replication** (nodes a, b, c, the client C, and Zookeeper Z modelled as one abstract
  node). A partition removes b and c from the ISR, the leader a acknowledges a write alone, then a
  crashes. Durability is violated (Figure 10). The replication logic is modelled in detail (about 12
  LOC); Zookeeper and the client are sketched.
* **Paxos (synod) and bully leader election**: agreement is validated. Termination is not checkable
  in this model.
* **Flux** (process-pairs replica synchronization for streaming dataflow): certified to EOT=22,
  EFF=21, "the most thorough validation of the Flux protocol to date".
* **Summary:** 7 critical bugs found in 14 systems. The other 7 are certified up to their bounds.

**Figure 12 (buggy programs; minimal parameters to reach a counterexample):**

| Program | LOC | EOT | EFF | Crashes | Combinations | Random exe | Random wall (s) | Molly exe | Molly wall (s) |
|---|---|---|---|---|---|---|---|---|---|
| simple-deliv | 4 | 4 | 2 | 0 | 4.10×10³ | 4.08 | 0.16 | 2 | 0.12 |
| retry-deliv | 5 | 4 | 2 | 1 | 4.07×10⁴ | 75.24 | 1.28 | 3 | 0.12 |
| classic-deliv | 5 | 5 | 3 | 0 | 2.62×10⁵ | 116.16 | 1.81 | 5 | 0.24 |
| 2pc | 16 | 5 | 0 | 1 | 24 | 5.48 | 0.31 | 2 | 0.22 |
| 2pc-ctp | 25 | 8 | 0 | 1 | 36 | 8.56 | 1.04 | 3 | 1.01 |
| 3pc | 24 | 9 | 7 | 1 | 2.43×10²⁶ | 40.60 | 6.24 | 55 | 9.60 |
| Kafka | 18 | 6 | 4 | 1 | 1.85×10²⁵ | 1183.12 | 133.30 | 38 | 3.74 |

(Random = mean over 25 runs.)

**Figure 13 (bug-free programs; highest parameters reached in a 120 s sweep; "exe" = executions for
100% coverage):**

| Program | LOC | EOT | EFF | Combinations | exe |
|---|---|---|---|---|---|
| redun-deliv | 7 | 11 | 10 | 8.07×10¹⁸ | 11 |
| ack-deliv | 5 | 8 | 7 | 3.08×10¹³ | 673 |
| paxos-synod | 33 | 7 | 6 | 4.81×10¹¹ | 173 |
| bully-le | 11 | 10 | 9 | 1.26×10¹⁷ | 2 |
| flux | 41 | 22 | 21 | 6.20×10⁷⁶ | 187 |

Figure 11 plots executions against the bound for redun-deliv and ack-deliv as EOT grows with
EFF = EOT−3. Redundancy that is *visible in every run* (redun-deliv) lets each backward step prune
exponentially more. Redundancy revealed only under faults (ack-deliv) needs many more iterations.

### 3.10 Visualizations (paper §5; code `report/`)

* **Space-time (Lamport) diagram.** One column per process, time runs downward, one vertex per
  (process, time). A vertex is labelled with the time if the process sent or received a message then,
  and is a point otherwise. A crashed process gets a red box labelled "CRASHED" at its crash time and
  its timeline ends there. A message is an edge from `node_from_sendTime` to `node_to_(sendTime+1)`
  labelled with the relation name. Lost messages are drawn dashed and red with the label "(LOST)".
  Messages to nodes crashed for more than one step are not drawn. Graphviz is laid out with
  `rankdir=TD` and `splines=line`.
* **Lineage (provenance) diagram.** Goal nodes are ellipses labelled with the tuple. Negative goals
  are black-filled with white text. Rule-firing nodes are rectangles labelled with the head relation.
  In the paper's figures, messages are dashed edges and time runs *upward* (effects to causes).
* **Output.** An `output/` directory holding `runs.json` (per run: iteration, status, failureSpec,
  the full model of all tables at all times, messages, and provenance) plus `run_<i>_spacetime.dot/.svg`
  and `run_<i>_provenance.dot/.svg`, with an HTML index and per-run pages. The tutorial describes the
  page contents: the space-time diagram, the lineage of all `post` records, and a dump of every
  relation at every timestep.

### 3.11 Limitations (paper §7 and the code)

* Inputs, including topology, are fixed. LDFI is complementary to input generation (symbolic
  execution, QuickCheck).
* Internal determinism is required for completeness (§3.8).
* The synchronous abstraction cannot test reordering or delay, and gives no liveness guarantees for
  consensus.
* Future work: use lineage to **synthesize repairs** (the programmer's role). Nemo takes this up (§6).
* Code-level issues to avoid: the unenforced locality rules; imprecise wildcard provenance from
  recording only join variables; the negative-support parity filter left disabled; naive CNF
  conversion; full-assignment blocking clauses followed by post-hoc minimal-set filtering (they
  enumerate *all* models, which can be exponential, before filtering); a single-threaded evaluator
  (`C4Wrapper.synchronized`); and full recomputation for every hypothesis.

---

## 4. LDFI at Netflix (SoCC 2016)

Adapting LDFI to the Netflix production microservice architecture, using FIT (Failure Injection
Testing) and the internal Dapper-like tracing.

* **FIT.** A *failure scope* (blast radius) selects requests. Zuul decorates matching requests with
  failure metadata. *Injection points* (Hystrix, Ribbon, EVCache, Astyanax) check the request context
  and simulate the failure: delay, error return, exception.
* **Boolean encoding, restated (§2.2).** Each leaf-to-root path in the lineage graph is an
  alternative computation. Failing any node on the path invalidates it. To break the outcome, every
  path must be invalidated, which gives CNF.
  **Worked example (Figure 1):** "The write is stable" ← (Stored on RepA | Stored on RepB); each
  replica ← (Bcast1 | Bcast2) from the Client.
  `(RepA ∨ Bcast1) ∧ (RepA ∨ Bcast2) ∧ (RepB ∨ Bcast1) ∧ (RepB ∨ Bcast2)`.
  Minimal solutions: **{RepA, RepB}** and **{Bcast1, Bcast2}**, compared with the 16 elements of the
  power set.
* **Alternating execution (§2.3).** If an injected hypothesis does not cause a failure, the new run's
  lineage graph is **merged** with the current one, and a new formula is extracted and solved.
* **Challenges (§4):**
  1. No Dedalus. Call-graph traces stand in for lineage.
  2. Call graphs show **no redundancy**.
  3. It is hard to define "success". HTTP status codes are used inconsistently.
  4. There is no replay capability.
* **Solutions (§5):**
  * *Success* is taken from device-reported metrics. An experiment counts as having found a bug only
    if **more than 75% of affected requests fail**. **Missing metrics are treated as failure**,
    because the injected faults sometimes broke the reporting path itself.
  * *Replay* is replaced by **request classes**. `interaction ≡ nodes ∘ callgraph ∘ trace : R → 2^S`,
    and `r ∼ r' ⇔ interaction(r) = interaction(r')`. A classifier learns a partial function f from
    request attributes (URI, device type, query parameters, Falcor parameters) to the class, emitting
    a value only with high confidence. The deployed version is a single-label classifier over the
    lexicographically sorted service set, turned into a string. A multi-label formulation was
    explored.
  * *Lineage.* The model of redundancy for a class is **the conjunction of all call graphs observed
    under different fault experiments**. Each successful experiment adds an alternative computation,
    such as a fallback subgraph.
* **Service architecture (§6.1).** A daemon runs three job types: *training* (classifier from
  production traces), *model enrichment* (add call graphs from experiments that produced no
  user-visible error), and *experiments* (install the classifier and hypotheses on Zuul).
* **App Boot case study.** Dozens of services and hundreds of injection points, about 2^100 brute-force
  experiments. LDFI covered the space in **fewer than 200 experiments** and found **11 new critical
  failures**. Example: crashing `EC_MAP_LT` (the EVCache LOLOMO cache) did not fail, because the API
  fell back to MAPLOLOMO plus GROUP_SERVICE, GPS_FRONTEND and other services. That fallback subgraph
  became new support, so LDFI never again injects into EC_MAP_LT without also hitting the fallback.
* **Future work (§8).** Move from decision ("is there a falsifier?") to **optimization** ("what is the
  most likely falsifier?") using MTBF, topology, and version metadata. Co-evolve tracing and fault
  injection (OpenTracing baggage, SDN).

**Lesson for us.** Coarse lineage still works. It just needs more forward runs, because redundancy is
discovered rather than read off. Fine-grained lineage prunes more per step.

---

## 5. *Abstracting the Geniuses Away from Failure Testing* (ACM Queue 2017)

A position paper. The details that matter to us:

* Chaos Engineering and Jepsen depend on "superusers" who (1) observe the system, (2) build a mental
  model of how it tolerates faults, and (3) choose faults that hit weaknesses. LDFI automates steps 2
  and 3.
* "**Fault tolerance is redundancy.** A system is fault tolerant if it provides sufficient mechanisms
  to achieve its successful outcomes despite the given class of faults." An experiment should knock
  out *all supports* of an expected outcome.
* LDFI is described as a **materialized-view maintenance / how-to query** problem. The system is a
  query, expected outcomes are its results, and facts such as "replica A is up at t" or "connectivity
  X→Y during [i..j]" are base facts. The question is which changes to base data change the view
  (Meliou & Suciu's *Tiresias*).
* **Stated shortcoming: the determinism assumption.** Timing uncertainty is ignored. The authors call
  for *probabilistic* models of redundancy.
* "**Don't overthink fault injection.**" From the caller's point of view, all callee faults show up as
  error returns, corrupted responses, or delay. Test the caller's fault-handling logic.
* **Explanations should serve debugging too.** Subtract the (incomplete) explanations of bad outcomes
  from those of good outcomes. The root cause is likely near the *frontier* of the difference. This
  cites differential provenance (Chen et al. SIGCOMM'16) and leads directly to Nemo.
* Proposes collecting system-call-level provenance from unmodified distributed software, running in
  containers, to pick custom fault schedules. (Separately, Nemo's paper cites an LDFI deployment at
  eBay, reference [14] there.)

**Growing a Protocol (HotCloud 2017; skimmed).** Elastic's primary/backup replication protocol was
developed as a sequence of Dedalus versions, and LDFI was run in **continuous integration** on each
version. Two points: fault-tolerance bugs are tied to *schedules*, not inputs, so regression tests
cannot guard them across versions; and LDFI found a concurrent-writes bug that needs a primary
failover after partial replication, and showed the bug was dormant in earlier single-write versions.
**Takeaway:** LDFI should run as a CI regression gate over program versions.

---

## 6. Provenance-based debugging

### 6.1 Nemo: *Fixed It For You: Protocol Repair Using Lineage Graphs* (CIDR 2019)

**Model and assumptions (§3.1).** The omission fault model (loss plus crash, no Byzantine faults). At
least two processes communicating by messages. The input is a collection of provenance graphs from a
series of runs, where the last run is the failed one, supplied by an experiment selector (Molly). The
debugger runs in a loop with the bug finder: select a bug → strategies → suggestions → operator
applies them → resubmit.

**Two kinds of bug:**

* *Errors of commission* are a wrong line: a bad state transition, a misconfiguration, an off-by-one.
  Differential provenance handles these.
* *Errors of omission* are a missing mechanism: insufficient synchronization, missing retry or
  replication. There is no offending line.

**Correctness specs** are implications A → C over distributed state (the same as pre/post). For
Nemo's repairs to work, **the program and its specification must be written in the same
provenance-enhanced language.** A holds when the run is not vacuous.

**Graph algebra (§3.3).** P = ⋃_i {Prov_i^A, Prov_i^C}.

* Operations: A ∩ B (shared vertices and edges), A ∪ B, A − B (remove B's vertices and edges from A).
* Selectors: `prop_{x=y}(A)` (subgraph where property x = y), `normalize(A)` (hide run-specific
  details, for example **collapse chains of the same event type**; the code collapses `@next`
  persistence chains), `leaves(A)`, `roots(A)`, and `reachable_A(V)`.

**Strategies (§3.4):**

1. **Differential consequent provenance:** `Diff^C := leaves(Prov_1^C − Prov_f^C)`, the frontier of
   rules that fired in a good run but not in the failed run. Good for errors of commission.
   *Pitfall:* for async primary/backup it suggests "retry replicate", but every retry pattern has a
   matching loss pattern.
2. **Skeleton differential:** `SkelDiff^C := leaves((⋂_{i=1..s} normalize(Prov_i^C)) − Prov_f^C)`.
   The intersection over all successful runs is the protocol "skeleton", with incidental variation
   such as retry counts removed.
3. **Corrections generation** for errors of omission. **Make A harder to establish** rather than C
   easier: `Deps^A := reachable_{Prov_1^A}(leaves(prop_{A=true}(Prov_1^A))) ∪ leaves(prop_{C=true}(Prov_1^C))`.
   The triggers that establish C become dependencies of A. When C ranges over several nodes, the new
   dependencies must be *communicated*: add messages that tell A's node about C's remote state.

Also: **hazard-window analysis** (code) colors space-time vertices red where pre holds and blue where
post holds, showing the window between A and C. **Extensions** (code): if some run never achieved A,
suggest making the async rules on A's path more robust.

**Evaluation (§4).** From 52 TaxDC bugs: timing bugs (message–message, message–local, and local–local
races, plus "premature success") are *correctable*. That is 24 bugs. Logic bugs (state transition,
config, fallback, concept) are not correctable, but 12 get differential-provenance assistance. Case
studies:

* **CA-2083** (message–message race: a schema message and a data message race). Nemo synthesized a
  one-line fix that enforces the order, plus robustness suggestions.
* **ZK-1270** (message–local race). Fix: `success(L) :- sent_flag(L), ack(F).`
* **MR-2995** (state transition). No repair; differential provenance points at the missing
  "completion" message.
* **Async primary/backup** (premature success; Figure 1). Nemo proposes `ack_log` from the replicas
  to the client, with receipt from all replicas required before success. That is about 4 changed
  lines.

**Figure 1 program (the repository version, `case-studies/pb_asynchronous.ded`), verbatim core:**

```
request(Prim, Pload, Cli)@async :- begin(Cli, Pload), conn_out(Cli, Prim);
ack(Cli, Prim, Pload)@async :- request(Prim, Pload, Cli);
acked(Cli, Prim, Pload) :- ack(Cli, Prim, Pload);
acked(Cli, Prim, Pload)@next :- acked(Cli, Prim, Pload);
replicate(Rep, Pload, Prim, Cli)@async :- request(Prim, Pload, Cli), replica(Prim, Rep);
log(Prim, Pload) :- request(Prim, Pload, Cli);
log(Rep, Pload) :- replicate(Rep, Pload, _, _);
log(Rep, Pload)@next :- log(Rep, Pload);
pre(Pload) :- acked(Cli, Prim, Pload);
post(Pload) :- log(Node, Pload), primary(Prim, Prim), notin crash(Node, Node, _), Node != Prim;
```

It is run with `--EOT 6 --EFF 4 --crashes 1 --prov-diagrams --negative-support --nodes C,a,b,c`. The
EDB (network/primary/replica/client/conn_out facts with persistence, `begin("C","foo")@1`) is in the
file.

**Implementation (code).** Go. Molly output (a fork on the `graphing` branch that emits pre and post
provenance per run plus `timePreHolds`/`timePostHolds`) is loaded into **Neo4j**. Nodes are `Goal` and
`Rule` with `run`, `condition` ∈ {pre, post}, `condition_holds`, `type` ∈ {next, async, ...},
`table`, and `time`. Strategies are Cypher queries using APOC export/import to materialize derived
graphs. The output is an HTML report. **[Recommendation]** Implement the graph algebra natively over
our provenance store. A graph database is unnecessary.

### 6.2 Why-across-time (wat) provenance (Whittaker et al., SoCC 2018)

Neither causality nor data provenance is enough on its own. The causal history of an event
(happens-before) **over-approximates** its causes: it includes every earlier event, relevant or not.
Why-provenance is precise but assumes a *static* database of relational queries.

**Definitions** (deterministic state machine M = (S, s0, Σ, Λ, δ, ε), trace T ∈ Σ*, input i,
o = ε(δ*(s0,T), i)):

* A subtrace T' of T (a subsequence, not necessarily contiguous) is a **witness** of o iff
  ε*(s0, T'i) = o.
* A witness T' is **closed under supertrace in T** iff every supertrace of T' within T is also a
  witness.
* **Wat(M,T,i)** = the minimal elements of the set of witnesses closed under supertrace. The order
  matters: minimal(closed(witnesses)), *not* closed(minimal(witnesses)). Example: with
  `set(x,42); add(x,1)` and similar traces, a small witness can become wrong when later inputs are
  added back.
* **Claim 1:** for monotone relational queries over insert-only traces, Wat = MWhy (minimal
  why-provenance).
* **Claim 2:** wat-provenance refines causality.
* Wat-provenance is node-local. Cross-node explanation is obtained by recursing on the provenance of
  the witness messages.
* Computing Wat for an arbitrary black box is intractable. **Wat-provenance specifications** are
  hand-written functions `(T, i) → witnesses` for simple APIs (Redis get/set, S3, HDFS, Zookeeper).
  The prototype is **Watermelon**.

**Relevance to us.** (a) It justifies data lineage over Lamport causality. Molly's `pcausal` strategy
is the causal baseline, and it prunes less. (b) The supertrace-closure condition is the right notion
of "support" for **non-monotone** state (deletes and overwrites), which matters when our language has
mutable state through `@next` with negation. (c) Wat specifications are a way to bring **external
black-box services** (a database, a KV store) into our lineage when they are modelled as foreign
components.

---

## 7. Provenance foundations

### 7.1 Provenance semirings (Green, Karvounarakis, Tannen, PODS 2007)

* **K-relation:** a function R: U-Tup → K with finite support. K = (K, +, ·, 0, 1) is a *commutative
  semiring*: + is used for union and projection (alternative derivations) and · for join (joint use).
  Instances: (B, ∨, ∧) is set semantics; (N, +, ·) is bag semantics; (PosBool(B), ∨, ∧) gives
  c-tables; (P(Ω), ∪, ∩) gives event tables.
* **Positive algebra RA+** on K-relations: selection multiplies by the 0/1 predicate, projection sums,
  union adds, join multiplies. Proposition 3.4: the standard RA identities hold iff K is a
  commutative semiring. Proposition 3.5: query evaluation commutes with h iff h is a semiring
  homomorphism.
* **The paper's "why-provenance"** is (P(X), ∪, ∪, ∅, ∅), the set of contributing input tuples. Later
  papers call this *lineage*, Lin(X). It cannot tell *how* tuples contributed.
* **How-provenance, N[X]** (Definition 4.1): polynomials over tuple ids. It is the most general
  semiring: for any commutative K and valuation v: X→K there is a unique homomorphism
  Eval_v: N[X]→K (Proposition 4.2, Theorem 4.3: evaluation factors through N[X]).
* **Datalog** (§5): the annotation of a tuple is the **sum over all derivation trees of the product of
  their leaves' annotations**. Infinitely many trees require an **ω-continuous** semiring (naturally
  ordered, with sups of ω-chains, + and · ω-continuous). Examples: B, N∞, PosBool(B) for finite B, and
  (for Datalog provenance) **N∞[[X]]**, formal power series. Semantics = the least solution of the
  algebraic system given by the immediate-consequence operator (Theorem 5.6).
* **Theorem 6.5:** a tuple's provenance series is in N[[X]] (no ∞ coefficients) iff the instantiated
  program has no cycle of **unit rules** (rules whose body is a single IDB atom) through t.
* **Algorithm All-Trees:** bottom-up enumeration of derivation trees. A tree is moved to T∞ (infinite
  provenance) when a child is already in T∞, or when a proper descendant of the root carries the same
  tuple (a cycle). It decides whether P(t) is a polynomial and computes it. **Monomial-Coefficient**
  computes the coefficient of a given monomial, possibly ∞.
* **The semiring hierarchy** (secondary sources; Green ICDT 2009 as described in later surveys):
  N[X] → B[X] (drop coefficients) → Trio(X) (drop exponents) → Why(X) = sets of witness sets
  (multilinear B[X]) → PosBool(X) (Why with absorption: minimal witnesses) and Lin(X) (flattened set).
  For Datalog with recursion, *absorptive* semirings (a + a·b = a) such as PosBool(X) guarantee that
  naive and semi-naive fixpoints converge. See Bourgaux et al., KR 2022, which I identified but did
  not read in depth.

**What LDFI needs, in semiring terms.** Annotate every `clock(from,to,t)` fact with the variable
`m_{from,to,t}` (with from≠to and t < EOT) and every other EDB fact with 1 (true). The PosBool(M)
annotation of the goal g is φ_g = the disjunction of its minimal message supports. The falsifier
formula of Algorithm 2 is the **dual** of φ_g with literals read as losses: hitting every minimal
support is the same as satisfying the CNF ⋀_{S ∈ minsupp(g)} ⋁_{m ∈ S} lost(m). Crash variables then
widen each `lost(m)` to `O(m) ∨ ⋁_{t'≤t} C(from,t')`. Negation, and the other things Algorithm 2
over-approximates, sit outside the semiring framework; that is why Molly needs the conservative
negative-support rule.

### 7.2 Firing-graph rewrite (Köhler, Ludäscher, Smaragdakis, Datalog 2.0 2012)

This is the rewrite Molly cites.

* **P^F (record firings):** each rule `r: H(Ȳ) :- B1(X̄1),…,Bn(X̄n)` becomes
  `r_in: fire_r(X̄) :- B1(X̄1),…,Bn(X̄n).` and `r_out: H(Ȳ) :- fire_r(X̄).`, where X̄ is *all* the
  rule's variables.
* **P^G (reify as a graph):** `g(Bi(X̄i), in, fire_r(X̄)) :- fire_r(X̄).` and
  `g(fire_r(X̄), out, H(Ȳ)) :- fire_r(X̄).`, giving a labelled edge relation.
* **Statelog stage rewrite:** add a state argument S with `next(S,S1)`, record in which round a firing
  first occurred, and derive `len(F) = 1 + max len(body atoms)` and `len(A) = min over firings`. These
  are derivation lengths.
* **Views over g:** `ProvView(Q,X,L,Y)` gives the upstream subgraph of a debug atom Q. It also
  supports profiling (hot and cold rules).

### 7.3 Distributed provenance (ExSPAN, SIGMOD 2010)

For a *real* deployment of our language, as opposed to the simulator.

* **Reference-based distributed provenance:** `prov(@Loc, VID, RID, RLoc)`, meaning tuple vertex VID
  at Loc was derived by rule execution RID located at RLoc. `ruleExec(@RLoc, RID, R, VIDList)` holds
  the rule label and the input VIDs.
* **IDs:** `VID = SHA1(relation + attrs...)`, e.g. `SHA1("pathCost"+X+Y+C)`.
  `RID = SHA1(rule + location + input VIDs)`.
* Shipped tuples carry only the 20-byte RID and RLoc. The provenance stays distributed, and queries
  are recursive distributed queries over `prov`/`ruleExec`.
* The alternative is **value-based** provenance: ship the provenance expression (polynomials,
  optionally condensed into BDDs) with each tuple. It costs more bandwidth.

### 7.4 Negative (why-not) provenance (Wu et al., SIGCOMM 2014)

This is Molly's option 2 for negated subgoals, and the basis for *why-not* debugging queries.

* **Positive vertex types:** `EXIST([t1,t2],N,τ)`, `INSERT/DELETE(t,N,τ)` (base tuples),
  `DERIVE/UNDERIVE(t,N,τ)`, `APPEAR/DISAPPEAR(t,N,τ)`, `SEND(t,N→N',±τ)`, `RECEIVE(t,N←N',±τ)`,
  `DELAY(t,N→N',±τ,d)`.
* **Negative twins, all over time *intervals*:** `NEXIST`, `NINSERT/NDELETE`, `NDERIVE/NUNDERIVE`,
  `NAPPEAR/NDISAPPEAR`, `NSEND/NRECEIVE`, and `NARRIVE([t1,t2], N1→N2, t3, τ)` (sent at t3, did not
  arrive in the window).
* **Required properties:** soundness (consistent with the trace), completeness (no execution
  consistent with the explanation lacks e), and minimality.
* **Construction** is **top-down and on demand** through `QUERY(v)`, which returns v's children,
  because the negative graph is infinite. It uses a log of (±τ, N, t, rule, derivation-counter)
  entries. Examples: `QUERY(NEXIST([t1,t2],N,τ))` → the last DISAPPEAR before t1 plus
  NAPPEAR((tx,t2]), or NAPPEAR([0,t2]). `QUERY(NAPPEAR)` → NINSERT for base tuples, NDERIVE for each
  rule with a matching head for local tuples, or NRECEIVE for remote ones. `QUERY(NRECEIVE)` →
  NSEND/NARRIVE for each possible sender, over intervals between actual sends, with Δmax the maximum
  network delay.
* **PARTITION heuristic:** a missing derivation `A :- B, C` can be explained by the absence of B
  *or* of C over parts of the parameter space. Choosing the partition that gives the smallest
  explanation is set-cover hard, so they use a greedy choice of the largest subspace with a short
  look-ahead. **SENDERS** narrows the possible senders using topology or static analysis.
* Coalesce identical vertices with adjacent or overlapping intervals to keep the explanation minimal.

### 7.5 Efficient provenance in a production Datalog engine (Soufflé; Zhao, Subotić, Scholz, TOPLAS 2020)

* **Goal:** debugging queries over tens of millions of tuples at low overhead. Reported overhead:
  **1.27× runtime and 1.45× memory** on average. Full proof trees can have height > 200 (Doop).
* **Proof annotations:** each tuple stores `@rule` (the number of the rule that produced it) and
  `@height` (the height of its *minimal-height* proof tree). A rule
  `ρk: R(X) :- R1(X1),…,Rn(Xn), ψ` is rewritten to
  `R(X, k, max(@h1,…,@hn)+1) :- R1(X1,_,@h1),…,Rn(Xn,_,@hn), ψ`. EDB tuples have height 0.
* **Provenance lattice:** `(I1,h1) ⊑ (I2,h2) ⇔ I1 ⊆ I2 ∧ ∀t∈I1: h1(t) ≥ h2(t)`. The consequence
  operator is `T_P(I,h) = (Γ_P(I), h')` with `h'(t) = min_{g∈G_t} (max_{ti∈g} h(ti) + 1)`, or h(t) if
  t has no rule body configurations. Lemma 2: same tuples as Γ_P. Theorem 1: the heights at the
  fixpoint are minimal and each corresponds to a real proof tree.
* **Update semantics in semi-naive evaluation:** a re-derived tuple with a *smaller* height replaces
  the annotation. `Δ^{i+1} = (new^{i+1} − R^i) ∪ {t ∈ R^{i+1} | h^i(t) > h^{i+1}(t)}`.
  Complexity: O(n × max h) updates in the worst case, which is quadratic, and the bound is tight
  (Figure 11 example). It is rare in practice.
* **Data structures:** the insert index order *excludes* the annotation columns, so existence checks
  ignore annotations and set semantics are preserved. The retrieve order puts the annotations *last*,
  so updating them in place does not reorder the B-tree. The insert operation performs the update
  when the new height is smaller, upgrading an optimistic read lease to a write lease. There is a
  `PROV NOT IN` existence check in the RAM.
* **Proof construction ("subproof search"):** top-down and one level at a time. For tuple t with rule
  ρ(t), search `? :- R1(X1),…,Rn(Xn), ψ, matches(t, X…), h(Ri(Xi)) < h(t) ∀i`. This is "0-IDB
  Datalog": every relation is treated as EDB, and the search stops at the first solution, which is
  guaranteed to extend to a minimal-height proof. Subroutines are compiled into the RAM and to C++.
  Commands: `explain alias("a","b")`, `setdepth 6`.
* **Non-existence (`explainnegation`):** semi-automated. The user picks a rule and variable values,
  and the system marks which body atoms fail. The result is one *failing* proof tree.
* **Generalization (§3.x):** any annotation metric works if (1) its codomain is partially ordered, so
  updates are well defined; (2) it is compositional, `h(t) = f(h(t1),…,h(tn))`; and (3) it is monotone
  and bounded below. Examples: proof size, or the k smallest proof heights.

**The key contrast for us.** Soufflé keeps **one** minimal proof per tuple. LDFI needs **all**
alternative supports, because redundancy is the whole point. With a single proof per tuple the search
is still complete, since LDFI re-runs and finds new supports, but it takes far more forward steps. That
is the Netflix call-graph situation. Soufflé's machinery (annotation columns, update-in-place,
top-down subproof search over the materialized model) is still the right template for the *debugger
UI* and for **on-demand provenance reconstruction** (§10).

---

## 8. Relationship to the rest of bloom-remake (brief)

* Dedalus semantics (time, `@next`, `@async`, temporal stratification) come from the Dedalus cluster.
  LDFI needs a **synchronous, deterministic simulation mode** of the same language, with delivery at
  t+1 unless a fault is injected.
* **CALM and monotonicity:** for monotone programs, positive why-provenance is enough (paper §6).
  Non-monotone programs need why-not reasoning. Our CALM analysis can decide per rule whether negative
  support is needed, which saves work.
* **Lattices (Bloom^L):** threshold tests on monotone lattices, such as `size(votes) > n/2`, should get
  **k-of-n provenance**, not Molly's all-contributors-conjunctive aggregate provenance
  ([Recommendation] §10).

---

## 9. Hydro (note)

The Hydro homepage advertises deterministic simulation testing. The type system pushes users to
remove nondeterminism, the remaining `nondet!` points are explored exhaustively or by fuzzing, network
failure modes are part of the types (for example `TCP.fail_stop()` and `TCP.retry_on_fail()`), and a
failing schedule can be replayed deterministically. I could not reach the detailed docs (404), so I
cannot say whether Hydro uses lineage to prune. It is useful context: our simulator should *also*
support exhaustive and fuzzed exploration of ordering nondeterminism, which LDFI leaves out, next to
LDFI's lineage-guided exploration of faults.

---

## 10. [Recommendation] Design implications for a fast LDFI in bloom-remake

These are my proposals. They are not in the literature, except where cited.

1. **The fault model as an input to the evaluator, not EDB facts.** Keep the *logical* model
   (faults = deletions of `clock(from,to,t)` / crash = batch deletion) so the theory carries over.
   Implement `clock` as a **virtual relation** answered by a `FaultSchedule { omitted: set<(from,to,t)>,
   crash_at: map<node,t> }`, instead of materializing O(N²·EOT) facts. Async firings must still record
   *which channel-time they used*, because those are the lineage leaves.
2. **Crash semantics:** stop *all* rule evaluation at a crashed node from its crash time, as the paper
   says. Keep a `crash` oracle relation that is **available only to spec rules** (`pre`, `post`,
   helpers), enforced by the compiler.
3. **Complete provenance without a firing-table blowup.** Three options:
   * (a) Eager firings (Molly/Köhler): simple, but memory grows with every persistence step.
   * (b) **Lazy top-down reconstruction over the time-indexed model.** For stratified programs the set
     of firings *equals* the set of body instantiations that hold in the final model (negation read
     against the model), so no firing capture is needed during forward runs. Reconstruct only the cone
     of `pre`/`post` on demand, using Soufflé-style indexed subproof search. Aggregates are
     reconstructed from the group's contributors at that timestep. **This is the preferred default:**
     most forward runs only need the oracle check. Backward analysis is only needed for runs that
     pass.
   * (c) Online PosBool(M) annotations with absorption: good when supports are small, but there is a
     DNF-size risk.

   In every case, store the model with **validity intervals** (a tuple is present on [t1, t2]) rather
   than one copy per timestep. Persistence chains then become one interval, and collapsed chains are
   what Nemo's `normalize` shows users anyway.
4. **Encode the AND/OR DAG directly** with Tseitin variables (as Molly's Z3 path effectively does) or
   as a **monotone hitting-set problem**. Never distribute to CNF naively.
5. **Monotone crash encoding.** Use order variables `K(n,t)` = "n crashed at or before t", with
   `K(n,t) → K(n,t+1)`. A clock leaf (from,to,t) is falsified by `O(from,to,t) ∨ K(from,t)`, and
   `Σ_n K(n,EOT−1) ≤ maxCrashes`. The formula is then monotone in all fault variables, so **minimal
   models = minimal falsifiers**. Enumerate them with an incremental SAT solver: get a model, shrink
   it greedily to minimal, block it with `⋁_{v∈min} ¬v`, and repeat. This avoids enumerating every
   model and filtering afterwards, as Molly does. Loss literals exist only for `1 ≤ t < EFF`.
6. **Re-execution reuse.** The simulation is deterministic and time-stepped, so the state at time t
   depends only on faults at times < t. **Checkpoint the failure-free run (and every run) per
   timestep** and resume each hypothesis from the checkpoint at `min(fault times)`. Run hypotheses
   **in parallel**, since they are independent. Deduplicate hypotheses (and use symmetry) *before*
   execution.
7. **Pre-vacuity pruning before execution** (paper §4.3): drop a hypothesis if every post tuple it
   falsifies also has its matching pre tuple falsified.
8. **Quorum and threshold provenance.** For `count<>`/`size` compared against a threshold (`> n/2`),
   the support is **any k of the n contributors**. Encode "falsify" as the cardinality constraint "at
   least n−k+1 contributors lost", instead of treating every contributor as essential. This cuts the
   hypothesis count for Paxos and Raft.
9. **Merge lineage across runs** (the Netflix approach) as an optional accelerator. Keep the per-run,
   seed-superset search as the reference semantics, because Theorem B.2 is proved for it.
10. **Parameter sweep** as in §2.1.1, plus a "find all counterexamples" mode, plus "minimal
    counterexample parameters" reporting.
11. **Debugger:** space-time diagram, lineage graph, Nemo graph algebra, hazard windows, Soufflé-style
    `explain` / `explainnegation`, and Wu-style interval why-not queries.

---

## 11. MUST-IMPLEMENT CHECKLIST

Each item: **feature**, a precise description, and its source.

### A. Language features the LDFI tooling depends on

1. **Location specifier**: column 0 of every relation is typed LOCATION. Protocol rules must have all
   body atoms at the same location; a head at another location must be `@async`. — Molly
   README/tutorial; `DedalusTyper`
2. **Temporal heads**: none (same tick), `@next` (t+1, same node), `@async` (receiver, t+1 in the
   synchronous simulator unless lost). `@k` on a rule head is an error. — paper §2.2; `DedalusRewrites`
3. **Absolute-time body atoms**: `p(...)@k` in a body matches p at time k (e.g.
   `notin bcast(X,Pl)@1`, `begin(A,X)@1`). — `DedalusRewrites.rewriteBodyElem`
4. **Timestamped facts**: every fact carries `@k` and holds only at time k unless persisted. Time
   starts at 1. — `DedalusRewrites`; examples
5. **Explicit persistence** through frame rules `p(..)@next :- p(..);` Relations without them are
   ephemeral. — tutorial
6. **`notin` negation** with range restriction (negated variables bound positively; head variables
   bound). — README
7. **Temporal stratification**: negation cycles are allowed only through `@next`/`@async`; evaluate a
   stratified deductive fixpoint per timestep. — Molly `2pc.ded` requires it; C4Wrapper per-timestep
   installation
8. **Aggregates in heads** (`count<X>`, `min<X>`, `max<X>`, plus `sum`) with group-by = the other
   head columns. — examples; `splitAggregateRules`
9. **Arithmetic and comparison** in heads (`T-1`, `Id+1`, `S+C`) and qualifiers (`!=`, `==`, `<`,
   `>`, `/`, `*`), with proper precedence (unlike Molly's right-nested parse). — `DedalusParser`
10. **`include "file";`** for textual inclusion, resolved relative to the including file. — Molly
    parser (Molly resolves against the first file's directory)
11. **Type inference** over INT, STRING, LOCATION by unifying columns, with located error messages. —
    `DedalusTyper`
12. **Spec rules**: a distinguished class of rules (defining `pre`, `post`, and helpers) that may join
    across locations and may read oracle relations. — Molly examples (Kafka `good`, 2pc_assert)

### B. Failure model and simulation

13. **Fspec ⟨EOT, EFF, maxCrashes⟩ plus node list**; admissible = omissions only at send times in
    `1 ≤ t < EFF` (strict, matching the code), and at most maxCrashes crashes; `EFF < EOT`. — paper
    §2.1.1; `FailureSpec`
14. **Synchronous deterministic delivery**: async messages sent at t arrive at t+1; delivered-message
    order is deterministic. — paper §2.1
15. **Omission unit = channel-time (from, to, t)**: all messages from→to sent at t are dropped
    together; self-sends cannot be dropped. — `FailureSpec.generateClockFacts`; `MessageLoss`
16. **Permanent crash at time t**: node n sends nothing from t on. Implement the paper's semantics
    (no internal transitions either). — paper §2.1.1; code differs (§3.4)
17. **`crash(Observer, Node, CrashTime)` oracle** visible at all times and all observers, **only to
    spec rules**. — `FailureSpec.generateClockFacts`; `deliv_assert.ded`
18. **Failure-free baseline run** (EFF=0, crashes=0) as the starting point. — `Verifier`
19. **Parameter sweep mode**: EFF=0 and grow EOT until non-vacuous; grow EFF; on a violation grow EOT;
    at EFF=EOT−1 grow both; stop at a wall-clock bound; report minimal failing or maximal certified
    parameters. — paper §2.1.1
20. **Run-count estimator (`grossEstimate`)** for reporting "Combinations". — `FailureSpec`

### C. Invariants and the oracle

21. **`pre` → `post` invariants evaluated at EOT**; a run violates iff some failure-free post tuple is
    missing from post and present in pre (`isGood`); pre and post share a schema. — paper §2.3;
    `Verifier.isGood`
22. **Vacuity**: runs where pre does not hold are correct. — paper §2.3

### D. Provenance

23. **Complete rule/goal graph** for every pre/post tuple at EOT: goal nodes = facts (with time and
    location), rule nodes = firings with bindings, *all* alternative firings. — paper §4.2
24. **Provenance capture**: either a firings rewrite `r_prov` (Köhler) that records at least the join,
    head, and expression variables plus send and receive time for async rules, or an equivalent
    on-demand reconstruction over the materialized time-indexed model. — paper §4.1.2; Köhler 2012;
    Soufflé §3.3
25. **Aggregate provenance**: an aggregate tuple depends on all contributing group tuples at that time
    (baseline); split aggregation rules so the extra bindings do not change grouping. — paper §4.1.2;
    `ProvenanceReader.getAggregateSupport`
26. **Negative-subgoal support**, configurable: off (monotone, fast) / conservative (static
    reachability, time ≤ goal time, strict across temporal edges, optionally odd-negation parity) /
    surrogate (Wu). — paper Appendix B; `ProvenanceTableManager.possibleCauses`
27. **Message log per run**: (relation, from, to, sendTime, receiveTime | LOST), sends at EOT
    excluded. — `ProvenanceTableManager.messages`

### E. Hazard analysis and solving

28. **`clocks()` formula**: goal = AND over firings; firing = OR over premises; clock leaf with
    from≠to = `O(f,t,time) [if time<EFF] ∨ ⋁ C(f, t'≤time)`; other leaves = false; negative goal = OR
    over possible causes. DAG-memoised. — paper Algorithm 2; `Z3Solver`
29. **Crash constraints**: each node crashes at most once; at most maxCrashes crash; crash variables
    only for nodes that sent in the lineage, from their first send time up to EOT−1. — `Z3Solver`;
    `SAT4JSolver`
30. **Seeded incremental search**: hypotheses for a passing faulty run must contain its fault set. —
    `Verifier.runFailureSpec`
31. **One problem per goal tuple, union of solutions, minimal sets only.** — paper §4.3;
    `Solver.solve`; `SetUtils.minimalSets`
32. **Normalization**: drop omissions subsumed by a crash of the sender at or before the loss time
    (and the receiver rule, fixed); drop empty solutions. — `Solver.solutionToFailureSpec`
33. **Pre-vacuity pruning** of hypotheses. — paper §4.3

### F. Driver

34. **Forward/backward worklist loop** with a visited set, stop-at-first or find-all, emitting a lazy
    stream of runs (status, fault set, model, messages, provenance). — paper Algorithm 1; `Verifier`
35. **Symmetry reduction**: node permutations fixing the program EDB and absent from rule literals;
    equivalence of fault sets under such permutations. — `symmetry/`
36. **Random fault-injection baseline** for benchmarks. — paper §5.2; `Verifier.random`
37. **Causal-only (happens-before) lineage ablation**. — `strategy pcausal`

### G. Reporting and debugging

38. **Space-time (Lamport) diagram** per run: processes, per-time vertices, message edges labelled
    with the relation, lost messages dashed red, crashes as red "CRASHED". — paper §5; `SpacetimeDiagramGenerator`
39. **Lineage graph rendering**: goals as ellipses (negative goals inverted), firings as boxes, with an
    upstream-subgraph view for one goal. — `ProvenanceDiagramGenerator`; Köhler `ProvView`
40. **Machine-readable run report** (`runs.json`-equivalent) plus HTML index. — `HTMLWriter`
41. **Nemo graph algebra and strategies**: ∩, ∪, −, prop, normalize (collapse `@next` chains),
    leaves, roots, reachable; DiffC, SkelDiffC, DepsA corrections; hazard windows. — Nemo CIDR'19 §3
42. **Minimal-height proof explanation** (`explain t`, fragment depth) and interactive
    `explainnegation`. — Soufflé TOPLAS'20 §3–4
43. **Interval-based why-not provenance** (NEXIST/NAPPEAR/NDERIVE/NRECEIVE/NSEND/NARRIVE) for missing
    events. — Wu et al. SIGCOMM'14

### H. Provenance-semiring core (engine-level)

44. **Pluggable semiring annotations** on evaluation (B, N, N[X], Why(X), PosBool(X) with absorption,
    Lin(X), tropical/min-height), including detection of unit-rule cycles, where coefficients become
    infinite. — Green et al. PODS'07 §4–7; Soufflé §3.x
45. **Distributed provenance for real deployments**: reference-based `prov`/`ruleExec` with hashed
    VID and RID. — ExSPAN SIGMOD'10

---

## 12. TEST PROGRAMS (end-to-end, with expected behaviour)

All paths are relative to `molly/src/test/resources/examples_ft/` unless noted. Nodes are given as the
`-N` list. "CE" = counterexample found. Expected results are taken **verbatim from Molly's
`CounterexampleSuite.scala`**, the paper's Figures 12 and 13, and the Nemo README/paper. Our
implementation should reproduce the CE / no-CE verdicts exactly. Execution counts are a performance
target, not a correctness criterion, because our search order and minimization may differ.

### 12.1 Reliable-delivery family (nodes a, b, c; spec `delivery/deliv_assert.ded`; EDB `delivery/bcast_edb.ded`: full `node` mesh plus `bcast("a","hello")@1`)

| Program | EOT | EFF | Crashes | Expected |
|---|---|---|---|---|
| `delivery/simplog.ded` (simple-deliv) | 6 | 3 | 0 | **CE** (drop a→b at t=1) |
| `delivery/simplog.ded` | 4 | 2 | 0 | **CE** (paper minimum; Molly takes 2 executions) |
| `delivery/rdlog.ded` (retry-deliv) | 6 | 3 | 0 | no CE |
| `delivery/rdlog.ded` | 25 | 23 | 0 | no CE (`TableOfCorrectPrograms`: infinite retries mask any finite loss pattern when nothing crashes) |
| `delivery/rdlog.ded` | 6 | 3 | 1 | **CE** (a crashes after reaching c but not b) |
| `delivery/rdlog.ded` | 4 | 2 | 1 | **CE** (paper minimum; 3 executions) |
| `delivery/classic_rb.ded` (classic-deliv) | 6 | 3 | 0 | **CE** (omission model) |
| `delivery/classic_rb.ded` | 5 | 3 | 0 | **CE** (paper minimum; 5 executions) |
| `delivery/classic_rb.ded` | 6 | 0 | 2 | no CE (correct under fail-stop) |
| `delivery/replog.ded` (redun-deliv) | 6 | 3 | 0 | no CE |
| `delivery/replog.ded` | 6 | 3 | 1 | no CE; the failure-free run's lineage alone should certify it (paper Figure 6) |
| `delivery/replog.ded` | 8 | 6 | 1 | no CE (`TableOfCorrectPrograms`) |
| `delivery/replog.ded` | 11 | 10 | — | no CE; 11 executions (Figure 13) |
| `delivery/ack_rb.ded` (ack-deliv) | 6 | 3 | 1 | no CE; needs several iterations because ACK-triggered retries appear only under faults |
| `delivery/ack_rb.ded` | 8 | 6 | 1 | no CE (`TableOfCorrectPrograms`) |
| `delivery/ack_rb.ded` | 8 | 7 | — | no CE; 673 executions (Figure 13) |

### 12.2 Commit protocols (nodes a, b, C, d; spec `commit/2pc_assert.ded` unless noted)

| Program | Spec | EOT | EFF | Crashes | Expected |
|---|---|---|---|---|---|
| `commit/2pc.ded` | 2pc_assert | 7 | 3 | 0 | no CE |
| `commit/2pc.ded` | 2pc_assert | 6 | 3 | 1 | **CE** |
| `commit/2pc.ded` | 2pc_assert | 6 | 0 | 1 | **CE** (blocking: the coordinator crashes after prepare; Figure 8) |
| `commit/2pc.ded` | 2pc_assert | 6 | 0 | 2 | **CE** |
| `commit/2pc.ded` | 2pc_assert | 5 | 0 | 1 | **CE** (paper minimum; 2 executions) |
| `commit/2pc.ded` | 2pc_assert_optimist | 6 | 0 | 1 / 2 | **CE** / **CE** (even ignoring coordinator failure) |
| `commit/2pc_timeout.ded` | 2pc_assert_optimist | 6 | 0 | 1 / 2 | no CE / no CE |
| `commit/2pc_timeout.ded` | 2pc_assert | 6 | 0 | 1 / 2 | **CE** / **CE** |
| `commit/2pc_ctp.ded` | 2pc_assert | 6 | 0 | 1 / 2 | **CE** / **CE** (CTP still blocks) |
| `commit/2pc_ctp.ded` | 2pc_assert | 8 | 0 | 1 | **CE** (paper minimum; 3 executions) |
| `commit/3pc.ded` | 2pc_assert | 8 | 0 | 1 / 2 | no CE / no CE (nonblocking under fail-stop) |
| `commit/3pc.ded` | 2pc_assert | 9 | 7 | 1 | **CE**: agreement violated (the coordinator aborts while agents commit after losses; Figure 9; 55 executions in the paper) |

Specs to copy verbatim: `2pc_assert.ded`
(`pre("termination",X) :- prepared(_,_,X,_); post("termination",X) :- decision(A1,X,_), decision(A2,X,_), A1 != A2; decision(C,X,"c") :- commit(C,X); decision(C,X,"a") :- abort(C,X); disagree(X) :- decision(_,X,V1), decision(_,X,V2), V1 != V2; pre("decide",X) :- decision(_,X,_); post("decide",X) :- decision(_,X,V), notin disagree(X);`).
`2pc_assert_optimist.ded` adds `post("termination", X) :- begin(A, X)@1, crash(A, A, _);`. The
reusable timer is `util/timeout_svc.ded`:

```
timer_state(H, I, T-1)@next :- timer_svc(H, I, T);
timer_state(H, I, T-1)@next :- timer_state(H, I, T), notin timer_cancel(H, I), T > 1;
timeout(H, I) :- timer_state(H, I, 1);
```

### 12.3 Kafka replication bug (`kafka.ded` + `fake_zk.ded` + `util/timeout_svc.ded`; nodes a, b, c, C, Z)

| EOT | EFF | Crashes | Expected |
|---|---|---|---|
| 7 | 4 | 1 | **CE**: b and c drop out of the ISR, the leader a acks alone, then a crashes (Figure 10) |
| 7 | 4 | 0 | no CE |
| 6 | 4 | 1 | **CE** (paper minimum; 38 Molly executions vs. 1183 random) |

Spec: `pre(X) :- ack(_, X, _); post(X) :- ack(_, X, _), write(R, X, _), notin crash(R, R, _);`

### 12.4 Agreement protocols (bug-free within the bound)

| Program | Nodes | EOT | EFF | Crashes | Expected |
|---|---|---|---|---|---|
| `paxos_synod.ded` | a, b, c | 8 | 3 | 1 | no CE (`TableOfCorrectPrograms`) |
| paxos-synod | a, b, c | 7 | 6 | — | no CE; 173 executions (Figure 13) |
| bully-le (`util/leader.ded` is related; the exact paper file is unconfirmed) | — | 10 | 9 | — | no CE; 2 executions (Figure 13) |
| flux (`flux/flux.ded` + includes) | — | 22 | 21 | — | no CE; 187 executions (Figure 13) |

The Paxos spec is agreement over values accepted by non-crashed acceptors:
`important(A,M) :- accepted(A,_,M), notin crash(A,A,_); pre(M) :- important(_,M); disagree(M) :- important(_,M), important(_,N), M != N; post(M) :- important(_,M), notin disagree(M);`.
Two proposers, `proposal("a","peter")@1` and `proposal("b","foobar")@1`, with seeds 4, 5, 6. This
program uses `count<>`, `max<>`, and arithmetic quorum tests (`Cnt2 > Cnt1 / 2`), so it exercises
aggregate provenance. [Recommendation] Also use it to benchmark k-of-n quorum provenance (§10 item 8).

### 12.5 Nemo case studies (`github.com/numbleroot/nemo/case-studies/`)

* `pb_asynchronous.ded`: EOT 6, EFF 4, crashes 1, nodes C, a, b, c, negative support on → **CE**
  (premature ack). After applying Nemo's `ack_log` repair (the client requires acks from all
  replicas): **no CE**. The corrections strategy should suggest adding a dependency from `pre`
  (`acked`) to the replication-acknowledgment triggers of `post`.
* `CA-2083-hinted-handoff.ded`, `ZK-1270-racing-sent-flag.ded`, `MR-2995-failed-after-expiry.ded`,
  `CA-2434-bootstrap-synchronization.ded`, `MR-3858-hadoop.ded`: each should produce a CE in Molly.
  ZK-1270 should be repaired by `success(L) :- sent_flag(L), ack(F).`, and MR-2995 should produce a
  differential-provenance pointer to the missing completion message.

### 12.6 Unit-level tests to port

* **`grossEstimate`:** (EOT3, EFF0, C0, [a,b,c]) = 1; (3,0,2,[a,b]) = 16; (3,0,1,[a]) = 4;
  (3,0,2,[a,b,c,d]) = 96; (3,2,0,[a,b]) = 16; (3,1,0,[a,b]) = 4; (3,2,2,[a,b]) = 121;
  (6,4,1,[a,b,c,C,Z]) ≠ 0 (no overflow; needs bignums). — `FailureSpecSuite`
* **Solver normalization:** Fspec(EOT4, EFF2, C1, [A,B]) with solution {C(A,2), O(A,B,1), O(A,B,2),
  O(A,B,3), O(B,A,1)} → crashes {C(A,2)}, omissions {O(A,B,1)}. The empty solution → none. —
  `SolverSuite`
* **Rewrites:** the deductive, inductive, and async rules become exactly the forms in the §3.4 table.
  — `DedalusRewritesSuite`
* **Provenance** (failure-free, EOT=2, one node "loc"):
  * `a(loc,x,1)@1; a(loc,y,1)@1; b(loc,x,1)@1; b(loc,y,1)@1; c(L,V) :- b(L,M,V), a(L,M,V);` →
    `c(loc,1)` has **2** rule firings.
  * With `proxy("loc",A,B) :- a(L,A,B); c(L,V) :- b(L,V), proxy(L,_,V);` → **2** distinct
    derivations.
  * `counts(L,A,count<B>) :- derived(L,A,B)` over facts (A:100, 200, 300; B:400) → `counts(loc,A,3)`
    has 3 contributors and `counts(loc,B,1)` has 1.
  * Grouping regression: `vote_cnt(M, count<I>) :- vote(M,V), member(M,V,I)` →
    `vote_cnt(M,2)` with 2 contributors.

  — `ProvenanceSuite`
* **Derivation trees:** a goal with two rules, each over two alternative leaves, enumerates **4**
  distinct derivations. The important clocks of `log(A,data,2)` derived by persistence plus a send
  from B are `{(B,A,1)}`. — `DerivationTreesSuite`
* **Symmetry:**
  * Empty program over {A,B,C}: crash(A,2) ≡ crash(B,2) ≡ crash(C,2), and O(A,B,2) ≡ O(B,A,2), but
    O(B,A,2) ≢ O(B,A,1).
  * A literal "A" in a rule body removes A from the symmetry candidates.
  * With EDB `foo(A,1), foo(B,1), foo(C,2)`: A ≡ B, and B ≢ C.

  — `SymmetryCheckerSuite`
* **Netflix toy lineage** (SoCC'16 Figure 1): supports {RepA,Bcast1}, {RepA,Bcast2}, {RepB,Bcast1},
  {RepB,Bcast2} → minimal falsifiers are exactly **{RepA,RepB}** and **{Bcast1,Bcast2}**.
* **Paper §4.3 formula:** `(O(a,c,2) ∨ C(a,2) ∨ C(a,1)) ∧ (O(b,c,1) ∨ C(b,1))`. With maxCrashes=1 the
  minimal falsifiers include {O(a,c,2), O(b,c,1)}, {C(a,1 or 2), O(b,c,1)}, and {O(a,c,2), C(b,1)},
  and **exclude** {C(a,·), C(b,1)}, which needs two crashes. (Constructed from the paper's formula; the
  exact expected set depends on EFF ≥ 3.)
* **Semiring example** (PODS'07, my Datalog transcription of the RA query
  q(R) = π_ac(π_ab R ⋈ π_bc R ∪ π_ac R ⋈ π_bc R)): EDB `r(a,b,c)[p]`, `r(d,b,e)[r]`, `r(f,g,e)[s]`;
  rules `q(X,Z) :- r(X,Y,_), r(_,Y,Z).` and `q(X,Z) :- r(X,_,Z), r(_,_,Z).` Expected N[X]:
  q(a,c)=2p², q(a,e)=pr, q(d,c)=pr, **q(d,e)=2r²+rs**, **q(f,e)=2s²+rs**. Under bags (p=2, r=5, s=1)
  the values are 8, 10, 10, 55, 7 (paper Figure 3). Lin(X) gives q(d,e)=q(f,e)={r,s}, which is the
  paper's point that lineage cannot tell them apart. PosBool: q(d,e)=r, q(f,e)=s.
* **Soufflé height example:** transitive closure `reach(a,N) :- reach(a,M), edge(M,N)` on the
  paper's Figure 11 graph should require Θ(k²) annotation updates, and the final heights must equal
  the minimal proof heights. Point-to example: `vpt(b,l1)` has height 7 in iteration 2 and is updated
  to 3 in iteration 3.

### 12.7 Additional protocols to model (from the literature; no ready-made expected results)

* Elastic primary/backup with failover and concurrent writes (Growing a Protocol): expect a CE that
  requires a primary crash after partial replication.
* Molly's `raft/raft.ded` (+ `election.ded`, `raft_assert.ded`, `raft_edb.ded`): the spec flags
  "disagree" (different entries at the same index on non-crashed nodes) and "two leaders" in the same
  term. No expected result is published. Use it as a smoke test and as a bridge to our full Raft.
* Molly's `chain_replication.ded`, `pipeline.ded` (HDFS pipeline, after Gunawi et al.),
  `real_heartbeat.ded`, `chord.ded`, `ramp/*.ded`, `gstore/*.ded`: unpublished expectations.

---

## 13. Open questions and ambiguities (decisions for implementers)

These are judgment calls. I have stated a default for each.

* **EFF boundary** (`< EFF` vs `≤ EFF`). Default: strict, matching Molly's code, so the published
  test verdicts reproduce.
* **Crash semantics** (paper: the node stops everything; code: the node only stops sending). The two
  are externally equivalent. Default: the paper's semantics, since it is cleaner and matches real
  fail-stop.
* **Which spec relations to check when `pre`/`post` are missing.** The paper says to fall back to all
  persistent relations; the code does not. Default: require `pre`/`post` and emit a clear error
  otherwise.
* **The Paxos and bully-le files for Figure 13 are not fully identified.** `paxos_synod.ded` exists;
  for the bully election, `util/leader.ded` is the best candidate but unconfirmed.
