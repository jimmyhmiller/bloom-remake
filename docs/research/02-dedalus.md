# 02 — Dedalus: Datalog in Time and Space, and its formal semantics

Research cluster 02 for **bloom-remake**. Audience: the people implementing the Rust engine, the
language front-end, the analyses (stratification, safety, CALM/confluence), and the
Molly-style fault-injection tooling.

Conventions used in this report:

* **[TR]**, **[DL11]**, etc. are source tags; the full list with URLs is in §0.
* Code blocks marked *verbatim* are copied from the cited source. Where the PDF text extraction
  garbled a symbol (for example `≠` rendered as `6=` or `�=`), I restored the obvious symbol and say so.
* Anything marked **(analysis)** is my own derivation or recommendation. It is not a claim made by the papers.
* In verbatim excerpts from [DL11] and [MAR], multi-word identifiers appear with spaces (`p pos`, `i do`,
  `refers to`, `m priority queue`, `start round 1i`). The PDFs typeset underscores and subscripts, and
  the extraction lost them. Read them as `p_pos`, `i_do`, `refers_to`, `m_priority_queue`,
  `start_round_1_i`.

---

## 0. Sources

### 0.1 Read (full text or the sections listed)

| Tag | Source | What I read |
|---|---|---|
| [TR] | Alvaro, Marczak, Conway, Hellerstein, Maier, Sears. *Dedalus: Datalog in Time and Space.* UC Berkeley Tech. Rep. UCB/EECS-2009-173, Dec 16 2009. https://www2.eecs.berkeley.edu/Pubs/TechRpts/2009/EECS-2009-173.pdf | Full text |
| [DL11] | Same authors, *Datalog Reloaded* (Datalog 2010 workshop), LNCS 6702, pp. 262–281, 2011. https://dsf.berkeley.edu/papers/datalog2011-dedalus.pdf (byte-identical to https://www.neilconway.org/docs/dedalus_dl2.pdf) | Full text |
| [DI] | Hellerstein. *The Declarative Imperative: Experiences and Conjectures in Distributed Logic.* SIGMOD Record 39(1), 2010 / UCB/EECS-2010-90. https://www2.eecs.berkeley.edu/Pubs/TechRpts/2010/EECS-2010-90.pdf | §2–4 |
| [DS-TR] | Alvaro, Ameloot, Hellerstein, Marczak, Van den Bussche. *A Declarative Semantics for Dedalus.* UCB/EECS-2011-120, Nov 2011. https://www2.eecs.berkeley.edu/Pubs/TechRpts/2011/EECS-2011-120.pdf | Main body; appendix proofs not read |
| [TPLP] | Ameloot, Van den Bussche, Marczak, Alvaro, Hellerstein. *Putting Logic-Based Distributed Systems on Stable Grounds.* Theory and Practice of Logic Programming, 2016 (accepted July 2015). Preprint: https://vdbuss.github.io/dedalus.pdf | §3–6; proofs skimmed; online appendix not read |
| [MAR] | Marczak, Alvaro, Conway, Hellerstein, Maier. *Confluence Analysis for Distributed Programs: A Model-Theoretic Approach.* Datalog 2.0 (2012), LNCS 7494. TR UCB/EECS-2012-171: https://www.neilconway.org/docs/dedalus_confluence_tr.pdf | Full text including appendices A–M |
| [CRON] | Ameloot, Van den Bussche. *Positive Dedalus programs tolerate non-causality.* JCSS 80(7):1191–1213, 2014. https://vdbuss.github.io/cron_full.pdf | §1–7 |
| [RT] | Ameloot, Neven, Van den Bussche. *Relational transducers for declarative networking.* JACM 60(2), 2013 (PODS 2011). https://vdbuss.github.io/rtdn_jacm.pdf (also arXiv:1012.2858) | §2–5 |
| [SURV] | Ameloot. *Declarative networking: recent theoretical work on coordination, correctness, and declarative semantics.* SIGMOD Record 43(2), 2014. https://databasetheory.org/sites/default/files/2016-06/ameloot.pdf | Full |
| [BSP] | Interlandi, Tanca. *On the CALM Principle for BSP Computation.* AMW 2015. https://ceur-ws.org/Vol-1378/AMW_2015_paper_27.pdf ; extended version arXiv:1405.7264v3 (2017), published in TPLP 2018 as *A Datalog-based computational model for coordination-free, data-parallel systems* | AMW full; arXiv v3 theorem sections |
| [SYNC] | Interlandi, Tanca, Bergamaschi. *Datalog in Time and Space, Synchronously.* AMW 2013. https://ceur-ws.org/Vol-1087/paper2.pdf | Full |
| [LDFI] | Alvaro, Rosen, Hellerstein. *Lineage-driven Fault Injection.* SIGMOD 2015. https://people.ucsc.edu/~palvaro/molly.pdf | §2–4, Appendix B |
| [MOLLY-SRC] | https://github.com/palvaro/molly, commit `a3a6d79` (2018-11-04): `DedalusParser.scala`, `ast/AST.scala`, `DedalusRewrites.scala`, `FailureSpec.scala`, `DedalusTyper.scala`, `Harness.scala`, `tutorial.md`, `src/test/resources/examples_ft/**/*.ded` | Read |
| [THESIS] | Alvaro. *Data-centric Programming for Distributed Systems.* PhD thesis, UCB/EECS-2015-242. https://www2.eecs.berkeley.edu/Pubs/TechRpts/2015/EECS-2015-242.pdf | Ch. 3 §3.1–3.2 |
| [KCALM] | Hellerstein, Alvaro. *Keeping CALM: When Distributed Consistency is Easy.* CACM 63(9), 2020. arXiv:1901.01930 | §2–4 |
| [QRW] | Chu et al. *Optimizing Distributed Protocols with Query Rewrites.* PACMMOD / SIGMOD 2024. arXiv:2404.01593 | §2–4 |
| [HFD] | Hydroflow Datalog front-end, tag `hydroflow_datalog_core-v0.9.0` of https://github.com/hydro-project/hydro (`hydroflow_datalog_core/src/grammar.rs`, `lib.rs`). DFIR docs at HEAD: `docs/docs/dfir/concepts/life_and_times.md`, `distributed_time.md`, `dfir_lang/src/graph/ops/defer_tick.rs` | Read |
| [WM] | Zinn, Green, Ludäscher. *Win-Move is Coordination-Free (Sometimes).* ICDT 2012. arXiv:1312.2919 | Skimmed |
| [FT] | Power, Koutris, Hellerstein. *The Free Termination Property of Queries Over Time.* arXiv:2502.00222 (2025) | Abstract, intro, Thm 36 |
| [CCALM] | Hellerstein. *Complete CALM: A Coordination Criterion for Specifications.* arXiv:2602.09435 (v4, June 2026) | Abstract only |
| [BUD] | Bud cheat sheet: https://github.com/bloom-lang/bud/blob/master/docs/cheat.md | Read through a summarizing fetch, so exact wording is not guaranteed |

### 0.2 Not accessed or not read in full

* **Ameloot, Ketsman, Neven, Zinn, *Weaker Forms of Monotonicity for Declarative Networking*
  (PODS 2014 / TODS 40(4) 2016).** I found no open PDF. §10.2 is based only on [SURV], [FT] and [WM].
* Greco & Zaniolo (1998) and Saccà & Zaniolo (PODS 1990) on the `choice` construct: not read
  directly. I describe choice the way the Dedalus papers describe and use it.
* Chomicki & Imieliński (PODS 1988, Datalog1S), Baudinet/Chomicki/Wolper, Ross 1990 (modular
  stratification), Przymusinski 1988 (perfect models): not re-read. I use their standard definitions.
* Lobo et al. 2012 and Ma et al. 2013 (ASP semantics for Dedalus-like languages): not read.
* The TPLP online appendix with full proofs, and the ICDT 2012 paper by Ameloot & Van den Bussche
  on deciding eventual consistency (known only through [SURV]).

---

## 1. Executive summary

1. **Dedalus is Datalog¬ with time reified as data.** Each predicate has a *time suffix*
   (a timestamp attribute) and, in the distributed version, a *location specifier*. There are exactly
   three rule kinds: **deductive** (same timestep), **inductive** (`@next`, successor timestep, same node)
   and **asynchronous** (`@async`, nondeterministically chosen timestep, possibly another node) [TR §2, §6; DL11 §2, §5].
2. **Nothing persists by default.** Every fact holds for one timestep only. Persistence, deletion,
   update, sequences and queues are all written as inductive rules: the "frame rule"
   `p(X)@next :- p(X), notin p_neg(X)` [TR §3; DL11 §3]. An engine has to recognize these rules and
   implement them as storage plus deltas instead of re-deriving every fact on every step
   [TR §5 Alg. 1; DI footnote 9; HFD `.persist`].
3. **Negation is safe across time but not within a timestep.** Only the *deductive reduction* (the
   deductive rules alone) must be syntactically stratified. Cycles through negation are allowed when
   they pass through an `@next` or `@async` edge ("temporal stratification"). A temporally stratified
   Dedalus0 program is locally stratified and has a unique perfect model [DL11 §4.1].
4. **Async = choice over arrival time.** An `@async` rule is sugar for `time(S), choose((head vars, T[, body loc]), (S))`
   [TR §6.3; DL11 §5.3]. In the original papers `S` may also be ⊤ ("never", a lost message) and may be *earlier*
   than the send time. The later formal semantics add **causality** (a message never arrives in the sender's
   causal past), **finiteness** (finitely many messages arrive at any step) and **fairness**
   (every message eventually arrives, no loss) [TPLP §4.3–4.5, §5.1.4].
5. **The formal meaning of a program is a set of stable models,** one per admissible assignment of
   arrival timestamps. It provably coincides with the traces of fair runs of a standard operational
   transition-system semantics (TPLP Theorem 4). That operational semantics is the reference
   interpreter we should build: per node and per step, compute the deductive fixpoint, then fire
   inductive rules (next state) and async rules (outbox) *once* each on that fixpoint [TPLP §5.1].
6. **Output means ultimate facts:** facts that are *eventually always true*. A program is
   **confluent** if every stable model has the same ultimate model [MAR §2.2, §3.1]. Confluence is
   undecidable for full Dedalus [MAR Lemma 1]. Full Dedalus captures the While queries and has PSPACE
   data complexity for consistent programs [CRON §4]. With entanglement it simulates Turing machines
   [DS-TR footnote 1].
7. **Decidable, confluent sublanguages:** **Dedalus⁺** (semipositive, i.e. negation only on EDB, plus
   *guarded asynchrony*, i.e. every async head relation is persisted) is confluent and captures PTIME.
   **Dedalus_S** allows stratified negation across async edges. Its semantics is stratum-by-stratum
   ultimate models, implemented by a synthesized **coordination rewrite** (ack counting plus two-round
   voting that computes `p_done()` "sealing" predicates) [MAR §3.2, §4].
8. **Monotone is not enough without persistence.** A positive program whose message relations are not
   persisted can still be order-sensitive: `T() :- A(), B()` fires only if A and B *arrive at the same step*
   [CRON Fig. 5; MAR guarded asynchrony]. Our CALM analysis must treat ephemeral (non-persisted) relations
   fed by async rules as a source of order-sensitivity.
9. **CRON:** *positive, consistent* Dedalus programs still compute the right answer when messages are
   delivered "into the past" (non-causally). The purely semantic form of the CRON conjecture is false in
   both directions [CRON Thm 6.1, §6.1]. Practical consequence: during crash recovery, a positive program
   can replay its message log all at once.
10. **CALM formally ([RT]):** in the relational-transducer model, *coordination-free* ⟺ *monotone*
    ⟺ *computable by an oblivious transducer* (one that never reads `Id`/`All`). With knowledge of the
    distribution policy, strictly more queries become coordination-free (domain-distinct-monotone and
    domain-disjoint-monotone) [SURV §3.1; FT Thm 36]. Under synchronous BSP, CALM holds for
    *monotone + connected* queries [BSP Thm 1].
11. **Molly runs Dedalus programs in a *synchronous*, bounded simulation.** An EDB relation
    `clock(From, To, SndTime, RcvTime)` drives every non-deductive rule. Message loss deletes a clock
    fact (receive time "NEVER"); crashes delete a node's clock facts from some time onward. Rules are
    rewritten to join `clock` and to record provenance ("firings" relations). `pre`/`post` relations
    express invariants [LDFI §2, §4; MOLLY-SRC].
12. **Modern uses** keep the same core: [QRW] rewrites Dedalus programs (decoupling, partitioning)
    and compiles them to Hydroflow. The Hydroflow Datalog front-end uses `:-` (same tick), `:+`
    (next tick), `:~` (async to `@node`), `.persist`, and aggregates `min max sum count choose collect_vec index()` [HFD].

---

## 2. The versions of Dedalus and how they differ

The papers are not fully consistent with each other. The differences below matter for implementation.

| Aspect | [TR] 2009 | [DL11] 2011 | [MAR] 2012 / [THESIS] | [DS-TR] 2011 / [TPLP] 2016 | [LDFI]/[MOLLY-SRC] | [QRW] 2024 |
|---|---|---|---|---|---|---|
| Time domain | ℤ ("no specific interpretation beyond successor") | ℤ | ℕ | ℕ, starting at 0 | integers 1..EOT | ℕ |
| Position of time attr. | last ("time suffix") | last | 2nd column (after location) in [MAR]; last in [THESIS] | 2nd column: `R(x, s, ā)` | appended last by the rewrite | last; location second to last |
| Location specifier | `#` prefix, any attribute | `#` prefix | first column | first column, implicit in sugar | first column, by convention | second-to-last column |
| Async arrival | chosen from `time` including ⊤ (never); may precede send time | same | choice + "causality rewrite", never before send | choice + causality + finiteness; **no loss** (fair runs) | synchronous: always t+1, or NEVER if dropped | `delay(...)` builtin, constrained t < t′ |
| FD in `choose` | head vars + body time T | head vars + T, and body location for communication rules | body loc, body time, *all body vars* → S | body loc x, body time s, dest y, head tuple v̄ → t | n/a | tuple distinguishing each head fact → t′ |
| EDB | timestamped facts; "guarded EDB" via `r_pos` | same, `r(...)@C` fact sugar | EDB facts hold at **every** timestep | input facts preserved at every step | facts given `@t`, must be persisted by rules | "input" relations via async channels |
| Entanglement | allowed | allowed | forbidden | forbidden (footnote: gives Turing machines) | no syntax for it | n/a |
| Aggregates | heads of deductive rules | same | none in the formalism | none | anywhere (C4 evaluator) | anywhere in heads |
| Deductive stratification | required for unique model (temporal stratification) | same | required | required (Def 2.1 / 4.1) | relies on C4 | assumed |

**Terminology drift.** "Minimal model" [TR] became "perfect model" [DL11]. "Modularly stratified"
[TR §4.1] became "locally stratified" [DL11 §4.1]. **Treat [DL11] as the authoritative statement of the
Dedalus0 results** and the TR as an early draft. The TR's Lemma 2 proof appeals to modular stratification
over time. DL11 replaces it with "every temporally stratifiable Dedalus0 instance is locally stratifiable,
and thus has a unique perfect model".

