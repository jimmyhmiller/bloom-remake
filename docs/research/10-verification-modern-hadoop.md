# 10 — Verifying Dedalus/Bloom programs; candidates for a modern Hadoop successor

Research cluster 10 for **bloom-remake**. Part A covers verification: how to prove or bounded-check properties of Dedalus/Bloom/Hydro-style programs, and what verification subsystem we should build. Part B covers the "modern Hadoop": what replaced MapReduce/HDFS, how well each successor fits a Dedalus + lattice language, three candidate designs, and a recommendation.

The report is written for implementers. Where a paper was read, section numbers and verbatim code are cited. Where something was **not** accessed, the report says so (see §0.2).

---

## 0. Scope and sources

### 0.1 Primary sources actually read (full text, via PDF extraction or source code)

| Tag | Source | URL |
|---|---|---|
| DEDALUS | Alvaro et al., *Dedalus: Datalog in Time and Space*, EECS-2009-173 (TR version of Datalog 2.0 2010 paper) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2009/EECS-2009-173.pdf |
| STABLE | Ameloot, Van den Bussche, Marczak, Alvaro, Hellerstein, *Putting Logic-Based Distributed Systems on Stable Grounds*, TPLP 2015 | https://arxiv.org/pdf/1507.05539 |
| LDFI | Alvaro, Rosen, Hellerstein, *Lineage-driven Fault Injection*, SIGMOD 2015 | https://people.ucsc.edu/~palvaro/molly.pdf |
| MOLLY-SRC | Molly source + examples (Scala), `github.com/palvaro/molly` (cloned; files cited by path) | https://github.com/palvaro/molly |
| SOCC16 | Alvaro et al., *Automating Failure Testing Research at Internet Scale*, SoCC 2016 | https://people.ucsc.edu/~palvaro/socc16.pdf |
| NEMO | Oldenburg, Zhu, Ramasubramanian, Alvaro, *Fixed It For You: Protocol Repair Using Lineage Graphs*, CIDR 2019 | https://people.ucsc.edu/~palvaro/p122-oldenburg-cidr19.pdf |
| GROWING | Ramasubramanian et al., *Growing a Protocol*, HotCloud 2017 | https://www.usenix.org/system/files/conference/hotcloud17/hotcloud17-paper-ramasubramanian.pdf |
| BLOOMUNIT | Alvaro, Hutchinson, Conway, Marczak, Hellerstein, *BloomUnit: Declarative Testing for Distributed Programs*, DBTest 2012 | https://dsf.berkeley.edu/papers/dbtest12-bloom.pdf |
| CALM-CIDR | Alvaro et al., *Consistency Analysis in Bloom: a CALM and Collected Approach*, CIDR 2011 | https://dsf.berkeley.edu/papers/cidr11-bloom.pdf |
| CALM | Hellerstein, Alvaro, *Keeping CALM: When Distributed Consistency is Easy*, CACM 2020 | https://arxiv.org/pdf/1901.01930 |
| BLAZES | Alvaro, Conway, Hellerstein, Maier, *Blazes: Coordination Analysis for Distributed Programs*, ICDE 2014 | https://arxiv.org/pdf/1309.3324 |
| BLOOML | Conway et al., *Logic and Lattices for Distributed Programming*, SoCC 2012 | https://dsf.berkeley.edu/papers/socc12-blooml.pdf |
| TRANSDUCERS | Ameloot, Neven, Van den Bussche, *Relational Transducers for Declarative Networking* (JACM 2013; arXiv) | https://arxiv.org/pdf/1012.2858 |
| FAIRNESS | Ameloot, *Deciding Correctness with Fairness for Simple Transducer Networks*, ICDT 2014 | https://openproceedings.org/2014/conf/icdt/Ameloot14.pdf |
| FVN | Wang, Jia, Liu, Loo, Sokolsky, Basu, *Formally Verifiable Networking*, HotNets 2009 | https://conferences.sigcomm.org/hotnets/2009/papers/hotnets2009-final6.pdf |
| DRIVER | Wang, Loo, Liu, Sokolsky, Basu, *A Theorem Proving Approach Towards Declarative Networking*, TPHOLs 2009 | https://netdb.cis.upenn.edu/papers/formaldn_tphol09.pdf |
| DNV-SLIDES | Wang, Basu, Loo, Sokolsky, *Declarative Network Verification* (PADL 2009) — slides only | https://netdb.cis.upenn.edu/research/talks/padl09_dnv.pdf |
| NDLOG-SEM | Nigam, Jia, Wang, Loo, Scedrov, *An Operational Semantics for Network Datalog* | https://netdb.cis.upenn.edu/fvn/ndlogsemantics.pdf |
| EPR | Padon, Losa, Sagiv, Shoham, *Paxos Made EPR*, OOPSLA 2017 | https://www.cs.tau.ac.il/~sharonshoham/papers/oopsla17.pdf |
| IVY | Padon, McMillan, Panda, Sagiv, Shoham, *Ivy: Safety Verification by Interactive Generalization*, PLDI 2016 | https://www.cs.tau.ac.il/~sharonshoham/papers/pldi16.pdf |
| DUOAI | Yao, Tao, Gu, Nieh, *DuoAI*, OSDI 2022 | https://www.usenix.org/system/files/osdi22-yao.pdf |
| DISTAI | Yao et al., *DistAI*, OSDI 2021 | https://www.cs.columbia.edu/~suman/docs/distai.pdf |
| KONDO | Zhang, Hance, Kapritsos, Chajed, Parno, *Inductive Invariants That Spark Joy*, OSDI 2024 | https://www.usenix.org/system/files/osdi24-zhang-nuda.pdf |
| ENDIVE | Schultz, Dardik, Tripakis, *Plain and Simple Inductive Invariant Inference for Distributed Protocols in TLA+*, FMCAD 2022 | https://arxiv.org/pdf/2205.06360 |
| KATARA | Laddad et al., *Katara: Synthesizing CRDTs with Verified Lifting*, OOPSLA 2022 | https://arxiv.org/pdf/2205.12425 |
| FLO | Laddad, Cheung, Hellerstein, Milano, *Flo*, POPL 2025 | https://arxiv.org/pdf/2411.08274 |
| LADDAD-PHD | Laddad, *Programming Models for Correct and Modular Distributed Systems*, EECS-2025-85 | http://www2.eecs.berkeley.edu/Pubs/TechRpts/2025/EECS-2025-85.pdf |
| REWRITES | Chu et al., *Optimizing Distributed Protocols with Query Rewrites*, SIGMOD 2024 | https://arxiv.org/pdf/2404.01593 |
| BIGGER | Chu et al., *Bigger, not Badder: Safely Scaling BFT Protocols*, PaPoC 2024 | https://hydro.run/papers/david-papoc-2024.pdf |
| FREETERM | Power, Koutris, Hellerstein, *The Free Termination Property of Queries Over Time*, ICDT 2025 | https://arxiv.org/pdf/2502.00222 |
| KEEPCALM | Laddad et al., *Keep CALM and CRDT On*, VLDB 2023 | https://arxiv.org/pdf/2210.12605 |
| NEWDIR | Cheung, Crooks, Hellerstein, Milano, *New Directions in Cloud Programming*, CIDR 2021 | https://arxiv.org/pdf/2101.01159 |
| BOOM-A | Alvaro et al., *BOOM Analytics*, EuroSys 2010 | https://dsf.berkeley.edu/papers/eurosys10-boom.pdf |
| RDD | Zaharia et al., *Resilient Distributed Datasets*, NSDI 2012 | https://www.usenix.org/system/files/conference/nsdi12/nsdi12-final138.pdf |
| DATAFLOW | Akidau et al., *The Dataflow Model*, VLDB 2015 | https://www.vldb.org/pvldb/vol8/p1792-Akidau.pdf |
| ABS | Carbone et al., *Lightweight Asynchronous Snapshots for Distributed Dataflows*, 2015 | https://arxiv.org/pdf/1506.08603 |
| FLINK-STATE | Carbone et al., *State Management in Apache Flink*, VLDB 2017 | https://www.vldb.org/pvldb/vol10/p1718-carbone.pdf |
| NAIAD | Murray et al., *Naiad: A Timely Dataflow System*, SOSP 2013 | https://sigops.org/s/conferences/sosp/2013/papers/p439-murray.pdf |
| DIFFDF | McSherry et al., *Differential Dataflow*, CIDR 2013 | https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf |
| DBSP | Budiu et al., *DBSP: Automatic Incremental View Maintenance for Rich Query Languages*, VLDB 2023 | https://arxiv.org/pdf/2203.16684 |
| ANNA | Wu, Faleiro, Lin, Hellerstein, *Anna: A KVS for Any Scale*, ICDE 2018 | https://dsf.berkeley.edu/jmh/papers/anna_ieee18.pdf |
| CLOUDBURST | Sreekanti et al., *Cloudburst: Stateful Functions-as-a-Service*, VLDB 2020 | https://arxiv.org/pdf/2001.04592 |
| DELTA | Armbrust et al., *Delta Lake*, VLDB 2020 | https://www.vldb.org/pvldb/vol13/p3411-armbrust.pdf |
| LAKEHOUSE | Armbrust et al., *Lakehouse*, CIDR 2021 | https://www.cidrdb.org/cidr2021/papers/cidr2021_paper17.pdf |
| RAY | Moritz et al., *Ray*, OSDI 2018 | https://www.usenix.org/system/files/osdi18-moritz.pdf |
| OWNERSHIP | Wang et al., *Ownership: A Distributed Futures System for Fine-Grained Tasks*, NSDI 2021 | https://www.usenix.org/system/files/nsdi21-wang.pdf |

### 0.2 Sources only partially accessed or not accessed (be careful)

