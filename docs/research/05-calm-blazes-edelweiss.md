# 05: CALM, Blazes, Edelweiss (coordination analysis and storage reclamation)

Research cluster report for **bloom-remake**. Audience: the people implementing the compiler's
analysis passes (monotonicity/CALM, Blazes-style coordination labels, seal/punctuation
synthesis, Edelweiss-style automatic GC) and the runtime primitives they need.

Everything below comes from the papers and source code listed in section 0. Where a paper is
ambiguous or internally inconsistent, the text says so and marks our proposed resolution as
**[RESOLUTION]**. Material I could not access is marked **[NOT ACCESSED]**.

---

## 0. Sources actually read

| Work | What I read | URL |
|---|---|---|
| Hellerstein, *The Declarative Imperative* (SIGMOD Record 39(1), 2010; PODS'10 keynote) | full PDF | https://dsf.berkeley.edu/papers/sigrec10-declimperative.pdf |
| Alvaro, Conway, Hellerstein, Marczak, *Consistency Analysis in Bloom: a CALM and Collected Approach* (CIDR 2011) | full PDF | https://dsf.berkeley.edu/papers/cidr11-bloom.pdf |
| Ameloot, Neven, Van den Bussche, *Relational Transducers for Declarative Networking* (PODS 2011 / JACM 2013) | full arXiv version | https://arxiv.org/abs/1012.2858 |
| Zinn, *Weak Forms of Monotonicity and Coordination-Freeness* (arXiv 2012) | full | https://arxiv.org/abs/1202.0242 |
| Ameloot, *Declarative Networking: Recent Theoretical Work on Coordination, Correctness, and Declarative Semantics* (SIGMOD Record 43(2), 2014) | full | https://databasetheory.org/sites/default/files/2016-06/ameloot.pdf |
| Ameloot, Ketsman, Neven, Zinn, *Weaker Forms of Monotonicity for Declarative Networking* (PODS 2014 / TODS 40(4) 2016) | **[NOT ACCESSED]** in full (paywalled). Abstract and secondary summaries only. | https://dl.acm.org/doi/10.1145/2809784 |
| Zinn, Green, Ludäscher, *Win-move is Coordination-free (Sometimes)* (ICDT 2012) | **[NOT ACCESSED]** directly; content via the Zinn 2012 report and the Ameloot survey | — |
| Marczak, Alvaro, Conway, Hellerstein, Maier, *Confluence Analysis for Distributed Programs: A Model-Theoretic Approach* (Datalog 2.0, 2012) | full PDF | https://www.neilconway.org/docs/dedalus_confluence_cr.pdf |
| Alvaro, Conway, Hellerstein, Maier, *Blazes: Coordination Analysis for Distributed Programs* (ICDE 2014) | full arXiv v2 (1309.3324) including figures (rendered pages), plus tech report UCB/EECS-2013-133 | https://arxiv.org/abs/1309.3324 , https://www2.eecs.berkeley.edu/Pubs/TechRpts/2013/EECS-2013-133.pdf |
| Conway, Alvaro, Andrews, Hellerstein, *Edelweiss: Automatic Storage Reclamation for Distributed Programming* (PVLDB 7(6), VLDB 2014) | full PDF | https://www.neilconway.org/docs/vldb2014_edelweiss.pdf |
| Edelweiss sample programs + compiler output | all 10 input/rewritten pairs | http://boom.cs.berkeley.edu/vldb14 |
| Edelweiss prototype source (`bud` branch `gc-proto-v1`, commit `3cb93c6`, 2014-05-08) | `lib/bud/bud_meta.rb` (RCE/RSE rewrites), `collections.rb` (`BudSealed`, `BudRangeCompress`), `multirange.rb`, `test/tc_gc.rb` | https://github.com/bloom-lang/bud/tree/gc-proto-v1 |
| Bud master (commit `cbcc907`, 2020) CALM labeling code | `lib/bud/labeling/labeling.rb`, `lib/bud/meta_algebra.rb`, `lib/bud/rewrite.rb`, `test/tc_labeling.rb`, `lib/bud/lattice-lib.rb` | https://github.com/bloom-lang/bud |
| bud-sandbox shopping carts | `cart/*.rb` | https://github.com/bloom-lang/bud-sandbox |
| Hellerstein & Alvaro, *Keeping CALM: When Distributed Consistency is Easy* (CACM 63(9), 2020) | full arXiv version | https://arxiv.org/abs/1901.01930 |
| Bailis, Fekete, Franklin, Ghodsi, Hellerstein, Stoica, *Coordination Avoidance in Database Systems* (PVLDB 8(3), VLDB 2015) | extended arXiv version, sections 1–5, 7 | https://arxiv.org/abs/1402.2237 |
| Ketsman & Koch, *Datalog with Negation and Monotonicity* (ICDT 2020) | intro and results summary | https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ICDT.2020.19 |
| Laddad, Power, Milano, Cheung, Crooks, Hellerstein, *Keep CALM and CRDT On* (VLDB 2023) | sections 1–3 | https://arxiv.org/abs/2210.12605 |
| Power, Koutris, Hellerstein, *The Free Termination Property of Queries Over Time* (2025) | abstract, intro | https://arxiv.org/abs/2502.00222 |
| Hellerstein, *Complete CALM: A Coordination Criterion for Specifications* (arXiv 2602.09435 v4, June 2026) | sections 1–5 | https://arxiv.org/abs/2602.09435 |
| Hydro (`hydro_lang`, commit `9e2a120`, 2026-09-24) | `live_collections/stream/mod.rs`, `nondet.rs`, `properties/mod.rs` | https://github.com/hydro-project/hydro |

Two terms in the task statement need correcting. The Edelweiss paper never uses the name
"Bloom^-". It calls the restricted language "Edelweiss, a sublanguage of Bloom". Separately,
the Blazes label that the ICDE/arXiv version calls **Diverge** is called **Split**
("split brain") in the tech report. The arXiv version's own derivation figure still prints
"Split" too.

---

## 1. Executive summary for implementers

1. **CALM** (Consistency As Logical Monotonicity) says a program has a consistent,
   coordination-free distributed implementation **iff** it is monotone. The formal version
   (Ameloot et al.) uses relational transducer networks. "Coordination-free" there means:
   for every input, *some* data placement lets every node reach its final output with
   heartbeats only and no message reads. The compiler therefore needs a **sound, conservative
   monotonicity analysis**. Anything it cannot prove monotone is a *point of order* and needs
   either coordination or a proof obligation.
2. There are **weaker monotonicity classes**. If nodes know the partitioning policy, the
   domain-distinct-monotone queries (adom-monotone) become coordination-free. If data is
   placed by domain values, the domain-disjoint-monotone queries also become
   coordination-free. This is the theory behind "partition-aware" non-monotone operators.
3. **Blazes** is a dataflow analysis with two phases. Phase one annotates each
   component path as CR/CW/OR_gate/OW_gate and each stream as Seal_key/Rep. Phase two
   propagates stream labels (Async < Run < Inst < Diverge, with internal NDRead and Taint)
   using four inference rules and two reconciliation rules. Where the result is worse than
   Async, it synthesizes coordination. It prefers **sealing** (punctuations per partition plus
   a local unanimous vote among the partition's producers) and falls back to **total
   ordering** (Zookeeper, i.e. consensus). Sealing is only legal when the seal key
   *injectively determines* some attribute of the component's partition key (`compatible`).
   Checking that requires a functional-dependency chase.
4. **Edelweiss** takes programs that only accumulate state (no deletion, persisted channels,
   channels derived monotonically from persistent state). It automatically rewrites them to
   (a) stop resending acked messages (**ARM**), (b) delete tuples that can never again
   influence output (**DR+**, using `X.notin(Y)` with persistent Y), (c) delete from both
   sides of a key-matched negation (**DR−**, using a range-compressed "seen keys" set), (d)
   compress gap-free integer sets (**range**), and (e) use **punctuations/seals** to reclaim
   join inputs. The prototype implementation (bud `gc-proto-v1`) gives the exact safety
   conditions, and they are reproduced below.
5. **Invariant confluence** (Bailis et al.) is the transactional analogue: a set of
   transactions can run coordination-free under invariant I **iff** any two I-valid,
   I-T-reachable states with a common ancestor merge (union) into an I-valid state.
6. **Modern practice** (Hydro 2026) puts Blazes-like properties into the type system
   (`Bounded/Unbounded`, `TotalOrder/NoOrder`, `ExactlyOnce/AtLeastOnce`). It requires
   commutativity/idempotence proofs for folds over unordered or duplicated streams, and it
   forces every nondeterministic operator to carry a `nondet!` justification.

---

## 2. CALM theory

### 2.1 The conjectures (Hellerstein, PODS 2010 / SIGMOD Record 2010)

Verbatim statements (SIGMOD Record 39(1), section 4):

> **CONJECTURE 1. Consistency And Logical Monotonicity (CALM).** A program has an eventually
> consistent, coordination-free execution strategy if and only if it is expressible in
> (monotonic) Datalog.

> **CONJECTURE 2. Causality Required Only for Non-monotonicity (CRON).** Program semantics
> require causal message ordering if and only if the messages participate in non-monotonic
> derivations.

> **CONJECTURE 3. Dedalus Time ⇔ Coordination Complexity.** The minimum number of Dedalus
> timesteps required to evaluate a program on a given input data set is equivalent to the
> program's Coordination Complexity.

> **CONJECTURE 4. Fateful Time.** Any Dedalus program P can be rewritten into an equivalent
> temporally-minimized program P′ such that each inductive or asynchronous rule of P′ is
> necessary: converting that rule to a deductive rule would result in a program with no
> unique minimal model.

Supporting ideas from the same paper that matter for implementation:

- **Pipelined Semi-Naive (PSN)** evaluation "makes monotonic logic embarrassingly parallel":
  a monotone (sub)program proceeds with no synchronization between deductions. Our engine
  should evaluate monotone strata in a pipelined, delta-at-a-time way across nodes, with no
  per-iteration barriers.
- **"Counting requires waiting; waiting requires counting."** Coordination is needed at
  non-monotone stratification boundaries. A stratum boundary is a global barrier.
  Coordination protocols are themselves aggregations: 2PC needs unanimous votes, Paxos a
  majority.
- **Coordination Complexity** is the number of strata a program must pass through
  sequentially. For stratified programs it is the maximum stratum number, which can be
  analyzed syntactically.
- Monotone Dedalus persistence is `p(X)@next :- p(X).`. Deletion or overwrite needs negation
  (`state(X)@next :- state(X), !del_state(X).`) and is therefore non-monotone.
- Refinements the paper suggests: `MIN(x) < 100` is monotone despite being an aggregate (the
  CIDR'11 paper repeats this). Threshold tests over monotone aggregates are monotone.

### 2.2 Formal CALM (Ameloot, Neven, Van den Bussche, PODS'11 / JACM'13)

**Model: a relational transducer network.**

- A transducer schema is (S_in, S_sys, S_msg, S_mem, k). S_sys = {Id, All} (unary). `Id` is
  this node's identifier; `All` is the set of all nodes.
- A transducer is a set of queries Q_snd^R (R ∈ S_msg), Q_ins^R, Q_del^R (R ∈ S_mem), and
  Q_out (arity k), all over S_in ∪ S_sys ∪ S_msg ∪ S_mem.
- A transition I, I_rcv → J, J_snd, J_out does the following. Let I′ = I ∪ I_rcv. Then
  J_snd(R) = Q_snd^R(I′) and J_out = Q_out(I′). Memory updates ignore conflicting
  insert/delete pairs:
  J(R) = (ins \ del) ∪ (ins ∩ del ∩ I(R)) ∪ (I(R) \ (ins ∪ del)).
  **Outputs can never be retracted.**
- A network is a finite, connected, undirected graph. Messages sent go into the multiset
  buffers of all neighbours. There are two kinds of transitions: a **heartbeat** (read
  nothing) and a **delivery** (read exactly one fact). A run is infinite. A **fair** run has
  every node heartbeat infinitely often and every buffered fact eventually delivered.
- Input instance I is distributed by a **horizontal partition** H with ∪_v H(v) = I (overlap
  allowed).
- **Consistent**: for every I, every fair run on every horizontal partition of I has the
  same output. **Network-topology independent**: consistent on every network and computing
  the same query Q.
- **Coordination-free** (the key definition): Π is coordination-free on N if for every input
  I there *exists* a horizontal partition H and a run on H that reaches quiescence using
  **only heartbeat transitions**. Π is coordination-free if this holds on every network.
- **Oblivious**: does not use Id or All. **Inflationary**: never deletes. **Monotone
  transducer**: uses only monotone local queries.

**Results** (Theorem 6, Proposition 11, Theorem 12, Corollaries 13, 14, 17):

- Every monotone query can be distributedly computed by an oblivious, inflationary, monotone
  transducer. The strategy is to flood all inputs and continuously re-apply Q.
- Every network-topology-independent oblivious transducer is coordination-free (put all data
  everywhere).
- Every query computed by a coordination-free transducer is monotone.
- **CALM Property (Cor. 13):** the following are equivalent: Q is computed by a
  coordination-free transducer; Q is computed by an oblivious transducer; Q is monotone.
- **Datalog version (Cor. 14(3)):** Q is computable by a coordination-free
  nonrecursive-Datalog transducer ⇔ by an oblivious one ⇔ Q is expressible in Datalog. The
  literal "expressible in (monotonic) Datalog" direction of the original conjecture is
  **false**, because there are monotone queries outside Datalog, even within PTIME (Afrati,
  Cosmadakis, Yannakakis).
- Theorem 16: a transducer that does not use Id computes only monotone queries. Cor. 17: the
  following are equivalent: computable obliviously; computable without Id; computable without
  All. Example 15 shows that using All can make even the identity query
  non-coordination-free (a node pings when it is not alone).
- Example 10: the **emptiness query** on S is not coordination-free. Each node sends its Id
  if its local S is empty, and outputs true when Ids from `All` have all arrived.
- Lemma 5(1): a coordinating "Ready" multicast (acks plus `done(v,v′)` messages plus
  checking All) gives every node the entire input and a flag that it has it. This is the
  generic coordination construction. It uses no deletions.
- Dedalus can simulate arbitrary Turing machines in an eventually consistent way
  ("entanglement": timestamps copied into data). So eventually consistent Dedalus is not
  contained in PTIME.

**Implications for us.** Membership (`All`) and self-identity (`Id`) are exactly the
capabilities that enable non-monotone (coordinating) computation. The analysis should treat
**negation or aggregation over a relation whose completeness depends on membership** (for
example, "received a vote from every node") as a coordination point. That is what 2PC/Paxos
quorum logic looks like in Datalog.

### 2.3 Weaker monotonicity: policy-aware CALM (Zinn et al.; Ameloot et al. PODS'14/TODS'16)

Models from Zinn 2012 (arXiv 1202.0242), building on Zinn, Green, Ludäscher ICDT'12:

| Model | What nodes know / how data is placed | Coordination-free class |
|---|---|---|
| N0 (= Ameloot) | arbitrary placement, no knowledge of policy | **F[N0] = M** (monotone) |
| N1 | a **partitioning policy** P maps each possible ground input fact to a nonempty set of nodes. Each node has an oracle "is n_j ∈ P(f)?" for any fact it can construct (all constants known to it) | **F[N1] = M_adom** (a.k.a. *domain-distinct-monotone*) |
| N2 | **domain-guided** placement: F: dom → 2^N \ {∅}, and P(R(c1..cn)) = ∪_i F(ci) for non-nullary R (a fact is replicated to every node responsible for any of its constants) | **F[N2] = M_weak-adom** (a.k.a. *domain-disjoint-monotone*) |
| N3 | N2 plus knowledge of the global active domain | all computable queries |

Strict hierarchy: M = F[N0] ⊊ F[N1] ⊊ F[N2] ⊊ F[N3] = C.

Definitions (Zinn 2012, Defs 3.1 and 3.4, verbatim in substance):

- **Q is adom-monotone** if Q(I) ⊆ Q(I ∪ {f}) for every input I and every fact
  f = R(c1..cn) with **at least one** ci ∉ adom(I).
- **Q is weak-adom-monotone** if Q(I) ⊆ Q(I ∪ {f}) for every I and every non-nullary fact f
  whose constants **all** lie outside adom(I). Equivalently: Q(I) ⊆ Q(I ∪ I′) whenever
  adom(I) ∩ adom(I′) = ∅ and I′ has no nullary facts.

Examples (Ameloot survey):

- `R \ S` (unary R, S) is domain-distinct-monotone but not monotone.
- The complement of transitive closure is domain-disjoint-monotone but not
  domain-distinct-monotone.
- Win-move is coordination-free under domain-guided distribution (ICDT'12 title result).

Datalog characterizations:

- Zinn Lemma 3.2: **semi-positive Datalog ⊆ M_adom**. The proof replaces each ¬R by a
  complement relation R^c over the active domain, which makes the program positive.
- Remark 3.3 separates SP from stratified Datalog¬ with the query "a path of length one but
  not length two".
- The TODS abstract says the classes are captured by explicit Datalog variants, "one such
  fragment is based on stratified Datalog where rules are required to be connected with the
  exception of the last stratum". It also says coordination-freeness = "computations that do
  not require knowledge about all other nodes in the network". **[NOT ACCESSED]**: the exact
  syntactic fragments in the TODS paper. Do not implement those syntactic tests from this
  report. Use the semantic definitions above to design conservative checks.

**Implication for us.** If the runtime exposes the *partitioning function* to the program
(e.g. `owner(key)` / "is this fact routed to me?"), some non-monotone operators become
locally decidable. The typical case is negation over a relation partitioned so that all
facts sharing the negated key land on the same node. This is the theoretical basis for
Blazes' "seal on a compatible key" and for Hydro-style co-partitioning. The analysis can
accept `X.notin(Y, k)` at node n without coordination when (a) Y is partitioned by k with a
policy known to n, (b) n owns k, and (c) Y is otherwise complete at n for keys n owns. That
is still a sealing obligation for Y's in-flight messages, **unless** Y is purely an input
relation placed by the policy.

### 2.4 Keeping CALM (Hellerstein & Alvaro, CACM 2020)

- Definition 1: P is **monotonic** if S ⊆ T ⇒ P(S) ⊆ P(T).
- Theorem 1 (CALM): "A program has a consistent, coordination-free distributed
  implementation if and only if it is monotonic."
- **Confluence** as program consistency: a single-machine operation is confluent if it
  produces the same set of outputs for any nondeterministic ordering *and batching* of a set
  of inputs. **Confluent operations compose.**
- Motivating pair: **distributed deadlock detection** (cycle existence) is monotone and
  coordination-free. **Distributed garbage collection** (non-reachability) is non-monotone
  and needs coordination.
- Observation 1: coordination-freeness is equivalent to availability under partition. CALM
  is the positive counterpart of CAP: monotone programs get all three properties.
- Design patterns: immutable variables; tombstones; CRDTs (lattices).
  - Shopping cart as two grow-only sets. Checkout is still non-monotone.
  - "In later work [Bloom^L] we go further to make checkout monotonic ... the checkout
    operation is enhanced with a manifest from the client of all its update message IDs that
    preceded the checkout message: replicas can delay processing of the checkout message
    until they have processed all updates in the manifest." (This is the `monotone_cart.rb`
    CartLattice in bud-sandbox; see TEST PROGRAMS.)
- "Coordination in its place": reclaiming tombstones (distributed deletion) can be
  coordinated lazily in the background. That is exactly the role Edelweiss plays.
  Compensation ("apologies") is an alternative to coordination.
- Open questions: expressiveness vs Immerman–Vardi; analyzing code that has been *repaired*
  with coordination (still syntactically non-monotone, since coordination "controls
  non-determinism" rather than removing it); stochastic CALM; monotone program synthesis.

### 2.5 Model-theoretic confluence for Dedalus (Marczak et al., Datalog 2.0 2012)

This is the Dedalus-level formalization closest to what our engine will run.

- **Ultimate model**: the output-schema facts that are *eventually always true* in a stable
  model. Each stable model corresponds to one assignment of message timestamps obeying the
  causality constraint (a message sent at local time s cannot arrive in the sender's past).
  A program is **confluent** iff every EDB instance has exactly one ultimate model;
  otherwise it is **diffluent**.
- Lemma 1: confluence of Dedalus programs is **undecidable**. Lemma 2: Dedalus subsumes
  PSPACE.
- **Dedalus+** = semipositive (¬ only on EDB) plus **guarded asynchrony**: every relation
  that heads an async rule also has the persistence rule `p(X)@next ← p(X)`.
  - Lemma 3: temporally inflationary. Theorem 1: confluent. Theorem 2: captures exactly
    PTIME.
  - Corollary 1: async rules can be replaced by `@next` rules without changing the ultimate
    model.
- **Dedalus^S** = guarded asynchrony plus a PDG with no cycle through negation (edges:
  deductive / inductive / async / negated). It is evaluated stratum by stratum; each stratum
  is a Dedalus+ program. It is confluent and captures PTIME.
- **Coordination rewrite P(S)**: add `p_done()` to every rule body containing `¬p(...)`.
  Define `p_done` over the **collapsed PDG** (SCCs collapsed; an SCC with an async edge is
  *async recursive*):
  - EDB: `p_done().`
  - Non-async-recursive p: `p_done() ← r1_done(), ..., r_ip_done().` with one `rj_done` per
    rule defining p.
  - Deductive rule j: `rj_done() ← p1_done(), ..., piq_done().` over all relations in
    predecessor nodes.
  - Asynchronous rule j: record sent messages, receivers ack, a sender that has all acks
    and nothing more to send tells the receiver so. The vacuous case (nothing to send) is
    also announced. A receiver notified by all nodes derives `rj_done()`. (The exact rules
    are in Appendix D of their TR, **[NOT ACCESSED]**.)
  - Async-recursive SCC: a **two-round voting protocol** led by the minimum node id. The
    paper gives these rules verbatim (subscript i is the SCC):

```
rj_not_done() ← pj_to_send(X), ¬pj_ack(X).
rj_done() ← ¬rj_not_done().
all_ack_i() ← r1_done(), ..., r_ip_done().
not_node_min(L1) ← node(L1), node(L2), L2 < L1.
node_min(L) ← ¬not_node_min(L), node(L).
start_round_1_i() ← node_min(#L,L), ¬round_1_i().
round_1_i()@next ← start_round_1_i().
round_1_i()@next ← round_1_i(), ¬start_round_2_i().
vote_1_i(#N)@async ← start_round_1_i(), node(N).
complete_1_i(#M,N)@async ← vote_1_i(#N), all_ack_i(#N), node_min(#N,M).
incomplete_1_i(#M,N)@async ← vote_1_i(#N), ¬all_ack_i(#N), node_min(#N,M).
complete_k_i(N)@next ← complete_k_i(N), ¬start_round_1_i().        (k = 1,2)
incomplete_k_i(N)@next ← incomplete_k_i(N), ¬start_round_1_i().    (k = 1,2)
recv_k_i(N) ← complete_k_i(N).
recv_k_i(N) ← incomplete_k_i(N).
not_all_recv_k_i() ← node(N), ¬recv_k_i(N).
not_all_comp_k_i() ← node(N), ¬complete_k_i(N).
start_round_1_i() ← ¬not_all_recv_k_i(), not_all_comp_k_i().
sent_i() ← ¬all_ack_i().
sent_i()@next ← sent_i(), ¬vote_1_i().
start_round_2_i() ← ¬not_all_recv_1_i(), ¬not_all_comp_1_i(), node_min(#L,L).
vote_2_i(#N)@async ← start_round_2_i(), node(N).
complete_2_i(#M,N)@async ← vote_2_i(#N), all_ack_i(#N), ¬sent_i(#N), node_min(#N,M).
incomplete_2_i(#M,N)@async ← vote_2_i(#N), sent_i(#N), node_min(#N,M).
done_recursion_i() ← ¬not_all_recv_2_i(), ¬not_all_comp_2_i().
p_done() ← done_recursion_i().     for every p in the SCC
```

- **Lemma 5 (Sealing):** once `p_done(l,t)` holds, it holds at all later times, and p's
  contents at l never change after t.
- Worked diffluent example, the **asynchronous marriage ceremony** (Example 3):

```
i_do(X)@async ← i_do_edb(X).
runaway() ← ¬i_do(bride), i_do(groom).
runaway() ← ¬i_do(groom), i_do(bride).
runaway()@next ← runaway().
i_do(X)@next ← i_do(X).
```

  It has an ultimate model with `runaway()` and one without. Example 4 rewrites it into
  Dedalus+ by pushing negation to the EDB (`i_dont(X)@async ← ¬i_do_edb(X).`).
- **Known limitation later fixed:** the whole-relation `p_done` seal is "not defined for
  unbounded input relations" (Blazes §IX). Blazes generalizes it to **per-partition** seals.

### 2.6 Monotone ≠ positive, and other refinements

- **Ketsman & Koch (ICDT 2020).** The monotone fragment of stratified Datalog (even two
  strata, no ≠) is strictly more expressive than positive Datalog. With ≠, even a single
  stratum is strictly more expressive. The monotone fragment of semi-positive Datalog without
  ≠ equals positive Datalog. "Negation-bounded" (conflict-free) semi-positive Datalog has the
  property "monotone ⇔ rewritable to positive"; deciding conflict freedom is EXPTIME-complete.
  **Takeaway:** a syntactic "no negation" test is sound but incomplete. Semantic monotonicity
  is undecidable in general. Keep the analysis conservative and let users supply proofs or
  annotations.
- **Bloom^L refinements** (bud `rewrite.rb`, `lattice-lib.rb`). The CALM analysis treats a
  rule as monotone iff every method call is in `MONOTONE_WHITELIST`
  (`== + <= - < > * ~ +@ pairs matches combos flatten new lefts rights map flat_map pro merge schema cols key_cols val_cols payloads lambda tabname current_value`),
  or is a declared lattice **morphism** or **monotone function**. Any other call on a
  collection sets the rule's `nm` flag. `notin(y)` records a non-monotone dependency on `y`
  only. The deletion superator `<-` is non-monotone. Examples of lattice methods:
  - morphs: `lmax.gt`, `lmax.gt_eq`, `lmax.+`, `lmin.lt`, `lbool.when_true`, `lmap.at`,
    `lmap.key?`, `lmap.key_set`, `lmap.intersect`, `lset.intersect`, `lset.contains?`,
    `lset.pro`, `lset.eqjoin`
  - monotone (not morphism): `lset.size`, `lmap.size`, `lset.group_count`, `lpset.pos_sum`,
    `lbag.size`

  This is how threshold tests become monotone.
- **Keep CALM and CRDT On (VLDB 2023).** For a CRDT state lattice (D, ⊔), a query Q is
  monotone if i ≤ j ⇒ (Q(i) ⇒ Q(j)).
  - Threshold queries (`|{txn ∈ S | ...}| > 50`, `|A| + |R| > 100` on a 2P-set) can be
    answered locally and are sequentially consistent: they return true or ABORT ("unknown").
  - `A − R` ("potato and Ferrari" early read) cannot be answered locally.
  - Monotone functions compose; a single non-monotone stage makes the pipeline non-monotone.
- **Free termination (2025).** This is the dual *completeness* question: when can a node
  unilaterally stop because its answer can never change? Under acyclic state modifications
  (typical of CRDTs) the only freely terminating queries are threshold queries over the
  state's partial order. If updates form a group or ring (IVM), free termination is
  impossible. Antitone queries are also characterized as coordination-free in their
  generalized sense. **Relevance:** our runtime can emit "final" results early for threshold
  (morphism into `lbool`) outputs, and must not for others.
- **Complete CALM (June 2026)** generalizes CALM from transducers and set inclusion to
  arbitrary **specifications** Spec = (E, Obs, ⪯), where E fixes the histories, Obs maps a
  history to its admissible outcomes, and ⪯ is a declared refinement order.
  - Def. 8 (monotone): for all H1 ⊑ H2 and every o ∈ Obs(H1) there is o′ ∈ Obs(H2) with
    o ⪯ o′.
  - Theorem 1: coordination-free ⇔ monotone. Theorem 2 gives an operational version via
    I/O automata.
  - Def. 11 (**properly coordinated variant**): Obs′ ⊆ Obs, and Spec′ is monotone over its
    admitted histories. This answers the "can we verify coordinated code?" question from
    Keeping CALM, which program-level syntactic CALM cannot (Theorem 3).
  - Theorem 4: membership authority plus a total-order service always yields a monotone
    residual. Example 6: consensus = one non-monotone membership step, then monotone vote
    counting.
  - Theorem 5 (**Complete CAP**): CAP-achievable ⇔ *distributed-monotone* (monotone under
    every partition-constrained future).
  - It subsumes transducer CALM, CRDTs, I-confluence and HATs.

  **Relevance:** it is the right *verification* target for "this module uses Raft
  internally but exposes a monotone interface". See section 7 (recommendations).

### 2.7 Invariant confluence (Bailis et al., VLDB 2015)

System model. Each transaction runs on a replica snapshot, commits locally, and states are
merged with ⊔ (set union in the base model; assumed commutative, associative, idempotent,
and D0 ⊔ Di = Di). Definitions (verbatim in substance):

- Def 1: replica state R is **I-valid** iff I(R) = true.
- Def 2 (**transactionally available**): if T can reach servers holding versions of every
  item it touches, T eventually commits, or aborts only by its own choice or because
  committing would violate a declared invariant on its replica state.
- Def 3 (**convergent**): with no new writes and no indefinite delays, each pair of servers
  eventually holds the same versions of common items.
- Def 4 (**globally I-valid**): all replicas always hold I-valid state.
- Def 5 (**coordination-free execution**): each t's progress depends only on the versions
  t reads.
- Def 6 (**I-confluence**): T is I-confluent w.r.t. I if for all I-T-reachable states
  Di, Dj with a common ancestor, Di ⊔ Dj is I-valid. (I-T-reachable means reachable by a
  partially ordered sequence of transactions and merges in which every intermediate state is
  I-valid.)
- **Theorem 1**: a globally I-valid system can execute T with coordination-freedom,
  transactional availability and convergence **iff** T is I-confluent w.r.t. I.

Table 2 (invariant, operation, I-confluent?), verbatim:

| Invariant | Operation | I-C? |
|---|---|---|
| Attribute equality | Any | Yes |
| Attribute inequality | Any | Yes |
| Uniqueness | Choose specific value | **No** |
| Uniqueness | Choose some value | Yes |
| AUTO_INCREMENT | Insert | **No** |
| Foreign key | Insert | Yes |
| Foreign key | Delete | **No** |
| Foreign key | Cascading delete | Yes |
| Secondary indexing | Update | Yes |
| Materialized views | Update | Yes |
| > | Increment [counter] | Yes |
| < | Increment [counter] | **No** |
| > | Decrement [counter] | **No** |
| < | Decrement [counter] | Yes |
| [NOT] CONTAINS | Any [set, list, map] | Yes |
| SIZE= | Mutation [set, list, map] | **No** |

Other results: TPC-C has ten of twelve invariants I-confluent. "Choose some unique value" is
I-confluent given replica membership (partitioned ID namespaces). Recency and linearizable
reads are *not* achievable with transactional availability.

**Analysis technique** (Bailis §5): static pairwise lookup of (invariant, operation) pairs
in the table. Unrecognized pairs are conservatively flagged non-I-confluent. I-confluence is
undecidable in general; an SMT/model-checker extension is possible.

**Relation to CALM.** CALM gives *determinism* (a liveness/convergence property).
I-confluence adds *safety* via invariants and permits acceptable nondeterminism. Bailis poses
"invariant-scoped monotonicity" as an open problem. Complete CALM (2026) claims to subsume
I-confluence by putting the invariant into Obs (Obs(H) = ∅ for invalid histories).

---

## 3. Bloom's CALM analysis (CIDR 2011) and the bud implementation

### 3.1 Points of order (CIDR 2011 §2, §4.4)

- A Bloom program is a dataflow graph. Sources are input interfaces, sinks are output
  interfaces, **collections are nodes**, and **rules are edges** (an edge A→B when B is on
  the lhs of a statement referencing A on the rhs, directly or through a join).
- Edge annotations: `<+`/`<-` edges are temporal (marked "+/−"). `<~` edges are
  asynchronous (dashed). An edge whose statement uses **aggregation, negation, or deletion
  via `<-`** is non-monotone (white circle).
- Any SCC containing both a non-monotone edge and a temporal edge collapses into a
  **temporal cluster** (octagon), treated as a single non-monotone node.
- "Any non-monotonic edge in the graph is a **point of order**, as are all edges incident to
  a temporal cluster, including their implicit self-edge."
- An underspecified dataflow (no path from input to output) is flagged "??".
- A program with non-monotonicity can be made consistent by adding coordination at its
  points of order.
- The coordination module itself adds syntactic points of order. It must be verified
  manually and annotated so the analysis skips it ("avoid attempts to coordinate the
  coordination logic").
- Shopping-cart case study: the destructive cart has points of order on every action.
  The disorderly cart has points of order only at checkout (coordinate once per session).
  The analysis can "push back" points of order late in the dataflow or "localize" them.

### 3.2 bud labeling (`lib/bud/labeling/labeling.rb`) and its test oracle

A later refinement (named `TestBlazes` in the tests) computes per-path labels over the
rule dependency graph:

- Edge label = `"A"` if the rule op is `<~`, `"N"` if the rule is non-monotone (`nm`), else
  `"Bot"`.
- SCCs are computed from the transitive closure and collapsed into `<cluster>_IN`/`_OUT`
  nodes. Paths are enumerated from input interfaces to output interfaces. Every channel gets
  a synthetic `<chan>_INPUT` source with an `"A"` edge.
- Path collapse, left to right: `collapse(l, r)`:

```
Bot·x = x;  x·Bot = x;  x·x = x;  D·_ = _·D = D;
A·N  = D          # "CALM": async followed by non-monotone
otherwise keep the sequence [l, r] (irreducible)
```

- Output report per sink: `disjunction` over paths, where D dominates, N∨A = D, N∨N = N,
  A∨A = A, and Bot is the identity.
- **Guarded asynchrony (`GuardedAsync`):** consider two distinct channels whose dataflows
  meet at a collection (`meet`). If both paths pass through a persistent `BudTable` or a
  lattice (`guarded`), the meeting is safe. Otherwise the target and everything downstream
  is a `divergent_pred`, reported as `D`. This catches "unguarded asynchrony": a
  *monotone* join of two ephemeral (scratch) buffers fed by different channels is
  nondeterministic, because the two messages may arrive in different timesteps.

Test oracle (`test/tc_labeling.rb`, verbatim expectations):

- `TestNM` (`response <= guard1.notin(guard2, :val => :val)` with `c1 <~ i1; c2 <~ i2;`
  `guard1 <= c1; guard2 <= c2`) → output `{"response" => "D"}`, path report
  `{"i1"=>"A", "i2"=>"D"}`.
- `TestGroup` (`response <= guard1.group([:val], count)`) → `D`; path `{"i1"=>"D"}`.
- `TestMono` (`response <= (guard1 * guard2).lefts(:val => :val)`) → `A` for both paths.
- `TestDeletion` (adds `c3 <~ dguard; guard2 <- (guard2 * c3).lefts(:val => :val)`) → paths
  `dguard→response` and `i2→response` are `D`.
- `TestNestMod` (the NM module imported) → `D`.
- Unguarded async: `BugButt` (`result <= (ls * rs).lefts` over scratches) and `HalfGuard`
  (`result <= (lt * rs).lefts`) → races unguarded. `FullGuard` (`(lt * rt).lefts` over
  tables) → guarded.

`lib/bud/meta_algebra.rb` has an older variant with the label lattice
M < A, M < N, A < D, N < D, and a **directional** N·A = A: a non-monotone op *followed* by
async is fine, while A·N reaches D. Tag rules: nm with `<~` → D, nm → N, `<~` → A, else M.
It also computes `d_begins`, the first point on each path that "turns D", and `a_preds`, the
last async edge before it: "ordering this edge prevents diffluence". **That is the
coordination-placement heuristic: order the last async edge before the first A·N
transition.**

---

## 4. Blazes (ICDE 2014)

### 4.1 System model

- A **component** is a logical unit of computation and storage with input and output
  interfaces. A **path** is an (input interface → output interface) pair, and each path is
  annotated separately.
- **Streams** are unbounded, unordered collections of messages connecting an output
  interface to an input interface. Components are assumed **deterministic** given the same
  inputs in the same order.
- Logical vs physical dataflow: the analysis is over the **logical** dataflow. A component
  instance binds a component to a resource with its own clock and state; stream instances
  are the physical channels.
- **Runs** are executions over finite stream batches (the replay unit).
- **Punctuations**: "A punctuation guarantees that the producer will generate no more
  messages within a particular logical partition of the stream." TR §2 adds: "Punctuations
  must contain metadata describing the contents of the partition that they seal, because
  (given our weak assumptions regarding stream order) a punctuation for a partition may
  arrive before some of the contents of that partition." In the implementation, a seal
  message carries a **digest of the set of messages** the producer generated for that
  partition (TR §8.2).

### 4.2 Anomalies, properties and mechanisms (Fig. 5)

Anomalies on an output stream, from least to most severe:

1. nondeterministic order (**Async**)
2. cross-run nondeterminism (**Run**)
3. cross-instance nondeterminism (**Inst**)
4. persistent replica divergence (**Diverge**, called Split in the TR)

Component properties: P1 Confluent, P2 Convergent, P3 Sealable. Delivery mechanisms: M1
Sequencing (preordained total order), M2 Dynamic ordering (Paxos etc.), M3 Sealing. Which
of these prevents which anomaly:

| Anomaly | Prevented by |
|---|---|
| ND orders | M1 |
| Cross-run ND | M1 ∨ P1 |
| Cross-instance ND | M1 ∨ M2 ∨ P1 |
| Replica divergence | M1 ∨ M2 ∨ P1 ∨ P2 |

Footnote: "M3 and P3 together are semantically equivalent to P1."

- Confluent = same set of outputs for all orderings of inputs. Confluence implies
  convergence; the converse does not hold. Convergent components allow cross-instance
  nondeterminism when their changing state is read, for example GETs into a replicated
  cache.
- Dynamic ordering (Paxos) prevents divergence and cross-instance nondeterminism **but not
  cross-run nondeterminism**, because the chosen order depends on arrival order. So replay
  still differs.

### 4.3 Annotations (Figs. 7, 8)

Component path annotations (C.O.W.R.):

| Severity | Label | Confluent | Stateless |
|---|---|---|---|
| 1 | CR | X | X |
| 2 | CW | X | |
| 3 | OR_gate | | X |
| 4 | OW_gate | | |

- `gate` is a set of attribute names: "the partitions of the input streams over which the
  non-confluent component operates."
- `OR*`/`OW*`: "if the programmer does not know the partitions ... each record belongs to a
  different partition." **[RESOLUTION]** Treat `*` as *unknown*: `compatible(*, K) = false`
  always. That is the only sound choice.

Stream annotations:

- **Seal_key**: the stream is punctuated on the attribute subset `key`, with "at least one
  punctuation corresponding to every stream record" (needed for progress).
- **Rep**: a boolean. The stream is replicated to more than one consumer instance with
  identical contents. In the YAML and the derivations it is attached to the consuming
  component (`Report: Rep: true`). **[RESOLUTION]** Model `Rep` as a property of a
  component: "its instances are replicas that must agree".

Stream labels (Fig. 8), columns = ND order / ND contents / transient replica divergence /
persistent replica divergence:

| S | Label | ND order | ND contents | Transient div. | Persistent div. |
|---|---|---|---|---|---|
| 0 | NDRead_gate (internal) | X | X | | |
| 0 | Taint (internal) | X | X | | |
| 1 | Seal_key | X | | | |
| 2 | Async | X | | | |
| 3 | Run | X | X | | |
| 4 | Inst | X | X | X | |
| 5 | Diverge (TR: Split) | X | X | X | X |

Async is the **default** label for any stream without an annotation.

### 4.4 The label derivation algorithm (§V-A)

**Step 0, cycles.** "To rule out infinite paths, it reduces each cycle in the graph to a
single node with a collapsed label by selecting the label of highest severity among the
cycle members."

**Step 1, order.** Start with the components whose inputs are unconnected (sources), and
process each component once all of its input stream labels are known (topological order on
the SCC-collapsed DAG).

**Step 2, inference** (Fig. 9). For each path p through a component with input label `l`
and annotation `a`, add conclusions to `Labels[out(p)]`. `Labels` also contains all input
stream labels of paths into that output interface.

```
(1)  l ∈ {Async, Run},  a = OR_gate                    ⟹  NDRead_gate
(2)  l ∈ {Async, Run},  a = OW_gate                    ⟹  Taint
(3)  l = Inst,          a ∈ {CW, OW_gate}              ⟹  Taint
(4)  l = Seal_key,      a = OW_gate, ¬compatible(gate,key) ⟹  Taint
```

Meaning:

- **Taint**: "the internal state of the component may become corrupted by unordered inputs".
- **NDRead_gate**: "the output stream may have transient nondeterministic contents".
- Rule 3: transient disagreement among replicated streams becomes permanent divergence when
  those streams modify downstream state.

**Step 3, reconciliation** (Fig. 10):

```
protected(NDRead_gate) ≡ ∀l ∈ Labels: l = NDRead_gate ∨ ∃key: l = Seal_key ∧ compatible(gate, key)

(R-a)  Taint ∈ Labels                                   ⟹ add (Rep ? Diverge : Run)
(R-b)  ∃gate: NDRead_gate ∈ Labels ∧ ¬protected(NDRead_gate) ⟹ add (Rep ? Inst : Run)
Finally return the element of Labels with highest severity.
```

**Step 4, merge.** "The labels for each output interface are merged into a single label".
That is the max by severity. The result becomes the input label of the downstream stream.

**Seal compatibility** (§V-A.1):

```
injectivefd(A, B)  ⟺  A ↦ B via some injective (distinctness-preserving) function
compatible(partition, seal) ≡ ∃ attr ⊆ partition | injectivefd(seal, attr)
```

In prose: "at least one of the attributes in gate is injectively determined by all of the
attributes in key." Example: sealing company name *Yahoo!* implicitly seals ticker `YHOO`,
because name→ticker is injective. It does **not** seal HQ city *Sunnyvale*, which is not
injective. The identity function (projection without transformation) is the ubiquitous
injective FD.

**Worked applicability example.** Given the queries in Fig. 6, an input stream sealed on
`campaign` is compatible **only** with CAMPAIGN (`group by campaign, id`). The other queries
combine results across campaigns.

#### 4.4.1 Ambiguities in the published rules, and resolutions

1. **`protected` quantifies over all of `Labels`**, which includes the (Async) label of the
   request stream on the very path that produced NDRead. Read literally, it could never hold
   in the CAMPAIGN example, yet the paper derives Async there. The prose says: "unless *all
   streams with which it can 'rendezvous'* are sealed on a compatible key."
   **[RESOLUTION]** Define `protected(NDRead_gate at path p)` as: every input stream s of
   the same component that feeds a **W path** (i.e. writes the state that p reads) has label
   `Seal_K` with `compatible(gate, K)`. The labels of p's own input and the other NDRead
   labels are excluded.
2. **Seal consumption.** In the sealed Storm derivation, the Count path `OW_{word,batch}`
   with input `Seal_batch` (no rule fires, so Labels = {Seal_batch}) outputs **Async** in
   the paper's figure, not Seal_batch. **[RESOLUTION]** A compatible seal is *consumed* by an
   O-path: the synthesized sealing protocol makes that path's output deterministic, and the
   output label is Async. Optional extension: if the path's output schema carries attributes
   K′ with injectivefd(K, K′) and the component emits per-partition results, emit
   Seal_{K′} downstream.
3. **Seal through confluent paths.** The paper preserves `Seal_batch` through `Splitter`
   (CR) with the default rule (p). That is only sound if the seal attributes survive into
   the output via an injective FD. **[RESOLUTION]** Propagate Seal_K through a C path iff
   injectivefd(K_in, K_out) holds for the attributes in the output schema. Otherwise degrade
   to Async.
4. **A seal flowing into an incompatible OR path** matches no rule: rule 1 needs
   Async/Run and rule 4 needs OW. A sealed stream is still order-nondeterministic (Fig. 8),
   so that is a soundness gap. **[RESOLUTION]** Add rule
   `(1′) l = Seal_key, a = OR_gate, ¬compatible(gate,key) ⟹ NDRead_gate`.
5. **Typos in derivations.** The POOR derivation labels the OR step "(2)" (it is rule 1)
   and writes `OR_campaign` where the annotation is `OR_id`. The arXiv figure prints the
   final label as `Split` while the text says Diverge. The TR's Storm YAML says
   `label: C` for Splitter where the text says CR. Implement from the rule tables, not from
   the derivation figures.

#### 4.4.2 Reference derivations (use as tests)

Storm wordcount. Annotations: Splitter CR; Count `OW_{word,batch}`; Commit CW.

- No seal: Async →(p, CR) Async →(rule 2, OW) {Async, Taint} →(R-a, not Rep) **Run**
  →(p, CW) **Run**. Blazes recommends a transactional (totally ordered commit) topology.
- Input `Seal_batch`: Seal_batch →(p) Seal_batch → Count: compatible({word,batch}, {batch})
  holds (batch ↦ batch is the identity), so no Taint; output **Async** → Commit CW
  **Async**. Deterministic without global coordination.

Ad reporting. Report: `click → response` is CW; the query path `request → response` is
`OR_{...}`; the component is Rep. Cache: `request→response` CR, `response→response` CW,
`request→request` CR.

- THRESH (`request→response` CR): **Async** end to end.
- POOR (`OR_{id}`), no seal: NDRead_id, unprotected, Rep ⇒ Inst. Then Inst into the Cache's
  CW path ⇒ (rule 3) Taint ⇒ Rep ⇒ **Diverge**.
- CAMPAIGN (`OR_{id,campaign}`) with click stream `Seal_campaign`: NDRead_{id,campaign} is
  protected ⇒ **Async**.
- WINDOW (`OR_{id,window}`) with click stream `Seal_window` ⇒ **Async**.
- Negative test (not in the paper, follows from the rules): CAMPAIGN with `Seal_window` ⇒
  not compatible ⇒ **Diverge**.

### 4.5 Coordination selection and synthesis (§V-B, TR §6, §8.2)

"When possible, Blazes will recognize the compatibility between sealed streams and component
semantics, synthesizing a seal-based strategy that avoids global coordination. Otherwise, it
will enforce a total order on message delivery to those components."

**Sealing strategy** (for non-confluent paths whose inputs are compatibly sealed). The
consumer must:

- **(a)** "participate in a protocol with each producer to ensure that the local
  per-producer partition is complete". Implementation: each producer, after its last record
  for partition k, sends `seal(k, digest_of_its_records_for_k)`. The consumer buffers
  records and compares the buffered records against the digest.
- **(b)** "perform a unanimous voting protocol to ensure that it has received partition data
  from each producer". The set of producers per partition came from Zookeeper, one call per
  campaign.

Additional rules:

- "When there is only one producer instance per partition, Blazes need not synthesize a
  voting protocol" ("independent seals").
- Once complete, the partition is released for processing with no further synchronization.
- The voting is "a local form of one-way coordination, limited to the 'stakeholders'
  contributing to or consuming individual stream partitions."

**Ordering strategy.** Use a totally ordered messaging service (Zookeeper for Bloom; Storm
"transactional topologies" for Storm) so that all replicas process state-modifying events
in the same order.

**Placement.** Coordination goes on the input streams of the components whose paths
produced Taint/NDRead (the "dataflow locations where adding coordination logic would achieve
deterministic outcomes", recorded by the analysis, TR §4.2).

**Design lessons** (§X):

- "replication should be placed upstream of confluent components"
- "caches should be placed downstream of confluent components"
- **coordination locality**: the number of nodes that must communicate to deterministically
  process a segment of data. It can conflict with spatial locality. Their example:
  clustering ads by id spread campaigns across ad servers and slowed sealing.

### 4.6 Integrations

**Grey box (Storm).** A reusable adapter extracts the topology. Programmers supply YAML.
Verbatim (arXiv version):

```yaml
Splitter:
  annotation:
    - { from: tweets, to: words, label: CR }
Count:
  annotation:
    - { from: words, to: counts, label: OW,
        subscript: [word, batch] }
Commit:
  annotation: { from: counts, to: db, label: CW }
```

```yaml
Cache:
  annotation:
    - { from: request, to: response, label: CR }
    - { from: response, to: response, label: CW }
    - { from: request, to: request, label: CR }
Report:
  Rep: true
  annotation:
    - { from: click, to: response, label: CW }
POOR: { from: request, to: response, label: OR,
        subscript: [id] }
THRESH: { from: request, to: response, label: CR }
WINDOW: { from: request, to: response, label: OR,
          subscript: [id, window] }
CAMPAIGN: { from: request, to: response, label: OR,
            subscript: [id, campaign] }
```

**White box (Bloom), §VII.** Modules map to components; module input/output interfaces map
to component interfaces. The module's internal rule dataflow is analyzed to derive
annotations automatically:

1. **C vs O**: by the CALM syntactic test at statement granularity. A path with no
   non-monotone operation is confluent.
2. **R vs W**: "Bloom's type system distinguishes syntactically between transient event
   streams and stored tables. A simple flow analysis automatically determines if a component
   accumulates state over time."
3. **Subscripts (gate)**: "If the Bloom statement is an aggregation (group by), the
   subscript is the set of grouping columns. If the statement is an antijoin (not in), the
   subscript is the set of columns occurring in the theta clause." The footnote example:
   `select * from R where x not in (select x from S where y = 'Yahoo!')` is deterministic
   for R tuples once (a) no more S records with y='Yahoo!' will arrive, or (b) there will
   never be a corresponding S.x.
4. **Lineage/FD chase**: query "Bloom's system catalog, which details how each rule
   application transforms (or preserves) attribute values that appear in the module's input
   interfaces". injectivefd is "sound but incomplete": the identity function and transitive
   applications of it. "Given S ≡ π_a π_ab π_abc R, S.a is injectively functionally
   determined by R.a."

**Evaluation** (EC2):

- Storm wordcount: the seal-based "nontransactional" topology reached about **1.8×** the
  throughput of the transactional one on 5 workers and **3×** on 20.
- Bloom ad reporting (10 micro ad servers, 3 medium reporting servers, 3 small Zookeeper
  nodes; 1000 log entries per server in batches of 50): the ordered strategy's processing
  time grew about 3× when ad servers doubled, and seal strategies closely tracked
  uncoordinated runs. "Independent seal" had lower latency. Non-independent seals showed a
  step-like shape because they wait for a seal from every producer.
- The uncoordinated baseline produced observably inconsistent answers across replicas.

**Known limitations** (for us to fix):

- Annotations are manual in grey-box mode.
- FD reasoning is identity-only.
- Cycles are collapsed coarsely.
- One output interface per component in the exposition.
- No lattice/morphism awareness: Bloom^L threshold paths should be C.
- The ordering fallback is a global total order and not scoped per key.
- There is no proof that the synthesized protocols are correct. Complete CALM's "properly
  coordinated variant" is the tool for that.

---

## 5. Edelweiss (VLDB 2014)

### 5.1 Setting: Event Log Exchange (ELE) and Bloom semantics

ELE: processes "accumulate and exchange immutable logs of messages or events", and "masking"
facts replace deletion. The problem is unbounded storage. Hand-written GC/checkpoint
protocols are subtle and must evolve along with the program.

Bloom recap (Tables 2, 3):

- Collections: `table` (persistent), `scratch` (recomputed each timestep, a view),
  `channel` (asynchronous; the location specifier column is prefixed `@`). Operators: `<=`
  (same timestep), `<+` (next timestep), `<-` (delete at next timestep), `<~` (async; lhs
  must be a channel; messages may be delayed, reordered, **or dropped**).
- Persistence: "A table is persistent unless it appears on the lhs of a deletion rule (<-).
  A scratch is persistent if it is defined via monotone rules over persistent collections."
- `X.notin(Y)` is monotone in X and anti-monotone in Y. X is the *positive* input and Y the
  *negative* input.
- Keys: the schema `[:k1,...] => [:v1,...]`. The key columns functionally determine the
  rest *at a given node*.

### 5.2 The Edelweiss sublanguage (§2.3)

1. Deletion rules cannot be used (`<-`).
2. Channel messages are stored persistently: the lhs of any rule reading a channel must be
   persistent.
3. Channels are derived from persistent collections: the rhs of a rule with a channel on the
   lhs must be monotone operators over persistent collections.

Consequences: nodes only accumulate knowledge; a decision to send a message is never
retracted; a received message is remembered forever. Note from the artifact page: "the
Edelweiss sub-language restrictions ... are not currently enforced" by the prototype. **We
must enforce them** (or verify them per rewrite).

### 5.3 Table 1: mechanisms (verbatim)

| Technique | Goal | Requirements | Mechanism | § |
|---|---|---|---|---|
| Avoidance of Redundant Messages (ARM) | Avoid sending duplicate messages | Receiver logic ignores duplicates | Add logic to send acks; avoid sending ack'ed messages | 3.1 |
| Positive Difference Reclamation (DR+) | Reclaim storage for X in X.notin(Y) | X, Y are persistent; logic downstream of X is reclaim-safe | Reclaim from X upon match in Y | 3.2 |
| Negative Difference Reclamation (DR−) | Reclaim storage for X and Y in X.notin(Y) | X, Y are persistent; logic downstream of X and Y are reclaim-safe; notin quals cover X's keys | Create range collection for X's keys; reclaim from X and Y upon match | 5.2 |
| Range Compression | Efficient storage of gap-free sequences | Column values contain one or more gap-free sequences; no nonkey columns | range collection type | 3.3, 4.1.2 |
| Punctuations | Bounded storage for join input collections | Join appears as input to notin; punctuation matches join predicate | sealed collection type, supplied by user, or inferred from rule semantics | 4.1.3, 4.2, 6.2 |

In the prototype code ARM is called **RCE** (Redundant Communication Elimination), and DR+/DR−
together are **RSE** (Redundant Storage Elimination).

### 5.4 ARM / RCE: exact algorithm (`bud_meta.rb: rce_rewrite`, `rce_for_channel`)

Eligible channels C:

- (i) C is the lhs of at least one `<~` rule.
- (ii) C appears on the rhs of at least one rule.
- (iii) **for every** rule with C on the rhs: the op is not `<-`, and the lhs is a
  persistent table (or a terminal, allowed for dev convenience although "technically this
  isn't safe").
- (iv) No `<~` rule into C reads a lattice (a prototype limitation).

In other words, the receiver is idempotent. The code comment calls (iii) "overly
conservative but safe": chains through scratches that all end in persistent storage should
also qualify, provided guarded async holds.

Rewrite for channel `c` with key columns K:

1. Add `range :c_approx, K` (the sender's conservative lower bound on delivered keys).
2. Add `channel :c_ack, [:@rce_sender] + K`.
3. Add the rule `c_ack <~ c {|m| [m.source_addr, m.k1, ..., m.kn]}` (the receiver acks
   every delivery).
4. Add the rule `c_approx <= c_ack.payloads`.
5. Rewrite **every** rule with `c` on the lhs to append `.notin(c_approx, 0 => :k1, 1 => :k2, ...)`
   (positional quals on the key columns).

Acks carry only the key columns, because the keys functionally determine the message at the
sender. Other ack schemes (cumulative TCP-style acks, piggybacking, gossip, tree multicast)
are all valid, since they only need to lower-bound the receiver's state.

Generated output for reliable unicast (verbatim `Unicast_rewrite_src.rb`):

```ruby
class Unicast_Rewrite
  include Bud
  state do
    channel :chn, [:id] => [:@addr, :val]
    channel :chn_ack, [:@rce_sender, :id]
    range :chn_approx, [:id]
    table :rbuf, [:id] => [:addr, :val]
    table :sbuf, [:id] => [:addr, :val]
  end
  bloom do
    (chn < sbuf.notin(chn_approx, 0 => :id).~)
    (sbuf < -(sbuf * chn_approx).lefts(0 => :id))
    chn_ack <~ chn {|c| [c.source_addr, c.id]}
    chn_approx <= (chn_ack.payloads)
    rbuf <= (chn)
  end
end
```

Input program (Fig. 1):

```ruby
class Unicast
  include Bud
  state do
    channel :chn, [:id] => [:@addr, :val]
    table :sbuf, [:id] => [:addr, :val]
    table :rbuf, sbuf.schema
  end
  bloom do
    chn  <~ sbuf
    rbuf <= chn
  end
end
```

Bloom semantics say that because sbuf is persistent, a new chn message is sent **every
timestep** until ARM suppresses it. That is correct but wasteful. The paper notes that "ARM
and DR+ are independent program rewrites, but they work together profitably: ARM introduces
set difference operations and DR+ exploits [them]".

### 5.5 DR+ (positive difference reclamation): exact conditions

Semantics: for `X.notin(Y)` with X and Y persistent, "any X tuple that has a match in Y will
never appear in the output of the notin again". Reclaim it if no *other* use of X can observe
the deletion.

Candidate collection (prototype `NotInCollector`): the "simple" form `X.notin(Y[, quals])`
where X is a collection or itself a notin chain. The "join" form is
`(A * B).<pairs|lefts|rights|outer>(preds) {tlist}.notin(Y[, quals])`. Only binary inner or
outer joins are supported, and a `notin` with a code block is not supported.

`check_neg_inner(X.notin(Y))` must satisfy all of:

- X ≠ Y (a self-negation is skipped).
- The notin has no code block.
- `can_reclaim_rel(X)` holds.
- `rel_is_inflationary(Y)` holds.

`rel_is_inflationary(Y)`:

- false if Y is on the lhs of a user `<-` rule;
- true if Y is a persistent table;
- if Y is a scratch: **every** rule defining Y must be deductive, monotone, and not in a
  code block, with an inflationary body collection (recursively). At least one defining rule
  must exist.

`can_reclaim_rel(X)` (ignoring the current rule):

- X must be a persistent table (`BudTable`, which includes `sealed`; range tables are
  excluded via `skip_reclaim`).
- For every other rule r that references X on its rhs, `is_safe_rhs_ref(X, r)` must hold:
  - false if X is referenced **inside a code block** (`in_body`);
  - false if r uses X non-monotonically, other than as a notin's negative input (for
    example `group`, `argagg`, `reduce`, or any non-whitelisted method);
  - if r **joins** X with Z: record a *seal dependency* (X can only be reclaimed once no
    future X⋈Z result can depend on it; see 5.7);
  - let D = lhs(r). False if D is deleted by a user rule. True if D is a persistent table.
    If r is `X.notin(...)` with X positive, recurse (the conjunction of RSE conditions is
    handled by intersection). True if X is the negative input of that notin, but then
    `check_neg_outer` must also pass for that rule (5.8), or X is marked unsafe globally.
    Otherwise (D is a scratch): every reader of D must be safe, recursively, and D must
    have **at least one** reader. An unread scratch is treated as an output, so X cannot be
    reclaimed (`RseNegateScratchLhsBad3`: `r2 <= t1` with r2 unread blocks reclamation of
    t1).
- **Global unsafety**: "If there is any rule that means we can't reclaim from a relation, we
  can't reclaim from that relation for other rules either."

Generated rules (before inlining by `optimize_rules`):

```
del_X_r<rule> <= (X * Y).lefts(<quals>)          # one per applicable rule; with no quals: del_X_r <= Y
rse_ready_X   <= del_X_r1                        # if only one condition
rse_ready_X   <= (del_X_r1 * del_X_r2 * ...).matches {|t0, ...| t0}   # conjunction across rules
seal_done_X_<Z> <= ...                           # chained seal dependencies, see 5.7
X <- <last table in the chain>
```

Combination semantics (from `tc_gc.rb` comments and tests):

- **Across rules: AND.** `RseNegateIntersect`: with `res1 <= t2.notin(t3); res2 <= t2.notin(t4); ...`,
  "reclaim t2 tuples when they appear in *all of* t3, t4, t5, and t6".
- **Within a chained negation: OR.** `RseChainedNeg`: in `t0 <= t2.notin(t3).notin(t4).notin(t5); t1 <= t2.notin(t6)`,
  "We can reclaim a tuple if it appears in t6 AND (t3 OR t4 OR t5)". The prototype creates
  one del table per rule, and every negation in the chain adds a union branch to it.
- **The lhs of the notin rule itself does not matter** (`RseDeleteDownstream`): given
  `Z <= X.notin(Y)`, it does not matter whether Z is persistent or deleted from.
- **Scratch lhs is fine when it is the positive side of another notin**
  (`RseNegateScratchLhs`: `r1 <= t1.notin(t2); r2 <= t1.notin(t3)` → reclaim t1 when in
  both t2 and t3).
- A negated scratch is OK if it is derived monotonically from persistent collections
  (`RseNegateScratchRhs`). It is **not OK** if it is derived via notin, from an ungrounded
  scratch, or from a channel (`RseNegateScratchRhsBad`: "the channel has a sender-side
  persistent ground, but that doesn't matter").

### 5.6 Range compression (§3.3; `collections.rb: BudRangeCompress`; `multirange.rb`)

- A `range` collection has **no non-key columns** (a compile error otherwise).
- On the first inserted tuple, the first column holding an `Integer` becomes the compressed
  column. If none exists, compression is skipped. (The authors note a smarter heuristic
  would sample several tuples.)
- Storage is a map from (all other columns) → `MultiRange`, a sorted list of disjoint
  `[lo, hi]` buckets. Insert extends or merges adjacent buckets; iteration enumerates the
  values. It is a 1-D range tree / generalized low-water mark.
- IDs must come from gap-free sequences per sender. Edelweiss uses 64-bit ids = 32-bit node
  id ∥ 32-bit local sequence number, so each sender's ids are contiguous.
- Effect: for unicast, `chn_approx` compresses to a single `[k, n]`, a **logical clock**.
  For broadcast, you get one clock per node, a **vector clock**. "ARM and range compression
  essentially 'discovers' the relationship between event histories and logical clocks."
- Range tables are never reclaimed from; they are the permanent residue.
- `seal_*` tables are also `range` collections.

### 5.7 Punctuations, seals and join reclamation (§4.1.3, §4.2, §6.2; `create_join_del_rules`, `install_join_dependency`)

"A punctuation is a guarantee that no more tuples matching a predicate will appear in a
collection." For `(X * Y).notin(Z)`: to reclaim y ∈ Y, we must know that every X tuple that
will ever join with y has already arrived, and that every current join result involving y
has a match in Z.

Seal tables, created on demand as `range` collections:

- `seal_<rel>` with schema `[:ignored]`: a whole-relation seal.
- `seal_<rel>_<col>` with schema `[col]`: "no more `rel` tuples with `col = v`".
- Users supply seals by inserting into these tables. A **`sealed`** collection
  (`BudSealed < BudTable`) raises a compile error on any `<=`, `<<` or `<+` after bootstrap.
  After bootstrap the runtime inserts `seal_<name> <= [["..."]]` automatically if that seal
  table exists.

Construction for a join-notin `(A * B).pairs(preds){tlist}.notin(Y, quals)` in rule r:

```
r<id>_A_B_joinbuf  <= (A * B * Y).combos(<join preds>, <Y cols> => <tlist cols>) {|x,y,z| x + y}
r<id>_A_B_missing  <= (A * B).pairs(<join preds>) {|x,y| x + y}.notin(r<id>_A_B_joinbuf)
# to reclaim a ∈ A (symmetric for B):
del_A_r<id> <= (A * seal_B).lefts.notin(missing, <all A cols => A_prefixed cols>)             # whole-relation seal on B
del_A_r<id> <= (A * seal_B_<bcol>).lefts(:<acol> => :<bcol>).notin(missing, ...)             # per join predicate acol=bcol
del_A_r<id> <= (A * B[_keys]).lefts(<preds on B's keys>).notin(missing, ...)                  # if join preds cover ALL of B's key columns
```

In words: reclaim a once (i) B is sealed (entirely, or on a's join value), (ii) no join
result containing a is missing from Y, or (iii) B's key is fully covered by the predicate
and a has matched, so no new B tuple can match it.

**Pull-up shortcut** (`is_not_qual_local_to_rel`, `quals_tlist_pullup`). If the notin quals
reference only columns derived from A (possibly through a join equality), the notin is
pushed above the join and a simple rule is added: `del_A_r <= (A * Y).lefts(pulled-up quals)`.
It needs no seal, because any future join output involving a would also be filtered by the
persistent Y. Both kinds of rule feed the same del table (union).

**Seal dependencies for *other* joins** (`is_safe_rhs_ref`). If X is also joined, in a rule
that is not itself an RSE target, with Z (`t1 <= (Z * X)...`), reclamation of X is chained
behind one of:

- (a) a whole-relation seal on Z;
- (b) a seal on Z's join column (`seal_Z_<col>`), matched on the predicate;
- (c) if the join is a **semijoin** from X's perspective (the tlist references only X, e.g.
  `rights`/`lefts`, or a `pairs` whose tlist only uses X: "hidden semijoin"), *any single
  matching Z tuple* suffices, because later Z tuples produce no new distinct results;
- (d) if the join predicates cover **all key columns of Z**, a single match suffices (a key
  constraint acts like a seal). `Z_keys` is used when it exists.

Multiple seal dependencies are chained: "Only need to check the next seal dependency once
this seal dependency is satisfied."

**Seal inference from `flat_map`** (`infer_flat_map_seals`). For
`lhs <= rhs.flat_map {|t| t.vals.map {|x| [t.key, x]}}` where rhs has a single key column
that is projected into lhs column i, the rule `seal_<lhs>_<col_i> <= rhs {|t| [t.key]}` is
emitted, but only if that seal table already exists. That is: "no new dependencies will be
observed for a given write" (causal KVS: `seal_dep_id <= log {|x| [x.id]}`).

Fixed vs dynamic membership:

- **Fixed membership** (`sealed :node`): DR+ reclaims log entries once every node has
  acked.
- **Dynamic membership**: reclaiming from `log` would be *unsafe*, because a new node must
  receive the whole log. Fix: epochs. Use `(node * log).pairs(:epoch => :epoch)`. A
  punctuation "no more node facts for epoch k" (from a consensus-driven membership change)
  lets DR+ reclaim epoch-k messages delivered to all epoch-k members. Symmetrically, a seal
  on `log.epoch` lets it reclaim `node` entries.

### 5.8 DR− (negative difference reclamation): exact conditions and rewrite

Applies to `X.notin(Y, :A => :B)` when X and Y are persistent tables (not ranges), A is a
key of X, and the downstream uses of X and Y are reclaim-safe. The prototype
(`check_neg_outer`) requires **the negation qual columns on X to be exactly X's key
columns**. It is also only attempted when Y is not a range; heuristically, if the negative
input is a range, range compression is assumed sufficient and DR+ is used.

The duplicate subtlety (§5.2): after matching deletion d with insertion i, discarding both
is wrong if a *duplicate* copy of i can arrive later, because i would then reappear in the
view. Hence the rewrite (prototype `do_outer_reclaim`):

1. Create `range :X_keys, keycols(X)`.
2. Add `X_keys <+ X {|r| [r.k...]}` (deferred).
3. Rewrite **every** `<=`/`<+` rule with X on the lhs to append
   `.notin(X_keys, <key quals>)` (duplicate suppression). The prototype cannot stop external
   code from inserting duplicates directly and assumes it won't.
4. Reclaim Y tuples that match a key in X_keys **and** no longer appear in X:
   `del_Y_r <= ((Y * X_keys).lefts(<inverted quals>)).notin(X, <inverted quals>)`. This
   deliberately waits until X's tuple has actually been deleted, so it takes effect one tick
   later. That guarantees "ALL the conditions for reclaiming the matching inner_rel tuple
   have been met".
5. X itself is reclaimed by the normal DR+ rule, `del_X_r <= (X * Y).lefts(quals)`.
   X_keys is never reclaimed.

Generated KVS output (verbatim excerpt of `KvsReplica_rewrite_src.rb`):

```ruby
(ins_log < -(del_ins_log_r0 * del_ins_log_r4).matches { |t0, t1| t0 })
(ins_log <= ins_chn.payloads.notin(ins_log_keys, 0 => :id))
(del_log < -(del_del_log_r1 * del_del_log_r4).matches { |t0, t1| t0 })
del_ins_log_r0 <= (ins_log * seal_node).lefts.notin(r0_node_ins_log_missing, :id => :ins_log_id, :key => :ins_log_key, :val => :ins_log_val)
del_ins_log_r4 <= (ins_log * del_log).lefts(:id => :del_id)
del_del_log_r1 <= (del_log * seal_node).lefts.notin(r1_node_del_log_missing, :id => :del_log_id, :del_id => :del_log_del_id)
del_del_log_r4 <= ((del_log * ins_log_keys).lefts(:del_id => :id)).notin(ins_log, :del_id => :id)
ins_log_keys <+ ins_log {|r| [r.id]}
r0_node_ins_log_joinbuf <= (node * ins_log * ins_chn_approx).combos(ins_chn_approx.addr => node.addr, ins_chn_approx.id => ins_log.id) {|x,y,z| x + y}
r0_node_ins_log_missing <= (node * ins_log).pairs {|x,y| x + y}.notin(r0_node_ins_log_joinbuf)
view <= (ins_log.notin(del_log, :id => :del_id))
```

Reading: an insertion is reclaimed once (a) it has been delivered to every node
(`r0`: sealed node set and no missing acks) **and** (b) it has been deleted (`r4`). A
deletion is reclaimed once (a) it has been delivered everywhere **and** (b) its target id
has been seen and is already gone from `ins_log`.

The paper's DR+/DR− duality: "DR+ is effective when the negative input to the notin can be
range compressed, whereas DR− requires that the keys of the positive input be suitable for
range compression."

### 5.9 Example programs and their inferred reclamation (all from the paper and artifact)

| Program | Input rules → rewritten rules (Table 4) | What Edelweiss reclaims |
|---|---|---|
| Reliable unicast (§3) | 2 → 5 | sbuf once acked; chn_approx range-compressed to a logical clock |
| Reliable broadcast, fixed membership (§4.1) | 2 → 8 | log entry once delivered to all (sealed) nodes; node entries once log sealed |
| Reliable broadcast, epoch-based (§4.2) | 2 → 12 | epoch-k messages once `seal_node_epoch(k)` and delivered to all epoch-k members; nodes once `seal_log_epoch` |
| Causal broadcast | 6 → 14 | log once delivered everywhere and safe |
| Request-response | 7 → 16 | read_req when acked; req_log when responded (`did_resp`); did_resp via DR− with `req_log_keys`; resp_log when acked |
| KVS (§5) | 5 → 23 | insert: delivered to all AND deleted; delete: delivered to all AND target gone (DR−) |
| Causal KVS (§6) | 19 → 62 | log once replicated everywhere and locally safe; dep once its log entry is safe; safe_dep and dom "as soon as they are produced"; safe once dominated |
| Atomic registers (§7.1) | 4 → 11 | `write` once in write_log (DR+); write_log and dom via DR− with a write-id range |
| Atomic multi-register writes (§7.2) | 4 → 17 | write once reflected in write_log (notin pushed into the join); commit needs a client seal on `write.batch` |
| Atomic snapshot reads (§7.3) | 8 → 23 | read once in `snapshot_exists`; snapshot once its batch is in `read_commit` |

Notable lessons from the paper (use as regression tests):

- **Storage leak in place of data loss.** If the program semantics forbid reclamation,
  Edelweiss leaks storage instead of losing data. It is safe by construction. Two real bugs
  came out of this:
  - KVS with deletes by *key* (not insert id): deletions can never be reclaimed, and that
    is correct in principle.
  - Causal KVS with dominance via transitive closure of dependencies: dominated writes
    cannot be reclaimed, and this is also correct in principle. The fix (the COPS-style
    assumption that a write depends on the previous write to the same key, so dominance
    uses direct dependencies) enables reclamation.
- **Network partition** (Fig. 12): storage at replica A grows during a partition, because
  writes cannot be acked, and drops immediately on heal.
- **Dominated writes** (Fig. 11): a higher update percentage means less storage.

Causal KVS input program (Figure 6, verbatim):

```ruby
state do
  table :log, [:id] => [:key, :val, :deps]
  table :safe, [:id] => [:key, :val]
  table :dep, [:id, :target]
  range :safe_keys, [:id]
  table :safe_dep, [:target, :src_key]
  table :dom, [:id]
  scratch :pending, log.schema
  scratch :missing_dep, dep.schema
  scratch :view, safe.schema
end
bloom do
  pending <= log.notin(safe_keys, :id => :id)
  dep <= log.flat_map {|l| l.deps.map {|d| [l.id, d]}}
  missing_dep <= dep.notin(safe_keys, :target => :id)
  safe <+ pending.notin(missing_dep, 0 => :id)
         .map {|p| [p.id, p.key, p.val]}
  safe_keys <= safe {|s| [s.id]}
  safe_dep <= (dep * safe).pairs(:id => :id) {|d,s| [d.target, s.key]}
  dom <+ (safe_dep * safe).lefts(:target => :id, :src_key => :key)
        {|d| [d.target]}.notin(dom, 0 => :id)
  view <= safe.notin(dom, :id => :id)
end
```

Atomic register (Figure 8, verbatim):

```ruby
class AtomicRegister
  include Bud
  state do
    table :write, [:wid] => [:name, :val]
    table :write_log, [:wid] => [:name, :val, :prev_wid]
    table :dom, [:wid]
    scratch :write_event, write.schema
    scratch :live, write_log.schema
  end
  bloom do
    write_event <= write.notin(write_log, :wid => :wid)
    write_log <+ (write_event * live).outer(:name => :name) do |e,l|
      e + [l.wid.nil? ? 0 : l.wid]
    end
    dom <= write_log {|l| [l.prev_wid]}
    live <= write_log.notin(dom, :wid => :wid)
  end
end
```

### 5.10 Edelweiss and CALM

"most of the programs in this paper use non-monotonic operators (particularly negation) and
are not confluent. We are currently exploring how to harmonize the notion of CALM
consistency with ELE". Framing (Keeping CALM §3.4): accumulate monotonically on the critical
path, and reclaim (a form of coordination: acks, "delivered to all", seals from consensus)
in the background.

### 5.11 Edelweiss limitations and future work (stated by the authors)

- Reclamation techniques are "somewhat ad hoc". There is no characterization of which
  programs are boundedly-storable, and no user feedback explaining why something cannot be
  reclaimed.
- Not extended to **lattices** ("Lattices often require a form of periodic garbage
  collection to restore efficiency; extending Edelweiss to lattices is a natural direction").
- No checkpointing or summarization. No traditional reachability GC.
- Prototype gaps:
  - binary joins only; notin code blocks unsupported;
  - range compression only on integer columns, chosen from the first tuple;
  - scratches as X are not reclaimable, and projection/selection is not supported as X;
  - "propagate RSE forward" (reclaim join partners downstream of a notin) is not done;
  - RCE requires channels to feed tables directly;
  - lattices are not allowed on the rhs of RCE'd channels.

---

## 6. Modern practice: Hydro's type-level stream properties (2026)

Hydro (`hydro_lang`, commit 9e2a120) encodes Blazes-style stream properties **in Rust
types**:

- `Stream<Type, Loc, Bound, Order = TotalOrder, Retries = ExactlyOnce>`.
  - `Bound ∈ {Bounded, Unbounded}`. Unbounded values change asynchronously.
  - `Order ∈ {TotalOrder, NoOrder}`. NoOrder: "the order of elements may be affected by
    non-determinism."
  - `Retries ∈ {ExactlyOnce, AtLeastOnce}`. AtLeastOnce: "duplicates may occur, but
    messages will not be dropped."
  - `MinOrder`/`MinRetries` compute the weaker of two properties when streams merge. This
    is the analogue of Blazes' max-severity merge.
- `fold`/`reduce` carry the bounds `C: ValidCommutativityFor<O>` and
  `Idemp: ValidIdempotenceFor<R>`. `NotProved` is only valid for `TotalOrder` or
  `ExactlyOnce`. On `NoOrder` the closure needs a `commutative = ...` proof; on
  `AtLeastOnce` it needs `idempotent = ...`. Proofs are either `manual_proof!(/** reason */)`
  or machine-checked via Verus macros (`verus_proof_commutative_fold!`, and others).
  `MonotoneProof` and `OrderPreservingProof` exist as well.
- Every nondeterministic operator (`batch`, `sample_every`, `timeout`, `assume_ordering`,
  `assume_retries`, and others) takes a `NonDet` guard created with `nondet!(/** reason */)`.
  This makes nondeterminism explicit and auditable. Its simulator can script the
  nondeterministic choices through hooks.

**Relevance.** This is the natural evolution of Blazes labels: Async ≈ `NoOrder`;
duplicates ≈ `AtLeastOnce`; seals ≈ `Bounded` batches. It turns the analysis from a
post-hoc pass into a type discipline with explicit escape hatches. Our language could expose
the same lattice of stream properties as inferred types on relations and channels (see
section 7).

---

## 7. Recommendations for bloom-remake (our design, not from the papers)

1. **One property lattice per collection/edge**, inferred over the Dedalus IR, which merges
   CIDR'11 points of order, the bud labeling lattice, and Blazes labels:
   - Order: Total ≤ Async.
   - Contents determinism: Deterministic ≤ Run ≤ Inst ≤ Diverge.
   - Boundedness: Sealed(K) / Unbounded.
   - Duplication: ExactlyOnce / AtLeastOnce.

   Rules become transfer functions on this lattice. Keep the paper's 4+2 rules as a
   verified sub-case.
2. **Syntactic monotonicity is the baseline.** Mark every operator monotone or non-monotone
   with a whitelist (bud-style). Lattice methods declare `morph`/`monotone`. Allow `nondet`
   or `assume` escape hatches with written justifications (Hydro-style). Report **points of
   order** with source spans, plus the "last async edge before the first A·N transition"
   placement hint.
3. **Guarded asynchrony** must be enforced or diagnosed. It is a Dedalus+ precondition and
   the bud `GuardedAsync` check.
4. **Seals are a runtime primitive**: a per-relation punctuation `seal(rel, key=v, digest)`,
   plus producer-set discovery (via our membership/consensus module) and the unanimous
   producer vote. Seals are needed both for Blazes sealing and for Edelweiss join
   reclamation.
5. **Coordination synthesis library.** Include Marczak's `p_done` rewrite for whole
   relations, Blazes per-partition sealing, and an ordering fallback through our own
   Paxos/Raft implementation.
6. **Verification.** For every Edelweiss rewrite, run **differential testing** of the
   rewritten program against the original under the Molly-style fault/delay schedules (a
   different cluster), and compare observable outputs. For coordinated modules, check the
   Complete-CALM "properly coordinated variant" property at the interface.
7. **FD chase.** Track attribute lineage through projections, renames and join equalities
   (identity = injective), plus key constraints. Allow user-declared injective functions.
8. **I-confluence checker** as an optional pass over declared invariants (Table 2 lookup;
   conservative for unknown pairs).

---

## 8. MUST-IMPLEMENT CHECKLIST

Each item: a precise one-line description, then the source.

**Monotonicity / CALM core**

1. **Monotone-operator classification**: select, project, join, union, intersection,
   recursion are monotone. Negation (`notin` in its negative argument), aggregation,
   deletion (`<-`/negated persistence), and any non-whitelisted function are non-monotone.
   (CIDR'11 §2; bud `rewrite.rb` `MONOTONE_WHITELIST`)
2. **Lattice-aware monotonicity**: lattice methods declared `morph` or `monotone` count as
   monotone. Threshold tests (e.g. `lmax.gt(k)` → `lbool`, `lset.size`) are monotone.
   (Bloom^L via bud `lattice-lib.rb`; Keep CALM & CRDT On §3)
3. **Point-of-order report**: every non-monotone edge, and every edge incident to a temporal
   cluster (an SCC with both a non-monotone and a temporal edge), is a point of order.
   Report it with its source location. (CIDR'11 §4.4)
4. **Path labeling with collapse A·N = D**, plus per-sink disjunction (D dominates;
   N∨A = D). (bud `labeling.rb`)
5. **Guarded asynchrony check**: two distinct channels meeting at a collection are safe only
   if both paths pass through persistent storage or a lattice. Otherwise report divergence.
   (bud `GuardedAsync`; Marczak Dedalus+ "guarded asynchrony")
6. **Coordination placement hint**: for each path that becomes D, find the last async edge
   before the first D transition. (bud `meta_algebra.rb` `d_begins`/`a_preds`)
7. **Stratified negation with the coordination rewrite**: add `p_done()` guards to negated
   atoms. `p_done` is computed over the collapsed PDG with acks for async rules and two-round
   min-id voting for async-recursive SCCs. (Marczak et al. Datalog 2.0 2012 §4.2)
8. **Sealing invariant**: once `p_done` is true it stays true, and p never changes
   afterwards. Assert this at runtime in debug builds. (Marczak Lemma 5)
9. **Dedalus+ recognizer**: a semipositive program with guarded asynchrony is confluent.
   Certify it as coordination-free. (Marczak Thm 1)
10. **Membership / self-id awareness**: treat negation or aggregation whose completeness
    depends on `node`/`All` (e.g. "votes from all nodes") as coordination. Only programs
    using `Id` can compute non-monotone queries. (Ameloot et al. Thm 16, Cor 17; Keeping
    CALM §2.2)
11. **Policy-aware negation (optional/advanced)**: expose the partitioning function so a
    node can decide local absence for facts routed to it. This enables
    domain-distinct-monotone operators like `R \ S` without global coordination. (Zinn 2012;
    Ameloot survey §3.1)
12. **Early/final output for threshold queries**: emit final (never-retracted) answers for
    monotone boolean thresholds. Never do so for non-monotone queries. (Free termination
    2025; Keep CALM & CRDT On)

**Blazes**

13. **Component path annotations** CR, CW, OR_gate, OW_gate, OR*/OW*, with severities 1–4.
    (Blazes Fig. 7)
14. **Stream annotations** Seal_key (at least one punctuation per record's key) and Rep.
    Async is the default. (Blazes §IV-A.2)
15. **Stream labels and severities**: NDRead_gate(0), Taint(0), Seal_key(1), Async(2),
    Run(3), Inst(4), Diverge(5). (Blazes Fig. 8)
16. **SCC collapse**: each cycle becomes one node whose label/annotation is the max severity
    of its members. (Blazes §V-A)
17. **Inference rules 1–4** exactly as in Fig. 9, plus the soundness patch (1′) for Seal
    into an incompatible OR. (Blazes Fig. 9; §4.4.1 here)
18. **Reconciliation**: Taint ⇒ Rep?Diverge:Run; an unprotected NDRead ⇒ Rep?Inst:Run;
    return the max severity. (Blazes Fig. 10)
19. **`protected` over rendezvous (state-writing) input streams** with compatible seals.
    (Blazes §V-A.3 prose; §4.4.1 here)
20. **`compatible(gate, key)` ≡ ∃attr ∈ gate: injectivefd(key, attr)**, with `*` never
    compatible. (Blazes §V-A.1)
21. **Injective-FD chase over attribute lineage** (identity projections, renames, join
    equalities; S ≡ π_a π_ab π_abc R ⇒ injectivefd(R.a, S.a)). (Blazes §VII-B.2)
22. **White-box annotation extraction**: C iff no non-monotone op on the path; W iff the
    path reaches persistent state; gate = group-by columns, or antijoin theta columns,
    traced back to input interface attributes. (Blazes §VII-B)
23. **Grey-box annotation file** (YAML-like `{from, to, label, subscript}` per path;
    `Rep: true`) for foreign or opaque components. (Blazes §VI)
24. **Sealing protocol**: per-producer seal messages carry a digest of that producer's
    partition. The consumer compares buffered records against the digests, runs a unanimous
    vote over the partition's producer set (from membership/consensus), and skips the vote
    when there is a single producer. It then releases the partition. (Blazes §V-B.1; TR
    §8.2)
25. **Ordering fallback**: route state-modifying inputs of tainted replicated components
    through a total-order service (our Paxos/Raft). (Blazes §V-B.2)
26. **Output-label report per sink**, including where coordination was inserted and why.
    (Blazes §V)

**Edelweiss**

27. **Edelweiss sublanguage checker**: no user deletions; channel receivers persist; channel
    senders are monotone over persistent state. (Edelweiss §2.3)
28. **Persistence inference**: a table is persistent unless deleted from. A scratch is
    persistent iff it is defined by monotone rules over persistent collections. (Edelweiss
    §2.2.1; `rel_is_inflationary`)
29. **ARM/RCE rewrite**: an ack channel keyed on the channel's key columns, a range
    `*_approx`, and `.notin(approx)` appended to every sender rule. Eligibility requires
    every receiver rule to derive into persistent storage with no `<-`. (Edelweiss §3.1;
    `rce_rewrite`)
30. **DR+ rewrite** with the full safety analysis: `can_reclaim_rel`, `is_safe_rhs_ref`
    (no in-block refs, no non-monotone refs other than as a negative input, persistent or
    safely-consumed lhs, an unread scratch counts as output), `rel_is_inflationary(Y)`, and
    global unsafety. (Edelweiss §3.2; `bud_meta.rb`)
31. **Condition combination**: AND across rules (intersection of per-rule del tables); OR
    within a chained `notin(...).notin(...)`. (tc_gc `RseNegateIntersect`, `RseChainedNeg`)
32. **Join reclamation** via joinbuf/missing buffers, with whole-relation seals, per-join-
    column seals, key-covering joins, and semijoin shortcuts. Also the notin pull-up when
    quals are local to one join input. (Edelweiss §4.1.3, §4.2; `create_join_del_rules`,
    `install_join_dependency`)
33. **DR− rewrite**: X_keys range, deferred key copy, duplicate-suppressing negation on all
    X-inserting rules, and reclaiming Y only after X's tuple is gone. It requires the quals
    to equal X's keys. (Edelweiss §5.2; `do_outer_reclaim`, `check_neg_outer`)
34. **Range collection type**: all-key schema; one integer column compressed into sorted
    disjoint [lo,hi] buckets per value of the other columns. Never reclaimed. (Edelweiss
    §3.3; `BudRangeCompress`, `MultiRange`)
35. **Gap-free id generation** (node-id ∥ local seq) so ack sets compress to clocks.
    (Edelweiss §4.1 fn. 3)
36. **`sealed` collection type**: immutable after bootstrap (writes are compile/runtime
    errors), with an automatic whole-relation seal emitted after bootstrap. (Edelweiss
    §4.1.3; `BudSealed`)
37. **Seal tables** `seal_<rel>` / `seal_<rel>_<col>` as range collections that users or
    consensus can insert into. Epoch punctuations for dynamic membership. (Edelweiss §4.2)
38. **flat_map seal inference**: a single-key parent projected into a child column implies
    a seal on that child column when the parent tuple is seen. (`infer_flat_map_seals`;
    Edelweiss §6.2)
39. **Reclamation must be semantics-preserving**: deletions introduced by the rewrite are
    exempt from "is deleted" checks, and differential tests are required. The failure mode
    must be a leak, never data loss. (Edelweiss §8.2)

**Related analyses**

40. **I-confluence pairwise checker** using Table 2 (invariant × operation). Unknown pairs
    are non-I-confluent. (Bailis et al. §5)
41. **Complete-CALM interface check (stretch)**: for modules that use coordination
    internally, check that the exposed outcome specification is monotone under its declared
    refinement order. (Complete CALM Def. 8, 11)

---

## 9. TEST PROGRAMS

Each entry: program, source, expected behavior. "Analysis" means the static pass; "Runtime"
means execution under randomized delay, reorder, duplication and drop (Molly-style harness).

### 9.1 CALM / monotonicity

1. **Distributed transitive closure** (Ameloot Ex. 3, flooding plus TC).
   - Analysis: monotone, coordination-free.
   - Runtime: identical final output for every placement and delivery order.
   - Coordination-free witness: all data at every node reaches quiescence with heartbeats
     only.
2. **Distributed deadlock detection** (Keeping CALM §1.3.1): cycles in a partitioned
   waits-for graph.
   - Analysis: monotone. Runtime: every detected cycle is a true cycle and the final set is
     the same everywhere.
3. **Distributed garbage collection** (Keeping CALM §1.3.2; Marczak App. B, **[NOT ACCESSED]**
   full code).
   - Analysis: point of order at the negation of reachability.
   - Uncoordinated runtime: some schedule declares a reachable object garbage.
   - With the `p_done` rewrite: always correct.
4. **Emptiness query** (Ameloot Ex. 10).
   - Analysis: non-monotone, needs coordination via All/Id.
   - Runtime: correct only with the vote-based protocol.
5. **Asynchronous marriage ceremony** (Marczak Ex. 3, verbatim in §2.5).
   - Analysis: diffluent (two ultimate models: with and without `runaway()`).
   - The Example 4 rewrite, `i_dont(X)@async ← ¬i_do_edb(X). ...`, is Dedalus+ and gives a
     unique model.
   - As a Dedalus^S program with the coordination rewrite, its unique model equals the
     stratified model.
6. **Toggle program** (Declarative Imperative §3.4.4):
   `state(X)@next :- state(X), !del_state(X). state(1)@next :- !state(X). del_state(X) :- state(X).`
   - Stratifiable only because of `@next`. It must be accepted, and `state` alternates.
   - Without `@next` it must be rejected as unstratifiable.
7. **`R \ S`** with a policy-aware placement (Zinn/Ameloot survey).
   - Analysis (optional feature): domain-distinct-monotone.
   - Runtime: correct without global coordination when S is placed by the policy.
8. **Complement of transitive closure**: domain-disjoint-monotone and not
   domain-distinct-monotone. It needs domain-guided placement.
9. **Path of length 1 but not 2** (Zinn Remark 3.3): stratified, not semi-positive, not
   adom-monotone. Negative test for any "SP ⇒ coordination-free under N1" optimization.
10. **Threshold queries** (Keep CALM & CRDT On Ex. 2 and the 2P-set rate limit):
    - `|{txn: GIFTCARD ∧ amount>100}| > 50` and `|A|+|R| > 100` may return final `true`
      early.
    - `A − R` (potato/Ferrari) must be flagged non-monotone.

### 9.2 Bloom-level analysis oracle (port verbatim from bud)

11. The `tc_labeling.rb` suite:
    - `TestNM` → `{"response"=>"D"}`, paths `{"i1"=>"A","i2"=>"D"}`
    - `TestGroup` → `D`
    - `TestMono` → `A`, `A`
    - `TestDeletion` → `dguard→response` and `i2→response` are `D`
    - `TestNestMod` → `D`
    - `BugButt` and `HalfGuard` → unguarded races (`guarded=false`)
    - `FullGuard` → guarded (`true`)
12. **Shopping carts** (bud-sandbox `cart/`):
    - `destructive_cart.rb` (KVS read-modify-write per action): points of order on every
      action.
    - `disorderly_cart.rb`: points of order only at checkout (the group/accum after
      `checkout_msg`).
    - `ReplicatedDisorderlyCart`: same, plus multicast replication.
    - `monotone_cart.rb` + `cart_lattice.rb` (the `lcart` lattice with `is_complete`/`summary`
      monotone and a checkout manifest `lbound..op_id`): fully confluent. The runtime must
      raise an error when two checkouts merge into one cart.

### 9.3 Blazes

13. **Storm wordcount**: Splitter CR; Count OW_{word,batch}; Commit CW.
    - No seal ⇒ sink label **Run**, ordering inserted.
    - `Seal_batch` ⇒ **Async**, no global coordination; the deterministic output must match
      the ordered version.
    - Performance sanity: sealing beats ordering (the paper saw 1.8× at 5 workers and 3× at
      20).
14. **Ad reporting** (Bloom, ~125 LOC in the paper; rebuild it in our language). Report
    (Rep) with click CW and query OR_*; Cache with CR/CW paths and a self-edge.
    - THRESH ⇒ Async.
    - POOR ⇒ Diverge. The runtime shows replicas disagreeing without coordination and
      agreeing with ordering.
    - CAMPAIGN + Seal_campaign ⇒ Async via the sealing protocol.
    - WINDOW + Seal_window ⇒ Async.
    - CAMPAIGN + Seal_window ⇒ Diverge (negative test).
    - Independent-seal vs shared-seal topologies: the shared case must wait for every
      producer's seal.
15. **FD chase unit tests**:
    - S ≡ π_a π_ab π_abc R ⇒ injectivefd(R.a, S.a).
    - A computed column f(a) is not injective unless declared.
    - Company→ticker declared injective seals ticker; company→city does not.
16. **Antijoin subscript** (Blazes fn. 5): `R.notin(S where y='Yahoo!', x)` gets gate {x}.
    It is deterministic once S is sealed on y='Yahoo!', or on x.
17. **Cycle collapse**: a component in a cycle with an OW member gives the whole SCC OW
    severity. Cache's self-edge must not create a Cache↔Report cycle (Blazes fn. 3).

### 9.4 Edelweiss (port all ten artifact programs plus the bud-gc unit tests)

For each program, run the original and the rewritten program under the same randomized
network schedules. **Observable outputs must be identical, and storage must plateau.**

18. **Reliable unicast** (Fig. 1): after delivery, `sbuf` is empty and `chn_approx` holds one
    range. Messages stop being re-sent.
19. **Reliable broadcast, fixed membership** (`sealed :node`): log entries are reclaimed
    only after all nodes ack. With one node partitioned, nothing is reclaimed until it heals.
20. **Reliable broadcast, epoch membership**: no reclamation until `seal_node_epoch(k)`.
    Then epoch-k entries delivered to all members are reclaimed. Adding a node to a new
    epoch never loses messages destined for it.
21. **Causal broadcast**: log entries are reclaimed once delivered everywhere and safe.
22. **Request-response**: `did_resp` is reclaimed via DR−, and duplicate requests after
    reclamation must *not* produce a second response.
23. **KVS**: an insertion is reclaimed after delivery-to-all and deletion. A duplicate
    re-delivery of a reclaimed insertion must not resurrect it (DR− duplicate suppression
    via `ins_log_keys`).
    - Negative variant: deletes by *key* ⇒ deletions are never reclaimed (expected leak).
24. **Causal KVS**: storage stays flat under 100% dominating updates. It grows during a
    partition and drops on heal (Figs. 11, 12).
    - Negative variant: dominance by transitive closure ⇒ dominated writes are never
      reclaimed.
25. **Atomic registers** (single, multi-key batch writes with a client seal on
    `write.batch`, snapshot reads). Reads must match a serial order. Snapshots are reclaimed
    after `read_commit`.
26. **bud-gc `tc_gc.rb` unit programs** (port each with its assertions):
    - `RseSimple`: reclaim `[5,10]` after `res_approx` gets it; `sbuf` = `[[6,12]]`.
    - `RseQual`: after `sbuf_val_seen <+ [[5]]`, `sbuf` = `{[3,6]}`.
    - `RseChainedNeg` and `RseNegateIntersect`: the AND/OR semantics from 5.5.
    - `RseNegateIntersectDelete`: no reclamation when one negated input is user-deleted.
    - `RseDeleteDownstream`: reclamation allowed.
    - `RseNegateScratchLhs`: allowed.
    - `RseNegateScratchLhsBad`, `Bad2`, `Bad3`: no reclamation.
    - `RseNegateScratchRhs`: allowed.
    - `RseNegateScratchRhsBad`: no reclamation.
    - `RseRhsRef`: `t1` = `[[1,1]]` after 2 ticks.
    - `RseRhsRefBad`: t1, t4, t7, t11, t13 all retain `[[1,1],[2,2]]`.
    - The `JoinRse*` family, `JoinReclaim*` (with seal, reordered, multiple preds,
      intersect seal, semijoin, lefts semijoin, hidden semijoin, with join, on keys, on
      multiple keys), `SealedCollection`, `ReliableBroadcast` space test, `SimpleCausal*`,
      `CausalDomRights`, `ImpliedSemiJoin`, `JoinOnKeys*`, `FlatMapImpliedSeal*`.
27. **Range compression**: the ids 1..4, 8 are stored as buckets `[1-4]`, `[8]`. Inserting
    5–7 merges them into `[1-8]`. A non-integer column disables compression. Declaring a
    non-key column on a range is a compile error.

### 9.5 Invariant confluence

28. The Table 2 pairs as unit tests: uniqueness + choose-specific ⇒ non-I-C; FK +
    cascading delete ⇒ I-C; counter `>` + decrement ⇒ non-I-C; and so on. Add the payroll
    example from Bailis §2: removing users under a unique-id invariant is I-C; inserting two
    users with the same id (`{Stan:5}` ⊔ `{Mary:5}`) is not.

---

## 10. Open questions this report could not settle

- The exact Datalog fragments capturing domain-distinct and domain-disjoint monotonicity
  (TODS 2016). **[NOT ACCESSED]**
- Marczak et al. Appendix D (ack/done rules for non-recursive async edges) and Appendix B
  (distributed GC program). These are in their TR, which I did not locate.
  **[NOT ACCESSED]**
- Blazes' treatment of multiple output interfaces and of seals propagating through
  order-sensitive components is under-specified. See the resolutions in 4.4.1.
- Baccaert & Ketsman, *A Generalized CALM Theorem for Non-Deterministic Computation in
  Asynchronous Distributed Systems* (Information Systems 138, 2026), is cited by Complete
  CALM. **[NOT ACCESSED]**