---

## 3. Dedalus0: the timestep sublanguage (no asynchrony)

### 3.1 Data model and syntactic restrictions [TR §2.1; DL11 §2.1]

* Countably infinite universe of constants C and of variables A. Time is modeled by an infinite relation
  `successor` isomorphic to the successor relation on the integers: `successor(x,y)` iff `y = x+1`.
* **Schema:** the final attribute of every Dedalus0 predicate ranges over ℤ. It is the **time suffix**.
* **Time suffix rule:** in a well-formed rule every body subgoal uses *the same* existential variable `T`
  as its time suffix. The head's time suffix `S` must be constrained in exactly one of two ways:
  1. **deductive:** the body contains `S = T`;
  2. **inductive:** the body contains `successor(T, S)`.
* **Positive and negative predicates:** for every extensional predicate `r`, the program gets two
  distinguished predicates `r_pos` and `r_neg` with the same schema, plus the rule
  `r_pos(A1..An, S) ← r(A1..An, T), S = T;`. `r_pos` holds at least the contents of `r`. `r_pos` and
  `r_neg` may appear freely in heads and bodies.
* **Guarded EDB:** no well-formed rule may mention an extensional predicate except the `r_pos` rule above.
  EDB facts therefore enter only through `r_pos`, which lets other rules add derived `r_pos` facts.

Unsugared examples, *verbatim* [DL11 Ex. 1]:

```
deductive: p(A, B, S) ← e(A, B, T ), S = T ;
inductive: q(A, B, S) ← e(A, B, T ), successor(T , S);
```

Consequences stated in [TR §3]: rules cannot join across timesteps, since there is one time variable
per body. Rules cannot derive "backwards in time" or "skip into the future" in Dedalus0.

### 3.2 Sugar [TR §2.2; DL11 §2.2]

* Body time suffixes may be omitted, since they are all the same implicit `T`.
* The head carries no time suffix for deductive rules and `@next` for inductive rules (`@next` stands
  for `successor(T,S)`).
* Facts: `r(A1,…,An)@C` with a constant timestamp `C`.

*Verbatim* [DL11 Ex. 2]:

```
deductive: p(A, B) ← e(A, B);
inductive: q(A, B)@next ← e(A, B);
fact: e(1, 2)@10;
```

[DI Fig. 1] gives the same idea with the desugaring side by side (*verbatim*):

```
toggle(1) :- state(0).                 toggle(1, T) :- state(0, T).
toggle(0) :- state(1).                 toggle(0, T) :- state(1, T).
state(X)@next :- toggle(X).            state(X, S) :- toggle(X, T), succ(T, S).
announce(X)@async :- toggle(X).        announce(X, S) :- toggle(X, T), choice({X,T}, {S}).
```

### 3.3 State idioms

**Simple persistence** [TR §3.1; DL11 §3.1]. `p_pos(A1,...,An)@next ← p_pos(A1,...,An);`. A fact true at
time i is true for all j ≥ i. The TR footnote gives the temporal-logic reading □-"henceforth".

**Mutable persistence, i.e. deletion** [DL11 §3.2], *verbatim*:

```
p pos(A1 , A2 , [...], An)@next ←
    p pos(A1 , A2 , [...], An),
    ¬ p neg(A1 , A2 , [...], An);
```

A fact `p_neg(C…)@k` blocks `p_pos(C…)@k+1`, and by induction every later timestep, *unless* some other
rule re-derives `p_pos` at a later timestep. "A persistent fact, once stated, remains true until it is
retracted" [DL11]. The thesis adds: "once retracted, a fact remains false until it is re-asserted"
[THESIS §3.1].

Worked example with expected answers, *verbatim* [DL11 Ex. 3]:

```
p pos(A, B) ← p(A, B);
p pos(A, B)@next ← p pos(A, B), ¬p neg(A, B);
p(1,2)@101;
p(1,3)@102;
p neg(1,2)@300;
```

"The following facts are true: p(1,2)@200, p(1,3)@200, p(1,3)@300. However, p(1,2)@301 is false because
p(1,2) was 'deleted' at timestep 300." Note that `p(1,2)` **is still true at 300**. Deletion takes effect
at the next timestep.

**The `persist` macro** [DL11 §3.2]: `persist[p_pos, p_neg, 2]` expands to the mutable persistence rule
above. The arguments are the predicate, the deletion predicate and the arity. The thesis abbreviates it
to `persist[p]` [THESIS].

**Update** [DL11 §3.2]: an update at time T is the pair `p_neg(C…)@T; p_pos(D…)@T+1;`. "Every update is
atomic across timesteps": the old value stops existing in the same timestep in which the new value
appears (T+1).

> **Insert-vs-delete conflict (analysis).** In Dedalus the persistence rule and an insertion rule
> (for example `p(X)@next :- ins(X)`) are separate rules. If `p_neg(c)` and `ins(c)` both hold at T,
> then `p(c)` still holds at T+1 because the insertion rule derives it. **Insert wins.** The
> relational-transducer model instead uses no-op semantics: `R+ = ins \ del`, `R− = del \ ins` [RT §2].
> Bloom's `<-`/`<+` interplay is a third convention (outside this cluster). The language spec
> must pick one and document it. Dedalus semantics (insert wins) needs no special casing.

**Sequences** [DL11 §3.3], *verbatim*:

```
seq(B)@next ← seq(A), successor(A,B), event(_);
seq(A)@next ← seq(A), ¬event(_);
seq(0);
```

These rules use the successor relation *as data* (entanglement-like). They keep exactly one `seq` value
per timestep.

**Queues** [DL11 §3.4]. Aggregates are allowed in the heads of deductive rules,
`p(A1,…,An, ρ1(An+1),…,ρm(An+m))`. The body must bind A1..An+m. There is one output row per satisfying
assignment of A1..An, i.e. SQL GROUP BY. Priority queue, *verbatim* [DL11 §3.4]:

```
persist[m priority queue pos, m priority queue neg, 3]
omin(A, min<C>) ←
    m priority queue(A, _, C);
priority_queue(A, B, C)@next ←
    m priority queue(A, B, C),
    omin(A, C);
m priority queue neg(A, B, C) ←
    m priority queue(A, B, C),
    omin(A, C);
```

with input facts

```
priority queue(‘bob’, ‘bash’, 200)@123;
priority queue(‘eve’, ‘ls’, 1)@123;
priority queue(‘alice’, ‘ssh’, 204)@123;
priority queue(‘bob’, ‘ssh’, 205)@123;
```

Each timestep dequeues, *per user A*, the minimum-priority tuple into `priority_queue` at the next step
and deletes it atomically. The paper states the inputs as `priority_queue` facts, but the program drains
`m_priority_queue`. The inputs presumably belong in `m_priority_queue`. Dropping `A` from `omin` gives a global FIFO. "A queue establishes a mapping
between Dedalus0's timesteps and the priority-ordering attribute." The same queue appears as [DI Fig. 2],
*verbatim*:

```
q(V,R)@next :- q(V,R), !del_q(V,R).
qmin(V, min<R>) :- q(V,R).
p(V,R)@next :- q(V,R), qmin(V,R).
del_q(V,R) :- q(V,R), qmin(V,R).
```

> The [DI] version groups by `V`, the value, while the text says `R` is the position. As printed, the
> min is per value, not global. Treat it as illustrative. The DL11 version is the precise one.

**Soft state / TTL** [DI §3.3.1], *verbatim*:

```
q(A, TTL, Birth)@next :- q(A, TTL, Birth), !del_q(A),
                          now() - Birth < TTL.
```

Here `now()` is a *wall-clock foreign function*, so wall-clock time enters as a builtin, separate from logical time.

**Asynchronous service / rendezvous** [DI Fig. 3], *verbatim*:

```
pending(Id, Sender, P) :- request(Id, Sender, P).
pending(Id, Sender, P)@next :- pending(Id, Sender, P),
                               !response(Id, Sender, _).
service_out(P, Out)@async :- request(Id, Sender, P),
                             service_in(P, Out).
response(Sender, Id, O) :- pending(Id, Sender, P),
                           service_out(P, O).
```

An external function (`service_in`) is modeled as an async rule, so its results arrive later.

**Counter with request/response** [THESIS §3.1], *verbatim*:

```
counter(0).
counter(X+1)@next :- counter(X), request(_, _).
counter(X)@next :- counter(X), notin request(_, _).
response(@From, X)@async :- counter(X), request(To, From).
response(From, X)@next :- response(From, X).
```

### 3.4 Stratification [TR §4.1; DL11 §4.1]

* **Lemma 1.** A Dedalus0 program without negation has a unique minimal model, because it is pure Datalog.
* **Definition 1.** A program is *syntactically stratifiable* if its predicate dependency graph has no
  cycle through a negative edge.
* **Definition 2.** The **deductive reduction** of P is the subset of P consisting of exactly its deductive rules.
* **Definition 3.** P is **temporally stratifiable** if its deductive reduction is syntactically stratifiable.
* **Lemma 2** [DL11]. Every temporally stratifiable Dedalus0 instance is locally stratifiable
  (Przymusinski), so it has a unique perfect model. Reason: the head time of an inductive rule is strictly
  greater than the body time, so no ground atom can depend negatively on itself.

Example 4, which is temporally stratifiable but not syntactically stratifiable, *verbatim* [DL11]:

```
persist[p pos, p neg, 3]
p_pos(A, B, T) ←
    insert p(A, B, T);
p_neg(A, B, T) ←
    p_pos(A, B, T),
    delete p(T);
```

The dependency graph has the cycle p_pos → p_neg → ¬ → p_pos, but the negative edge is inside the
inductive persistence rule, so `p_pos@n` depends on `p_neg@n`, which depends on `p_pos@n`, which feeds
`p_pos@n+1`.

**Aggregation.** The Dedalus papers do not spell out stratification for aggregates. Following Mumick &
Shmueli, which they cite, treat an aggregate head as depending *negatively* (non-monotonically) on its body
predicates for stratification purposes. **(analysis)**

### 3.5 Temporal safety and quiescence [TR §4.2; DL11 §4.2]

Classic Datalog safety has three parts: no functions, range restriction, finite EDB. The `successor`
relation breaks it, since there are infinitely many timestamps. Dedalus replaces it with:

* **Definition 4 (instantaneous safety).** A rule is instantaneously safe if it is deductive,
  function-free and range-restricted. A program is instantaneously safe if its deductive reduction is.
* **Definition 5.** Two sets of ground atoms are **equivalent modulo time** if they match atom-for-atom on
  predicate and all non-time attributes.
* **Definition 6.** An instance is **quiescent at T** if the atoms with time suffix T are equivalent modulo
  time to those with suffix T−1.
* **Lemma 3** [DL11] (Observation 1 in [TR]). If an instance is quiescent at T, it stays quiescent until
  the timestamp V of the next EDB fact, i.e. for all U with V > U ≥ T. If no EDB fact has a timestamp
  greater than T, it is quiescent from then on. Proof idea: the state at T is determined by the state
  at T−1 plus the EDB at T, and "the integer value of the timestep does not influence the derivation".
* **Definition 7.** A Dedalus0 instance with finite EDB is **temporally safe** if it is quiescent from
  some time T onward.
* **Definition 8 (instantaneous predicate).** An IDB predicate `e` is instantaneous if, for every `p` that
  `e` transitively depends on, either `p` heads no inductive rule, or the body of *each* inductive rule
  with head `p` contains at least one positive instantaneous predicate.
  **(analysis)** The definition is recursive, and the intended reading is the **least fixpoint**: a
  persisted predicate `p@next :- p` must *not* count as instantaneous. With a finite EDB (EDB facts at
  finitely many timestamps), instantaneous predicates hold at only finitely many timestamps.
* **Conservative test** [DL11]. A program is temporally safe if every rule is one of:
  1. an instantaneously safe rule;
  2. an inductive rule whose head predicate also occurs in the body with the same variable bindings for
     every attribute except time. This covers persistence and mutable persistence;
  3. an inductive rule with at least one instantaneous predicate as a positive subgoal.
* **Lemma 4** [DL11]. A temporally stratifiable Dedalus0 instance with finite EDB, all of whose rules are
  of kinds 1–3, is temporally safe.

Examples, *verbatim* [DL11]:

```
Example 5 (safe, despite infinitely many derivations):
persist[p pos, p neg, 2]
p(1, 2)@123;

Example 6 (unsafe deductive rule, not range-restricted):
p(A, B) ← q(A);

Example 7 (temporally unsafe by infinite oscillation):
flip flop(B, A)@next ← flip flop(A, B);
flip flop(0, 1)@1;
```

"We can imagine interesting examples of temporally unsafe programs, and do not forbid them in Dedalus0."
**Implementers should therefore warn about unsafe programs, not reject them.**

**Ultimate periodicity (analysis).** Suppose a Dedalus0 program has no entanglement, no arithmetic that
creates values, and a finite EDB. Then the Herbrand base modulo time is finite. After the last EDB
timestamp E, the state at t+1 is a deterministic function of the state at t. By pigeonhole the sequence of
states is *ultimately periodic*, and "quiescent" is the special case of period 1. `flip_flop` has period 2.
[CRON §7] notes, citing Baudinet et al., that only periodic phenomena seem to be finitely representable.
Arithmetic in heads (`counter(X+1)@next`, `tic(F,H,I+1)@next`) or entanglement breaks finiteness, and then
there may be no period at all.

### 3.6 Evaluation over storage: "temporal evaluation" [TR §5, Algorithm 1]

Running persistence rules literally costs O(t) derivations to see the state at time t. The TR gives a
rewrite plus a driver loop in the style of semi-naive evaluation. *Verbatim* modulo layout:

```
Algorithm 1 Temporal Evaluation
// rewrite program
foreach persistent predicate P(A1, ..., An) do
    // include "old facts" in the current timestep
    addRule P(A1, ..., An, T) ← P_store(A1, ..., An).
    // identify "new" derived facts
    addRule ∆+P(A1, ..., An) ← P(A1, ..., An, T), ¬P(A1, ..., An, T − 1).
    // identify "new" facts that are "to-be-deleted"
    addRule ∆−P ← P_neg(A1, ..., An, T).
end for
foreach rule R do
    foreach persistent predicate P(A1, ..., An, T) in R's body do
        substitute P_store(A1, ..., An) for P(A1, ..., AN, T)
    end for
end for
// evaluate program starting at "minimum" EDB timestamp
let t = u ∈ Z : ∃P(A1, ..., An, u), ∄Q(B1, ..., Bm, v) : v < u
repeat
    replace T with t in the rewritten program, and compute
        a fixpoint of the result via semi-naive evaluation
    foreach persistent predicate P(A1, ..., An) do
        P_store(A1, ..., An) := P_store(A1, ..., An) ∪ ∆+P(A1, ..., An)
        P_store(A1, ..., An) := P_store(A1, ..., An) \ ∆−P(A1, ..., An)
    end for
    // t becomes the least larger timestamp with an atom
    if ∃u ∈ Z : u > t, ∃P(A1, ..., An, t), ∄v : v > t ∧ v < u then
        let t = u
    else
        let t = ∅
    end if
until t = ∅
```