- **DNV (PADL 2009) full paper**: the Springer and UPenn repository full texts were not retrievable. DNV content below comes from the DNV slides, the FVN (HotNets 2009) paper and the DRIVER (TPHOLs 2009) paper, which summarize DNV's translation.
- **Ameloot & Van den Bussche, ICDT 2012 / ToCS 2015 ("Deciding confluence ...")**: the UHasselt server was down. The decidability result is taken from the ICDT 2014 FAIRNESS paper, which restates it: diffluence is NEXPTIME-complete for *simple transducer networks*.
- **"Towards verifiable declarative networks"**: no paper with this title was found. The closest works are FVN/DNV/DRIVER and NDLOG-SEM.
- **Mechanized Dedalus/Bloom in Coq/Lean/Isabelle**: none found. Plain Datalog has been mechanized: Benzaken, Contejean, Dumbrava, *Certifying Standard and Stratified Datalog Inference Engines in SSReflect*, ITP 2017 (https://hal.science/hal-01745566), and Shahin, *A Shallow Embedding of Datalog in Lean*, SLE 2026 preprint (https://arxiv.org/abs/2605.02113). Neither handles time or distribution. **Status: an open gap.**
- **I4 (SOSP 2019), SWISS (NSDI 2021), FOL-IC3 (PLDI 2020), IC3PO, UPDR**: characterized from DuoAI's related-work section and head-to-head table, not from the original papers.
- **TLA+/Apalache**: from the Apalache README/paper abstract (https://github.com/apalache-mc/apalache; OOPSLA 2019 *TLA+ Model Checking Made Symbolic*, https://dl.acm.org/doi/pdf/10.1145/3360549). **Alloy 6/Electrum**: from search summaries of the Electrum Analyzer (ASE 2018) papers.
- **Apache Iceberg, Materialize, Dask, Velox, DuckDB internals**: general knowledge, not re-fetched. Iceberg is mentioned only at the level of "snapshot/manifest metadata committed by atomic pointer swap".
- **Verdi (PLDI 2015), IronFleet (SOSP 2015), FoundationDB simulation (SIGMOD 2021), Elle (VLDB 2021), Porcupine**: cited from general knowledge as related tooling. None was re-read.

---

# PART A — Verifying Dedalus / Bloom / Hydro-style programs

## A1. What "correct" means for a Dedalus program: the property classes

A verification subsystem has to support several property classes. Each maps to different machinery:

1. **Local well-formedness** (always checkable, decidable, syntactic):
   - *Temporal stratifiability*: the **deductive reduction** of the program (only the rules without `@next`/`@async`) must be syntactically stratified (DEDALUS Def. 2–3). Lemma 2: then there is a unique minimal model, because the program is modularly stratified over time. Example 4 in DEDALUS is the canonical program that is temporally stratifiable but not syntactically stratifiable (`p`/`p_neg` with `persist`).
   - *Instantaneous safety*: deductive rules are function-free and range-restricted (DEDALUS Def. 4).
   - *Temporal safety*: "henceforth quiescent after some time T" (Def. 7). **Quiescence at T** means the atoms at T are **equivalent modulo time** to the atoms at T−1 (Defs. 5–6). The conservative test (Lemma 3) assumes a finite EDB and requires every rule to be one of: (1) instantaneously safe; (2) an inductive rule whose head predicate also occurs in the body with the same bindings except time (a persistence rule); (3) an inductive rule with at least one positive *instantaneous predicate* (Def. 8) in its body.
2. **Safety invariants**: "no reachable global state satisfies bad(...)". Examples: agreement in consensus, "at most one leader per term" (Raft), uniqueness of a proposal per round.
3. **Outcome properties with vacuity** (Molly's `pre → post`): evaluated on the *final* state of a bounded run. The execution is vacuously correct if `pre` does not hold (LDFI §2.3). This is the right shape for fault tolerance, because under enough failures nothing happens at all. NEMO §3.2 restates it: the antecedent A holds when the run is not vacuous, and the consequent C holds when it upheld the property.
4. **Determinism / confluence / eventual consistency**: all runs on the same input produce the same output. CALM defines this as confluence. Ameloot distinguishes *confluence* (any two finite traces can be extended to agree) from *consistency* (all infinite **fair** traces give the same output). Consistency is strictly stronger. The counterexample is a "message join" program that needs simultaneous delivery of two messages (FAIRNESS §1).
5. **Liveness / termination**: e.g. 2PC termination, Paxos progress. LDFI checks bounded versions ("by EOT, after failures stop at EFF"). Unbounded liveness needs liveness-to-safety reductions (Padon et al., POPL 2018 — not re-read) or ranking arguments. **Liveness is out of scope for v1 except in its bounded form.**
6. **Refinement / rewrite correctness**: an optimized (decoupled, partitioned) program is correct if every run of P′ produces "the same output facts with the same timestamps as some run of P", i.e. history equivalence in the style of linearizability (REWRITES §2 "Correctness").

### A1.1 Semantic foundations the verifier must implement

Three semantics are in play. Our verifier needs all three, and it must be explicit about which one each tool uses:

- **(S-async) Asynchronous interleaving semantics.** This is the ground truth. It is the transducer-network model of TRANSDUCERS/CALM: each node repeatedly (1) ingests an unordered batch of messages, (2) evaluates its queries, (3) sends. The declarative equivalent is STABLE's stable-model semantics. In STABLE, each `@async` rule `R(ū)|y ← B` becomes a **dynamic choice of arrival timestamp** (STABLE §4.3, rules (3)–(6), after Saccà & Zaniolo):
  ```
  cand_R(x,s,y,t,ū)  ← B⇑x,s, all(y), time(t), ¬before(y,t,x,s)     (9)  [causality version]
  chosen_R(x,s,y,t,w̄) ← cand_R(x,s,y,t,w̄), ¬other_R(x,s,y,t,w̄)       (4)
  other_R(x,s,y,t,w̄)  ← cand_R(x,s,y,t,w̄), chosen_R(x,s,y,t′,w̄), t≠t′  (5)
  R(y,t,w̄)            ← chosen_R(x,s,y,t,w̄)                           (6)
  before(x,s,x,t)     ← all(x), tsucc(s,t)                              (7)
  before(x,s,y,t)     ← before(x,s,z,u), before(z,u,y,t)                (8)
  before(x,s,y,t)     ← chosen_R(x,s,y,t,w̄)                             (10)
  ```
  The plain choice transformation (§4.3) admits non-causal models, where messages arrive before they are sent in the happens-before sense. The causality transformation (§4.4) excludes them. The *causality-finiteness* transformation (§4.5) additionally forbids a node from receiving infinitely many messages at one step. STABLE proves that these stable models match the operational semantics. **This gives us a direct encoding of bounded Dedalus executions into ASP (answer set programming).** See A3.5.
- **(S-sync) Synchronous (pseudo-synchronous) semantics** for LDFI. Delivered messages arrive at `SndTime+1` in a deterministic order. Only omissions and crashes are nondeterministic (LDFI §2.1, following Interlandi et al.'s synchronous Dedalus semantics). This semantics is **incomplete for ordering bugs** and for consensus termination, which needs to distinguish delay from failure (LDFI §7). It is also dramatically cheaper to explore.
- **(S-FO) First-order transition-system semantics** for unbounded proofs. The state is a first-order structure (relations). Each action is one node tick. The network is a set of message tuples (EPR §2.1, §5). Treating messages as a grow-only set is "consistent with the messaging model we assume, in which messages may be lost, duplicated, and reordered" (EPR §5.1).

---

## A2. Survey of prior verification work, with implementation-relevant details

### A2.1 Declarative networking: FVN / DNV / DRIVER (UPenn, 2009)

- **What it does.** It translates NDlog programs to **PVS** axioms (DNV, PADL'09; FVN §3.1; DRIVER §3). The key idea is proof-theoretic semantics: "the set of NDlog rules defining a predicate is equivalent to an inductively defined data type in PVS" (FVN §3.1). Each IDB predicate becomes an `INDUCTIVE bool` definition. Rules r1 and r2 of the path-vector protocol become (verbatim, FVN):
  ```
  path(S,D,(P: Path),C): INDUCTIVE bool =
    (link(S,D,C) AND P=f_init(S,D)) OR
    (EXISTS (C1,C2:Metric) (P2:Path) (Z:Node):
       link(S,Z,C1) AND path(Z,D,P2,C2) AND C=C1+C2
       AND P=f_concatPath(S,P2) AND f_inPath(S,P2)=FALSE)
  ```
  Properties are written as PVS theorems, for example route optimality:
  ```
  bestPathStrong: THEOREM FORALL (S,D:Node) (C:Metric) (P:Path): bestPath(S,D,P,C) =>
     NOT (EXISTS (C2:Metric) (P2:Path): path(S,D,P2,C2) AND C2<C)
  ```
  The proof took 7 interactive steps and "a fraction of a second" of PVS time.
- **Soft state.** DRIVER adds creation-time `Tc` and lifetime `Tl` attributes to component interfaces to model soft-state expiry.
- **Reverse direction.** Component-based models verified in PVS can be compiled to NDlog (DRIVER §4). Each component becomes a rule, and location specifiers are supplied by the designer.
- **Limitations** (these motivate our design):
  1. Proofs are *interactive*, a human in PVS.
  2. The model is a centralized least-fixpoint view with no asynchrony, message loss or crash semantics, so it proves properties of the *converged* result rather than of executions.
  3. Aggregates and lists need hand-written theories.
  - **Lesson:** the *mapping* from rules to inductive definitions is the right one for any theorem-prover export (Lean/Coq). It says nothing about distributed executions.
- **NDLOG-SEM** (Nigam, Jia, Wang, Loo, Scedrov) gave the *operational* semantics of NDlog's pipelined semi-naïve evaluation (PSN) and found real engine bugs: PSN "can yield unsound results; it can diverge; and it can compute the same derivation multiple times" when inserts and deletes interleave. The authors propose PSNν and prove it correct for non-recursive programs, using linear logic with subexponentials. **Lesson for our engine and verifier:** incremental evaluation with deletions across nodes is subtle. Our runtime's incremental algorithm (counting, DRed, or DBSP-style Z-sets) must come with a proof or a differential test against naive re-evaluation. The verifier should also use a *reference* semantics, not the optimized engine, when it decides whether a counterexample is real.

### A2.2 BloomUnit (DBTest 2012): declarative tests plus CALM-guided schedule pruning

- **Test specs are Bloom programs.** They use no temporal operators (`<~ <+ <-`) and have a single output interface `fail`. Deriving a tuple into `fail` is a violation. Verbatim FIFO spec (BLOOMUNIT Fig. 4):
  ```ruby
  module FIFOSpec
    bloom do
      fail <= (pipe_out_log * pipe_out_log).pairs do |p1, p2|
        if p1.src == p2.src and p1.dst == p2.dst and
           p1.ident < p2.ident and p1.time >= p2.time
          ["out-of-order delivery: #{p1.inspect} < #{p2.inspect}"]
        end
      end
    end
  end
  ```
- **Automatic `_log` tables.** BloomUnit creates a `<name>_log` table for every collection, holding every tuple ever inserted plus the node-local timestep. Specs are therefore "one-shot" queries over the whole trace. **We must implement this. It is the cheapest powerful spec mechanism: a trace-as-a-database.**
- **Input generation.** Input constraints are written in **Alloy**. Alloy enumerates small satisfying instances and breaks symmetry. In the case study this found a real bug, a checkout racing with client actions.
- **Schedule exploration with CALM pruning.** This is the most important algorithmic idea in the paper. "Assuming no message omissions, monotonic code will produce the same output for any given input, regardless of network nondeterminism. Therefore exploring a single delivery order is sufficient to test a monotonic program fragment. ... we need only explore all delivery orderings for messages that could concurrently be in-flight and destined for an agent at which a nonmonotonic operation will process the message." Reordering messages that land in a set at D does not change D's final state. Reordering the messages feeding B's `count` does. With losses the space is still intractable, so BloomUnit *samples* using a stochastic channel model in a single-site simulator. **We can do better:** combine this reduction with DPOR and with LDFI's lineage pruning (A3.4).

### A2.3 Molly / LDFI (SIGMOD 2015) in implementable detail

**Architecture (LDFI Fig. 1).** Molly alternates two steps:
- *Forward step*: a concrete synchronous evaluation of the program with a given failure set, producing outputs plus lineage.
- *Backward step*: "hazard analysis". It extracts the lineage of good outcomes, converts it to a CNF formula, and asks a SAT solver for failure sets that falsify every known derivation.

Molly repeats until it either witnesses a violation or exhausts the candidate failure sets.

**Failure specification (LDFI §2.1.1).** `Fspec = ⟨EOT, EFF, Crashes⟩`:
- **EOT**: bound on global logical time.
- **EFF**: time after which no message is lost ("failure quiescence"). EFF < EOT, so the protocol has time to recover. EFF = 0 means fail-stop only.
- **Crashes**: maximum number of crash-stop failures.

Byzantine and crash-*recovery* failures are excluded. A failure set is *admissible* if it has no omissions after EFF and no more than Crashes crashes. The **sweep**: set EFF=0 and raise EOT until non-trivial correct executions appear. Then raise EFF until a violation appears (then raise EOT) or until EFF = EOT−1 (then raise both). Continue until a wall-clock bound. Report the minimal parameters for a counterexample, or the maximal parameters explored.

**Input language** (MOLLY-SRC `README.md`; LDFI Fig. 2), verbatim:
```
log(Node, Pload) :- bcast(Node, Pload);
node(Node, Neighbor)@next :- node(Node, Neighbor);
log(Node, Pload)@next :- log(Node, Pload);
log(Node2, Pload)@async :- bcast(Node1, Pload),
                           node(Node1, Node2);
```
Syntax details:
- `notin` is negation, and all its variables must be bound positively.
- EDB facts carry `@t`, e.g. `bcast("a", "hello")@1;`.
- `include "file.ded";` is supported.
- Aggregates are written `count<X>`, `max<X>` in the head.
- Arithmetic appears in the head (`seed(A, S+C)@next`) and comparisons in the body (`Cnt2 > Cnt1 / 2`).
- The first attribute is the location.

**Correctness specs (LDFI Fig. 3)**, verbatim:
```
missing_log(A, Pl) :- log(X, Pl), node(X, A), notin log(A, Pl);
pre(X, Pl)  :- log(X, Pl), notin crash(_, X, _);
post(X, Pl) :- log(X, Pl), notin missing_log(_, Pl);
```
If `pre`/`post` are not defined, all persistent relations are treated as outcomes. The Molly repository's version of `deliv_assert.ded` differs slightly: `pre(X, Pl) :- log(X, Pl), notin bcast(X, Pl)@1, notin crash(X, X, _);`. It also shows that **body atoms can carry a time annotation (`@1`) in specs**.

**Program rewrites (LDFI §4.1).**
1. *Clock encoding.* Add a `clock(From, To, SndTime)` EDB. Molly's implementation (`FailureSpec.scala`) actually uses the 4-ary `clock(from,to,sendTime,deliveryTime)`, with `deliveryTime = NEVER = 99999` for lost messages.
   - Local transitions need `clock(n,n,t)` for all t < EOT.
   - A message loss a→b at t deletes `clock(a,b,t)`, and only if t ≤ EFF.
   - A crash of a at t deletes `clock(a,b,u)` for all b and all u ≥ t.
   - **Implementation detail from source:** Molly always adds the self-clock facts `clock(n,n,t,t+1)` for every node, even crashed ones. A crashed node's *state keeps persisting*; it just cannot send. Molly also emits `crash(node, crashedNode, crashTime, t)` facts so specs can exclude crashed nodes (`notin crash(X, X, _)`).
   - **We should adopt cleaner semantics:** after a crash no rules fire at that node, and its last state is frozen and visible to specs.
2. *Datalog rewrite.* Add `Time` as the last attribute. Rewrite by rule kind:
   - Deductive rules: all premises share Time.
   - Inductive rules: `head(..., SndTime+1) :- body(..., SndTime), clock(Node, Node, SndTime)`.
   - Async rules: `head(Node2, ..., SndTime+1) :- body(Node1, ..., SndTime), ..., clock(Node1, Node2, SndTime)`.
3. *Lineage rewrite* (after Köhler et al.). For each rule r, add a *firings* relation `r_prov` with the same body that records all premise bindings. Aggregate rules get **two** provenance rules, so that recording extra bindings does not change the grouping. Verbatim:
   ```
   r(X, count<Z>) :- a(X, Y), b(Y, Z)
   ↓
   r_bindings(X, Y, Z) :- a(X, Y), B(Y, Z)
   r_prov(X, count<Z>) :- r_bindings(X, _, Z)
   ```

**Proof tree extraction (LDFI §4.2).** Query the firings relations to build a bipartite *rule/goal* derivation graph. There is an edge from each goal to every firing that derived it, and from each firing to its premises. Each derivation graph yields a forest of proof trees, one per independent support.

**SAT encoding (LDFI §4.3 + `Z3Solver.scala`).** This is the precise algorithm to port:
- For a goal (outcome tuple): `φ(goal) = ∧_{rule firing r deriving goal} φ(r)`. Every derivation must be falsified.
- For a rule firing: `φ(r) = ∨_{subgoal g of r} φ(g)`. Falsifying any premise falsifies the firing.
- Leaf with its own clock (a message send from→to at time t): `φ = MessageLoss(from,to,t) [only if t < EFF] ∨ ∨_{t′ ∈ [firstSendTime(from), t]} Crash(from, t′)`. Crashes before a node's first send are equivalent to crashing at the first send, so they are pruned.
- Leaf without a clock (EDB etc.): `false`, which cannot be falsified.
- Per important node, exactly one of {`Crash(node,t)` for t ∈ [firstSend, EOT−1]} ∪ {`NeverCrashed(node)`} holds.
- Global: `Σ NeverCrashed ≥ N − maxCrashes`, as a pseudo-Boolean constraint.
- Enumerate **all models** with blocking clauses (`¬∧(model literals)`). Drop empty models. Keep only **subset-minimal** solutions (`SetUtils.minimalSets`). Convert back to failure specs, discarding omissions subsumed by crashes (`Solver.solutionToFailureSpec`).
- One SAT problem per goal tuple. The union of solutions is the set of candidate counterexamples.
- With `pre`/`post`, a candidate is reported for each fault set that falsifies a `post` record **unless it also falsifies the corresponding `pre` record** (vacuous).

**Main loop (`Verifier.scala`).**
1. Run failure-free, with `eff=0, crashes=0`.
2. Take the provenance of `post` and solve.
3. For each candidate not yet in `alreadyExplored`:
   - Run the program with that candidate's faults.
   - If the run is good, re-solve using the run's own provenance, **seeded** with the current faults, so that extra faults are added on top. Enqueue the new candidates.
   - If the run is bad, emit a counterexample.
- `isGood(model)`: either `post@EOT` equals the failure-free `post`, or every missing `post` tuple is absent from `pre` (vacuity).
- Options: `useSymmetry` uses a `SymmetryChecker`. It decides whether two failure scenarios are isomorphic under a permutation of **location-typed** constants that do not appear as literals in rules, and applies only to EDB-symmetric nodes. `negativeSupport` makes Molly "explore beneath negative subgoals when constructing provenance trees". `causalOnly` is also an option. Solvers: Z3 or SAT4J.

**Completeness (LDFI Appendix B, Algorithm 1).** The program is a stratified Datalog¬ program P plus an EDB E with clock facts. For goal g ∈ P(E):
1. Compute the rule/goal graph G and the clock formula φ.
2. For each model A of φ, let `D = {clock(f,t,l) | A ⊨ O_{f,t,l} ∨ (A ⊨ C_{f,l′} ∧ l′ < l)}`.
3. If g ∉ P(E \ D), yield D.

The guarantee holds for a given input, Fspec and EOT, under the assumption that programs are **internally deterministic**: "if some execution produces a proof tree of an outcome, any subsequent execution with the same faults will also" (LDFI §7).

**Results to reproduce as regression targets** (LDFI Figs. 12–13):

| Program | LOC | EOT | EFF | Crashes | Combinations | Molly exe | Molly wall (s) | Random exe (avg) |
|---|---|---|---|---|---|---|---|---|
| simple-deliv (bug) | 4 | 4 | 2 | 0 | 4.10×10³ | 2 | 0.12 | 4.08 |
| retry-deliv (bug) | 5 | 4 | 2 | 1 | 4.07×10⁴ | 3 | 0.12 | 75.24 |
| classic-deliv (bug) | 5 | 5 | 3 | 0 | 2.62×10⁵ | 5 | 0.24 | 116.16 |
| 2pc (bug: blocking) | 16 | 5 | 0 | 1 | 24 | 2 | 0.22 | 5.48 |
| 2pc-ctp (bug) | 25 | 8 | 0 | 1 | 36 | 3 | 1.01 | 8.56 |
| 3pc (bug: agreement under omissions) | 24 | 9 | 7 | 1 | 2.43×10²⁶ | 55 | 9.60 | 40.60 |
| Kafka ISR (bug: durability) | 18 | 6 | 4 | 1 | 1.85×10²⁵ | 38 | 3.74 | 1183.12 |
| redun-deliv (ok) | 7 | 11 | 10 | – | 8.07×10¹⁸ | 11 | – | – |
| ack-deliv (ok) | 5 | 8 | 7 | – | 3.08×10¹³ | 673 | – | – |
| paxos-synod (ok, agreement) | 33 | 7 | 6 | – | 4.81×10¹¹ | 173 | – | – |
| bully-le (ok) | 11 | 10 | 9 | – | 1.26×10¹⁷ | 2 | – | – |
| flux (ok) | 41 | 22 | 21 | – | 6.20×10⁷⁶ | 187 | – | – |

(For the bug-free rows, the column extraction from the PDF did not include the Crashes value.)

**Follow-ups.**
- **SOCC16** ran LDFI at Netflix. Call graphs from request tracing stood in for lineage. Implicit redundancy (caches, fallbacks) is *not* visible in call graphs and had to be inferred. Faults were injected through Netflix's FIT service.
- **GROWING** used LDFI to track correctness as a protocol evolves.
- **NEMO** adds a query language over provenance graphs from good and bad runs, with three strategies:
  - *Differential Consequent Provenance*: which parts of the good-run provenance of C are missing in the bad run.
  - *Skeleton* variants.
  - **Corrections Generation**: "make it harder to establish A". It rewrites the antecedent's dependencies to include the triggers of C: `Deps_A := reachableProv(leaves(prop_{A=true}(Prov_A))) ∪ leaves(prop_{C=true}(Prov_C))`.
  - Of 52 cataloged bugs, 24 were potentially repairable by corrections. This works only because the program and its spec are in the same language.
- **What LDFI does not do**: message reordering, crash-recovery, Byzantine faults, liveness, input generation, and non-deterministic protocols (anti-entropy, randomized consensus).

### A2.4 How LDFI relates to bounded model checking (our analysis)

- **LDFI is a bounded model checker for a restricted nondeterminism alphabet.** The only nondeterministic choices are the presence or absence of `clock` facts (omissions and crashes). Delivery order is fixed by the synchronous semantics, and inputs are fixed. The bound is ⟨EOT, EFF, Crashes⟩. Like BMC, it is sound (every reported counterexample is a real run) and complete *within the bound and the abstraction*.
- **Why lineage prunes so well.** For a **negation-free (monotone) program**, g ∈ P(E∖D) iff some derivation of g uses only facts in E∖D. Every derivation over a subset of the EDB is also a derivation over the full EDB, so the failure-free run's *complete* provenance of g, read as a positive Boolean formula over clock facts (the PosBool provenance semiring), is **exactly** the Boolean function "g survives failure set D".
  - For monotone programs, **one failure-free run plus one hitting-set enumeration finds every relevant failure set**, and no further backward steps are needed because failure runs cannot create new supports. Each candidate still needs one forward run to evaluate the spec, because `pre`/`post` usually contain negation (e.g. `notin missing_log`) evaluated on top of the monotone outcome relations. This is our observation, not a claim made in LDFI. It follows from the monotonicity of Datalog and the Appendix B formalization. Here "monotone" means the rules deriving the goal relations (e.g. `log`) from clock-guarded messages, not the spec rules.
  - The loop exists because of **negation and aggregation**. Retry and timeout logic (`notin ack`, `notin accepted`) creates *new* derivations only in failure runs, which is the ack-deliv case. So the backward step on one run under-approximates the supports, and forward re-execution discovers the new ones.
  - Practical consequence: we should (a) run CALM analysis first and use the single-shot SAT mode for monotone outcomes; (b) for non-monotone programs, track which negated subgoals were *read* (why-not provenance, Molly's `negativeSupport`) so that we know when re-execution can change the support.
- **LDFI is a dualization problem.** The supports of g form a monotone DNF over "message delivered" variables. The minimal falsifying failure sets are its *minimal transversals* (minimal hitting sets), subject to the Fspec cardinality and time constraints. Enumerating minimal models is exactly what Molly does with blocking clauses plus minimal-set filtering. An implementation can use a MaxSAT/MinUNSAT-style enumerator, or a direct hitting-set algorithm, instead of generic model enumeration. This matters because full model enumeration then post-filtering for minimality wastes solver time.
- **Symbolic alternative ("symbolic LDFI").** Encode the whole bounded execution with symbolic clock variables in one SAT/SMT problem, in the style of Apalache/Alloy. The outcome's truth becomes a circuit over the clock variables, and the solver searches for an admissible failure set with `pre ∧ ¬post`. This handles negation natively, because the circuit includes the negated subgoals. The cost is grounding the whole execution (nodes × EOT × tuples) instead of only the lineage that actually occurred. **Recommendation:** implement both and choose by program size and monotonicity.
- **Reordering.** To go beyond the synchronous abstraction, add delivery-delay variables with bounded delay `δ ∈ [1, Δ]`. This is exactly STABLE's choice of arrival timestamp, restricted to a window. Apply BloomUnit's CALM reduction: only messages consumed by non-monotone operators need delay variables. The rest can be delivered at +1 without loss of generality with respect to final outcomes. **Caveat:** this is only for outcomes at quiescence/EOT. Intermediate-state invariants can still observe order.

### A2.5 CALM and confluence: what can be proven statically

- **CALM theorem** (CALM Thm. 1): "A program has a consistent, coordination-free distributed implementation if and only if it is monotonic". Here monotone means S ⊆ T ⇒ P(S) ⊆ P(T) (CALM Def. 1).
  - The formal model (Ameloot et al.) is relational transducer networks with confluence as consistency.
  - "Coordination" is defined by requiring messages "under all possible partitionings", including co-locating all data on one machine (CALM §2.2).
  - Refinements: policy-aware and domain-guided monotonicity for weaker classes (Ameloot et al. PODS 2014 — cited from CALM, not read).
- **Decidability.** Monotonicity of Datalog¬ is undecidable (REWRITES §3.2, citing [40]). **Confluence is undecidable in general, but decidable (NEXPTIME-complete) for *simple transducer networks***. Consistency under fairness is also NEXPTIME-complete for them (FAIRNESS Thm. 4.2). "Simple" means (FAIRNESS §2):
  - each transducer is recursion-free and inflationary (no deletions from memory);
  - send rules are *message-positive* (no message atoms under negation) and *static*;
  - output and memory insertion rules are message-positive and *message-bounded*;
  - the network is globally recursion-free (no cycles in the positive message dependency graph).

  This class captures exactly the distributed queries expressible as unions of conjunctive queries with negation. **Implication:** a complete confluence decision procedure is theoretically available only for a toy fragment, and its complexity is prohibitive. We should rely on (a) conservative syntactic monotonicity analysis plus (b) bounded confluence *testing*.
- **Syntactic monotonicity analysis (Bloom, CALM-CIDR §4.4).** Build a dataflow graph with collections as nodes and rules as edges:
  - Mark `<+`/`<-` edges as "+/−" (they spend a timestep).
  - Mark `<~` edges as async (dashed).
  - Mark non-monotone edges (aggregation, negation, deletion `<-`) with a circle.
  - Collapse every SCC containing both a circle and a +/− edge into a **temporal cluster**.

  **Points of order** are all non-monotone edges plus all edges incident to a temporal cluster. A program can be made consistent by adding coordination at its points of order.
  - Simple sufficient condition for a component to be monotone (REWRITES §3.2): its inputs are persisted, and its rules contain no negation or aggregation.
  - Lattice extension (BLOOML): monotone functions on lattices, *morphisms* (distribute over join), and threshold tests (e.g. `lmax.gt_eq(k)` → `lbool`) keep programs monotone even with "aggregation-like" operations. Details are in the BloomL cluster report.
- **Blazes** (ICDE 2014) makes the analysis *seal-aware* and component-level:
  - **Component path annotations**: `CR` (confluent, stateless), `CW` (confluent, stateful), `OR_gate` (order-sensitive, stateless), `OW_gate` (order-sensitive, stateful). `gate` is the set of partition attributes; `OR*`/`OW*` means each record is its own partition.
  - **Stream annotations**: `Seal_key` means the stream is punctuated on `key`, with at least one punctuation per record's key value. `Rep` marks replicated streams.
  - **Derived labels**, by increasing severity: `Async` < `Run` (cross-run nondeterminism) < `Inst` (cross-instance) < `Diverge` (permanent replica divergence). The internal labels are `NDRead_gate` and `Taint`.
  - **Inference rules (Fig. 9)**:
    1. `{Async,Run}` + `OR_gate` → `NDRead_gate`
    2. `{Async,Run}` + `OW_gate` → `Taint`
    3. `Inst` + `CW` or `OW_gate` → `Taint`
    4. `Seal_key` + `OW_gate` with `¬compatible(gate,key)` → `Taint`
  - **Reconciliation (Fig. 10)**: if `Taint ∈ Labels`, add `Rep ? Diverge : Run`. If some `NDRead_gate` is not `protected`, add `Rep ? Inst : Run`. Here `protected(NDRead_gate) ≡ ∀l ∈ Labels: l = NDRead_gate ∨ ∃key (l = Seal_key ∧ compatible(gate,key))`, and `compatible(partition, seal) ≡ ∃attr ⊆ partition: injectivefd(seal, attr)` (an injective functional dependency preserves sealing). Return the highest-severity label.
  - Cycles are collapsed to their highest-severity member. Blazes then synthesizes coordination: **sealing** (cheap, local) where compatible, otherwise **ordering** (global).
  - **We should implement this analysis over our compiled dataflow.** Most of the annotations can be derived automatically from rule syntax (negation/aggregation give O*, persistence gives W). Seals should be first-class stream markers.
- **Free termination** (FREETERM, ICDT 2025) is the "completeness" dual of CALM: when can a node *unilaterally terminate* after producing all its results, without coordination? It uses algebraic (semiring/automata) models that bridge transducers and CRDTs. It is relevant to our batch-job completion detection (Part B) and to deciding when a verifier may stop a run early.
- **Hydro's type-level determinism** (LADDAD-PHD Ch. 4; FLO):
  - Streams carry an ordering marker (`TotalOrder`/`NoOrder`), a retry marker (`ExactlyOnce`/`AtLeastOnce`) and a boundedness marker (`Bounded`/`Unbounded`).
  - Operators require algebraic properties. For example, `fold` over `NoOrder` requires a commutative closure, and over `AtLeastOnce` an idempotent one.
  - Nondeterministic APIs are `unsafe` (later a `nondet!` token). The "safe core guarantees end-to-end (eventual) determinism".
  - The thesis lists **"Algebraic Property Verification"** as open future work: the closures' commutativity/idempotence was "left to the developer ... a major potential source of bugs" (LADDAD-PHD §Future Work).
  - **We should close that gap:** check lattice laws (associativity, commutativity, idempotence of `merge`; monotonicity of user functions; morphism-ness when claimed) with SMT when the function is in a decidable fragment, and otherwise with property-based testing plus shrinking.
  - FLO formalizes *streaming progress* and *eager execution* as the two properties that give deterministic, fresh outputs.
- **Katara** (OOPSLA 2022) is verified lifting. From a *sequential* data type plus an `opOrder` constraint that orders non-commutative operations, it synthesizes a CRDT state as a composition of semilattices, with query and merge functions. Correctness is encoded in SMT "in the style of a bisimulation", with synthesized inductive invariants. Katara first uses a *bounded* encoding (bounded operation logs) to prune candidates, then proves unbounded correctness. **Lesson:** the verifier should support a "refines sequential spec" check for user lattices, bounded first and then unbounded.

### A2.6 Inductive invariants for distributed protocols (Ivy/EPR and inference)

**Paxos Made EPR (OOPSLA 2017)** is the blueprint for unbounded proofs of our programs, because its models are *already relational*: state = sets of messages sent.

- **Verification condition.** INV is inductive iff INIT ∧ ¬INV, INV ∧ TR ∧ ¬INV′, and INV ∧ ¬P are all unsatisfiable. A satisfying (s, s′) for the second is a **CTI** (counterexample to induction) (EPR §2.1).
- **EPR** is the ∃*∀* relational fragment. It is decidable and has the finite model property: a satisfiable formula has a model of size bounded by the number of ∃-quantifiers plus constants.
- **Extended EPR**: build the **quantifier alternation graph** over sorts, with an edge sᵢ→s for each function symbol f: s₁..sₖ→s and for each ∃x:s under ∀x₁:s₁...∀xₖ:sₖ (after NNF). If the graph is **acyclic** ("stratified"), satisfiability stays decidable with finite models, because Skolemization yields finitely many ground terms (EPR §2.2).
- **Methodology** (EPR §3, Fig. 2):
  1. Model the protocol in uninterpreted many-sorted FOL. Axiomatize interpreted domains: a total order ≤ (reflexive, transitive, antisymmetric, total; EPR Fig. 1) instead of naturals, sorts plus membership relations instead of sets, and sort + `apply` instead of maps.
  2. For debugging, use **semi-bounded verification**: bound only the sorts on cycles of the QA graph.
  3. To reach EPR, add **derived relations** that name existential subformulas, with update code generated automatically for single-tuple inserts.
  4. **Rewrite** the code and invariant to use them. Rewrites are checked with an auxiliary inductive invariant.
- **Paxos model** (EPR Fig. 3), sorts `node, quorum, round, value`:
  - `axiom ∀q1,q2:quorum. ∃n:node. member(n,q1) ∧ member(n,q2)` (quorum intersection)
  - Relations: `start_round_msg(round)` (1a), `join_ack_msg(node,round,round,value)` (1b), `propose_msg(round,value)` (2a), `vote_msg(node,round,value)` (2b), `decision(node,round,value)`, and constant `⊥:round`.
  - Actions `start_round, join_round, propose(r,q), vote, learn(n,r,v,q)`. Guards are `assume` statements, and sending is a monotone insert. `max{(r′,v′) | φ}` is sugar for `(r=⊥ ∧ ∀r′,v′.¬φ) ∨ (r≠⊥ ∧ φ(r,v) ∧ ∀r′,v′. φ(r′,v′) → r′ ≤ r)` (eq. 3). This is alternation-free if φ is existential.
- **The invariant** (eqs. 4–15):
  - (4) agreement.
  - (5) unique proposal per round.
  - (6) vote ⇒ proposal.
  - (7) decision ⇒ ∃ quorum that all voted.
  - (8)–(11) join_ack faithfully reports the max vote, and no votes at ⊥.
  - (13) the key "choosable" invariant, rewritten with `left_round`: `∀r1,r2,v1,v2,q. propose_msg(r2,v2) ∧ r1<r2 ∧ v1≠v2 → ∃n. member(n,q) ∧ ¬vote_msg(n,r1,v1) ∧ left_round(n,r1)`.
  - (14), (15) are representation lemmas for the derived relations `left_round(n,r) ≡ ∃r′,r″,v. r′>r ∧ join_ack_msg(n,r′,r″,v)` and `joined_round(n,r) ≡ ∃r′,v. join_ack_msg(n,r,r′,v)`.

  The first rewrite attempt yields the CTI of EPR Fig. 6: one node, two rounds, a stale `joined_round`. It is fixed by rewriting `propose`'s max to range over `vote_msg` directly.
- **Evaluation.** Z3 checks all VCs in seconds with no timeouts. The original FO models diverged unbounded, and semi-bounded runs grew quickly with the number of rounds (EPR §9). Verified protocols: Paxos, Multi-Paxos, Vertical, Fast, Flexible and Stoppable Paxos.
- **Direct mapping to our language.** `start_round_msg`, `join_ack_msg`, `propose_msg` and `vote_msg` are **Dedalus async channel relations that are persisted**, and the derived relations `left_round`/`joined_round` are **projections/deductive views**. A Dedalus Paxos compiles to almost this model. The piece missing from Dedalus is a first-class **quorum** construct, because `count<> > n/2` is arithmetic and outside EPR.

**Ivy (PLDI 2016).** RML (relational modeling language) is Turing-complete, but updates are quantifier-free and there is no arithmetic, so every loop-free VC is in EPR.
- The user interactively generalizes CTIs, shown graphically, into universally quantified conjectures, assisted by "BMC + Auto Generalize". The first sanity step is BMC; for example, omitting "IDs are unique" in leader election produces an error trace (IVY Fig. 4).
- **We want this UX:** CTIs as small graphs of nodes and messages, drawn like Lamport diagrams.

**Invariant inference** (from DUOAI's evaluation and related work on 27 protocols):
- **I4**: model-check a small finite instance, generalize the invariant, and check it. ∀-only, with no completeness guarantee.
- **DistAI**: simulate the protocol, enumerate ∀-only candidates that hold on samples, then weaken monotonically with Ivy. Guaranteed to find an ∃-free invariant if one exists.
- **FOL-IC3**: first to handle ∃, but slow because of heavy SMT use.
- **SWISS**: template enumeration. Solves Paxos and Flexible Paxos in hours, and fails on more complex variants.
- **DuoAI**: simulate at several instance sizes, then build a **minimum implication graph** of formulas (stronger → weaker). Enumerate the strongest candidates that hold on samples, then run in parallel:
  - top-down refinement: check all candidates and weaken;
  - bottom-up refinement: find a ∀-only inductive core, then add small subsets of ∃-candidates.

  Quantifier alternation must follow a fixed order of sorts, which keeps VCs decidable. DuoAI solved 26/27 protocols, including Paxos, Multi-Paxos, Stoppable and Fast Paxos. For Multi-Paxos it had 615 candidates (581 with alternation), but only 2 were needed.
- **Endive** (FMCAD 2022) does inference for TLA+.
- **Our advantage:** our Datalog engine *is* the simulator, and candidate invariants are just Datalog denial queries evaluated over sampled states. Candidate filtering is batch query evaluation, which our engine should do very fast. **DuoAI-style inference is an excellent fit.**

**Kondo (OSDI 2024) invariant taxonomy.** *Regular invariants* are mechanically derivable:
- **Message invariants**:
  - *Send*: `∀m ∈ network: p(m, hosts[m.src])`, i.e. a message exists only if its sender took a step that sends it, with a relation to the sender state.
  - *Receive*: `∀h: q(hosts[h]) ⇒ ∃m ∈ network: h = m.dst ∧ r(hosts[h], m)`.
- **Monotonicity invariants**: `∀σ,σ′: lteq(σ,σ′)` for grow-only counters and sets, via history preservation.
- **Ownership invariants**: at most one owner per resource, and none while it is in transit.
- *Protocol invariants* relate hosts only and **can be discovered on the synchronous version P_sync** of the protocol, where send and receive are one atomic step. In all evaluated protocols, protocol invariants from P_sync plus automatically generated regular invariants sufficed.

**Why this matters to us.** In Dedalus, regular invariants fall straight out of syntax:
- Every `@async` rule gives a send invariant, from the rule body.
- Every persisted relation with no deletion, and every lattice-typed attribute (merge-only), gives a monotonicity invariant for free.
- Every deductive rule consuming a channel relation gives a receive invariant.
- Molly's synchronous semantics is essentially P_sync.

So our verifier can auto-generate the "boring half" of every inductive invariant. **CALM monotonicity makes invariants cheaper:** if a send-rule body reads only persisted monotone state, the send invariant can refer to the sender's *current* state ("the body still holds"), with no history variables.

### A2.7 Other verification and testing ecosystems (for positioning)

- **TLA+**:
  - TLC is explicit-state model checking.
  - **Apalache** translates TLA+ to quantifier-free SMT constraints (Z3) for **bounded model checking** and **inductive invariant checking for fixed/bounded parameters** (Konnov, Kukovec, Tran, OOPSLA 2019).
  - TLAPS is for proofs.
  - A Dedalus program maps naturally to TLA+: variables are one relation-valued function per relation (a set of tuples), and there is one action per (node, tick). **Export to TLA+ is cheap and gives users access to TLC/Apalache.**
- **Alloy 6 / Electrum**: mutable relations plus past/future LTL. SAT-based BMC via Kodkod (Pardinus), and complete model checking via NuSMV/nuXmv. The model is *relational*, like ours. BloomUnit used Alloy for input generation, and we should likewise use a relational model finder for generating test inputs (topologies, configs).
- **Verdi / IronFleet / Disel / Grove**: proof-assistant verification of implementations (Coq/Dafny), with months of expert effort. Verdi's *verified system transformers*, network semantics layered from idealized to faulty, are conceptually related to our "synchronous → asynchronous" semantics layering.
- **Deterministic simulation testing** (FoundationDB, TigerBeetle, Antithesis, Hydro's `sim`): Hydro's simulator enumerates decisions for batch releases and merge orders (hydro-project issue tracker; LADDAD-PHD notes DST only needs to inject nondeterminism into `unsafe` regions). **We get this almost for free, because all nondeterminism in Dedalus is in channel delivery, `choice`, clocks, and explicit nondeterministic operators.**
- **History checkers**: Elle (Kingsbury & Alvaro, VLDB 2021) and Porcupine/Knossos for linearizability. These are needed to test Raft/KV linearizability and transactional isolation in the flagship programs.

### A2.8 Runtime invariants and rule tracing in BOOM Analytics (EuroSys 2010, §6)

- **Watchdog rules (BOOM-A §6.1)**: "system invariants can easily be written declaratively and enforced by the runtime". JOL added a `die` relation: inserting a tuple into it triggers a Java listener that throws an exception, so "one writes an Overlog rule with an invariant check in the body, and the die relation in the head". The authors cite a similar `panic` relation (Gupta et al.) and used these local-node invariants heavily in Paxos: "Assertions that we specified early in the implementation of Paxos aided our confidence in its correctness as we added features and optimizations". **For us:** the same `violation` relations used by V2–V5 must also be enforceable in production, with a configurable action (abort node, alert, or log with provenance).
- **Tracing by metaprogramming (BOOM-A §6.2)**: for each rule, generate a tracing rule with the same body whose head records the rule name and timestamp. This is automated by a rewrite over the rules meta-tables (Evita Raced). A code coverage tool for rule firings was built "in less than a day" and immediately found dead rules. It is the same construction as Molly's firings relations. **For us:** one provenance/firing capture mechanism serves profiling, coverage, LDFI and debugging.

### A2.9 Hydro rewrite correctness (verification of optimizations)

REWRITES (SIGMOD 2024) gives **preconditions** for correct-by-construction rewrites of Dedalus components. The correctness notion: every run of P′ yields output facts with the same timestamps as some run of P. The fault model is an asynchronous network with up to f general-omission failures.

**Decoupling** (split a component C into C₁ and C₂ on different nodes):
- *Mutually independent* decoupling: neither component references the other's IDB outputs.
- *Monotonic* decoupling: C₁ is independent of C₂ and C₂ is monotonic. The rewrite persists C₂'s inputs.
- *Functional* decoupling: C₂ is functional, meaning no negation or aggregation and at most one IDB relation per rule body. It is therefore effectively stateless.

**Partitioning**:
- A *parallel disjoint correct* distribution policy.
- Co-hashing, loosened with functional dependencies and co-partition dependencies.
- *Partial partitioning* for "state machine" components.

The authors scaled 2PC 5× and Paxos 3× this way. **BIGGER** (PaPoC 2024) extends the rewrites to BFT protocols. It proves them correct with a *Borgesian simulator*, a gadget node that can emit any message a Byzantine node could, showing that the set of generatable messages is unchanged. **For us:** these rewrites are optimizer passes, and their preconditions are the static checks the verifier must run before applying them. Checking the rewritten program by differential testing, or bounded equivalence checking against the original, is a cheap extra safety net.

---

## A3. Recommendation: the verification subsystem for our language

We recommend a **layered, all-in-one toolchain** in which every layer consumes the *same* compiled IR (rules plus relation metadata) and the *same* property language. The layers trade cost for strength:

```
 V0 static analyses (always on, compile time)            -- decidable, syntactic
 V1 property/spec language (shared by all layers)
 V2 deterministic simulation + randomized & fuzz testing  -- cheap, unsound-for-absence
 V3 LDFI engine ("Molly-2")                                -- complete within Fspec, sync model
 V4 bounded async model checking (explicit+POR, symbolic)  -- complete within bounds, async model
 V5 unbounded proofs: Dedalus→FOL/EPR + auto-invariants + inference -- sound for all executions
 V6 exporters: TLA+ (TLC/Apalache), Ivy/mypyvy, Lean       -- escape hatches
```

### A3.1 V0 — static analyses (compile time)

1. **Stratification / temporal stratification check** (DEDALUS Defs. 2–3). Reject non-stratified deductive reductions with a cycle witness.
2. **Temporal safety check** (DEDALUS Lemma 3) → emit warning "may never quiesce". Also runtime **quiescence detection**: hash the node state modulo time and stop simulations and model checking at quiescence. Periodic timers make programs *ultimately periodic* rather than quiescent. For Datalog1S-style programs (Chomicki's temporal deductive databases), least models are eventually periodic with a period, so a state-repetition check modulo time generalizes quiescence detection.
3. **Monotonicity / points-of-order analysis** over the rule dataflow graph (CALM-CIDR §4.4). Lattice-aware: monotone functions, morphisms and threshold tests are monotone (BLOOML).
4. **Blazes seal analysis** over the compiled dataflow (A2.5), with automatic annotation: negation/aggregation over a stream → `O*`; persisted state → `W`. Seals declared on channels are carried as `Seal_key`. FD inference supports `compatible()`.
5. **Determinism certificate** per output relation, reporting one of:
   - (a) confluent (monotone path);
   - (b) confluent given seals S;
   - (c) order-sensitive at points of order X, coordinated by protocol Y (e.g. declared consensus);
   - (d) nondeterministic by design (uses explicit `choice`/`nondet`).
6. **Lattice law checks** for every user lattice and user function:
   - ACI of `merge`, the identity/bottom law;
   - monotonicity of functions declared `monotone`;
   - `f(a ⊔ b) = f(a) ⊔ f(b)` for functions declared `morphism`.

   Discharge via SMT when the function is in a supported fragment (linear integer arithmetic, bit-vectors, finite maps over them, sets). Otherwise use property-based testing with shrinking, and report it as "tested, not proven". Closes LADDAD-PHD's open problem.
7. **Rewrite preconditions** (REWRITES §3–4) for optimizer passes: independence, monotone, functional, co-hashing with FDs/CDs.

### A3.2 V1 — property / spec language (proposed; adapt to final surface syntax)

All specs are **ordinary rules in the language**, following BloomUnit/Molly/Nemo. They can therefore be run, provenance-tracked and repaired like program code.

```
// (1) Safety invariant = denial constraint: any derivable `violation` fact is a bug.
//     Checked at every global state (V4/V5) or every node tick (V2/V3).
violation("agreement", V1, V2) :- decision(_, _, V1), decision(_, _, V2), V1 != V2;

// (2) Outcome spec (Molly): evaluated on the final state (EOT or quiescence).
//     Execution is vacuous if `pre` is empty.
pre(X, Pl)  :- log(X, Pl), notin crash(X, X, _);
post(X, Pl) :- log(X, Pl), notin missing_log(_, Pl);

// (3) Trace queries: every relation R automatically has R_log(loc, ..., t) (BloomUnit),
//     plus hb(n1,t1,n2,t2) (happens-before over (node, tick)) derived from channel sends.
violation("fifo", S, D, I1, I2) :- deliver_log(D, S, I1, T1), deliver_log(D, S, I2, T2),
                                   I1 < I2, T1 >= T2;

// (4) Failure model for V3/V4 (Fspec) and harness config
failures { eot: 12, eff: 8, crashes: 1, omissions: true, crash_recovery: false }

// (5) Declarations used by V5 (EPR mode). Types that only use comparisons become ordered sorts.
type Ballot ordered;       // total-order axioms; only <, <=, =, max/min, "fresh greater than"
type Node   location;
type Value  opaque;
members Acceptors : Node;  // static membership => `majority<>` compiles to the quorum abstraction
promised(B) :- majority<N in Acceptors> p1b(N, B, _, _);   // runtime: count > |Acceptors|/2
                                                            // V5: ∃q:quorum ∀n. member(n,q) → p1b(n,B,..)
```

Semantics notes:
- Invariants are stratified Datalog queries over the state. A *non-recursive* stratified Datalog query is equivalent to a first-order formula (relational calculus), which is what V5 needs. Recursive invariant queries are allowed in V2–V4 but rejected in V5 unless the user supplies an axiomatization.
- ∃∀ invariants (e.g. EPR eq. 7, "some quorum all voted") are written with double negation (`unsupported`, `supported`) or with a `majority<>` sugar that V5 recognizes.
- **Bounded liveness**: `eventually(post) within k after EFF`. Future-time LTL beyond that is not supported in v1. Past-time temporal properties are expressible as ordinary rules using `@next` persistence (`once_R@next :- R; once_R@next :- once_R;`).

### A3.3 V2 — deterministic simulation and randomized testing

- A single-process simulator that runs *all* nodes, with every source of nondeterminism drawn from a seeded PRNG:
  - channel delivery delay and order;
  - omissions;
  - crashes and restarts;
  - timers (periodics);
  - `choice`/`nondet` operators.
- **Exact replay from a seed.** Shrink failing schedules to a minimal reproducer (delta debugging over the fault list).
- Only inject nondeterminism where V0 found order-sensitivity (Hydro's observation). Monotone paths are delivered in any fixed order.
- Outputs: Lamport/space-time diagrams, and lineage graphs of `violation`/`post` tuples, taken from the same provenance machinery as V3.
- History checking for flagship services: record `invoke/ok` histories, then check linearizability (Porcupine-style WGL search) or transactional anomalies (Elle-style cycle detection).

### A3.4 V3 — LDFI engine ("Molly-2")

Port the algorithm in A2.3 with these improvements:
1. **Engine mode**: synchronous simulation with a `clock(from,to,t)` fault mask, applied as an input filter at the network layer. No program rewriting is needed, because our runtime owns the channels. Crash semantics: no firings after the crash time; state is frozen and visible to specs; `crash(node, time)` is available to specs.
2. **Provenance**: record firings for *all* derivations (not just first), i.e. why-provenance / PosBool. Aggregates use the two-level rewrite. `notin` subgoals are recorded as negative dependencies (why-not provenance), so that V3 knows which absent facts a derivation depended on.
3. **Monotone fast path** (A2.4): if the outcome's derivations have no negation or aggregation on a path from a message, a single failure-free run plus one hitting-set enumeration yields every relevant failure set. No backward iteration is needed; each candidate still gets one forward run to evaluate the (possibly non-monotone) `pre`/`post`.
4. **Solver**: minimal hitting-set enumeration over supports under Fspec cardinality. Use CaDiCaL/Kissat-class SAT with an incremental interface, or Z3's PB constraints as in Molly. Enumerate *minimal* solutions directly (e.g. MARCO/MCS-style), not all models followed by filtering.
5. **Symmetry reduction**: permute location constants that are EDB-symmetric and do not appear literally in rules (Molly `SymmetryChecker`).
6. **Sweep** mode exactly as in LDFI §2.1.1, reporting minimal counterexample parameters or maximal certified parameters.
7. **Outputs**: minimal failure set, Lamport diagram, lineage of `pre`/`post`, and Nemo-style differential provenance between good and bad runs. Also Nemo "corrections" suggestions: strengthen antecedent dependencies.
8. **Extensions beyond Molly** (each flagged as experimental until validated):
   - crash-recovery: a crash deletes non-durable relations and a restart resumes persistence from durable ones. This needs a `durable` annotation on relations.
   - bounded reordering at points of order (A2.4).
   - symbolic whole-run encoding (A2.4).

### A3.5 V4 — bounded asynchronous model checking

Two back ends:

- **Explicit-state with reductions.** DFS over (global state, in-flight multiset).
  - Reductions: (i) CALM-POR: only messages whose consumer rules are non-monotone at the destination create branching orderings; others are delivered in canonical order (BloomUnit §5). (ii) DPOR on node ticks that are independent: different nodes, and no message between them in the window. (iii) Symmetry over location constants. (iv) State hashing modulo time (quiescence).
  - Bounds: depth, number of in-flight messages, and failure budget as in Fspec.
- **Symbolic (SAT/SMT/ASP).** Ground the program for k ticks per node and n nodes:
  - (a) **ASP route**: implement STABLE's transformation (A1.1) with a bounded time domain. Choice rules pick an arrival time in `(s, s+Δ]` or ⊥ (lost, if the omission budget allows). Causality comes from the `before` rules. The property is an integrity constraint `:- pre(X), not post(X).` at the horizon, or `:- violation(_).` at any time. Each stable model is a counterexample run. clingo has a C API usable from Rust.
  - (b) **SAT route**: Kodkod-style: one Boolean per (relation, tuple, node, time) and per (message, arrival time). Rules become Tseitin clauses with completion for least-model semantics. Stratified negation is handled stratum by stratum, and positive recursion within a tick needs *loop formulas* or bounded unrolling. **Prefer ASP for recursion-heavy programs, and SAT for mostly non-recursive per-tick logic.**
- The V4 bound certificate states the model (async vs sync), n, k, Δ and the failure budget.

### A3.6 V5 — unbounded proofs via translation to first-order logic / EPR

**Translation** (our design, following EPR §2.1/§5 and Kondo's network model):

- **Sorts**: from declared attribute types. `location` → `node`; `ordered` → a sort with total-order axioms; `opaque` → an uninterpreted sort. Integer arithmetic on non-ordered types is rejected in EPR mode. SMT/LIA mode is allowed but may be incomplete.
- **State vocabulary**:
  - one relation symbol per persisted relation, with the location as the first argument;
  - one per channel relation (the network), as a grow-only set by default. The optional `exactly_once` mode tracks `delivered`.
  - lattice attributes: max/min → ordered sort with a functional-dependency axiom; set lattice → relation; map-of-lattice → relation plus FD.
- **Actions**:
  - `tick(n)`: nondeterministically choose `D_m ⊆ {x̄ | m(n, x̄) ∈ net}` for each channel m. Model this as fresh relations with a universal subset axiom. **Arbitrary subsets, not single messages, are required for soundness when batches matter.** Evaluate node n's deductive strata as *definitions* (non-recursive strata unfold to FO). Then set persisted relations′ from the `@next` heads and `net′ = net ∪ async heads`.
  - `env_input(n, ...)` for external inputs and timers.
  - `crash(n)` (safety only).
- **Recursion inside a tick**: a positive recursive IDB used only positively in guards may be over-approximated by "any relation closed under the rules" (sound for safety). If used negatively, reject, or require a user axiomatization. **Polarity analysis is required**, and it must be *transitive*: if an over-approximated relation R feeds, even through persisted relations or later ticks, into anything used under `notin`, in an aggregate, or in the property itself, the over-approximation becomes unsound. Such uses must be rejected, or R must be modeled exactly (bounded unrolling in semi-bounded mode).
- **Aggregates**:
  - `max/min` over ordered types → eq. (3) of EPR;
  - `majority<>` → quorum sort plus intersection axiom;
  - `count` beyond majority, `sum`, etc. → SMT mode only, or rejected.
- **Checks**:
  1. Build the QA graph and report cycles with the formula fragments responsible, as EPR Fig. 4 does. Offer auto-derived relations: projections of channel relations onto their key columns (`joined_round`-style) and "exists-greater" views (`left_round`-style). Update rules for these are generated automatically; Dedalus views make this free.
  2. Discharge the three VCs (INIT ⇒ INV, INV ∧ TR ⇒ INV′, INV ⇒ P) with Z3/CVC5. Give a decidability guarantee when the QA graph is acyclic.
  3. Render CTIs as finite relational structures (IVY/EPR style), drawn as node/message graphs.
  4. **Semi-bounded mode**: bound only the sorts on QA cycles (EPR §3.1.3).
- **Automatic regular invariants** (Kondo, specialized to Dedalus):
  - For every `@async` rule `m(@y, ū)@async :- B(x, ...)`: *send invariant* `∀ m(y,ū) ∈ net. ∃ sender x: B_persisted(x, ...)`. It is valid when the persisted, negation-free part of B is monotone. Otherwise it needs history or is skipped.
  - For every persisted relation without deletion, and every lattice attribute: *monotonicity invariant*, which is trivially inductive and used as a lemma.
  - For every rule deriving persisted facts from a channel premise: *receive invariant* `∀ state fact ⇒ ∃ msg ∈ net`. This is ∀∃, so it is emitted with an auto-derived projection relation to keep EPR where possible.
- **Invariant inference** (DuoAI-style):
  1. Run V2/V4 on small instances (2–4 nodes, 2–3 values/rounds) and collect reachable states.
  2. Enumerate candidate invariants: ∀*∃* formulas over relation literals up to size k, with alternation order consistent with an acyclic sort order.
  3. Filter candidates by evaluating their denial-query form over the samples in batch with our engine.
  4. Run top-down and bottom-up refinement with the SMT checker.
  5. Present the result as readable denial rules.
- **Protocol invariants via the synchronous version** (Kondo): let users prove invariants on the sync semantics (V3's semantics) first, then lift them with the auto-generated regular invariants.

### A3.7 V6 — exporters

- **TLA+**: variables are relation-valued, with one action per (node, tick). This gives TLC and Apalache for free.
- **Ivy/mypyvy**: the V5 FOL model in its native syntax.
- **Lean 4**: each IDB relation becomes an inductive predicate (the FVN/DRIVER mapping), and a Dedalus run becomes a trace structure. This is for manual proofs.

**Status:** there is no existing mechanized Dedalus semantics (§0.2). Building one in Lean, with STABLE's stable-model semantics and the operational semantics proven equivalent, would be a research contribution. It is not a v1 requirement.

### A3.8 Worked mapping: how Paxos gets verified end to end

1. **V0**: `accepted`/`promised` use max-lattices, so they are monotone. The `max<>` over `p1b` in the proposer is non-monotone, but it runs over a *majority-sealed* set, so it is a point of order that consensus itself resolves. The certificate says decisions are order-insensitive only given the protocol's invariants, which V5 proves.
2. **V3**: `paxos_synod.ded`-style agreement spec `pre(M) :- important(_, M); post(M) :- important(_, M), notin disagree(M);`. Expect no counterexample at EOT 7 / EFF 6 (LDFI Fig. 13).
3. **V4**: async BMC with 3 acceptors, 2 proposers and Δ = 3, with no violation of `violation("agreement",...)`.
4. **V5**: auto-translation yields EPR Fig. 3 plus auto-derived `joined_round`/`left_round`. The inference engine should rediscover eqs. (5)–(15). The CTI of EPR Fig. 6 is a good regression test for the CTI renderer.

---

# PART B — A modern Hadoop that fits Dedalus + lattices

## B1. What BOOM did (baseline to beat)

**BOOM Analytics** (EuroSys 2010) rebuilt HDFS and the Hadoop JobTracker in Overlog/JOL:

- **BOOM-FS**:
  - NameNode metadata as relations (Table 1):
    - `file(fileid, parentfileid, name, isDir)`
    - `fqpath(path, fileid)`
    - `fchunk(chunkid, fileid)`
    - `datanode(nodeAddr, lastHeartbeatTime)`
    - `hb_chunk(nodeAddr, chunkid, length)`
  - Recursive `fqpath` derived by two rules (Fig. 3, verbatim excerpt):
    ```
    fqpath(Path, FileId) :- file(FileId, FParentId, _, true), FParentId = null, Path = "/";
    fqpath(Path, FileId) :- file(FileId, FParentId, FName, _), fqpath(ParentPath, FParentId),
                            PathSep = (ParentPath = "/" ? "" : "/"), Path = ParentPath + PathSep + FName;
    ```
  - Each fixpoint is an atomic durable transaction via Stasis, with per-table durability.
  - Heartbeat timeouts remove DataNodes.
  - Chunks move over a Java data path.
  - Size: HDFS ~21,700 lines of Java; BOOM-FS 1,431 Java + 469 Overlog, after 4 person-months (Table 2).
  - **High availability**: basic Paxos in 53 rules, "corresponding nearly line-for-line" to Lamport's description. Multi-Paxos plus the rest came to ~400 lines. NameNode actions became Paxos decrees, in two person-days.
  - Scale-out: metadata partitioning.
- **BOOM-MR**: the JobTracker scheduling state (job/task/taskAttempt relations, Table 3) in Overlog. It included the **LATE** speculative execution policy. Java still ran the data path.

**Why a straight remake is not enough.** In BOOM the Datalog layer was *control plane only*. The shape of MapReduce (disk-materialized shuffles, per-job barriers, a single master) has been superseded. Our language should run the control plane **and** the data plane, and should show off lattices and CALM.

## B2. Survey: what replaced MapReduce/HDFS, and fit with Dedalus + lattices

| System | Core idea (verified detail) | Fault tolerance | Coordination | Fit to Dedalus/lattices |
|---|---|---|---|---|
| **Spark RDD** (NSDI'12) | Immutable partitioned datasets with an interface of `partitions()`, `preferredLocations(p)`, `dependencies()`, `iterator(p, parentIters)`, `partitioner()` (Table 3). **Narrow** deps (each parent partition used by ≤1 child partition) pipeline; **wide** deps shuffle. | **Lineage recomputation** of lost partitions only. Checkpoint long lineages (§5.4). | Stage barriers at shuffles; centralized driver. | High: lineage = provenance; narrow/wide = co-partitioned vs repartitioned rules. Batch-only. |
| **Dataflow model** (VLDB'15) | Event time vs processing time. **Windowing** via `AssignWindows(datum) → Set<Window>` and `MergeWindows(Set<Window>)` (sessions). `GroupByKeyAndWindow`. **Triggers** decide when panes emit. **Watermarks** are (often heuristic) lower bounds on event time. **Accumulation modes**: discarding, accumulating, accumulating & retracting. | Runner-specific. | Watermarks are the only global progress signal. | Very high: watermarks are max-lattices; a watermark trigger is a monotone threshold; retractions correspond to Z-set weights. |
| **Flink** (ABS 2015; VLDB'17) | Stateful streaming with keyed state. | **Asynchronous Barrier Snapshotting**: a coordinator injects barriers at sources. A task blocks each input that delivered a barrier until all inputs have, then snapshots and forwards the barrier (Alg. 1). For cycles, in-flight records on back-edges are logged as downstream backup (Alg. 2). | Barrier alignment is a per-epoch "seal". | High: barriers are punctuations/seals; ABS is itself expressible as a Dedalus program. |
| **Naiad / timely** (SOSP'13) | Timestamps are partially ordered (epoch plus loop counters). Vertices implement `OnRecv`/`OnNotify`, `SendBy`/`NotifyAt`. **Progress tracking**: *pointstamps* (t, location) with **occurrence** and **precursor** counts over a *could-result-in* order. A notification is delivered when its pointstamp is in the **frontier** (precursor count 0). | Checkpoint/restore (not a focus). | Distributed progress protocol. | Very high: Dedalus time generalizes to partially ordered timestamps; frontiers are antichain lattices. |
| **Differential dataflow / Materialize / DBSP** (CIDR'13; VLDB'20; VLDB'23) | Collections of (data, time, diff). **Shared arrangements** (indexed, shared state). **DBSP**: streams over abelian groups; operators lift ↑f, delay z⁻¹, integration I, differentiation D. Incremental Q^Δ = D∘Q∘I, with the **chain rule** (Q₁∘Q₂)^Δ = Q₁^Δ∘Q₂^Δ. Z-sets are tuples → integer weights. Supports recursive/stratified Datalog. | Replay from durable inputs (Materialize). | Timestamps/frontiers. | Very high: this is incremental Datalog. Z-sets add *retractions*; lattices cover the monotone subset. |
| **Ray / ownership / Exoshuffle / lineage stash** | Distributed futures. **Ownership**: the caller owns a future's metadata, does reference counting and **lineage reconstruction**, and descendants fate-share with the owner (NSDI'21). **Exoshuffle** (SIGCOMM'23) implements shuffle as a library on futures, with a CloudSort record of $0.97/TB. **Lineage stash** (SOSP'19) does decentralized causal logging of nondeterministic events, off the critical path. | Lineage re-execution; causal logging. | Minimal (owner-local). | Medium: dynamic imperative task graphs fit a declarative language poorly. The *lineage stash* idea fits very well (see C2). |
| **Dask** | Python task graphs, single scheduler. | Recompute lost keys. | Central scheduler. | Low. |
| **Lakehouse: Delta / Iceberg** (VLDB'20; CIDR'21) | Tables of immutable columnar files on object stores plus a **transaction log**. Delta: `_delta_log/000001.json`... records contain actions `metaData`, `add`/`remove` (remove carries a timestamp and stays as a tombstone), `protocol`, `commitInfo`, and `txn(appId, version)` for exactly-once streaming writes. Periodic Parquet **checkpoints**. Commits use optimistic concurrency with **atomic put-if-absent**/rename to claim the next version. | Object-store durability; readers use snapshots. | **Exactly one coordinated step**: claiming the log version. | High: file sets are 2P-set lattices (adds and tombstones grow only); the version claim is the only point of order. Great CALM showcase. |
| **Anna** (ICDE'18) | KVS built from **coordination-free actors** (thread per core, private state, gossip merges). State is a `MapLattice<Key, ValueLattice>`. **Consistency via lattice composition**: causal = `PairLattice<VectorClock (MapLattice<proxy, MaxIntLattice>), value>`, merged lexicographically. Also read committed, item-cut isolation, monotonic reads/writes, read-your-writes, PRAM, writes-follow-reads. | Replication plus gossip. | None on the data path. | Very high: this *is* Bloom^L as a system. |
| **Cloudburst** (VLDB'20) | Stateful FaaS on Anna. **LDPC** (logical disaggregation with physical colocation): executor-local caches. **Lattice capsules** wrap state; repeatable-read and causal consistency across function DAGs. | Retry functions; Anna durability. | Session metadata only. | High for serving and ad hoc compute; weak for bulk shuffles. |
| **Hydro vision** (NEWDIR, REWRITES, BIGGER, KEEPCALM) | PACT agenda: separate Program semantics, Availability, Consistency and Targets. Rule-driven decoupling and partitioning. CRDT query models with CALM. | By construction plus rewrites. | Minimized by analysis. | Native. |
| **DuckDB / Velox / smallpond** | Vectorized single-node engines. **smallpond** (DeepSeek, 2025) runs DuckDB over the 3FS distributed file system with no long-running services. GraySort of 110.5 TiB in 30 m 14 s on 50 compute + 25 storage nodes (3.66 TiB/min). | Re-run tasks. | Minimal. | Low as an architecture; very high as a **performance bar** for our data-plane kernels. |

**Evaluation criteria** for our language: (1) coordination-free potential (CALM); (2) lineage-based fault tolerance using our provenance machinery; (3) showcases lattices; (4) uses Dedalus time natively; (5) modern relevance; (6) implementability; (7) verifiability with Part A tools.

| | CF | Lineage | Lattices | Time | Modern | Impl. | Verif. |
|---|---|---|---|---|---|---|---|
| MapReduce+HDFS remake | ◐ | ◐ | ○ | ○ | ○ | ● | ● |
| Spark-style lineage batch | ◐ | ● | ○ | ○ | ◐ | ● | ● |
| Dataflow/Flink streaming | ◐ | ◐ | ● | ● | ● | ◐ | ◐ |
| Naiad/differential/DBSP | ● | ◐ | ● | ● | ● | ◐ | ◐ |
| Ray/Dask futures | ◐ | ● | ○ | ○ | ● | ◐ | ○ |
| Lakehouse (Delta/Iceberg) | ● | ○ | ● | ● | ● | ● | ● |
| Anna/Cloudburst | ● | ○ | ● | ◐ | ◐ | ● | ● |

(● strong, ◐ partial, ○ weak. This is our judgment, informed by the papers above.)

## B3. Three candidate designs

Rules are sketched in the Molly-style Dedalus syntax used throughout this report. Actual syntax is decided elsewhere.

### Candidate C1 — "BOOM-2": lineage-recoverable batch analytics plus FS

A modern BOOM Analytics: Spark-style execution instead of MapReduce, and a BOOM-FS successor.

**Components** (all in our language unless noted):
1. **FS2 metadata service**:
   - relations `file`, `fqpath` (recursive view), `fchunk`, `replica(chunk, node)`, `lease(file, client, expiry)`, `chunkserver(node, lastHb)` (a max-lattice on the heartbeat time);
   - rules for re-replication (`under_replicated(C) :- count<N> replica(C,N) < R`), placement, and garbage collection.
   - The metadata log is replicated by **our Raft**. The monotone metadata reads (lookups against a snapshot) are served by followers.
2. **Chunk servers**: the data path is a Rust byte-streaming primitive; the control path is rules.
3. **Planner**: compiles a Datalog/SQL query into stages. Relations are partitioned by a hash/range `partitioner`. A rule whose body relations are **co-partitioned** is a narrow dependency and is pipelined. Otherwise, insert a **shuffle channel** `x(@hash(K), ...)@async :- ...`.
4. **Shuffle with seals, not barriers**:
   ```
   shuffle(@R, Job, K, V)@async   :- map_out(@M, Job, K, V), part_of(K, R);
   seal(@R, Job, M)@async         :- map_done(@M, Job), reducer(Job, R);
   // per-reducer completion: a sealed partition (Blazes Seal_key on Job,M)
   missing_seal(R, Job)           :- reducer(Job, R), mapper(Job, M), notin seal(R, Job, M);
   ready(R, Job)                  :- reducer(Job, R), notin missing_seal(R, Job);
   ```
   Reducers whose state is a lattice (max/min, set union, HyperLogLog, bloom filters, deterministic top-k) **start merging before `ready`**. Duplicates from retries and speculative attempts are then harmless, because merge is idempotent. Commutative but non-idempotent aggregates (sum, count) are made safe by keying partial results by mapper id: a map lattice `mapper → partial` whose entries are deterministic per mapper. Every reducer still waits for `ready` before *emitting* a final value that claims completeness, such as "the count is 42". Mid-stream reads are threshold tests (e.g. `count ≥ k` over the map lattice) and are safe early. This is CALM made operational.
5. **Scheduler in rules**: locality, delay scheduling, fair share, and **LATE** speculation as in BOOM-MR.
6. **Lineage relations** `derived_from(partition, task, input_partition)` are recorded by the runtime's provenance. **Recovery rule**: `recompute(P) :- lost(P), needed(P);`, recursively applied to lost ancestors. Wide dependencies checkpoint to FS2 based on a cost rule.
7. **Output commit**: rename or version-claim through Raft, idempotent by task attempt id.

**CALM analysis**: maps and shuffles are monotone. Per-partition seals replace global stage barriers. Job completion (∀ partitions done) is the only global point of order.

**Fault tolerance**: re-execution from lineage, speculative duplicates (safe because tasks are deterministic), FS2 replication, and Raft for metadata.

**Showcases**: BOOM heritage, Raft, lineage = provenance, and LDFI on the shuffle/commit protocol.

**Weaknesses**: batch-only (2012-era architecture), limited lattice showcase, and it duplicates what Spark already does well.

**End-to-end tests**: WordCount, Grep, TeraSort (small) and PageRank (iterative), each with worker kills mid-job, plus NameNode failover with Raft. Compare with the BOOM Analytics claims: same job completion, bounded slowdown on failure.

### Candidate C2 — "Tide": incremental, streaming-first dataflow with lattice frontiers and CALM-minimized logging

Successor to Naiad, Differential, Flink and the Dataflow model. **Batch is a bounded stream** (FLO's Bounded/Unbounded distinction).

**Components**:
1. **Partially ordered time**: Dedalus local ticks are generalized to timestamps `(epoch, iter...)` with product order, as in Naiad. Loops (recursive rules across nodes) use iteration coordinates.
2. **Progress tracking as a Dedalus program**. Operators report pointstamp count deltas `(t, loc, ±k)`. These are aggregated in a PN-counter lattice per pointstamp; the P and N parts are grow-only. The frontier is the set of pointstamps with nonzero occurrence count and no active precursor under could-result-in (NAIAD §2.3; the distributed protocol is §3.3, whose safety property is "no local frontier ever moves ahead of the global frontier"). The frontier is non-monotone as a *set*, but **its lower bound only advances**. Model it as a monotone "frontier-lower-bound" lattice fed by sealed per-worker progress batches, and verify it with V4/V5. This is the single most delicate component and should be a flagship verification target.
3. **Watermarks as lattices**:
   ```
   src_wm(Op, Src, max<T>) :- wm_msg(Op, Src, T);            // per-source max lattice
   frontier(Op, min<W>)    :- src_wm(Op, Src, W), sources_sealed(Op);  // min over a sealed key set
   fire(Op, K, Win)        :- pane(Op, K, Win), win_end(Win, E), frontier(Op, F), F >= E;
   ```
   `F >= E` is a monotone threshold test on a max-lattice (BLOOML-style). **Triggers are therefore coordination-free.** Late data (arriving after fire) is handled by accumulation mode: accumulating & retracting emits (−old, +new) weights.
4. **Windows**: `AssignWindows`/`MergeWindows` as rules. Session merging is a union-find-like fixpoint (recursive rule), which is fine in Datalog.
5. **Collections with weights** (DBSP Z-sets) for general queries with retractions. The pure-set/lattice fast path is used when V0 proves a stream insert-only and monotone. **Shared arrangements** (indexed state shared across queries) are a runtime feature.
6. **Fault tolerance: CALM-minimized causal logging**. This is the lineage-stash idea specialized by CALM:
   - Deterministic operators are replayed from durable inputs.
   - Only **nondeterministic events at points of order** (delivery order into order-sensitive operators, `choice`, timers) are causally logged, piggybacked asynchronously as in lineage stash.
   - Monotone operators need **no logging at all**, because any replay order yields the same result by CALM.
   - Optionally, ABS snapshots (barriers = seals) truncate replay.
   - Sinks are idempotent lattice merges or transactional (C3's `txn(appId, version)`).
7. **Durable input log**: Raft-replicated partitions (a Kafka-like log) implemented in our language.

**Showcases**: everything at once — Dedalus time, lattices, CALM (triggers and monotone operators without coordination), provenance (debugging plus replay scope), Raft (input log), and verification (the progress protocol and the snapshot/logging protocol under LDFI and V5).

**Weaknesses**: the most complex candidate. Progress tracking and arrangements are hard to make fast. Retractions go beyond pure lattices and need the Z-set machinery.

**End-to-end tests**: Nexmark queries; the Dataflow-model session-window example with late data under each accumulation mode; incremental transitive closure / connected components under edge insert and delete (compare with naive recomputation); crash of a worker mid-epoch (exactly-once output check); and a TPC-H subset as incrementally maintained views.

### Candidate C3 — "Lattice Lakehouse" (LLH): object-store tables, lattice metadata, serverless compute

Successor to HDFS + Hive/Delta/Iceberg + Anna/Cloudburst.

**Components**:
1. **Blob layer**: an S3-compatible interface to an external object store, or our own chunk servers from C1. Data files are immutable and columnar.
2. **Table metadata as lattices**:
   ```
   // commit records: map lattice from version -> commit; a version is claimed exactly once
   add(T, File, Stats, V)    :- commit_action(T, V, "add", File, Stats);
   remove(T, File, V)        :- commit_action(T, V, "remove", File, _);
   live(T, File, AtV)        :- add(T, File, _, V1), committed(T, V1), V1 <= AtV, snapshot(T, AtV),
                                notin removed_by(T, File, AtV);
   removed_by(T, File, AtV)  :- remove(T, File, V2), committed(T, V2), V2 <= AtV, snapshot(T, AtV);
   ```
   Adds and removes are both grow-only (a 2P-set per table). For a fixed `AtV` whose prefix is sealed, `live` is deterministic. Time travel is just a query at an older `AtV`, i.e. Dedalus time as data.
3. **Commit protocol**: optimistic concurrency. A writer computes its read set and write set, then **claims version V+1** through our Raft (or object-store put-if-absent, as in DELTA). Conflict detection is a rule: `conflict(Tx) :- read_set(Tx, F), remove(T, F, V), V > base(Tx)`. Streaming writers include `txn(appId, version)` for exactly-once (DELTA §3.1.2).
4. **Checkpointing** of the log: a compaction rule materializes live files at version V into a checkpoint object.
5. **Metadata cache**: an Anna-style lattice KVS replica set with gossip, serving snapshot lookups without coordination. Consistency levels via lattice composition (read committed / causal).
6. **Compute**: stateless executors (Cloudburst-style LDPC caches) running compiled query fragments from the C2 engine in bounded mode. They scale to zero.
7. **Maintenance**: compaction, vacuum (physical delete after retention) and overwrite/delete-where. These are **non-monotone**, so they go through commit, and the CALM analysis makes this explicit.

**Showcases**: the crispest CALM story (appends coordination-free; delete/overwrite/compaction coordinated at one point), lattices, Raft, time travel as Dedalus time, and industry relevance (this is what actually replaced HDFS).

**Weaknesses**: the compute engine is not the star here, and lineage plays a small role. Performance depends on the columnar execution kernels.

**End-to-end tests**: concurrent appenders (all commit, with no conflicts); append vs delete conflict (exactly one aborts); crash between data write and commit (orphan files are never visible; vacuum removes them later — an LDFI target); a streaming writer crash with replay (exactly-once through `txn`); time-travel reads during compaction; and a TPC-DS subset.

## B4. Recommendation

**Build "Tide" (C2) as the compute engine on top of the Lattice Lakehouse (C3) storage, and deliver it through BOOM-2 (C1) milestones.** Rationale:
- **Showcase value.** C2 exercises every distinctive feature of the language: Dedalus time, lattices, CALM-driven coordination avoidance, provenance, Raft, and LDFI-verifiable protocols. C3 adds the cleanest "coordinate only here" storage story and matches how the industry actually replaced HDFS.
- **Unification.** Our Datalog runtime is already an incremental per-tick evaluator. Batch jobs are bounded streams (FLO), so one engine covers MapReduce/Spark-style batch, Flink/Dataflow streaming and Materialize-style incremental views. Hadoop's two halves become (i) a lakehouse table layer and (ii) an incremental dataflow engine.
- **Delivery plan**:
  - *M1 ("Hadoop parity")*: C1's FS2 (metadata in rules plus Raft) and batch jobs with seals and lineage recovery. Reproduce BOOM Analytics: WordCount/TeraSort under failures; fqpath; LATE.
  - *M2*: C3 table format on FS2/object store, with the commit protocol verified by LDFI and V5.
  - *M3*: C2 streaming (watermark lattices, triggers, Z-set retractions, CALM-minimized causal logging), with Nexmark.
  - *M4*: C2 progress tracking for iteration (distributed recursive queries, incremental graph analytics). Verify the progress protocol.
- **Not recommended as the core**: Ray/Dask-style dynamic futures (an imperative task-graph model that fights the declarative language); a faithful MapReduce remake (only as the M1 compatibility layer); and adopting DuckDB/Velox as the engine. Those are the **performance bar** for our vectorized join/aggregate kernels, not a design to copy.

---

# MUST-IMPLEMENT CHECKLIST

**Verification — static (V0)**
1. **Temporal stratification check**: the deductive-rule subset must be syntactically stratified; negation cycles through `@next` are allowed — DEDALUS §4.1 Defs. 2–3, Lemma 2.
2. **Temporal safety test**: every rule is instantaneously safe, a persistence rule, or an inductive rule with a positive instantaneous predicate — DEDALUS §4.2 Def. 8, Lemma 3.
3. **Runtime quiescence detection**: the state at T equals the state at T−1 modulo time, used to stop simulations and model checking — DEDALUS Defs. 5–7.
4. **Points-of-order analysis**: dataflow graph, non-monotone edges (negation, aggregation, deletion), temporal clusters (SCCs with a non-monotone and a +/− edge) — CALM-CIDR §4.4.
5. **Lattice-aware monotonicity**: monotone functions, morphisms and threshold tests are treated as monotone — BLOOML (see the BloomL cluster).
6. **Blazes labels and seal inference**: CR/CW/OR_gate/OW_gate; Seal_key/Rep; Async<Run<Inst<Diverge with NDRead/Taint; `compatible()` via injective FDs — BLAZES §IV–V, Figs. 7–10.
7. **Per-output determinism certificate**: confluent / confluent given seals / coordinated at X / nondeterministic by design — CALM Thm. 1; BLAZES.
8. **Lattice law verification**: ACI and identity of `merge`, monotonicity and morphism claims, via SMT, falling back to property-based tests — LADDAD-PHD future work; KATARA §4.
9. **Rewrite precondition checks** for decoupling (independent/monotonic/functional) and partitioning (co-hashing, FD/CD, partial) — REWRITES §3–4.

**Verification — specs and harness (V1–V2)**
10. **Denial-constraint invariants**: any derivable `violation(...)` is a bug — BLOOMUNIT §3 (`fail`).
11. **`pre`/`post` meta-outcomes** evaluated at EOT or quiescence, with vacuity when `pre` is empty — LDFI §2.3; NEMO §3.2.
12. **Automatic `R_log` trace relations** with node-local time, plus a happens-before relation for specs — BLOOMUNIT §3.
13. **Time-annotated body atoms in specs** (`notin bcast(X,Pl)@1`) — MOLLY-SRC `deliv_assert.ded`.
14. **Deterministic simulator**: all nondeterminism (delivery, omissions, crashes, timers, choice) from one seed, with replay and shrinking — A3.3 (DST practice; Hydro sim).
15. **History checkers** (linearizability, isolation anomalies) for Raft/KV/transactions — Elle/Porcupine (general knowledge).
16. **Production watchdogs**: `violation`/`die` relations enforced at runtime with a configurable action, plus rule-firing coverage and profiling from the same firing capture — BOOM-A §6.1–6.2.

**Verification — LDFI (V3)**
17. **Fspec ⟨EOT, EFF, Crashes⟩** with admissibility (no omissions after EFF, at most Crashes crashes) and the sweep procedure — LDFI §2.1.1.
18. **Synchronous execution mode** with a `clock(from,to,t)` fault mask; defined crash semantics — LDFI §2.1, §4.1.1; MOLLY-SRC `FailureSpec.scala`.
19. **Complete provenance capture** (all derivations): firings per rule, two-level aggregate rewrite, rule/goal graph, proof-tree forest — LDFI §4.1.2–4.2.
20. **Why-not provenance for `notin` subgoals** (negative support) — MOLLY-SRC `Verifier.scala` `negativeSupport`; NEMO.
21. **Hazard-analysis encoding**: goal = ∧ firings; firing = ∨ premises; message leaf = O(f,t,time) [time<EFF] ∨ C(f,t′≥firstSend); exactly one crash time per node; Σ NeverCrashed ≥ N−C; enumerate minimal solutions — LDFI §4.3; MOLLY-SRC `Z3Solver.scala`, `Solver.scala`.
22. **Forward/backward loop**: seeded re-solving, an explored set, `isGood` with vacuity — MOLLY-SRC `Verifier.scala`; LDFI App. B, Alg. 1.
23. **Monotone single-shot mode**: when the goal relations are derived without negation or aggregation, one failure-free run plus one hitting-set enumeration yields every relevant failure set (one forward run per candidate still evaluates `pre`/`post`) — this report A2.4 (derived from LDFI App. B).
24. **Symmetry reduction** over EDB-symmetric location constants not mentioned in rules — MOLLY-SRC `SymmetryChecker.scala`.
25. **Counterexample outputs**: Lamport diagram, lineage graph, differential provenance, corrections suggestions — LDFI §5; NEMO §3.4.

**Verification — bounded async and unbounded (V4–V6)**
26. **CALM-guided schedule reduction**: only permute deliveries into non-monotone consumers — BLOOMUNIT §5.
27. **ASP/SAT bounded encoding** of async runs: choice of arrival time, `before` causality, finiteness — STABLE §4.3–4.5.
28. **Dedalus → many-sorted FOL transition system**: relations as state, grow-only network, node-tick action with arbitrary delivered subsets, env inputs — EPR §2.1, §5.
29. **Quantifier-alternation-graph check** (acyclic means decidable), plus semi-bounded mode — EPR §2.2, §3.1.3.
30. **Axiom library**: total order for `ordered` types; quorum sort with intersection axiom behind `majority<>`; `max` via eq. (3) — EPR Figs. 1, 3.
31. **Auto-derived projection relations** to break ∀∃ cycles (`joined_round`/`left_round`-style) — EPR §6.
32. **Inductive check with graphical CTIs** (finite relational structures) — IVY §2; EPR Fig. 6.
33. **Auto-generated regular invariants** from syntax (send, receive, monotonicity) — KONDO §3.1.
34. **DuoAI-style invariant inference** using our engine to evaluate candidate denial queries on simulated states — DUOAI §2–6.
35. **Exporters**: TLA+ (TLC/Apalache), Ivy/mypyvy, Lean inductive predicates — FVN §3.1 mapping; Apalache README.

**Modern Hadoop (Part B)**
36. **Partitioned channels**: hash/range distribution policies, co-partitioning inference, narrow vs wide dependency classification — RDD §4 (Table 3); REWRITES §4.1.
37. **First-class seals/punctuations per key**, so reducers start on monotone merges and wait only for non-monotone ones — BLAZES §IV; C1.
38. **Lineage-based recomputation** of lost partitions, with checkpoint truncation — RDD §5.4.
39. **Watermark/frontier lattices** and threshold triggers; AssignWindows/MergeWindows; accumulation modes including retraction — DATAFLOW §2.
40. **Partially ordered timestamps** and progress tracking (pointstamps, occurrence/precursor counts, could-result-in, frontier) — NAIAD §2.3 (single-node), §3.3 (distributed protocol).
41. **Z-set collections and incrementalization** (↑, z⁻¹, I, D, chain rule); shared arrangements — DBSP §2–3; McSherry VLDB'20.
42. **Barrier snapshots** (aligned; back-edge logging for cycles) and/or causal logging only at points of order — ABS Algs. 1–2; lineage stash (SOSP'19).
43. **Lakehouse log**: add/remove (tombstones), metadata, protocol and `txn(appId,version)` actions; checkpoints; a single coordinated version claim — DELTA §3.1.
44. **Lattice KVS with gossip**; consistency via lattice composition (vector-clock pair lattice for causal) — ANNA §VI-B/C; CLOUDBURST §5.2 (lattice capsules).
45. **BOOM-FS relations and fqpath recursion**; Paxos/Raft-replicated metadata; LATE speculation in rules — BOOM-A §3 (HDFS rewrite), §4 (Paxos availability), §7 (MapReduce port).

---

# TEST PROGRAMS

**LDFI corpus** (port verbatim from `palvaro/molly/src/test/resources/examples_ft/`). Expected results from LDFI Figs. 12–13; our Molly-2 must match the bug/no-bug verdict and be at most comparably many executions:
1. `delivery/simplog.ded` + `deliv_assert.ded` (simple-deliv): counterexample at EOT 4, EFF 2, 0 crashes. Dropping A→B at t=1 falsifies `log(B,...)`.
2. `delivery/rdlog.ded` (retry-deliv): counterexample needs a crash (EOT 4, EFF 2, 1 crash): redundancy in time but not space.
3. `delivery/classic_rb.ded` (classic-deliv): counterexample under omissions (EOT 5, EFF 3, 0 crashes): partial broadcast relayed before a crash-like loss.
4. `delivery/replog.ded` / redun-deliv (7 LOC): **no counterexample** up to EOT 11 / EFF 10 (Molly used 11 executions of 8.07×10¹⁸ combinations).
5. `delivery/ack_rb.ded` (ack-deliv): **no counterexample** up to EOT 8 / EFF 7 (673 executions). This is the negation-driven case: retries appear only in failure runs.
6. `commit/2pc.ded` + `2pc_assert.ded`: termination violated by a coordinator crash (EOT 5, EFF 0, 1 crash); agreement holds.
7. `commit/2pc_ctp.ded`: still blocks (EOT 8, EFF 0, 1 crash).
8. `commit/3pc.ded`: agreement violated under omissions plus a crash (EOT 9, EFF 7, 1 crash; 55 executions).
9. `kafka.ded` (+ `fake_zk*.ded`): durability violated. A partition removes b and c from the ISR, the leader acks alone, then crashes (EOT 6, EFF 4, 1 crash; 38 executions).
10. `paxos_synod.ded`: agreement holds up to EOT 7 / EFF 6 (173 executions).
11. Bully leader election (`util/leader.ded` family): no counterexample up to EOT 10 / EFF 9.
12. `flux/*.ded`: no counterexample up to EOT 22 / EFF 21 (187 executions).
13. `raft/raft.ded` + `raft_assert.ded`: `bad(N1,N2,"two leaders")` and `bad(...,"disagree")` never derived for correct Raft. Seed known Raft bugs (e.g. committing entries from previous terms by counting replicas) and expect counterexamples.
14. `negative_support_test.ded`: exercises why-not provenance (`bad` depends on the absence of `snd`).

**Invariant / unbounded verification**:

15. **Paxos (EPR Fig. 3)** compiled from our Dedalus Paxos:
    - V5 must verify eqs. (4)–(15) in EPR.
    - The pre-rewrite model must be reported as outside EPR, with a QA-graph cycle through round/value/quorum/node.
    - The EPR Fig. 6 CTI (one node, r1<r2, stale `joined_round`) must be reproducible when invariant (15) is omitted.
16. **Simplified consensus** (DUOAI §2): inference must find invariants (1)–(4), including the ∃quorum invariant (3).
17. **Ring leader election** (IVY §2): BMC finds a trace when "IDs unique" is omitted; a universal inductive invariant is found otherwise.
18. **Multi-Paxos, Flexible Paxos (quorum axiom variant), Stoppable Paxos**: stretch goals for V5 plus inference (DUOAI Table 1 lists them as solvable).

**Confluence / CALM**:

19. **Shopping cart** (CALM §2; CALM-CIDR). Add/remove as two grow-only sets is confluent. Checkout is a point of order. With the client "manifest" of update ids, checkout becomes safely sealed. Expect the V0 certificate to flip accordingly.
20. **Deadlock detection vs garbage collection** (CALM §1.3): reachability (monotone) is certified confluent; non-reachability is flagged.
21. **Ameloot "message join"** (FAIRNESS §1): confluent but not consistent under fairness. V4 should show fair runs with different outputs; the V0 certificate must not claim consistency.
22. **Blazes ad-tracking and word count**: annotations and seal compatibility as in BLAZES §VI. Sealing on `campaign` makes only the CAMPAIGN query deterministic.
23. **BloomUnit FIFO delivery** spec (Fig. 4) and the cart-checkout race: V4 must find the reordering bug that BloomUnit found by sampling.

**Modern Hadoop**:

24. **BOOM-FS parity**: `fqpath` recursion; file create/append/list; DataNode heartbeat timeout removes replicas; re-replication; NameNode failover via Raft with no metadata loss (LDFI on the metadata commit path).
25. **Batch**: WordCount, Grep, TeraSort (small), PageRank. Results must equal a single-node reference under random worker kills (lineage recovery) and with speculation enabled (deterministic duplicates).
26. **Streaming**: the Dataflow-model session-window example with late data under discarding/accumulating/retracting modes; Nexmark Q1–Q8; exactly-once output after a mid-epoch crash.
27. **Incremental recursion**: connected components and transitive closure under inserts and deletes; outputs equal naive recomputation after every epoch. This is also a differential test of the engine's incremental algorithm (motivated by NDLOG-SEM's PSN bugs).
28. **Lakehouse**: concurrent appends (all commit); append-vs-delete conflict (exactly one aborts); crash between data write and commit (never visible); streaming writer replay with `txn(appId,version)` (no duplicates); time-travel read during compaction.

---

## Appendix: key URLs not in the §0.1 table
- Molly repository: https://github.com/palvaro/molly
- Paxos Made EPR supplementary Ivy files: http://www.cs.tau.ac.il/~odedp/paxos-made-epr.html
- Kondo artifact: https://github.com/GLaDOS-Michigan/Kondo
- Apalache: https://github.com/apalache-mc/apalache
- Katara artifact: https://github.com/hydro-project/katara
- Hydro research list: https://hydro.run/research/
- Autocomp (Hydro rewrites) artifact: https://github.com/rithvikp/autocomp
- Exoshuffle: https://arxiv.org/pdf/2203.05072
- Lineage stash: https://stephanie-wang.github.io/pdfs/sosp19-lineage-stash.pdf
- Shared arrangements: http://www.vldb.org/pvldb/vol13/p1793-mcsherry.pdf
- smallpond: https://github.com/deepseek-ai/smallpond
- Datalog in Coq (SSReflect): https://hal.science/hal-01745566 ; Datalog in Lean: https://arxiv.org/abs/2605.02113
- Pardinus/Electrum (Alloy 6 temporal model finder): https://dl.acm.org/doi/10.1145/3238147.3240475