Claim: for each t, the result equals the minimal model of the original program with `successor`
truncated to the prefix ending at t. The loop "marches through time in order, *skipping steps that
have no changes*". [DI footnote 9] says the same: "An intelligent evaluation strategy for this logic
should in most cases use traditional storage technology rather than re-deriving tuples each timestep."
The DL11 version dropped this section.

> **Notes on Algorithm 1 (analysis).** (a) As printed, the time-skip condition is garbled: it
> quantifies over `P(…, t)` where it presumably means "some atom at u". The intent is clearly "jump to
> the next timestamp at which any EDB atom exists, provided the store is quiescent". (b) The loop applies
> ∪∆+ before \∆−, so a fact that is both newly derived and deleted at t is removed. That is correct:
> deletion at t removes it from t+1 onward. (c) The algorithm handles only the "persistent predicate"
> pattern. A general engine must still evaluate arbitrary inductive rules. The recommended engine shape is
> in §15.

Hydroflow implements the same idea. A `.persist` relation compiles to
`insert → unique::<'tick>() → difference::<'tick,'static>()`, with the previous tick's output fed back
through `defer_tick()` into the negative input of the difference. So "read outputs the *new* values for
this tick", and consumers that need the full relation get `persist::<'static>()` [HFD lib.rs, lines ~140–160].

---

## 4. Asynchrony: full Dedalus [TR §6; DL11 §5; THESIS §3.1]

### 4.1 Choice

`choose((X1),(X2))` in a rule body, where the variables of X1 and X2 occur elsewhere in the body, enforces
the functional dependency X1 → X2 by "choosing" one assignment of X2 per X1 [DL11 §5.1]. Following
Greco & Zaniolo, choice expands into an unstratifiable strongly connected component of rules. Each
possible choice then corresponds to a different **stable model**, so a nondeterministic program has many
stable models. "It may be helpful to think of one such model chosen non-deterministically — a
non-deterministic 'assignment of timestamps to tuples'."

The standard Datalog¬ encoding of choice, as used by [TPLP §4.3], is Saccà–Zaniolo dynamic choice with
three rules per async head `R`. Here `cand` lists the possible arrival times, `chosen` picks one and `other`
excludes the rest:

```
cand_R(x,s,y,t,ū)   ← B{ū,v̄,y}⇑x,s , all(y), time(t).                          (3)
chosen_R(x,s,y,t,w̄) ← cand_R(x,s,y,t,w̄), ¬other_R(x,s,y,t,w̄).                 (4)
other_R(x,s,y,t,w̄)  ← cand_R(x,s,y,t,w̄), chosen_R(x,s,y,t′,w̄), t ≠ t′.         (5)
R(y,t,w̄)            ← chosen_R(x,s,y,t,w̄).                                      (6)
```

(*verbatim* up to restoring `≠`.) `B{…}⇑x,s` is the original body with location x and timestamp s added to
every literal. `all` is the set of nodes. Rules (4) and (5) together force exactly one chosen `t` per
(x, s, y, w̄) in every stable model.

### 4.2 Distribution model: horizontal partitioning and location specifiers [DL11 §5.2]

* The system is a set of **agents**, each running *the same program* over a disjoint horizontal partition
  of every predicate. One attribute of each predicate is the **location specifier**, written with a `#`
  prefix. In [DI] and [THESIS] the prefix is `@`, as in `link(@Src, Dest, Weight)` or `response(@From, X)@async`.
* **Body locality:** every body predicate must use *the same* location-specifier variable. Rule bodies are
  therefore evaluated on one machine.
* A rule is **local** if its head has the same location variable as the body, and a **communication rule**
  otherwise. **Communication rules must be asynchronous** [DL11 §5.4]. As a result, "agents may only learn
  about time values at another agent by receiving messages (with unbounded delay) from that agent. Note
  that this model says nothing about the relationship between the agents' clocks; they could be
  non-monotonically increasing, or they could respect a global order."
* **Heterogeneous roles** such as coordinator and agents are simulated by giving every node the union of
  the programs and guarding each role's rules with a role flag in the input [TPLP §4.1.3 footnote 4].

### 4.3 Asynchronous rules [DL11 §5.3]

The relationship between head time S and body time T is unknown. S (but not T) may be ⊤, "never",
which means the deduction is lost:

```
time(⊤);
time(S) ← successor(S, _);
```

Each async rule with head `p(A1,…,An)` gets the extra body subgoals `time(S), choose((A1,…,An,T),(S))`.
Because all head variables appear in the determinant, each head tuple gets a unique S. [DL11] adds:
"communication rules include the location specifier appearing in the rule body among the
functionally-determining attributes of the choose predicate, even if it does not occur in the head."

*Verbatim* [DL11 Ex. 8, 9]:

```
r(A, B, S) ←
    e(A, B, T),
    time(S),
    choose((A, B, T), (S));

r(A, B)@async ← e(A, B);
```

Head location syntax [MAR §2.1], *verbatim*: `p(#D,L,W)@async ← b(#L,D,W), ¬c(#L,L).` "The head and body
location specifiers are D and L respectively. D may appear in the body, L may appear in the head, and L may
appear duplicated in the body."

### 4.4 Temporal monotonicity and paradoxes [DL11 §5.5]

Nothing in the TR/DL11 definition stops an async head from getting a timestamp *earlier* than its body.
That breaks the assumptions behind temporal stratification and "admits the possibility of temporal
paradoxes". The papers did not forbid it, "as doing so would reduce its expressiveness". They identify
three practically relevant cases:

* (a) **monotonic programs** "even with non-monotonicity in time": positive Datalog does not care about the
  values of any attribute, so there is an unambiguous least model. [CRON] later made this precise (§8);
* (b) non-monotonic programs whose semantics *guarantee* monotone time suffixes, e.g. by running a Lamport
  clock protocol;
* (c) non-monotonic programs where domain knowledge (the substrate) guarantees monotone timestamps.

The later formal semantics (§5) makes causal delivery part of the semantics.

### 4.5 Entanglement [DL11 §5.5]

*Verbatim*: `p(A, B, N)@async ← q(A, B)@N;`. The body time variable N (written unsugared) also appears as
an ordinary attribute of the head, "recording a binding of both the time value of the deduction and the
time value of its consequence". It allows reasoning about partial orders of time across machines and
"exposes the infinite successor relation to attributes other than the time suffix, allowing us to express
concepts such as infinite sequences." It is **excluded** from the formal semantics in [MAR], [DS-TR] and
[THESIS]. [DS-TR footnote 1]: entanglement "allows the simulation of arbitrary Turing machines". [MAR]
names "the prohibition on binding timestamps to non-timestamp attributes" as one reason ultimate models
are finite.

### 4.6 Lamport clocks [DL11 §5.6]

Async rules can send messages "into the past". This rewrite of `p(A,B)@async ← q(A,B)` restores causal
order (*verbatim*):

```
persist[p pos, p neg, 2]
p wait(A, B, N)@async ← q(A, B)@N;
p wait(A, B, N)@next ← p wait(A, B, N)@M, N ≥ M;
p(A, B)@next ← p wait(A, B, N)@M, N < M;
```

A message tagged with its send time N waits in `p_wait` until the receiver's local time M passes N. "If the
runtime is able to efficiently evaluate timesteps when the database is quiescent, then instead of 'waiting'
by evaluating timesteps, it will simply increase its logical clock to match that of the sender." Messages
sent "into the future" are processed at the timestep that receives them. The authors note that the usual
Lamport tie-breaking would need another use of choice or node-ID suffixes.

> **Implication (analysis).** Supporting this idiom requires (i) binding the current timestamp as a
> value in the body (`@M`), i.e. entanglement, and (ii) an engine able to *jump* its local clock forward
> across quiescent steps. Both are features to design in deliberately.

### 4.7 Reliable broadcast [DL11 §5.7]

*Verbatim*:

```
sbcast(#Member, Sender, Message)@async ←
    smessage(#Agent, Sender, Message),
    members(#Agent, Member);
sdeliver(#Member, Sender, Message) ←
    sbcast(#Member, Sender, Message);

smessage(Agent, Sender, Message) ←
    rmessage(Agent, Sender, Message);
buf_bcast(Sender, Me, Message) ←
    sdeliver(Me, Sender, Message);
smessage(Me, Sender, Message) ←
    buf_bcast(Sender, Me, Message);
rdeliver(Me, Sender, Message)@next ←
    buf_bcast(Sender, Me, Message);
```

"The @next is required in the rdeliver definition in order to prevent nodes from taking actions based upon
the broadcast before it is guaranteed to meet the reliability guarantee." The textbook assumption is that
a node which fails to receive a message has failed.

### 4.8 Network data independence [DI §3.2.1]

Location specifiers only say *where data must be stored*; the system induces communication. A rule whose
body joins across locations, *verbatim*:

```
path(@Src, Dest)@async
    :- link(@Src, X), path(@X, Dest).
```

needs communication to evaluate the body. It can be rewritten left-recursively so the body is local, and
the two forms correspond to two well-known routing protocols. **Under strict Dedalus, bodies must be local
(§4.2), so this rule is legal only as sugar that the compiler localizes.** **(analysis)** The strict,
local-body form should be the language's IR. Non-local bodies can be a front-end convenience with an
explicit, analyzable localization rewrite.

---

## 5. Declarative (stable-model) semantics [DS-TR; TPLP §3–4]

This is the reference formal semantics. [MAR] and [CRON] build on it, and it gives the ground truth for
test oracles.

### 5.1 Datalog¬ preliminaries as formalized there [TPLP §3.2; DS-TR §2.2]

* A rule φ is a tuple `(head(φ), pos(φ), neg(φ), neq(φ))` in [DS-TR]. [TPLP] drops `neq` and uses
  built-in `≠` and `<` relations. `neg(φ)` holds *atoms*; the negation is implicit.
* **Safety:** every variable of the head, of `neg` and of `neq` must occur in `pos`.
* Programs are **constant-free**: constants that matter come from unary input relations. This is a
  theory convenience, not a language restriction.
* **Semi-positive semantics:** T_P is the immediate-consequence operator. The output is the least fixpoint
  containing the input. Inputs may contain IDB facts, which is how facts from the previous step and
  delivered messages are injected.
* **Stratified semantics:** σ: idb → {1..k} with σ(R) ≤ σ(T) for positive IDB body atoms and σ(S) < σ(T)
  for negated ones. The program is evaluated stratum by stratum, and the result does not depend on which
  stratification is chosen.
* **Stable models (Gelfond–Lifschitz):** `ground(P,I)` is the set of ground rules over adom(I).
  `ground_M(P,I)` removes every rule whose `neg` intersects M and strips the remaining negative atoms.
  M is stable iff M equals the least model of `ground_M(P,I)` on I.

### 5.2 Dedalus programs in the formal papers

[DS-TR §2.3]: `D_time = {time/1, tsucc/2}`, instantiated only as `time = ℕ`,
`tsucc = {(s, s+1)}`. A general Dedalus rule is a constant-free Datalog¬ rule over `D^LT ∪ D_time`, where
`D^LT` adds a location column and a time column (in that order) as the first two components, plus a set
`cho(φ)` of choice operators. `B{x, s | ū}` denotes a body in which every literal has location variable
`x` and time variable `s`, `s` occurs only in time positions, and the variables are exactly x, s, ū. The
three admissible forms, *verbatim* modulo layout:

```
deductive:     R(x, s, v̄) ← B{x, s | v̄, w̄}.
inductive:     R(x, t, v̄) ← B{x, s | v̄, w̄}, tsucc(s, t).
asynchronous:  R(y, t, v̄) ← B{x, s | v̄, w̄, y}, time(t), choice(⟨x, s, y, v̄⟩, ⟨t⟩).
```

**Definition 2.1** [DS-TR]: a Dedalus program is a finite set of such rules whose deductive rules are
syntactically stratifiable. An input is a distributed database instance H over a network N (a finite
set of node ids) and `edb(P)`.

[TPLP §4.1.1] gives the user-level syntax actually formalized:

```
deductive:     R(ū) ← B{ū, v̄}
inductive:     R(ū)• ← B{ū, v̄}
asynchronous:  R(ū) | y ← B{ū, v̄, y}        -- "piped" to addressee node y
```

Here `•` means `@next` and `| y` means `@async` to node `y`. Location and time are implicit in both the
body and the head. **Definition 4.1** [TPLP]: all rules are safe and the deductive rules are syntactically
stratifiable. Programs are assumed constant-free, and every rule is assumed to have at least one positive
body atom.

**The four time points of an async rule** [TPLP §4.1.1]: body evaluation, send, arrival, visibility.
In the model-based semantics the first two coincide and so do the last two. "There is no upper bound on the
interval between these two pairs, although it will be finite."

### 5.3 From Dedalus to pure Datalog¬: `pure(P)` [TPLP §4.3–4.5]

Given a Dedalus program P, construct a Datalog¬ program `pure(P)` in three layers.

**(i) Dynamic choice layer** `pure_ch(P)`:

* Deductive rule `R(ū) ← B{ū,v̄}` becomes `R(x, s, ū) ← B{ū,v̄}⇑x,s`. (1)
* Inductive rule `R(ū)• ← B` becomes `R(x, t, ū) ← B{ū,v̄}⇑x,s, tsucc(s, t)`. (2)
* Asynchronous rule: rules (3)–(6) in §4.1. If several async rules share the head predicate R, each adds
  its own `cand_R` rule, while (4)–(6) are shared. As a result, **the same message fact sent by the same
  node at the same step gets one arrival time, even if several rules derive it** ("set sending semantics").
* `pure_ch(P)` is not syntactically stratifiable, and may not even be locally stratifiable, when a
  `cand_R` body depends negatively on R [TPLP §4.3.1].

**(ii) Causality layer** `pure_ca(P)`. It adds a happens-before relation `before(x, s, y, t)`, "local step
s of node x happens before local step t of node y" (*verbatim*, `≠` restored):

```
before(x, s, x, t) ← all(x), tsucc(s, t).                                     (7)
before(x, s, y, t) ← before(x, s, z, u), before(z, u, y, t).                  (8)
cand_R(x, s, y, t, ū) ← B{ū, v̄, y}⇑x,s , all(y), time(t), ¬before(y, t, x, s).  (9)   [replaces (3)]
before(x, s, y, t) ← chosen_R(x, s, y, t, w̄).                                 (10)
```

Rule (9) forbids choosing an arrival step that happens-before the send step. Rule (10) makes the send
step happen before the arrival step, so a reply can never arrive before the original send. **(analysis)**
Self-sends therefore arrive strictly later. If `y = x` and `t = s`, then (10) would derive
`before(x,s,x,s)`, which defeats the `¬before` in (9), so no stable model contains it.

**(iii) Finiteness layer** `pure(P)`. It rules out an infinite number of messages arriving at one step
(*verbatim*, `<` restored):

```
hasSender(y, t, x, s) ← chosen_R(x, s, y, t, w̄), ¬rcvInf(y, t).              (11)
isSmaller(y, t, x, s) ← hasSender(y, t, x, s), hasSender(y, t, x, s′), s < s′. (12)
hasMax(y, t, x)       ← hasSender(y, t, x, s), ¬isSmaller(y, t, x, s).          (13)
rcvInf(y, t)          ← hasSender(y, t, x, s), ¬hasMax(y, t, x).                (14)
```

The argument: finitely many nodes, each sending finitely many messages per step. If infinitely many
messages arrive at (y, t), some sender x must be sending to (y, t) from infinitely many of its own steps,
and then there is no maximum send step.

**Input** [TPLP §4.3.2]:
`decl(H) = {R(x, s, ā) | x ∈ N, s ∈ ℕ, R(ā) ∈ H(x)} ∪ {all(x) | x ∈ N} ∪ I_time`, where
`I_time = {time(s), tsucc(s,s+1)} ∪ {s < t} ∪ {s ≠ t}`. **Input facts are available at every timestep of
their node.**

**Definition 4.4** [TPLP]: a *model* of P on H is a stable model of `pure(P)` on `decl(H)`. Earlier layers
give "choice-models" (Def 4.2) and "causal models" (Def 4.3).

### 5.4 Why the extra layers are necessary (counterexamples) [TPLP §4.3.3, §4.4.3]

*Non-causality*, TPLP Fig. 4, *verbatim*:

```
A( ) | x ← Id(x)·
B ( ) | x ← A( ), Id(x)·
T ( ) ← A( ), ¬B ( )·
T ( )• ← T ( )·
B ( )• ← B ( )·
```

Intuitively `T()` should always be produced, because at least one `A()` must arrive before any `B()` is
sent. There is a choice-model in which the `B()` sent at step 1 arrives at step 0, before any `A()`. `B`
is then persisted from time 0, so `T` never fires. The `before` rules exclude that model.

*Infinite grouping*, TPLP Fig. 5, *verbatim*:

```
A( ) | y ← contact(y)·
first( )• ← A( )·
first( )• ← first( )·
T ( ) ← first( ), A( )·
T ( )• ← T ( )·
```

There is a causal model in which every `A()` sent from x arrives at step 0 of y, so y never sees two
separate arrivals and `T` is never created. The finiteness rules exclude it.

### 5.5 The earlier vector-clock encoding [DS-TR §4.1]

[DS-TR] first expressed causality through vector clocks inside Datalog¬, with no `before` relation.
*Verbatim*, symbols restored:

```
notZero(t) ← tsucc(s, t).                                            (4.1)
zero(t) ← time(t), ¬notZero(t).                                      (4.2)
rcvClock(x, s, y, s) ← all(x), all(y), x ≠ y, zero(s).               (4.3)
rcvClock(x, s, x, s′) ← all(x), tsucc(s, s′).                        (4.4)
rcvClock(x, s′, y, t) ← clock(x, s, y, t), x ≠ y, tsucc(s, s′).      (4.5)
isBehind(x, s, y, t) ← rcvClock(x, s, y, t), rcvClock(x, s, y, t′), t < t′.   (4.6)
clock(x, s, y, t) ← rcvClock(x, s, y, t), ¬isBehind(x, s, y, t).     (4.7)
R_snd(x, s, y, t, v̄) ← B{x,s|v̄,w̄,y}, all(y), clock(x, s, y, u), time(t), u ≤ t, chosen_R(x, s, y, v̄, t).   (4.8)
chosen_R(x, s, y, v̄, t) ← B{x,s|v̄,w̄,y}, all(y), clock(x, s, y, u), time(t), u ≤ t, ¬other_R(x, s, y, v̄, t). (4.9)
other_R(x, s, y, v̄, t) ← B{x,s|v̄,w̄,y}, all(y), clock(x, s, y, u), time(t), u ≤ t, chosen_R(x, s, y, v̄, t′), t ≠ t′. (4.10)
R(y, t, v̄) ← R_snd(x, s, y, t, v̄).                                   (4.11)
rcvClock(y, t, z, u) ← R_snd(x, s, y, t, v̄), clock(x, s, z, u).      (4.12)
```

`rcvClock(x,s,y,t)` means "x at local time s has a lower-bound estimate t of y's clock", and
`clock(x,s,y,t)` is the chosen maximum. A sender picks the arrival time `t ≥` its estimate of the
receiver's clock. Rule (4.12) ships the whole vector clock with every message. [DS-TR] enforced fairness
with a separate *model-level* condition: M is fair if, for each (x, s), only finitely many `R_snd(…, x, s, …)`
facts arrive. [TPLP] replaced both mechanisms with `before` and the finiteness rules. [DS-TR] also stresses
that its proof covers choice *in the presence of negation*. Earlier proofs of the choice encoding assumed
negation-free programs, and the vector-clock rules are not even locally stratified.

---

## 6. Operational semantics and the equivalence theorem [TPLP §5.1; DS-TR §3]

This is the semantics the Rust runtime should implement. It is also the semantics that the relational
transducer model [RT] and WebdamLog share.

### 6.1 Subprograms [TPLP §5.1.2]

* `deduc_P`: the deductive rules, evaluated with the **stratified** semantics to a fixpoint.
* `induc_P`: the inductive rules with `•` removed, applied in **one step, with no fixpoint**. Facts for
  the next step are not visible now.
* `async_P`: every `T(ū) | y ← B` rewritten as `T(y, ū) ← B`, also applied in one step with no fixpoint.
  The first component is the addressee.
* `induc_P` and `async_P` need not be stratifiable. They read only the completed deductive fixpoint.

### 6.2 Configurations, transitions, runs [TPLP §5.1.1, §5.1.3]

* A **configuration** is ρ = (st, bf). `st(x)` is the state of node x, an instance over sch(P).
  `bf(x)` is x's **message buffer**, a set of pairs (i, f) where i is a *send-tag* (the index of the
  transition that sent f). Send-tags make the buffer a multiset of in-flight messages and are invisible
  to the program.
* **Start:** `st(x) = H(x)` and `bf(x) = ∅`.
* **Transition** with send-tag i: (ρa, x, m, i, ρb), where the active node x receives an arbitrary subset
  m ⊆ bf_a(x). Then:

```
I   = st_a(x) ∪ untag(m)
D   = deduc_P(I)
δ_{i→y} = {(i, R(ā)) | R(y, ā) ∈ async_P(D)}      for each y ∈ N
st_b(x) = H(x) ∪ induc_P(D)
bf_b(x) = (bf_a(x) \ m) ∪ δ_{i→x}
st_b(y) = st_a(y),  bf_b(y) = bf_a(y) ∪ δ_{i→y}   for y ≠ x
```

  Messages to addressees outside N are ignored. **Input facts H(x) persist automatically. IDB facts
  persist only if an inductive rule re-derives them.** Delivered messages are simply facts of the head
  relation R in I. The program cannot tell a received fact from a local one ("communication is
  transparent", [DS-TR §3.2]). A transition with m = ∅ is a **heartbeat**.
* **Run:** an infinite sequence of transitions starting at `start(P,H)`, where transition i uses send-tag i.
  Parallel transitions can be simulated by sequential ones (Remark 2).
* **Local transitions are deterministic and computable in PTIME** (data complexity).

### 6.3 Fairness, arrival function, timestamps, traces [TPLP §5.1.4–5.1.5]

* A run is **fair** if (i) every node is active infinitely often and (ii) every message in a buffer is
  eventually delivered. A fair run exists for every input because heartbeats are always possible.
* The **arrival function** α_R(i, y, f) = k is the transition that delivers (i, f) to y. It always
  satisfies α > i.
* **Local timestamp:** loc_R(i) is the number of earlier transitions of the active node. Timestamps are
  per-node step counters starting at 0.
* **Trace:** `trace(R) = ⋃_i D_i ⇑ x_i, loc_R(i)`, the deductive fixpoint of every step, tagged with node
  and local time.
* **Happens-before** ≺_R on N×ℕ [TPLP §5.2.1] is the smallest transitive relation with (x,s) ≺ (x,s+1)
  and (x,s) ≺ (y,t) whenever x at step s sends a message that arrives at step t of y. It is a strict
  partial order (Lemma 1, Corollary 1).

**Theorem 4** [TPLP]: for every input H, (i) every fair run R has a model M with
`trace(R) = M|sch(P)^LT`, and (ii) every model M has a fair run R with `trace(R) = M|sch(P)^LT`.
([DS-TR Thm 6.1] states the same for its vector-clock version.) The proof of (ii) builds a run from a model
by topologically sorting local steps by the partial order induced from `before`/vector clocks, then
reading off deliveries.

**Open issues the authors list:** a characterization of *unfair* runs (the proof relies on fairness);
whether the causality rules are needed for eventually consistent programs (answered partially by [CRON]);
decidability of the output problem. It is expected to be undecidable when duplicate in-flight messages are
kept, and decidable if buffers are sets [TPLP §6].

---

## 7. Output, confluence and the confluent sublanguages [MAR]

### 7.1 Spatio-temporal schema and EDB [MAR §2.1–2.2]

`S+` adds a location column (first) and includes `<`/2 and `node`/1. `S*` adds location and timestamp
(first two columns) plus `<` (finite), `node` (finite), `time` (infinite) and `timeSucc` (infinite).
The rule forms, *verbatim*:

```
p(L,T,W) ← b1(L,T,X1), ..., bl(L,T,Xl), ¬c1(L,T,Y1), ..., ¬cm(L,T,Ym), node(L), time(T), ineq(ϕ).
p(L,S,W) ← b1(L,T,X1), ..., bl(L,T,Xl), ¬c1(L,T,Y1), ..., ¬cm(L,T,Ym), node(L), time(T), timeSucc(T,S), ineq(ϕ).
p(D,S,W) ← b1(L,T,X1), ..., bl(L,T,Xl), ¬c1(L,T,Y1), ..., ¬cm(L,T,Ym), node(L), time(T), time(S),
           choice((L, T, B),(S)), node(D), ineq(ϕ).
```

B is the tuple of all distinct body variables. D and L may appear in W and in the Xs and Ys; T and S may
not, so there is no entanglement. The head relation may not be `time`, `timeSucc` or `node`. Body relations
may not be `timeSucc`, `time` or `<`. **A Dedalus program is a finite set of causally rewritten
spatio-temporal rules. Only programs whose deductive rules are syntactically stratified are considered.**
**EDB facts exist at every timestep.** Every EDB instance maps `<` to a total order over the active domain.
This order is needed for the PTIME results and for the ∀-encoding in the coordination rewrite.

Example 1 [MAR], *verbatim*: `p(#L)@async ← q(#L).` with EDB `{node(n1), q(n1)}`. For each
S ∈ P(ℕ∖{0}) with |S| = |ℕ|, the stable models are exactly
`{node(n1)} ∪ {p(n1,i) | i ∈ S} ∪ {q(n1,i) | i ∈ ℕ}`. q is EDB, so it is true at every time. Each time
makes its own choice for p, which must be later than time 0. "The causality constraint rules out elements
of the power set with finite cardinality."

### 7.2 Ultimate models [MAR §2.2]

Let SO ⊆ S+ be the *output schema*. Define ^T: the spatial fact `r(p,c̄)` is in ^T iff r ∈ SO and there is
a t with `r(p,s,c̄) ∈ T` for all s > t. The **ultimate models** of an instance are {^T | T a stable model}:
"exactly the facts … that are eventually always true in a stable model". They are always finite, and there
are finitely many of them, thanks to the finite EDB, safety, the restrictions on time/timeSucc and the ban
on entanglement. Example 2: for Example 1 with SO = {p} there are exactly two ultimate models, {} and
{p(n1)}. [MAR Appendix F] explains the purpose: many stable models differ only in *when* things happen,
and ultimate models collapse those differences.

**Confluent** means one ultimate model for every EDB; otherwise the program is **diffluent** [MAR §3.1].

* **Lemma 1:** confluence of Dedalus programs is undecidable. The proof reduces from two-counter machines
  by making the output depend on whether two `@async` facts get equal timestamps [MAR App. G].
* **Lemma 2:** Dedalus subsumes PSPACE, via QBF [MAR App. H].

Marriage ceremony, diffluent, *verbatim* [MAR Ex. 3]:

```
i do(X)@async ← i do edb(X).
runaway() ← ¬i do(bride), i do(groom).
runaway() ← ¬i do(groom), i do(bride).
runaway()@next ← runaway().
i do(X)@next ← i do(X).
```

If the two votes get different first timestamps, runaway() is ultimately true. If they get the same
timestamp, it is not.

### 7.3 Dedalus⁺ [MAR §3.2]

* **Semipositive:** ¬ is applied only to EDB relations.
* **Guarded asynchrony:** every relation p that heads an async rule also has the persistence rule
  `p(X)@next ← p(X)`.
* Dedalus⁺ is the class of semipositive programs with guarded asynchrony.
* **Lemma 3:** Dedalus⁺ programs are *temporally inflationary*: f@t ∈ model implies f@t+1 ∈ model.
* **Theorem 1:** Dedalus⁺ programs are confluent.
* **Corollary 1:** replacing every async rule with an inductive rule (undo causality and choice, add
  `timeSucc(T,S)`) preserves the ultimate model.
* **Lemma 4:** removing `timeSucc` from every inductive rule except the persistence rules of async-headed
  relations, which makes those rules deductive, also preserves the ultimate model.
* **Theorem 2:** Dedalus⁺ captures exactly PTIME. It reduces to Datalog with EDB negation over an ordered
  domain.

The pushed-down marriage program is in Dedalus⁺, *verbatim* [MAR Ex. 4]:

```
i dont(X)@async ← ¬i do edb(X).
runaway() ← i dont(bride).
runaway() ← i dont(groom).
runaway()@next ← runaway().
i dont(X)@next ← i dont(X).
```

A confluent program that is *not* in Dedalus⁺, *verbatim* [MAR App. E]:

```
b(#N, I)@async ← b edb(#L, I).
b(I)@next ← b(I), ¬dequeued(I).
b lt(I, J) ← b(I), b(J), I < J.
dequeued(I)@next ← b(I), ¬b lt( , I), b lt( , ).
```

Every instance has one ultimate model in which `b` holds the maximum element of `b_edb` under `<`.

### 7.4 Dedalus_S (stratified negation over async) [MAR §4]

**PDG:** one node per relation, with an edge q → p if p heads a rule that has q in its body. An edge is
*asynchronous* or *inductive* if the rule is, and *negated* if the rule has ¬q. The PDG leaves out
node/time/timeSucc and the relations introduced by the causality and choice rewrites.
**Dedalus_S** is the class of programs with guarded asynchrony whose PDG has **no cycle through negation**,
counting temporal edges too. The stratum of r is the largest number of negated edges on any path from r.
Each stratum i is a Dedalus⁺ program P_i. Its EDB is all lower strata, and its output schema includes the
relations of stratum i+1. **Semantics:** the ultimate model of Pn(…P1(P0(E))…). Dedalus_S is confluent
(Corollary 2) and captures PTIME (Corollary 3).

> **(analysis)** Dedalus_S is **strictly more restrictive than temporal stratification.** It forbids
> negation cycles *through* `@next`, and the classic deletion pattern `p@next :- p, ¬del_p` together with
> a `del_p` that depends on `p` is such a cycle. Dedalus_S is an analysis target, "confluent by
> construction", not the general language.

### 7.5 The coordination rewrite P(S) [MAR §4.2, App. C, D]

The goal is to implement Dedalus_S semantics with plain Dedalus. Add `p_done()` to the body of every rule
containing `¬p(...)`, and synthesize rules defining `p_done`. The **sealing** property (Lemma 5) is that
if `p_done(l,t)` holds then `p_done(l,s)` holds for all s > t, and every `p(l,s,c̄)` with s > t already
holds at t. In words: p is complete ("sealed") once done.

* Collapse the PDG's strongly connected components. A component with an async edge is **async recursive**.
* EDB p: the rule is simply `p_done().`
* **Non-async-recursive node:** `p_done() ← r1_done(), …, r_ip_done()` over p's rules. For a deductive rule,
  `r_j_done() ← p1_done(), …, p_iq_done()` over its input components. For an async rule, use ack counting,
  *verbatim* [MAR App. D]:

```
p_j to_send(N,W) ← b1(#L,X1), ..., bl(#L,Xl), ¬c1(#L,Y1), ..., ¬cm(#L,Ym).
p_j to_send_done() ← b1_done(), ..., bl_done(), c1_done(), ..., cm_done().
p_j send(#N,L,X)@async ← p_j to_send(#L,N,X).
p_j ack(#N,L,X)@async ← p_j send(#L,N,X).
r_j done_node(#L,N)@async ← p1_done(#N), ..., piq_done(#N),
        ( ∀X. p_j to_send(#N,L,X) ⇒ p_j ack(#N,L,X) ).
r_j done() ← ( ∀N. node(N) ⇒ r_j done_node(N) ).
```

  `∀X.φ` is compiled to a walk over a total order built from `<` (App. D, *verbatim*):

```
pφ_min(W,X) ← p(W,X), ¬pφ_succ(W,_,X), pφ_succ_done().
pφ_max(W,X) ← p(W,X), ¬pφ_succ(W,X,_), pφ_succ_done().
pφ_succ(W,X,Y) ← p(W,X), p(W,Y), X < Y, ¬pφ_not_succ(W,X,Y), pφ_not_succ_done().
pφ_not_succ(W,X,Y) ← p(W,X), p(W,Y), p(W,Z), X < Z, Z < Y.
forallφ_ind(W,X) ← pφ_min(W,X), q(W,X).
forallφ_ind(W,X) ← forallφ_ind(W,Y), pφ_succ(W,Y,X), q(W,X).
forallφ(W) ← forallφ_ind(W,X), pφ_max(W,X).
```

  The vacuous case (p empty) is handled by duplicating the rule with `¬p(W,_)` in place of the ∀.
* **Async-recursive node:** the node with the minimum id acts as master and runs a **two-round voting
  protocol**. Round 1 asks every node whether it has unacknowledged messages. Round 2 asks whether any
  node has sent a message since round 1. Any "incomplete" vote restarts round 1. Excerpt, *verbatim*
  [MAR §4.2]:

```
not node min(L1) ← node(L1), node(L2), L2 < L1.
node min(L) ← ¬not node min(L), node(L).
start round 1i () ← node min(#L,L), ¬round 1i ().
round 1i ()@next ← start round 1i ().
round 1i ()@next ← round 1i (), ¬start round 2i ().
vote 1i (#N)@async ← start round 1i (), node(N).
complete 1i (#M,N)@async ← vote 1i (#N), all acki (#N), node min(#N,M).
incomplete 1i (#M,N)@async ← vote 1i (#N), ¬all acki (#N), node min(#N,M).
...
senti () ← ¬all acki ().
senti ()@next ← senti (), ¬vote 1i ().
start round 2i () ← ¬not all recv 1i (), ¬not all comp 1i (), node min(#L,L).
vote 2i (#N)@async ← start round 2i (), node(N).
complete 2i (#M,N)@async ← vote 2i (#N), all acki (#N), ¬senti (#N), node min(#N,M).
incomplete 2i (#M,N)@async ← vote 2i (#N), senti (#N), node min(#N,M).
done recursioni () ← ¬not all recv 2i (), ¬not all comp 2i ().
```

  Correctness relies on causality: acknowledgments arrive after the facts they acknowledge, and round-2
  responses arrive after all round-1 responses.

The worked example, distributed garbage collection, *verbatim* [MAR App. B]:

```
addr(Addr)@async ← addr edb(Addr).
refers to(#M, Src, Dst)@async ← local ptr edb(#N, Src, Dst), master(#M).
refers to(Src, Dst)@next ← refers to(Src, Dst).
reach(Src, Dst) ← refers to(Src, Dst).
reach(Src, Next) ← reach(Src, Dst), refers to(Dst, Next).
garbage(Addr) ← addr(Addr), root edb(Root), ¬reach(Root, Addr).
garbage(Addr)@next ← garbage(Addr).
```

Without coordination, garbage can be derived "prematurely", before every `refers_to` has arrived, and
reachable addresses end up in `garbage`. After the rewrite the relevant rule becomes (App. C, *verbatim*):
`garbage(Addr) ← addr edb(Addr), root edb(Root), ¬reach(Root, Addr), reach done( ).` The resulting
program has a single ultimate model: the one in which ¬reach waits until reach is complete.

**Relation to Bloom (from the paper):** "Dedalus_S corresponds closely to Bloom". The coordination
rewrite generalizes the coordination code a programmer would write by hand. **This is the formal basis
for automatically inserting coordination at points of order.**

---

## 8. Causality and the CRON results [CRON; DI §4.2]

**The conjecture** [DI Conjecture 2]: "Causality Required Only for Non-monotonicity (CRON). Program
semantics require causal message ordering if and only if the messages participate in non-monotonic
derivations." It was motivated by crash recovery (replaying a message log "from the future") and by
speculative execution.

**Formal setting** [CRON §3.6, §5]:

* The **output** of a run is the set of *ultimate* facts over `out(P)`: facts output by `deduc_P` in
  every transition of node x from some transition on. The node that produces a fact is ignored.
* P is **consistent** if, for every input, every fair run has the same output `outInst(P,H)`.
  Consistency is undecidable [CRON App. A].
* **Expressivity and complexity** [CRON §4]. Assume every node has `Id` (its own identity) and `Node`
  (all nodes). Consistent Dedalus programs compute exactly the **While** queries. The evaluation problem
  is in PSPACE and PSPACE-hard for some programs (a Turing-machine simulation, App. B). Non-monotone
  queries *need* `Id` and `Node` or equivalent information.
* **Non-causal semantics:** `pure_SZ(P)` is `pure(P)` with every `before` relation deleted (SZ stands for
  Saccà–Zaniolo). Local finiteness is kept. An **SZ-model** is a locally finite stable model of
  `pure_SZ(P)`. A consistent P **tolerates non-causality** if every SZ-model yields `outInst(P,H)`.

**Results:**

* The semantic form ("P computes a monotone query ⟺ P tolerates non-causality") **fails in both
  directions** [CRON §6.1].
  * *If* fails: a program computing the *non-monotone* emptiness query tolerates non-causality
    (Fig. 3, *verbatim*):

    ```
    empty(x) | y ← ¬S( ), Id(x), Node(y).
    empty(y)• ← empty(y).
    missing( ) ← Node(y), ¬empty(y).
    T ( ) ← Id(x), ¬missing( ).
    ```
  * *Only-if* fails: a program computing the *monotone* non-emptiness query does not tolerate
    non-causality (Fig. 4, *verbatim*):

    ```
    A( ) | x ← S( ), Id(x).
    A( )• ← A( ).
    B( ) | x ← A( ), ¬sentB ( ), Id(x).
    sentB ( )• ← A( ).
    T ( ) ← A( ), B( ).
    T ( )• ← T ( ).
    ```

    A B() sent once, at step 1, can be delivered at step 0 before any A(), which erases the program's
    only chance to produce T().
* **Theorem 6.1:** *every positive, consistent Dedalus program tolerates non-causality.* Positivity alone
  is not enough, because a positive program need not be consistent (Fig. 5, *verbatim*):

  ```
  A( ) | x ← Id(x).
  B( ) | x ← Id(x).
  T ( ) ← A( ), B( ).
  T ( )• ← T ( ).
  ```

  T() requires A() and B() to be delivered *at the same step*, and some fair runs never do that.

**Implementation consequences (analysis):**

1. For positive (negation- and aggregation-free), consistent components, crash recovery may load the whole
   message log at once instead of replaying it in causal order.
2. The CALM/confluence analyzer needs a notion of **persistence-guardedness**. Positive Datalog is only
   order-insensitive when every async-fed relation it joins is persisted, which is Marczak's *guarded
   asynchrony*. Fig. 5 is the minimal counterexample and should be a regression test for the analyzer.

---

## 9. How time and clocks are modeled (summary across the literature)

| Setting | Clock model | Source |
|---|---|---|
| Dedalus0 | One logical time line; `successor` relation isomorphic to ℤ; each timestep is one "instant"; no global clock implied | [TR §2], [DL11 §2] |
| Dedalus (async) | Per-agent local logical clocks; the arrival time is nondeterministic (choice); "says nothing about the relationship between the agents' clocks" | [DL11 §5.4] |
| Causal semantics | Local step counters starting at 0; happens-before as a strict partial order (`before`), or equivalently vector clocks in Datalog¬ | [TPLP §4.4], [DS-TR §4.1] |
| Lamport clocks | A user program built from entanglement plus the `p_wait` buffer; the receiver's clock jumps across quiescent steps | [DL11 §5.6] |
| Wall clock | `now()` foreign function; Bloom `periodic` collections; soft-state TTL | [DI §3.3.1], [BUD] |
| Synchronous Dedalus | A global round number read from an "input tape" of naturals; every node shares it; communication is an inductive rule to a DSR relation | [SYNC] |
| BSP/rsync | Global clock, bounded skew, bounded delay; the "syncausality" relation adds virtual NULL messages | [BSP] |
| Molly | Global synchronous rounds 1..EOT; EDB `clock(From,To,SndTime,RcvTime)` with RcvTime = SndTime+1, or NEVER if the message is dropped | [LDFI §4.1.1], [MOLLY-SRC] |
| Hydroflow/DFIR | Per-process tick counter; consecutive ticks with no gaps; a batch of inputs per tick; fixpoint per tick; `defer_tick` means next tick | [HFD] |

**Takeaway (analysis).** The engine should keep **one local, gap-free tick counter per node**, the
Dedalus step. It should also expose three things. First, an optional *time-skip* optimization when the node
is quiescent. It must be observationally equivalent to heartbeats. Its only visible effect is that an
entangled body variable (§4.5) can observe the jump. Second, the current step as a bindable value
(entanglement), gated behind analysis warnings. Third, wall-clock as a foreign relation that is
nondeterministic per step.

---

## 10. CALM formalizations related to Dedalus

### 10.1 Relational transducer networks [RT]

* **Transducer schema** Υ = (Υin, Υout, Υmsg, Υmem, Υsys), with Υsys = {Id/1, All/1}. A transducer has one
  query per output relation, insertion and deletion queries `Q_ins^R` and `Q_del^R` per memory relation, and
  a send query `Q_snd^R` per message relation. All of them read in ∪ out ∪ msg ∪ mem ∪ sys.
* **Local transition** I, I_rcv → J, J_snd, with I′ = I ∪ I_rcv: in and sys are unchanged; out only grows
  (`J|out = I|out ∪ Q_out(I′)`); memory is updated as `(I|R ∪ R+) \ R−` with `R+ = Q_ins \ Q_del` and
  `R− = Q_del \ Q_ins` (no-op on conflict); messages are `J_snd = ⋃ Q_snd(I′)`. Local transitions are
  deterministic.
* **Network:** a finite, connected, undirected graph. Sends are *epidemic*, meaning a message goes to all
  neighbors. Buffers are multisets and a delivered multiset is read as a set. Fairness: every node is
  active infinitely often, and a fact that stays in a buffer forever is delivered infinitely often.
  *k-delivery* variants bound the size of delivered batches.
* **Output:** the output facts at a *quiescence point*, after which out never changes (Prop. 4.1: every
  run has one). **Consistent:** every fair run on every horizontal partition gives the same output.
  **Network-independent:** the same result on every network.
* **Oblivious:** the transducer does not read Id or All. **Inflationary:** it never deletes from memory.
* **Coordination-free** [RT §5.1]: for every input I there *exists* a horizontal partition and a run that
  reaches quiescence using only heartbeat transitions, i.e. with no communication.
* **Results:**
  * Thm 4.9: every query in L ⊇ UCQ¬ is distributedly computable, by collecting everything with acks and
    then raising a `ready` flag. This construction reads Id and All.
  * Thm 4.10: every monotone query is computable by an oblivious, inflationary, monotone transducer.
  * Prop 5.7: coordination-freeness is undecidable for FO transducers.
  * Prop 5.8: network-independent + oblivious ⇒ coordination-free.
  * Thm 5.10: coordination-free ⇒ monotone.
  * **Thm 5.11 (CALM):** for L ⊇ UCQ, coordination-free ⟺ oblivious ⟺ monotone.
  * Thm 5.13: reading only All still yields only monotone queries. Id is what enables non-monotone ones.
  * The original "iff expressible in Datalog" form (Conjecture 5.9) is false, because there are monotone
    queries outside Datalog.

### 10.2 Weaker monotonicity and policy awareness [SURV §3.1; WM; FT]

*Not read in the original TODS/PODS paper (see §0.2).* From the secondary sources:

* **Policy-aware transducers** [WM]: each node knows the distribution policy, i.e. which facts it is
  "responsible" for. A node can then conclude that a fact is *globally absent* without communicating.
* **Domain-distinct-monotone** (Q(I) ⊆ Q(I ∪ J) whenever every fact of J contains a value not in adom(I)):
  exactly the Boolean queries that are coordination-free in the policy-aware setting [FT Thm 36, citing
  Ameloot et al.]. Example: R \ S for unary R and S [SURV].
* **Domain-disjoint-monotone** (J shares no values with I): coordination-free under *domain-guided*
  distributions, where each node gets all facts that mention the values it is responsible for [SURV].
  Example: the complement of transitive closure.
* Monotone ⊂ domain-distinct-monotone ⊂ domain-disjoint-monotone. [SURV] says Datalog variants capturing
  each class exist. I could not verify which variants from a primary source.
* **Semi-monotone Datalog¬¬** [WM]: relations are either only tested positively and only inserted into, or
  only tested negatively and only deleted from. Its disorderly (nondeterministic fixpoint) semantics is
  eventually consistent, and win-move becomes coordination-free.

> **Relevance (analysis):** if our system exposes partitioning metadata (hash or domain-guided
> sharding) to the analyzer, some negations can be certified coordination-free, e.g. anti-joins on the
> partition key where both sides are co-partitioned. This is the theory behind partition-aware
> points-of-order analysis.

### 10.3 Synchronous and BSP settings [SYNC; BSP]

* **Datalog¬_ST** [SYNC]: two rule forms. Local rules have head time t. Inductive rules have head time
  t′ = succ(t) and may address another node's *distributed shared relation* (DSR) `S(@l, …)`. The input tape
  supplies the global round number. The operational semantics is a synchronous transducer network. Its
  perfect model equals the union over rounds of the node states (Theorem 1). Negation is controlled by
  temporal stratification.
* **BSP/rsync** [BSP]: synchronous rounds, reliable delivery, bounded delay. Under *broadcasting*, every
  query, monotone or not, is computable in 2 rounds (Lemma 1), and Ameloot's coordination-free definition
  stops distinguishing anything: the emptiness query becomes "coordination-free". The paper uses *hashing
  transducer networks* (content-based addressing on key columns) and *syncausality*, i.e. Lamport's
  happens-before plus virtual NULL messages implied by bounded delay. With the **Snapshot Closed World
  Assumption** (SCWA), negation on a communication relation R is safe once NULL_R has arrived from all
  |N| nodes. The **coordination pattern** is a point from which predicate-level syncausality reaches every
  node, i.e. broadcast.
  * **Theorem 1** (arXiv:1405.7264v3 extended version; the AMW paper proves only the *if* direction for
    "chained" queries): a query is parallelly computable by a coordination-free transducer network
    **iff it is monotone and connected**. "Chained" in the AMW version: every body literal is
    linked to every other through shared variables; nullary relations are not connected.
  * Corollary 4: coordination-free ⟺ monotone and distributes over components.
  * Theorem 2: for L ⊇ UCQ, computable by an oblivious, inflationary transducer ⟺ embarrassingly parallel.

### 10.4 Hellerstein's conjectures and later work

[DI §4]:

* **Conjecture 1, CALM:** "A program has an eventually consistent, coordination-free execution strategy if
  and only if it is expressible in (monotonic) Datalog." Proven in the transducer form in [RT], with
  "Datalog" replaced by "monotone".
* **Conjecture 2, CRON:** see §8.
* **Conjecture 3, Dedalus Time ⇔ Coordination Complexity:** "The minimum number of Dedalus timesteps
  required to evaluate a program on a given input data set is equivalent to the program's Coordination
  Complexity", with coordination complexity measured in strata.
* **Conjecture 4, Fateful Time:** "Any Dedalus program P can be rewritten into an equivalent
  temporally-minimized program P′ such that each inductive or asynchronous rule of P′ is necessary:
  converting that rule to a deductive rule would result in a program with no unique minimal model."

I found no resolution of Conjectures 3 and 4 in the papers I read. Newer related work, abstracts only:
[FT] studies *free termination* (when a node may terminate unilaterally) and bridges transducers to
semiautomata and CRDTs. [CCALM] generalizes CALM to arbitrary specifications with refinement orders.
[KCALM] gives an accessible proof sketch and the definitions ("confluence of program outcomes").

---

## 11. Negation over asynchronous relations (consolidated)

This is the subtlest part of the semantics.

1. **Operational meaning.** At node y, step t, the literal `¬m(ā)` means "m(ā) is not in the deductive
   fixpoint of y at t". That fixpoint includes the messages delivered *at* t and any copies persisted
   from earlier steps. Since arrival steps are nondeterministic, the truth of such a negation is
   **schedule-dependent**.
2. **Syntax still allows it.** The async edge is not part of the deductive reduction, so
   `p(X)@async :- q(X), notin p(X)` and `r :- notin msg(...)` pass temporal stratification. The program has
   stable models, just possibly many ultimate models (diffluence). Examples: the marriage ceremony (§7.2),
   garbage collection (§7.5), and "did c1 and c2 arrive together?" (*verbatim* [MAR App. F]):

   ```
   p(#L,X)@async ← q(#L,X), ¬r(#L,X).
   r(X)@next ← q(X).
   r(X)@next ← r(X).
   concurrent() ← p(n1,c1), p(n1,c2).
   concurrent()@next ← concurrent().
   ```
3. **Negation over a *persisted* async-fed relation** is still non-monotone *in time*. `¬vote(C,A,X,"Y")`
   can be true early and false later. Protocols make it safe by combining (a) knowledge of the full
   membership (All/Node/`agent`) with (b) waiting until the set is complete, which is **coordination**.
   Example: the Molly 2PC coordinator (§12.5) with `missing_vote(C, X) :- agent(C, A), running(C, X),
   notin vote(C, A, X, "Y");`.
4. **Under non-causal delivery,** negation over messages admits paradoxes (TPLP Fig. 4, CRON Fig. 4).
5. **Fixes:**
   * stay in Dedalus⁺ by pushing negation down to EDB;
   * gate the negation with a sealing predicate (`p_done`) synthesized by the coordination rewrite;
   * use a coordination protocol written by the programmer;
   * rely on policy awareness (§10.2);
   * use synchrony, i.e. SCWA in BSP (§10.3).

---

## 12. How Molly uses Dedalus [LDFI; MOLLY-SRC]

### 12.1 Concrete syntax accepted by Molly's parser (`DedalusParser.scala`, verbatim excerpts)

```scala
lazy val ident = "[a-zA-Z0-9._?@]+".r
lazy val semi = ";"
lazy val number = "[0-9]+".r ^^ { s => s.toInt}
lazy val string = "\"[^\"]*\"".r ^^ { s => s.stripPrefix("\"").stripSuffix("\"")}
lazy val followsfrom = ":-"
lazy val timesuffix: Parser[Time] =
  "@next" ^^ { _  => Next() } |
  "@async" ^^ { _ => Async() } |
  '@' ~> number ^^ Tick
lazy val op = "==" | "!=" | "+" | "-" | "/" | "*" | "<" | ">" | "<=" | ">="
...
lazy val aggregate = ident ~ "<" ~ ident ~ ">" ^^ { ... Aggregate(aggName, aggCol) }
lazy val clause: Parser[Clause] = include | rule | fact
lazy val include = "include" ~> string <~ semi ^^ Include
lazy val fact = head <~ semi
lazy val rule = head ~ followsfrom ~ body <~ semi ^^ { ... Rule(head, body) }
lazy val bodyTerm = predicate ^^ { Left(_) } | expr ^^ { Right(_) }
lazy val predicate = opt("notin") ~ ident ~ "(" ~ repsep(atom, ",") ~ ")" ~ opt(timesuffix) ^^ ...
override val whiteSpace = """(\s|//.*|(?m)/\*(\*(?!/)|[^*])*\*/)+""".r
```

Observed features:

* Negation is written `notin`.
* Aggregates look like `count<X>`, `max<X>`, `min<X>` and may appear anywhere in a head.
* Expressions are right-nested and have no precedence (`Expr(Constant, op, Expression)`), e.g. `S+C`,
  `T-1`, `Cnt2 > Cnt1 / 2`.
* Strings are double-quoted. Bare identifiers are passed through unchanged to the C4 evaluator
  (`C4CodeGenerator.genAtom`). By the C4/Overlog convention, and in every example, variables are
  capitalized and constants are quoted strings or integers. `_` is a wildcard. The code generator moves
  negated atoms to the end of the body "as a workaround for a C4 bug".
* `include "file";` inlines another file.
* Facts carry `@<int>`. **A body atom may also carry a constant time `@1`**, as in
  `notin bcast(X, Pl)@1`. That is an extension beyond the TR/DL11 restriction that all body atoms share
  one time variable.
* **The location specifier is the first column of the first body predicate.** The AST notes this
  "Match[es] the Ruby solver's convention". Types INT, STRING and LOCATION are inferred by union-find over
  unified columns (`DedalusTyper.scala`). The first column of every table must have type LOCATION.
* There is **no syntax for entanglement** (binding the body time to a variable) and no `persist[...]`
  macro. Persistence is written out by hand.

### 12.2 Execution model

* **Synchronous simulation** [LDFI §2.1]: delivered messages arrive in a deterministic order, and the tool
  explores *failures*, not reorderings. The authors acknowledge that this forfeits completeness for
  asynchronous executions.
* **Failure spec** ⟨EOT, EFF, Crashes⟩: runs are bounded to EOT rounds, message omissions are only allowed
  at times < EFF (with EFF < EOT, so the protocol can recover), and at most `Crashes` fail-stop crashes
  occur. Molly sweeps EOT and EFF automatically [LDFI §2.1.1].
* **Clock EDB** (`FailureSpec.generateClockFacts`, *verbatim* excerpt):

```scala
val temporalFacts = for (
  from <- nodes; to <- nodes; t <- 1 to eot
  if !crashes.exists(c => c.node == from && c.time <= t)  // if the sender didn't crash
) yield {
  val messageLost = omissions.exists(o => o.from == from && o.to == to && o.time == t)
  val deliveryTime = if (from != to && messageLost) NEVER else t + 1
  Predicate("clock", List(StringLiteral(from), StringLiteral(to), IntLiteral(t), IntLiteral(deliveryTime)), ...)
}
val localDeductiveFacts = for (node <- nodes; t <- 1 to eot) yield {
  Predicate("clock", List(StringLiteral(node), StringLiteral(node), IntLiteral(t), IntLiteral(t + 1)), ...)
}
val crashFacts = for (crash <- crashes; node <- nodes; t <- 1 to eot) yield {
  Predicate("crash", List(StringLiteral(node), StringLiteral(crash.node), IntLiteral(crash.time), IntLiteral(t)), ...)
}
```

`NEVER = 99999` plays the role of ⊤ from [TR]. `crash(Observer, CrashedNode, CrashTime)` gets a time
column appended, and **every node sees every crash at every time**, i.e. a perfect failure detector
(analysis). The paper says a crashed node stops making internal transitions [LDFI §2.1.1]. In the code at
this commit, however, `localDeductiveFacts` produces `clock(n,n,t,t+1)` for *every* node, crashed or not,
so crashed nodes keep evaluating local and `@next` rules and are only prevented from sending. Assertions
exclude crashed nodes explicitly (`notin crash(_, X, _)`). **(Observed discrepancy; if we reimplement
Molly we must choose deliberately.)**

* **Rewrite to plain Datalog** (`DedalusRewrites.referenceClockRules`). Every body atom gets a time column
  `NRESERVED`. Then, depending on the rule kind:
  * *Deductive:* append `clock(Loc, Loc, NRESERVED, _)` to the body, which binds the time variable
    positively (range restriction). The head time is `NRESERVED`.
  * *Inductive:* append `clock(Loc, _, NRESERVED, _)`. The head time is `NRESERVED+1`.
  * *Async:* append `clock(Loc, To, NRESERVED, MRESERVED)`, where `To` is the head's first column. The head
    time is `MRESERVED`, the receive time.
  * `@<int>` in a rule head is rejected with the error "Rule head can't only hold at a specific time step".

  [LDFI §4.1.1] presents the paper version with a 3-ary clock and `SndTime+1`. The code uses a 4-ary clock
  that carries the receive time.
* **Aggregates** are split in two, *verbatim* doc comment: `agg(X, count<Y>) :- a(X, Z), b(Z, Y)` becomes
  `agg_vars(X, Y, Z) :- a(X, Z), b(Z, Y)` and `agg(X, count<Y>) :- agg_vars(X, Y, _)`. This keeps
  provenance bindings from changing the grouping.
* **Provenance rewrite** (after Köhler et al.): for every rule r, a "firings" relation `r_prov<i>` records
  the bound variables. Derivation graphs are built by querying the firings.
* **Invariants:** users define `pre` and `post`. A run is a counterexample if some `pre` row has no
  matching `post` row at EOT. If they are undefined, all persistent relations are treated as outcomes
  [LDFI §2.3]. Assertion rules routinely **join across locations**, e.g.
  `missing_log(A, Pl) :- log(X, Pl), node(X, A), notin log(A, Pl);`. This works because the simulation is
  one centralized Datalog program. An engine enforcing body locality needs an *assertion mode* that
  evaluates over a global snapshot.

### 12.3 LDFI algorithm, formal core [LDFI §4.3, App. B]

* A **fault set** is a set of `clock` facts to delete. A **falsifier** of a goal g is a fault set D with
  g ∈ P(E) but g ∉ P(E∖D). It is minimal if no proper subset is a falsifier.
* Each proof tree of a goal becomes a disjunction of message-omission variables `O(from,to,time)` and crash
  variables `C(node,time)`, any one of which falsifies that proof. Conjoining over all proof trees gives a
  CNF. Every SAT solution is a *potential* counterexample and is confirmed by a concrete rerun. UNSAT
  certifies the program for that Fspec and input.
* For negated goals, the conservative option over-approximates the possible causes of absence by taking
  any positive fact from which the negated relation is statically reachable, up to the goal's timestamp.
  The optimized version counts only paths through an odd number of negations.
* Soundness holds trivially because every candidate is confirmed by execution. **Completeness (Thm B.2):**
  every minimal falsifier is contained in some falsifier that LDFI returns.

### 12.4 Molly's own expected results (`Harness.scala`, verbatim table data)

These are ready-made end-to-end test oracles (nodes `a,b,c`, plus `C` as coordinator for commit protocols):

| Programs | EOT | EFF | Crashes | Counterexample expected |
|---|---|---|---|---|
| simplog + deliv_assert | 4 | 2 | 0 | **yes** |
| rdlog + deliv_assert | 25 | 23 | 0 | **yes** |
| rdlog + deliv_assert | 4 | 2 | 1 | **yes** |
| replog + deliv_assert | 8 | 6 | 1 | **yes** |
| classic_rb + deliv_assert | 5 | 3 | 0 | **yes** |
| 2pc + 2pc_assert | 5 | 0 | 1 | **yes** |
| 2pc_ctp + 2pc_assert | 6 | 0 | 1 | **yes** |
| 2pc_timeout + 2pc_assert_optimist | 6 | 0 | 2 | no |
| 3pc + 2pc_assert | 8 | 0 | 2 | no |
| 3pc + 2pc_assert | 9 | 7 | 1 | **yes** |
| kafka | 6 | 4 | 1 | **yes** (nodes a,b,c,C,Z) |
| kafka | 6 | 4 | 0 | no |
| ack_rb + deliv_assert | 8 | 6 | 1 | no |

The "scenarios" table in the same file, which uses the automatic sweep, also records:

* classic_rb fails under omissions (0 crashes) but is robust in the fail-stop model (2 crashes);
* replog is robust with 0 or 1 crashes;
* naive 2pc finds no counterexample with 0 crashes but does with 1 or 2;
* 2pc_timeout with the optimist assertion is fine with 1 or 2 crashes;
* 3pc finds no counterexample with 1 or 2 crashes;
* tokens finds a counterexample with 1 crash and none with 0.

### 12.5 Representative Molly programs (verbatim)

`delivery/simplog.ded`:

```
// simple broadcast.  make an attempt to send a message to all neighbors
include "./bcast_edb.ded";

node(Node, Neighbor)@next :- node(Node, Neighbor);
log(Node, Pload)@next :- log(Node, Pload);

log(Node2, Pload)@async :- bcast(Node1, Pload), node(Node1, Node2);
log(Node, Pload) :- bcast(Node, Pload);
```

`delivery/bcast_edb.ded`:

```
node("a", "b")@1;
node("a", "c")@1;
node("b", "a")@1;
node("b", "c")@1;
node("c", "a")@1;
node("c", "b")@1;

bcast("a", "hello")@1;
```

`delivery/deliv_assert.ded`:

```
// someone has a log, but not me.
missing_log(A, Pl) :- log(X, Pl), node(X, A), notin log(A, Pl);//, notin crash(_, A, _);

pre(X, Pl) :- log(X, Pl), notin bcast(X, Pl)@1, notin crash(X, X, _);
post(X, Pl) :- log(X, Pl), notin missing_log(_, Pl);
```

`delivery/rdlog.ded` adds retry: `bcast(N, P)@next :- bcast(N, P);`. `delivery/ack_rb.ded`:

```
node(Node, Neighbor)@next :- node(Node, Neighbor);
log(Node, Pload)@next :- log(Node, Pload);
ack(S, H, P)@next :- ack(S, H, P);
rbcast(Node2, Node1, Pload)@async :- log(Node1, Pload), node(Node1, Node2), notin ack(Node1, Node2, Pload);
ack(From, Host, Pl)@async :- rbcast(Host, From, Pl);
rbcast(A, A, P) :- bcast(A, P);
log(N, P) :- rbcast(N, _, P);
```

`commit/2pc.ded`:

```
include "./2pc_edb.ded";

// coordinator logic
prepare(Agent, Coord, Xact)@async :- running(Coord, Xact), agent(Coord, Agent);
abort(C, X)@next :- vote(C, _, X, "N");
commit(C, X)@next :- vote(C, _, X, "Y"), notin missing_vote(C, X);
missing_vote(C, X) :- agent(C, A), running(C, X), notin vote(C, A, X, "Y");
running(Coord, Xact) :- begin(Coord, Xact);
running(C, X)@next :- running(C, X), notin commit(C, X), notin abort(C, X);
commit(A, X)@async :- commit(C, X), agent(C, A);
abort(A, X)@async :- abort(C, X), agent(C, A);

// agent logic
vote(Coord, Agent, Xact, "Y")@async :- prepare(Agent, Coord, Xact), can(Agent, Xact);
prepared(A, C, X, "Y") :- prepare(A,C,X), can(A,X);

// frame rules
agent(C, A)@next :- agent(C, A);
can(A, X)@next :- can(A, X);
abort(C, X)@next :- abort(C, X);
commit(C, X)@next :- commit(C, X);
vote(C, A, X, S)@next :- vote(C, A, X, S);
prepared(C,A,X,Y)@next :- prepared(C,A,X,Y);
```

(A commented-out alternative `prepare` rule is omitted.) `commit/2pc_assert.ded`:

```
pre("termination", X) :- prepared(_, _, X, _);
post("termination", X) :- decision(A1, X, _), decision(A2, X, _), A1 != A2;
decision(C, X, "c") :- commit(C, X);
decision(C, X, "a") :- abort(C, X);
disagree(X) :- decision(_, X, V1), decision(_, X, V2), V1 != V2;
pre("decide",  X) :- decision(_, X, _);
post("decide", X) :- decision(_, X, V), notin disagree(X);
```

`util/timeout_svc.ded`, a reusable logical timer:

```
timer_state(H, I, T-1)@next :- timer_svc(H, I, T);
timer_state(H, I, T-1)@next :- timer_state(H, I, T), notin timer_cancel(H, I), T > 1;
timeout(H, I) :- timer_state(H, I, 1);
```

The repository also has `paxos_synod.ded` (with `count<>`, `max<>`, arithmetic `seed(A, S+C)@next`,
majority test `Cnt2 > Cnt1 / 2`), `util/leader.ded`, `util/heartbeat.ded`, `raft/election.ded`,
`ramp/*.ded`, `flux/*.ded`, `kafka.ded`, `chain_replication.ded` and `3pc.ded`. All are useful parser and
engine corpora.

---

## 13. Modern descendants: syntax and semantics worth copying

### 13.1 Chu et al., SIGMOD 2024 [QRW §2.3]

* Location and time are the **two rightmost** attributes of every IDB relation.
* Body literals share `l` and `t`. Library functions such as `hash` are modeled as *infinite EDB
  relations* usable only with bound inputs, i.e. binding patterns and lazy evaluation. They are replicated
  at every node and time.
* Rule kinds are called **synchronous** (= deductive), **sequential** (= inductive, `t'=t+1`) and
  **asynchronous** (different head l′, t′, using the built-in `delay((tuple…), t')` relation, a.k.a. choose
  or chosen, "constrained to reflect Lamport's happens-before … t < t′").
* Example, *verbatim* [QRW Listing 2]:

```
hashset(hashed,val,l,t') :− toStorage(val,leaderSig,l,t), hash(val,hashed),
    verify(val,leaderSig), t'=t+1
hashset(hashed,val,l,t') :− hashset(hashed,val,l,t), t'=t+1
collisions(val2,hashed,l,t) :− toStorage(val1,leaderSig,l,t), hash(val1,hashed),
    hashset(hashed,val2,l,t)
numCollisions(count<val>,hashed,l,t) :− collisions(val,hashed,l,t)
fromStorage(l,sig,val,collCnt,l',t') :− toStorage(val,leaderSig,l,t),
    hash(val,hashed), numCollisions(collCnt,hashed,l,t), sign(val,sig),
    leader(l'), delay((sig,val,collCnt,l,t,l'),t')
```

* **Components** are rule sets deployed together on one node. A component's inputs are the IDB relations
  it references but does not define; its outputs are the relations it defines but does not reference.
  Correctness of a rewrite is judged by linearizability-style history equivalence: the optimized program
  must produce the same outputs *with the same timestamps* as some run of the original. Rewrites include
  mutually independent decoupling, monotonic decoupling (redirect plus persist), functional decoupling,
  partitioning with co-hashing, and FD-based partitioning. They were applied to voting, 2PC and Paxos
  (2PC ×5, Paxos ×3 throughput).

### 13.2 Hydroflow Datalog front-end [HFD grammar.rs]

```
Declaration := .input  Ident `rust`      | .output Ident `rust`
             | .persist Ident            | .async  Ident `send-pipeline` `recv-pipeline`
             | .static Ident `rust`      | Rule
Rule        := Target (":-" | ":+" | ":~") Atom ("," Atom)* "."?
               -- ":-" Sync (same tick), ":+" NextTick, ":~" Async
Target      := Ident ("@" TargetExpr)? "(" TargetExpr,* ")"     -- "@node" only allowed with ":~"
Atom        := "!" Rel | Rel | "(" IntExpr BoolOp IntExpr ")"
Rel field   := Ident | "_" | "*" Extract (flatten) | "(" Extract,* ")" (untuple)
TargetExpr  := IntExpr (+ - * %) | min(x) | max(x) | sum(x) | count(*) | count(x,..)
             | choose(x) | collect_vec(x,..) | index()
```

Semantics from `lib.rs`:

* a `:+` rule routes through `defer_tick()`;
* a `:~` rule requires `@node` in the head, and a non-async rule may not have one;
* `.persist` compiles to the delta-vs-store scheme of §3.6;
* the example `result@b(a) :~ ints(a, b)` sends to node `b`.

The `choose(x)` aggregate is a nondeterministic pick per group, a user-visible form of choice.

### 13.3 DFIR process loop [HFD life_and_times.md]

"(1) … ingest a batch of data items … (2) Run the DFIR spec … until it reaches a 'fixpoint' on the current
batch … any data that appears in an outbound channel is streamed … (3) … advance the local clock and then
start the next tick."

Ticks are consecutive with no gaps. The current `defer_tick` doc says buffered data "will cause the next
tick … to fire (non-lazy)", and `defer_tick_lazy` exists as a variant that does not trigger a tick.
**(analysis)** DFIR *streams* async output during the fixpoint, while [TPLP] applies `async_P` to the
*completed* fixpoint. The two agree whenever each async rule's body only reads strata that are complete
when the rule fires. A stratified scheduler guarantees that, so pipelining sends is a valid optimization.

### 13.4 Bloom surface mapping [BUD; THESIS §3.3]

| Bloom | Dedalus |
|---|---|
| `<=` | deductive rule (same timestep) |
| `<+` | inductive rule (`@next`) |
| `<-` | derive into `p_neg` for the next step (deferred deletion) |
| `<~` | async rule (`@async`) |
| `table` | relation with a mutable persistence rule |
| `scratch` | relation with no persistence |
| `channel` | async-fed relation with a location specifier |
| `periodic` | wall-clock timer events |

[THESIS §3.3]: "Below the surface, Bloom is almost indistinguishable from Dedalus: it shares the same
first-order data model and model-theoretic semantics."

---

## 14. Edge cases and subtleties (checklist for the spec and the test suite)

1. **Deletion is visible one step late.** `p_neg(c)@k` removes `p(c)` from k+1 onward. `p(c)` still holds
   at k [DL11 Ex. 3].
2. **Insert beats delete** in pure Dedalus when both happen at the same step via separate rules (§3.3).
   Transducers use no-op instead [RT]. This must be specified.
3. **Ephemeral by default.** A relation without a persistence rule holds only at the step where it is
   derived or delivered. That includes received messages and EDB "events" given as `r(...)@C`.
4. **EDB conventions differ.** Facts can be timestamped events [TR, DL11, Molly `@1`] or hold at every
   step [MAR, TPLP, CRON]. Support both: *static/base* relations, true at all steps, and *timestamped
   input events*.
5. **Deductive reduction must be stratified.** Negation and aggregation through `@next`/`@async` edges is
   allowed. `p :- notin p` is illegal. `p@next :- notin p` is legal but oscillates, i.e. is temporally
   unsafe.
6. **Inductive and async rules read the completed deductive fixpoint.** Their bodies may negate or
   aggregate relations from any stratum [TPLP §5.1.2].
7. **Inductive and async rules are single-step, not recursive fixpoints.** A `@next` rule never sees its
   own output within the same step.
8. **The body-locality restriction:** all body atoms must share a location and a time. Cross-node joins
   require an explicit async hop, or a compiler localization rewrite (§4.8).
9. **Communication rules must be `@async`.** Local `@async` (send to self) is allowed and arrives strictly
   later under causality (§5.3).
10. **Set semantics per send step.** The same message fact derived by several rules at one step is one
    message with one arrival time [TPLP §4.3.1]. Copies sent at *different* steps are distinct messages
    that may arrive at different steps [TPLP §5.1.1]. At the receiver a batch is read as a set.
11. **Messages to unknown addresses are dropped** [TPLP §5.1.3].
12. **Loss is a semantic option, not the default.** [TR]/[DL11] allow ⊤. The fair semantics [TPLP, MAR,
    CRON] forbid loss. Molly models loss as fault injection. The runtime needs both a "fair network"
    (reliable eventual delivery, the default for correctness reasoning) and a "lossy network"
    (testing/Molly) mode.
13. **Timestamps are local.** There is no global time. Two nodes' step counters are unrelated except
    through happens-before.
14. **Async arrival can be arbitrarily late but is finite** under fairness. Only finitely many messages
    arrive per step [TPLP §4.5].
15. **Negation over async-fed relations is schedule-dependent** (§11). This is legal, but the analyzer
    must flag it.
16. **A positive program is not order-insensitive unless its async-fed relations are persisted**
    [CRON Fig. 5; MAR guarded asynchrony].
17. **Quiescence ≠ silence.** A node whose state has stopped changing may still emit the same async
    messages every step (retry loops such as `rdlog`). The engine cannot simply sleep. It has to model
    resend semantics, e.g. by pacing steps with a heartbeat timer, or prove the resends redundant under
    reliable channels.
18. **Time skipping:** jumping a quiescent node's clock forward is only observable via entanglement
    (§4.5–4.6). When entanglement is used, the jump must equal the number of skipped steps. The skip can
    also be exposed deliberately as Lamport-style clock advance.
19. **Arithmetic in heads** (`X+1`) and entanglement create new constants, which can make a program
    non-quiescent and give it infinitely many distinct facts. Temporal-safety analysis must treat such
    rules as unsafe unless they are guarded.
20. **Aggregates over empty groups produce no row**, per the GROUP BY semantics [DL11 §3.4]. Tests that
    need "count = 0" must use negation.
21. **`<` over the active domain** is assumed total in [MAR]. The ∀ encoding and the "minimum node =
    master" idiom rely on it. Values of every type need a deterministic total order.
22. **Molly-style assertion rules are non-local and time-specific** (`@1` in the body) and are evaluated
    at EOT (§12.2). They belong to a separate "global observer" rule class.
23. **Crash semantics in Molly** as implemented: crashed nodes still compute locally but cannot send, and
    all nodes see `crash` facts (§12.2).
24. **Stable-model ≠ minimal-model for choice programs.** Choice programs have many stable models.
    Negation-free Dedalus with async still has many stable models (one per timing), but a unique ultimate
    model when it is consistent.
25. **Unsafe rules** (a head variable not bound positively, e.g. `p(A, B) ← q(A)`) are errors.
    Temporally unsafe programs (oscillation, unbounded counters) are warnings [DL11 §4.2].

---

## 15. Known limitations, later fixes, and recommendations

### 15.1 Limitation → later fix

| Limitation in early Dedalus | Later treatment |
|---|---|
| No formal semantics for async; "temporal paradoxes" allowed [TR, DL11] | Stable models of `pure(P)` with causality and finiteness rules, equivalent to fair operational runs [DS-TR, TPLP] |
| ⊤ (message loss) inside the semantics | Fairness excludes loss [TPLP]; loss is handled as explicit fault injection [LDFI] |
| Entanglement is expressive but breaks finiteness | Excluded from formal fragments [MAR, DS-TR, THESIS]; Lamport clocks remain possible only with entanglement |
| Output undefined | Ultimate models and ultimate facts [MAR, CRON] |
| No confluence guarantees | Dedalus⁺ / Dedalus_S plus the coordination rewrite [MAR]; CALM-based analysis [KCALM, THESIS §3.2] |
| "Monotone ⇒ order-insensitive" stated informally | CRON Thm 6.1 (positive + consistent ⇒ tolerates non-causality) and counterexamples [CRON] |
| Modular vs local stratification confusion [TR] | Local stratification and perfect models [DL11] |
| Rederivation cost of persistence | Algorithm 1 [TR]; Hydroflow `.persist` [HFD] |
| Asynchronous-only theory | Synchronous variants [SYNC, BSP]; Molly's synchronous simulation [LDFI] |
| Rules unstructured at scale | Bloom modules [THESIS §3.3]; components [QRW] |

### 15.2 Recommended engine shape (analysis)

1. **IR = unsugared Dedalus.** Every relation carries explicit `loc` and `time` columns. Every rule is
   tagged deductive/inductive/async. Async rules carry an explicit `delay`/choice term. Every analysis in
   this report (temporal stratification, safety, PDG strata, Dedalus⁺/Dedalus_S membership, the coordination
   rewrite, the pure(P) translation, the Molly clock rewrite, provenance) is a rewrite or check over this IR.
2. **Per-node step loop,** following [TPLP §5.1.3] exactly:
   `I = base ∪ events(s) ∪ carried(s) ∪ delivered(s)`; `D = stratified_seminaive_fixpoint(deduc, I)`;
   `carried(s+1) = induc(D)`; `outbox += async(D)`. Only `induc` and `async` read the finished D.
3. **Persistence as storage.** Recognize identity-persistence rules, with or without a `notin p_neg` guard,
   and store those relations with deltas. Feed Δ into semi-naive evaluation. Other inductive rules run as
   one-step queries. This is Algorithm 1 and HFD `.persist`.
4. **Incremental across steps.** The deductive fixpoint at step s+1 should be maintained from step s, since
   most facts are persisted. Non-monotone strata need DRed-style or counting-based maintenance, or
   recomputation. Aim for incremental view maintenance (the "materialized recursive view maintenance" that
   [TR §1] mentions as a Datalog benefit).
5. **Quiescence detection and time skipping:** no inputs, no deliveries, carried state equal to the
   previous step, and an empty or identical outbox. Handle the resend case of §14 item 17.
6. **Networking semantics modes:** (a) *fair asynchronous*, the default: reliable eventual delivery, any
   order, batching allowed, arrival strictly after send; (b) *lossy*: Molly/⊤; (c) *synchronous rounds*:
   BSP and Molly EOT simulation; (d) *non-causal replay* for recovery of positive components (CRON).
7. **Deterministic simulator** that explores schedules (arrival choices, batchings, node orders) over the
   operational semantics, plus a Molly-style synchronous fault explorer with provenance and SAT. Optionally
   export `pure(P)` to an ASP solver as a spec-level oracle on small instances (analysis; Lobo et al. took
   the ASP route but I did not read that paper).

---

## 16. MUST-IMPLEMENT CHECKLIST

Each item gives a one-line description and its source.

**Language core**

1. **Time suffix:** every relation has an implicit timestamp attribute. Facts hold at exactly one timestep
   unless re-derived. [TR §2.1; DL11 §2.1]
2. **Location specifier:** one attribute (the first by convention, marked `#`/`@`) names the node owning
   the tuple, with horizontal partitioning. [DL11 §5.2; MAR §2.1]
3. **Deductive rules** (`:-`, no annotation): the head holds at the same node and step as the body.
   [DL11 §2.2; TPLP §4.1.1]
4. **Inductive rules** (`@next`): the head holds at the same node, step+1, evaluated once on the completed
   fixpoint. [DL11 §2.1; TPLP §5.1.2]
5. **Asynchronous rules** (`@async`): the head holds at the head-location node at a nondeterministically
   chosen later step; communication rules must be async. [DL11 §5.3–5.4; TPLP §4.1]
6. **Body locality:** all body atoms share one location variable and one time variable, checked
   statically. [TR §2.1; DL11 §5.2]
7. **Head-location syntax** for async rules (`p(#D, …)@async`, or Hydroflow's `p@D(…) :~`), with the head
   location bound in the body. [MAR §2.1; HFD]
8. **Timestamped facts** `p(c…)@C;` as input events at step C, plus *static* base relations true at every
   step. [DL11 §2.2; MAR §2.2; TPLP §4.3.2]
9. **Negation** (`notin`/`¬`/`!`) with safety: every head, negated and comparison variable is bound by a
   positive atom. [TPLP §3.2; DS-TR §2.2]
10. **Comparisons and arithmetic** (`== != < > <= >= + - * / %`) in bodies and heads. [MOLLY-SRC parser;
    HFD grammar]
11. **Aggregates in heads** (min, max, count, sum, plus choose/collect as extensions) with GROUP BY
    semantics, stratified as non-monotone. [DL11 §3.4; MOLLY-SRC; HFD]
12. **`persist[p, p_neg, n]` macro / `.persist` declaration**, expanding to
    `p@next :- p, notin p_neg`. [DL11 §3.2; HFD]
13. **Deferred deletion and update idiom:** `p_neg@T` removes p from T+1; an update is `p_neg@T` plus
    `p_new@T+1`. The insert-vs-delete conflict rule must be documented. [DL11 §3.2; RT §2]
14. **Include/modules:** `include "file";`, plus a component/module notion. [MOLLY-SRC; QRW §2.4]
15. **Comments** `//`, `/* */`, `#` (HFD). [MOLLY-SRC; HFD]
16. **Entanglement (optional, gated):** bind the body step as a value (`q(A)@N`), required for the Lamport
    clock idiom and flagged by the analyzer. [DL11 §5.5–5.6; DS-TR fn. 1]
17. **Built-in relations:** node / all / Id (self), total order `<` over values, wall-clock `now()` as a
    nondeterministic foreign relation, library functions as infinite relations requiring bound inputs.
    [MAR §2.1; RT §3.1; DI §3.3.1; QRW §2.3]
18. **User-level `choose((X),(Y))`**, a nondeterministic functional dependency, and/or a `choose(x)`
    aggregate. [DL11 §5.1; HFD]

**Static analysis**

19. **Deductive-reduction stratification** (temporal stratification). Reject negation or aggregation
    cycles that lie entirely within deductive rules. [DL11 Defs 1–3, Lemma 2]
20. **Instantaneous safety:** deductive rules must be range-restricted. [DL11 Def 4]
21. **Temporal-safety warning:** instantaneous-predicate least fixpoint plus the rule kinds 1–3 test; warn
    on oscillation or unbounded counters. [DL11 Defs 5–8, Lemma 4]
22. **PDG with edge kinds** (deductive/inductive/async, negated), SCC collapse, async-recursive
    detection. [MAR §4.1–4.2]
23. **Dedalus⁺ check** (semipositive plus guarded asynchrony), which certifies confluence. [MAR §3.2]
24. **Dedalus_S check** (no negation cycle through any edge, guarded asynchrony), which certifies
    confluence under the coordination rewrite. [MAR §4.1]
25. **Points-of-order / CALM report:** flag negation and aggregation over async-fed (or ephemeral
    async-fed) relations, and reads of `All`/`Id` (non-oblivious). [THESIS §3.2; RT Thm 5.11; CRON Fig. 5]
26. **Coordination rewrite:** synthesize `p_done` sealing predicates (acks, done_node, ∀ via `<`, two-round
    voting for async-recursive SCCs) and guard negations with them. [MAR §4.2, App. C–D]

**Runtime and semantics**

27. **Per-node local transition:** `D = deduc*(state ∪ delivered)`, `next = base ∪ induc(D)`,
    `send = async(D)`. [TPLP §5.1.3]
28. **Stratified semi-naive fixpoint per step,** with persistence implemented as storage plus deltas
    (Algorithm 1). [TR §5; DI fn. 9; HFD]
29. **Quiescence detection** ("equivalent modulo time") and skipping of quiescent steps, with correct
    handling of steady-state resends. [DL11 Defs 5–7, Lemma 3; TR §5]
30. **Message semantics:** a multiset in flight, a set per delivered batch, one arrival time per (sender,
    send step, receiver, fact), arrival strictly after send (causal), finitely many per step, fair eventual
    delivery by default. [TPLP §4.3–4.5, §5.1]
31. **Lossy and fault modes:** ⊤/NEVER delivery and fail-stop crashes, for testing. [TR §6.3; LDFI §2.1.1;
    MOLLY-SRC]
32. **Ultimate-model output:** compute "eventually always true" facts for a declared output schema, and
    compare them across runs to test confluence. [MAR §2.2; CRON §3.6]
33. **Schedule-exploring simulator** over the operational semantics (active-node choice, delivered subset
    choice, heartbeats). [TPLP §5.1; RT §3]
34. **Synchronous simulation mode** (global rounds, delivery at t+1) for Molly and BSP reasoning. [LDFI
    §2.1; SYNC; BSP]
35. **Non-causal replay** of message logs for positive, consistent components, e.g. crash recovery.
    [CRON Thm 6.1]

**Verification tooling (Molly on Dedalus)**

36. **Clock-relation rewrite:** each rule joins `clock(From, To, SndTime, RcvTime)`, with deductive,
    inductive and async variants and a `crash(Observer, Node, Time)` relation. [LDFI §4.1.1;
    MOLLY-SRC DedalusRewrites/FailureSpec]
37. **Provenance ("firings") capture per rule,** with aggregates split into a bindings rule and an
    aggregate rule, and derivation-graph extraction. [LDFI §4.1.2–4.2; MOLLY-SRC]
38. **`pre`/`post` invariant relations** evaluated globally at EOT, and the Fspec ⟨EOT, EFF, Crashes⟩
    sweep. [LDFI §2.1.1, §2.3]
39. **CNF of falsifiers** over O(from,to,t) / C(node,t) and SAT-guided search, with concrete re-execution
    for soundness. [LDFI §4.3, App. B]

**Formal spec artifacts**

40. **`pure(P)` translation** (dynamic choice rules (1)–(6), causality rules (7)–(10), finiteness rules
    (11)–(14)) as the normative definition of async semantics, usable as a test oracle. [TPLP §4.3–4.5]

---

## 17. TEST PROGRAMS (end-to-end), with expected behavior

Expected behavior is quoted or derived from the cited source. "UM" = ultimate model.

**Dedalus0 (single node, deterministic)**

1. **Persistence and deletion** [DL11 Ex. 3], program in §3.3. Expect `p_pos(1,2)` at 101..300, absent
   from 301 on. `p_pos(1,3)` from 102 onward. Quiescent from 302 on (the state at 302 is equivalent modulo
   time to the state at 301).
2. **Temporally stratifiable insert/delete** [DL11 Ex. 4]. Must be *accepted* even though it is not
   syntactically stratifiable. A `delete_p(T)` at T removes every p fact from T+1 onward.
3. **Unsafe rule** `p(A, B) ← q(A);` [DL11 Ex. 6]. Must be *rejected* as not range-restricted.
4. **flip_flop** [DL11 Ex. 7]. Accepted with a temporal-safety warning. Alternates (0,1) and (1,0) forever
   (period 2). The UM for `flip_flop` is empty.
5. **Toggle/announce** [DI Fig. 1], with a seed `state(0)@1` added by us. `state` alternates 0/1 every
   step; announce copies arrive asynchronously.
6. **Sequence** [DL11 §3.3]. With `event` facts at chosen steps, `seq` increments exactly at those
   steps and otherwise keeps its value. One value per step.
7. **Priority queue** [DL11 §3.4]. The paper lists the four input tuples as `priority_queue(...)@123`,
   but the program drains `m_priority_queue`. Load them into `m_priority_queue` at 123 (the paper's
   listing looks like a naming slip). Per-user discipline (derived): at 124, `priority_queue` holds
   (bob,bash,200), (eve,ls,1) and (alice,ssh,204) *simultaneously* (one per user); at 125 it holds
   (bob,ssh,205); after that the queue is empty. Global-FIFO variant (drop A from `omin`): eve@124,
   bob/bash@125, alice@126, bob/ssh@127.
8. **Queue** [DI Fig. 2]. The same idea, noting the GROUP BY caveat in §3.3.
9. **Counter / request-response** [THESIS §3.1]. The counter increments once per step that has any
   request, and each requester gets the pre-increment value.
10. **Soft-state TTL** [DI §3.3.1]. A fact disappears once `now() - Birth ≥ TTL` unless refreshed. Use a
    mocked `now()`.
11. **Timer service** `util/timeout_svc.ded` [MOLLY-SRC]. `timer_svc(H,I,3)@k` yields `timeout(H,I)` at
    step k+2 unless cancelled. (Derived: state 2@k+1, 1@k+2.)

**Asynchrony and semantics oracles**

12. **Reliable broadcast** [DL11 §5.7]. In fair runs every member eventually has `rdeliver`. Under loss
    plus a crashed sender, all non-failed nodes agree.
13. **Lamport clock p_wait** [DL11 §5.6]. A message tagged with send time N is released at the receiver
    only after local time > N. Requires entanglement.
14. **Marriage ceremony** [MAR Ex. 3/5]. Two UMs over schedules: {runaway()} and {}. **The analyzer must
    flag it as diffluent.**
15. **Marriage, Dedalus⁺ version** [MAR Ex. 4]. Exactly one UM on every schedule. The analyzer must
    certify it as Dedalus⁺.
16. **Garbage collection** [MAR App. B]. Some schedules put reachable addresses in `garbage`. After the
    coordination rewrite [MAR App. C], `garbage` equals the true unreachable set on every schedule.
17. **Max-element dequeue** [MAR App. E]. Confluent (b holds the max of b_edb) but *not* Dedalus⁺. The
    analyzer should not certify it, and a schedule explorer should find only one UM.
18. **Concurrent arrival** [MAR App. F]. `concurrent()` is in the UM iff c1 and c2 are delivered at the same
    step, so the program is diffluent.
19. **Async singleton** [MAR Ex. 1–2]. `p(#L)@async ← q(#L)` with q EDB. The UM set over all stable models
    is {{}, {p(n1)}}. p is *not persisted*, so p(n1) is ultimate only when arrivals eventually cover every
    step (a cofinite arrival set). The program is diffluent even with fair delivery. Adding
    `p(L)@next ← p(L)` (guarded asynchrony) makes it confluent, with UM {p(n1)}.
20. **Reachability "covered"** [TPLP Fig. 1]. Each node eventually derives `covered()` at every step iff
    all its local vertices are reachable from the distributed start vertices.
21. **Random ordering** [TPLP Fig. 2]. The output F/N is a total order of S whenever no two elements are
    ever delivered together.
22. **2PC agent/coordinator** [TPLP Fig. 3 / Ex. 3]. Every agent eventually stores the coordinator's
    decision: "no" if any agent votes no, else "yes".
23. **Non-causality sensitivity** [TPLP Fig. 4]. Under causal semantics `T()` is always produced. Under
    non-causal (SZ) semantics some model lacks it.
24. **Infinite grouping** [TPLP Fig. 5]. Under finite-arrival semantics `T()` is always produced.
25. **Transitive closure broadcast** [CRON Fig. 1]. Every node eventually stores the global transitive
    closure. Consistent, positive, and tolerates non-causality.
26. **Emptiness query** [CRON Fig. 3]. Consistent, non-monotone, *tolerates* non-causality: `T()` iff S
    is empty everywhere.
27. **Non-emptiness via B-once** [CRON Fig. 4]. Consistent, monotone query, does *not* tolerate
    non-causality.
28. **Positive but inconsistent** [CRON Fig. 5]. Some fair runs never produce `T()`. The analyzer must not
    certify it as order-insensitive.

**Fault-injection oracles (Molly corpus, synchronous semantics)**

29–41. The rows of the `Harness.scala` table in §12.4 (simplog, rdlog, replog, classic_rb, ack_rb, 2pc,
2pc_ctp, 2pc_timeout, 3pc, kafka, tokens) with exactly those ⟨EOT, EFF, Crashes⟩ and expected outcomes.
Also: `paxos_synod.ded`, `util/leader.ded` and `raft/election.ded` as parser and engine stress tests with
aggregates and arithmetic. Their expected outcomes are not recorded in the harness, so they are not
oracles here.

**Scaling rewrites**

42. **Hashset leader/storage** [QRW Listings 1–2]. After monotonic, functional and mutually independent
    decoupling and partitioning, outputs (the same facts with the same timestamps) must equal some run of
    the original.

---

*End of report.*

