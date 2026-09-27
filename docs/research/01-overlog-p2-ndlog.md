# 01 — Overlog, P2, NDlog, Declarative Networking, Evita Raced (plus JOL and C4)

Research report for the bloom-remake implementers. It covers the first generation of the Berkeley
distributed-logic work: Declarative Routing (2005), P2/Overlog (2005), NDlog (2006), Loo's thesis
(2006), Evita Raced (2008), the Java runtime JOL used by BOOM Analytics (2009–2010), the C runtime C4,
and the critiques that led to Dedalus. The goal is to give enough precise semantics, syntax, algorithms
and example programs that we can (a) implement an Overlog-compatible surface and (b) avoid the
semantic mistakes that pushed the group toward Dedalus and Bloom.

---

## 0. Sources actually read

I read the full text of the following documents, plus the source code where noted:

| Short name | Document | URL |
|---|---|---|
| **SOSP05** | Loo, Condie, Hellerstein, Maniatis, Roscoe, Stoica. *Implementing Declarative Overlays.* SOSP 2005 (incl. Appendix A Narada, Appendix B Chord) | https://dsf.berkeley.edu/papers/sosp05-p2.pdf |
| **SIGCOMM05** | Loo, Hellerstein, Stoica, Ramakrishnan. *Declarative Routing.* SIGCOMM 2005 (skimmed) | https://dsf.berkeley.edu/papers/sigcomm05-declarenet.pdf |
| **SIGMOD06** | Loo et al. *Declarative Networking: Language, Execution and Optimization.* SIGMOD 2006 (incl. Appendix A/B proofs) | https://dsf.berkeley.edu/papers/sigmod06-declar.pdf |
| **THESIS** | B. T. Loo. *The Design and Implementation of Declarative Networks.* PhD thesis, UCB/EECS-2006-177 (chapters 2, 3, 4.4, 5, 8.1, App. A.3, App. B) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2006/EECS-2006-177.pdf |
| **CACM09** | Loo et al. *Declarative Networking.* CACM 52(11), 2009 | https://www.cis.upenn.edu/~boonloo/papers/declarenet_cacm09.pdf |
| **EVITA** | Condie, Chu, Hellerstein, Maniatis. *Evita Raced: Metacompilation for Declarative Networks.* VLDB 2008 | http://www.vldb.org/pvldb/vol1/1453978.pdf (also https://dsf.berkeley.edu/papers/vldb08-evita.pdf) |
| **NR09** | Navarro, Rybalchenko. *Operational Semantics for Declarative Networking.* PADL 2009 | https://discovery.ucl.ac.uk/1361358/1/padl09.pdf |
| **NJWLS** | Nigam, Jia, Wang, Loo, Scedrov. *An Operational Semantics for Network Datalog* (tech report) | https://www.andrew.cmu.edu/user/liminjia/research/papers/ndlogsemans-tr.pdf |
| **IDODECLARE** | Alvaro, Condie, Conway, Hellerstein, Sears. *I Do Declare: Consensus in a Logic Language.* NetDB 2009 | https://dsf.berkeley.edu/papers/netdb09-idodeclare.pdf |
| **BOOM** | Alvaro et al. *BOOM Analytics: Exploring Data-Centric, Declarative Programming for the Cloud.* EuroSys 2010 (sections 2, 3, 4, 9) | https://dsf.berkeley.edu/papers/eurosys10-boom.pdf |
| **DECLIMP** | Hellerstein. *The Declarative Imperative: Experiences and Conjectures in Distributed Logic.* UCB/EECS-2010-90 (extended PODS 2010 keynote) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2010/EECS-2010-90.pdf |
| **DEDALUS-TR** | Alvaro et al. *Dedalus: Datalog in Time and Space.* UCB/EECS-2009-173 (intro and §7.2 only, for the Overlog critique) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2009/EECS-2009-173.pdf |
| **PADL12** | Loo, Gill, Liu, Mao, Taylor, Zhou, Zhou. *Recent Advances in Declarative Networking.* PADL 2012 (§2) | https://netdb.cis.upenn.edu/papers/dn_padl12.pdf |

Source code examined (all cloned and read):

- **P2** (C++, Intel/Berkeley): https://github.com/declarativitydotnet/p2 — `overlog/ol_lexer.lex`, `overlog/ol_parser.y` (original parser); `lang/parse/olg_lexer.lex`, `lang/parse/olg_parser.y` (Evita Raced parser); `lang/olg/*.olg` (Evita Raced compiler stages written in Overlog: `delta.olg`, `mview.olg`, `localize.olg`, `stratify.olg`, `aggview1.olg`, `magic.olg`, `systemr.olg`, …); `lang/eca/ecaContext.C`; `p2core/scheduler.C`, `p2core/table2.C`, `p2core/oper.h`, `p2core/ID.C`; `elements/aggwrap2.C`, `elements/insert2.h`; `aggregates/*`; `doc/chord.olg`, `doc/seAtomicity.olg`; `doc/tutorial/UserGuide.tex` ("Getting Started with Overlog and P2").
- **JOL** (Java Overlog Library, used for BOOM Analytics): https://github.com/bloom-lang/jol — `src/jol/lang/parse/*.rats` (grammar), `src/jol/core/runtime.olg` (the scheduler, itself in Overlog), `src/jol/core/Driver.java`, `src/jol/lang/plan/Rule.java`, `src/jol/types/table/{Table,Aggregation,TimerTable}.java`, examples.
- **C4** (C Overlog runtime by the BOOM group): https://github.com/bloom-lang/c4 — `src/libc4/parser/ol_parse.y`, `router.c`.

**Could not access:** Y. Mao, *On the Declarativity of Declarative Networking* (NetDB 2009 / SIGOPS OSR 43(4), https://dl.acm.org/doi/10.1145/1713254.1713260). Every copy I tried (ACM DL, ResearchGate, CiteSeerX, Semantic Scholar, the NetDB'09 site) was blocked or gone. What I say about it below comes only from its abstract as quoted by search engines ("classifies rules into deductive rules and Event-Condition-Action (ECA) rules… ECA rules that are less declarative are dominantly used in most of the proposed systems") and from how DEDALUS-TR §7.2 and PADL12 §2.2 cite it. I also did not read the Overlog Paxos source (`bitbucket.org/neilconway/overlog-paxos`); that Bitbucket repository no longer exists. I cite Liu et al. ICDE 2009 (BDD provenance for deletions) and Nigam et al. PPDP 2011 only as they are cited in CACM09 and PADL12; I did not read them.

---

## 1. Lineage and timeline

| Year | Artifact | What it contributed |
|---|---|---|
| 2004–05 | Declarative Routing (HotNets'04, SIGCOMM05), on the PIER engine | Routing protocols as recursive queries; distance-vector vs. dynamic source routing are the same query with the predicate order swapped. |
| 2005 | **P2 + OverLog** (SOSP05) | C++ dataflow engine (Click-style elements, about 20k LOC). OverLog adds `materialize` (soft-state tables with lifetime, size and key), event streams, `periodic`, `@` location specifiers, `delete` rules, and aggregates `agg<X>`. Narada in 16 rules, Chord in 47. |
| 2006 | **NDlog** (SIGMOD06, THESIS) | A cleaner, provable subset: Datalog plus location specifiers plus link-restricted rules; the localization rewrite; BSN and PSN (pipelined semi-naive) with proofs; "bursty update" eventual consistency; soft-state rule taxonomy. |
| 2007–08 | **Evita Raced** (EVITA) | The Overlog compiler rewritten in Overlog (metacompiler) over relational catalog tables. Stages include delta rewrite, localization, stratification, System R, Cascades, magic sets, histograms and a broadcast rewrite. It also defines the "one event per fixpoint" semantics. |
| 2009 | CACM09 survey; NR09 and Mao critiques; IDODECLARE (2PC and Paxos in Overlog on JOL) | Semantic ambiguities catalogued; consensus idioms identified. |
| 2009–10 | **JOL** plus BOOM Analytics (BOOM) | Java Overlog: typed `define`, `timer`, `async` rules, `#insert`/`#delete` event modifiers, Stasis-durable tables, metaprogramming. HDFS, Hadoop, Paxos and 2PC in Overlog. |
| 2009–10 | Dedalus (DEDALUS-TR), DECLIMP, C4 | Time reified as data. Overlog's "chain of fixpoints" semantics is replaced by model-theoretic semantics. C4 is a low-latency Overlog runtime from the same group. |

Terminology: **NDlog** is the formal, monotone, link-restricted core. **Overlog** is the full practical
language with soft state, events, deletes, key overwrite and periodic timers. CACM09 says Overlog "makes
some compromises between expressive richness and semantic guarantees."

---

## 2. Data model

### 2.1 Relations, tuples, values

- A program is a set of relations, which are sets of tuples. Datalog naming conventions apply: predicates, function symbols and constants start lower-case; variables start upper-case; `_` is a fresh "don't care" variable (the P2 lexer rewrites each `_` to a unique `$_N`).
- Arity and field types are **inferred from use** in P2/NDlog. SOSP05 says "the number and types of fields in relations are inferred from their (consistent) use in the program's rules." JOL and C4 instead require an explicit typed schema (§3.3, §3.4).
- **P2 value types** (`doc/types.txt`): `int32, uint32, int64, uint64, double, null, string, opaque`, timestamps, and a 160-bit `ID` type (`p2core/ID.h`: 5×32-bit words, arithmetic modulo 2^160, with the literal form `0x1234I`). All integer arithmetic is done in signed 64-bit. `infinity` lexes as the integer `-1`. The Evita parser also accepts `true` and `false` (as 1 and 0), vectors `[a,b]` and matrices `{[..],[..]}`.
- **Location specifier.** Every predicate has exactly one attribute marked `@`, whose value is a network address (e.g. `"IP:port"`, or anything else addressable). A tuple *lives at* the node named by its location specifier. "Network communication is implicit in OverLog: tuples must be stored at the address in their location specifier, and hence the runtime engine has to send some of its derived tuples across the network" (EVITA §2.1). SIGMOD06 Definition 1: "A location specifier is an attribute of type address in a predicate that indicates the network storage location of each tuple."
  - **Syntax variants.**
    - SOSP05 annotates the predicate: `member@Y(Y, A, ...)`, `f_now@Y()`.
    - SIGMOD06 puts `@` on every address-typed field, with the first field as the locspec: `path(@S,@D,@Z,P,C)`.
    - CACM09, THESIS and later P2 mark only the locspec field, in any position: `path(@Src,Dest,Path,Cost)`, `pingMsg(S,@D,E)`.
    - The Evita parser lexes `@Var` as a location variable token.
  - **NDlog address type safety** (SIGMOD06 Def. 6.2): "A variable that appears once in a rule as an address type must not appear elsewhere in the rule as a non-address type."
- **Local tuple**: a tuple whose locspec equals the local node's address (EVITA). **Local rule**: every predicate, head included, has the same locspec variable (SIGMOD06 Def. 3).

### 2.2 Table kinds: hard state, soft state, events

`materialize(name, lifetime, maxSize, keys(k1,...,kn)).` (P2 and NDlog; exact grammar is in `ol_parser.y`):

- `lifetime`: seconds, or `infinity`. `infinity` means **hard state**; finite means **soft state**; `0` means an **event** relation.
- `maxSize`: the maximum number of tuples, or `infinity`. When the table is full, P2 evicts in FIFO order of insertion/refresh time. EVITA footnote 3: "The P2 runtime replaces tuples in 'full' tables according to a FIFO order as needed during execution; replaced tuples are handled in the same way as tuples displaced due to primary-key overwrite."
- `keys(...)`: the primary-key **field positions**. In P2 these are **1-based and the location field is position 1**. For example, Chord's `materialize(node, infinity, 1, keys(1))` keys `node(NI,N)` on `NI`, and `finger ... keys(2)` keys `finger(NI,I,B,BI)` on `I`. `keys()` (empty) means "the implicit tuple ID is the primary key" (`table2.h` comment), which gives bag-like storage. NR09 formalizes keys as K(p) ⊆ {1..n} and *assumes* 1 ∈ K(p).
- **Any relation with no `materialize` is an event (stream) relation** (SOSP05; THESIS Def. 2.8: "An event relation is a soft-state relation with zero lifetime"). Event tuples are not stored: "input event tuples persist long enough for rule execution to complete and are then discarded" (THESIS §2.5.2).
- JOL has no lifetimes. There, `define(name, keys(0,1), {Type,...})` declares a (hard) table with **0-based** keys, and `define(name, {Type,...})` with no `keys` declares an event. Durability is per table (`stasis(...)`, committed via Stasis at the end of each fixpoint). Soft state in JOL is written by hand with `timer` plus `delete` rules.

### 2.3 Primary-key semantics: insert, refresh, replace, expire, evict

This is the precise algorithm from `p2core/table2.C` (`Table2::insert`, `flush`, `flushEntry`, `updateTime`), consistent with CACM09 §2.4.2 and THESIS §2.5.1 and §4.4.

```
insert(t):
  flush()                                   -- drop expired and over-size entries first
  if ∃ s in table with key(s) = key(t):
      if s == t:                            -- identical tuple
          if table has finite lifetime:
              s.time := now; move s to the front of the time queue
              fire REFRESH listeners(s)      -- "RefreshEvent"
          return false                      -- NOT a new insertion (no insert delta)
      else:                                 -- same key, different values: UPDATE
          remove(s)                         -- fires DELETE listeners(s)
  add t with time := now; fire INSERT listeners(t)
  flush()
  return true

flush():   -- invoked lazily on every insert, lookup and scan
  while oldest.time < now - lifetime: remove(oldest)   -- expiry (fires delete listeners)
  while size > maxSize:               remove(oldest)   -- FIFO eviction
```

Key points for implementers:

1. **Refresh vs. update.** "When a tuple is derived, if there exists another tuple with the same primary key but differences on other fields, an update occurs, in which the new tuple replaces the previous one. On the other hand, if the two tuples are identical, a refresh occurs, in which the existing tuple is extended by its TTL" (CACM09).
2. **Primary-key overwrite is an implicit delete followed by an insert.** Downstream rules see a deletion delta for the old tuple and an insertion delta for the new one (THESIS §5.4: "inserting a tuple where there is another tuple with the same primary key is considered an update, where the existing tuple is deleted before the new one is inserted").
3. **P2 expired tuples lazily**, on access. EVITA's event loop instead notes a "current time" at the start of each fixpoint and skips tuples that are expired as of that time. Lazy expiry makes the timing of the resulting delete deltas nondeterministic, since they depend on when the table is next touched. **We should expire deterministically at tick boundaries.**
4. **Hard-state tables** keep a derivation count per tuple (the counting algorithm) and delete a tuple only when the count reaches 0. **Soft-state tables** do not count derivations; a re-derivation just refreshes (THESIS §4.4, §2.5.1).
5. **Refresh is observable.** P2 exposes refresh listeners, and EVITA's delta streams "convey insertions, deletions, or timeout refreshes". THESIS Algorithm 5.6 compiles *refresh strands* so that refreshing a body tuple re-derives, and therefore refreshes, the soft-state heads that depend on it ("cascaded refresh").

### 2.4 Built-in relations and functions

- **`periodic(@N, E, T [, K])`**: a built-in event stream at node `N` that fires every `T` seconds, up to `K` times (infinite if `K` is omitted). `E` is a fresh random 64-bit event ID (THESIS Def. 2.9; P2 tutorial). It may appear only in rule bodies. `periodic(@X,E,0,1)` fires once at startup and is the standard initialization idiom (Narada `e1`–`e4`, Chord `i1`–`i4`).
- **JOL timers**: `timer(Name, physical|logical, Period, TTL, Delay);`. A rule references the timer as `Name(Period, TTL, Delay)`. Physical timers are in milliseconds; logical timers count fixpoints. TTL is the number of firings. Example: `timer(ltimer, logical, 2, 5, 1);`.
- **C4 timers**: `timer(name, ms);`.
- **Functions** begin with `f_`. Those used in the papers: `f_now()` (local wall clock), `f_rand()`, `f_coinFlip(p)`, `f_init(S,D)`, `f_concatPath(N,P)`, `f_inPath(P,N)`, `f_head`, `f_tail`, `f_isEmpty`, `f_cons`, `f_contains`, `f_idgen()`, `f_sha1`, `f_tostr`, `f_size`, `f_ifelse` (the target of `c ? a : b`). About 90 exist in the P2 source tree, many of them catalog helpers for Evita (`f_getattr`, `f_adornment`, `f_project`, `f_merge`, …). SOSP05's `f_now@Y()` (a function evaluated at another node) did not survive into later versions.
- **Aggregates**:
  - SOSP05, THESIS and CACM09 write `agg<X>` in the head: `min<D>`, `max<D>`, `count<*>`, `sum<X>`.
  - P2 source uses `a_` prefixes: `a_MIN<D>`, `a_MAX<D>`, `a_COUNT<*>`, `a_COUNTDISTINCT<X>`, `a_AVG`, `a_MKLIST`, `a_MKSET` (`aggregates/` directory).
  - An aggregate may sit on the location field: `lookup@BI(min<BI>, K, R, E)` (SOSP05 L3) or `l3 lookup(min<@BI>,K,R,E)` (THESIS). The group's result then decides where the tuple is sent.
  - JOL adds `generic<expr>`, `topk<V,k>`, `bottomk<V,k>`, `limit<V,k>`, and Java-defined aggregates.
  - C4 has `avg, count, max, min, sum`.
- **Range predicate `X in (A,B]`** in four open/closed forms `(A,B)`, `(A,B]`, `[A,B)`, `[A,B]`. This is **ring (modular) interval inclusion**, not linear. `p2core/oper.h` gives the exact semantics, for example for `(f,t]`:
  ```
  inOC(v,f,t) = (v > f && v <= t) || (t <= f && v > f) || (v <= t && t <= f)
  ```
  So when `t <= f` the interval wraps, and `(n, n]` is the whole ring. Chord relies on exactly this.

---

## 3. Syntax reference (the concrete grammars)

### 3.1 P2 OverLog (`overlog/ol_parser.y`, original planner)

```
program     := clause*
clause      := rule | fact | materialize | watch | watchmod | trace | traceTable | query | stage
materialize := 'materialize' '(' name ',' VALUE ',' VALUE ',' 'keys' '(' [VALUE {',' VALUE}] ')' ')' '.'
watch       := 'watch' '(' name ')' '.'
watchmod    := 'watchmod' '(' name ',' STRING ')' '.'        -- modifiers, see below
stage       := 'stage' '(' STRING ',' name ',' name ')' '.'   -- external C++ table function
fact        := functor '.'
rule        := [NAME] ['delete'] functor ':-' term {',' term} '.'
query       := 'Query' functor '.'
term        := functor | Var ':=' expr | bool_expr
functor     := name '(' [ '@' Var ',' ] args ')'
aggregate   := AGG '<' Var '>' | AGG '<' '@' Var '>' | AGG '<' '*' '>'
bool_expr   := expr relop expr | expr 'in' range | '!' bool | bool '&&' bool | bool '||' bool
relop       := '==' | '!=' | '<>' | '<' | '>' | '<=' | '>='
expr        := expr (+ - * / % << >> & | ^) expr | '~' expr | f_name '(' args ')' | atom | Var[..] | '(' expr ')'
atom        := int | float | "string" | null | infinity | 0x..I | [vector] | {matrix}
comments    := /* nested */   %% line   #! line  (and C-preprocessor #define / -D macros)
```

Notes:

- The rule name comes *before* the head (`r1 pong(@J,I) :- ping(@I,J).`). Rules end with `.`.
- `:=` is assignment. EVITA footnote 2: "OverLog's assignments are strictly syntactic replacements of strings with expressions; they are akin to '#define' macros." NDlog papers write `P = f_init(S,D)` for the same thing.
- **Known bug**, documented in the P2 user guide: constants inside *body* predicates are silently ignored, so `predicate(@Me, 5)` behaves like `predicate(@Me, _)`. The workaround is `predicate(@Me, C), C == 5`. We must of course implement constant matching correctly.
- **Watch modifiers**: `i` InsertEvent, `r` RefreshEvent, `d` DeleteEvent, `c` RecvEvent, `p` PeriodicEvent, `a` AddAction (before insert), `z` DeleteAction, `s` SendAction, `b` BeforeJoin, `j` AfterJoin, `h` HeadProjection. These are introspection taps on the dataflow.
- The original parser has **no negation**. SOSP05 wrote `not member@Y(...)` in the paper, but its Appendix A says "handling of negation is still incomplete, requiring that we rewrite some rules to eliminate negation", and uses the `count<*> == 0` idiom instead (§8.2).

### 3.2 Evita Raced OverLog (`lang/parse/olg_parser.y`)

This extends §3.1 with:

- `namespace name { statements }` (qualified names `::sys::rule`);
- `index(table, "type", keys(...))` for secondary indexes;
- **`notin pred(...)`** for stratified negation;
- `c ? a : b` (compiled to `f_ifelse`), `**` exponent, `|||` append;
- `weak ref`/`strong ref` and `says(...)<pred>` / `materializeSays` (the secure-networking "SecLog" extensions, out of scope);
- `new<V, loc, functor>` constructor terms ("compound tuples").

### 3.3 JOL (`src/jol/lang/parse/Core.rats`, a Rats! PEG grammar)

```
Program     := 'program' Name ';' (Clause ';')*          -- statements end with ';'
Clause      := Rule | Watch | Timer | Load | Fact | Import | Stasis | Define
Define      := 'define' '(' Table ',' 'keys' '(' ints? ')' ',' '{' Type {',' Type} '}' ')'   -- table
             | 'define' '(' Table ',' '{' Types '}' ')'                                      -- event
Stasis      := 'stasis' '(' Table ',' Keys ',' Schema ')'                                     -- durable table
Timer       := 'timer' '(' Table ',' ('physical'|'logical') ',' int ',' int ',' int ')'
Watch       := 'watch' '(' Table [',' [taeidrs]+] ')'
Import      := 'import' JavaTypeName
Load        := 'load' '(' "file" ',' Table [',' "sep"] ')'
Rule        := ['public'] ['async'] [Name] ['delete'] Head ':-' Body
Predicate   := ['notin'] TableName ['#insert' | '#delete'] '(' args ')'
TableName   := [ns '::'] lowerName
Aggregate   := Name '<' expr '>' | generic<..> | topk<V,k> | bottomk<V,k> | limit<V,k>
expr        := C/Java-like, with ?: || && == != <> < > <= >= << >> + - * / % ! casts,
               method calls, field refs, 'new Class(...)', [lists], 'x in [a - b)' ranges
```

- `#insert` and `#delete` let a rule trigger on the insertion or deletion *delta* of a table.
- `async` rules run on a thread pool and their results come back in a later timestep.
- `public` rules may fire on tables that belong to another program.
- Location specifiers (`@X`) are optional. Rules without them are purely local.

### 3.4 C4 (`src/libc4/parser/ol_parse.y`)

```
define(name, [memory|sqlite,] {@locType, type, ...});   timer(name, ms);
[name] [delete] head(...) :- [notin] p(...), ..., qual;  aggregates avg|count|max|min|sum
```

### 3.5 NDlog-specific syntax (SIGMOD06, THESIS)

- `#link(@S,@D,...)` is a **link literal**: a link relation used in a rule body (Def. 4).
- `#include(sp2,sp3,sp4)` is a macro that includes earlier rules.
- `Query shortestPath(@S,D,P,C).` names the output of interest.
- `materialize(#link, 10, infinity, keys(1,2)).`

---

## 4. Semantics

There are four layers, from the most declarative to the most operational. **The inconsistencies
between these layers are the main lesson of this cluster.**

### 4.1 NDlog: model-theoretic semantics (monotone core)

SIGMOD06 Def. 6: "A Network Datalog (NDlog) program is a Datalog program that satisfies the following syntactic constraints:
1. Location specificity: Each predicate has a location specifier as its first attribute
2. Address type safety …
3. Stored link relations: Link relations never appear in the head of a rule with a non-empty body …
4. Link-restriction: Any non-local rules in the program are link-restricted by some link relation.
**Since NDlog is a subset of Datalog, the semantics of a valid NDlog program are exactly those of Datalog.**"

- **Link relation** (Def. 2): a stored relation `link(@src,@dst,...)` describing physical connectivity, assumed bidirectional.
- **Link-restricted rule** (Def. 5): either local, or (i) exactly one link literal in the body, and (ii) every other literal, head included, has its locspec equal to the link's source or destination field. This guarantees that each rule can be rewritten into single-node bodies that send only along links (Claim 1, §5.6). CACM09: "In a fully-connected network environment, an NDlog parser can be configured to bypass the requirement for link-restricted rules."
- Negation was excluded ("we will not consider negated predicates", SIGMOD06 §2). Aggregates must be stratified: EVITA says "P2 only supports programs whose use of negation and aggregation is stratified, that is, there is no aggregation or negation on a recursive cycle of head/body rule dependencies."
- **Eventual consistency under the bursty model** (SIGMOD06 §4; THESIS §5.4). In the *continuous update model* updates arrive faster than fixpoints converge. In the *bursty update model* the network eventually quiesces. **Theorem 3/4:** FP_p = FFP_p, meaning the state PSN reaches after quiescence equals the result of running PSN from scratch on the quiesced base state. This holds centrally, and in a distributed setting **given FIFO delivery along each link**. The proof also needs updates to be applied in arrival order (Claim 3). CACM09 restates the precondition: "as long as the NDlog program is monotonic and messages between two network nodes are delivered in FIFO order."
- Termination. Pure Datalog is polynomial. With function symbols (`f_concatPath`, etc.) it is not, so they rely on termination tests (Krishnamurthy, Ramakrishnan, Shmueli JCSS'96) and on aggregate selections. For example, shortest path with cycles terminates only because of aggregate selection (§6.1).

### 4.2 Soft-state rule taxonomy and its semantics (THESIS §2.5, §5.5, App. A.3)

The THESIS definitions, verbatim in substance:

- **2.6** A *hard-state relation* is materialized with infinite lifetime. **2.7** A *soft-state relation* has a finite lifetime. **2.8** An *event relation* is a soft-state relation with zero lifetime.
- **2.10** A *hard-state rule* has only hard-state predicates in its head and body. **2.11** A *soft-state rule* has at least one soft-state predicate in its head or body.
- **2.12** A *pure soft-state rule* has a soft head and at least one soft body predicate. **2.13** A *derived soft-state rule* has a soft head and only hard body predicates. **2.14** An *archival soft-state rule* has a hard head and at least one soft body predicate.
- **2.15** An *event soft-state rule* has **exactly one** event predicate in its body. The reason: "Since they are not stored, NDlog does not model the possibility of two instantaneous events occurring simultaneously. Syntactically, this possibility is prevented by allowing no more than one event predicate in soft-state rule bodies." The P2 guide likewise lists "At most one event per rule" as a limitation, and both P2's ECA stage and JOL's planner throw "More than one event in rule".

Maintenance semantics:

- **Hard-state rules** use materialized view maintenance: insertions, cascaded deletions via the count algorithm, and updates modeled as delete plus insert.
- **Soft-state rules** use *cascaded refreshes* and **no cascaded deletions**: "all derived soft-state tuples are stored for their specified lifetimes and timeout in a manner consistent with traditional soft-state semantics" (§2.6).
- **Lifetime condition (App. A.3.1).** For a pure soft rule `s :- s1..sm, h1..hn`, with l(ts) ≥ p(ts) + r(ts) (lifetime ≥ derivation time + refresh interval) required for ts(∞)=1:
  - If l(s) ≥ max l(si), the derived tuple is stable.
  - If **l(s) < max l(si)**, "ts(∞) will oscillate between 0 and 1 in the eventual steady state". The proposed fix is a syntactic check that the head's lifetime is at least every soft body lifetime.
- **Derived soft-state rules** (soft head, hard body only) have no refreshes, so in steady state their heads have expired (ts(∞)=0).
- **Archival soft-state rules** (hard head, soft body) are **not eventually consistent**: the hard head persists after its soft support expires. The prescription is to treat them as logging-only, never used as input to other rules, and to enforce that syntactically.

### 4.3 Event semantics (ECA view)

A rule with an event predicate in its body means "*whenever* an event tuple arrives, join it with the *current* contents of the tables, and emit the head." SOSP05: periodic "is a stream… it is more appropriate to read this rule as 'generate a refreshEvent tuple … whenever you see a periodic tuple'." The P2 compiler (`lang/eca/ecaContext.C`) makes this explicit:

- Classify the body:
  - The single non-materialized predicate is **the event**. If there is none and there is exactly one materialized predicate, that predicate becomes the trigger. Rules with no event and more than one table are *first* rewritten by the delta/mview stage (§4.4).
  - Event types:
    - `INSERT`: `periodic`.
    - `DELTA_INSERT`: a table predicate used as a trigger, i.e. fire on insertion into that table.
    - `RECV`: any other event, conceptually a network or queue arrival.
    - `DELTA_DELETE`: removal rules.
  - Every other predicate becomes a `PROBE` (index lookup against the current table).
- Classify the head action:
  - `ADD` if the head is materialized and the rule is not `delete`.
  - `DELETE` if the head is materialized and the rule is `delete`.
  - `SEND` if the head is an event. A send is either local enqueue or remote transmit, depending on its locspec.

**Aggregates in event rules are per-event** (THESIS §5.5.1, `elements/aggwrap2.C`). The strand runs the join for *one* event tuple to completion, aggregates the results, and emits exactly **one** output tuple per event. The group-by fields are taken **from the event tuple** ("Map the event tuple to the result tuple via the external group-by map"). **The output is emitted even when the join is empty**: `count` yields 0 and `min`/`max` yield `null` (`aggregates/aggCount.C`, `aggMin.C`). Programs depend on both halves of this:
- Narada R5/R6 use `count<*>` then `C == 0` as negation.
- The final `doc/chord.olg` comments: "l3 will produce an output even if there are no fingers available with a null min node. Rule l4 ensures that only non-trivial destinations receive a lookup message" (`l4 lookup(@BI,K,R,E) :- forwardLookup(@NI,BI,K,R,E), BI != null.`).

**Aggregates over tables (no event)** are continuously maintained views. They "maintain an up-to-date aggregate … on a table and emit it whenever it changes" (SOSP05 §3.4). Evita's `aggview1.olg` rewrites a multi-table aggregate view into an intermediate materialized join table plus a single-table aggregate rule. In JOL an aggregate head is an `Aggregation` table that keeps its base tuples, so deleting the current min exposes the next min (`types/table/Aggregation.java`). Incremental min/max under deletions costs O(log n) time and O(n) space (SIGMOD06 §4, citing [27]).

### 4.4 Operational semantics: the "chain of fixpoints" (P2/Evita, JOL)

**EVITA §2.1, "Soft State, Events, and Fixpoints":** "In OverLog, events are defined to occur one-at-a-time between fixpoint computations at a given node. Hence each fixpoint computation on the OverLog rules operates with a traditional, static set of stored tuples: (a) the local tuples in materialized tables whose lifetime has not run out, (b) at most one local event fact across all event tables (the current event being processed), and (c) any derived local tuples that can be deduced from (a) and (b) via the program rules." Deletions: "OverLog deletions are defined to occur only after the fixpoint computation of the program that generates them."

**EVITA §2.2.2, the P2 event loop (verbatim steps):**
1. An event is taken from the system input queue, corresponding to a single newly arrived tuple to be inserted in a table ("the current event tuple").
2. The value of the system clock is noted ("the current time"). Soft-state tuples whose lifetime is over as of the current time are skipped (and removed).
3. The current event tuple is, logically, appended to its table.
4. The dataflow runs to a local fixpoint following traditional Datalog semantics, except that "any non-local derived tuples are buffered in a send queue; local deletions are postponed until the end of the fixpoint."
5. At fixpoint completion the send queue is transmitted and buffered deletions are performed.

**What the final P2 code actually does** (`p2core/scheduler.C`, `elements/insert2.h`/`delete2.h`, `doc/seAtomicity.olg`):
- It admits **exactly one external event per fixpoint**: "turn on the gate for exactly 1 external event to flow through". An assertion enforces "*exactly one* tuple from mpSwitch per fixpoint".
- It runs internal work until quiescence.
- Then `mpCommitManager->commit()`. Both `Insert2` and `Delete2` register CommitManager actions, so **table insertions *and* deletions become visible only after the fixpoint**. The regression test `seAtomicity.olg` checks this: in `t22 c(@X,I,a_count<*>) :- b(@X,I), tCounter(@X,I).`, the count must be 0 in the same fixpoint in which `t2 tCounter(@X,I) :- a(@X,I), I < 11.` inserted `tCounter(X,I)`. Otherwise it prints "If you see this msg, there is a bbbuuguguguguggg!!".
- Table insertions then surface as `DELTA_INSERT` events that drive later fixpoints: `t3 a(@X,J) :- tCounter(@X,I), J := I + 1.` counts to 11 across fixpoints.

In other words, final P2 already had Dedalus's `@next` semantics for table mutations, with instantaneous semantics for local events.

**EVITA also admits known deviations:** "the current P2 release has a design flaw in the deferral of processing certain tuples… it may (a) process multiple strata on one node at the same time, and (b) remove delete tuples from materialized tables before the end of the fixpoint that generates them."

**Materialized-view rules (no event)** are compiled by the Evita `mview.olg` stage into pipelined delta rules: one rule per body table predicate, with that predicate marked `DELTA` (i.e. `table1 :- delta table2, table3.` and `table1 :- table2, delta table3.`). A further stage, `delta.olg` (9 rules), generates a **removal rule** for each one: a `DELTA_DELETE` of the body predicate produces a `DELETE` of the head, implementing cascaded deletion. EVITA: "delta table denotes a stream conveying insertions, deletions, or timeout refreshes to tuples of the table".

**NR09's formalization of the implementation.** A state is `⟨M, E⟩`: a keys-consistent materialized store plus an external event multiset. Rule actions are `add | delete | send | exec`, where `send` places into the external queue (with network semantics) and `exec` places into the internal queue (same node). The procedure `Evaluate` has a **step** loop, which selects external events, and a **round** loop, which selects internal events and fires rules against the current `M` into Δ⁺/Δ⁻. Its open parameters are: external selection (one or all), internal selection (one or all), when updates apply (end of round or end of step), and one or two cycles. `Update(M,K,Δ,∇)` is `M := M \ ∇; for m ∈ Δ: M := (M \ {m' | K↓m = K↓m'}) ∪ {m}`, which is non-deterministic when Δ holds two key-conflicting tuples. NR09's finding: "the set of parameters required to emulate the semantics currently implemented in P2 corresponds to the version with two evaluation cycles, selecting one external event for processing each step, fully propagating the internal events in rounds until fix-point, and updating only after the end of the step." An implicit-action rule `h(x,..) :- t(y,..), ...` with an event head corresponds to the pair `send h :- ..., x≠y` and `exec h :- ..., x=y`.

**JOL's timestep** (BOOM §2): "Each timestep consists of three phases… In the first phase, inbound events are converted into tuple insertions and deletions on the local table partitions. The second phase interprets the local rules and tuples according to traditional Datalog semantics, executing the rules to a 'fixpoint'… In the third phase, updates to local state are atomically made durable, and outbound events (network messages, Java callback invocations) are emitted." Details from `core/runtime.olg` (JOL's scheduler, itself written in Overlog) and `Driver.java`:
- Insertions are queued per `(time, stratum)` and run in increasing stratum order (`strata(Program, Time, min<Stratum>)`).
- Deletions are deferred until no insertions remain in a lower stratum (`deletion_runnable1/2`). When a delta has both insertions and deletions, "we're not going to deal with the deletions yet" and they become a continuation.
- A deletion on a body table produces a deletion on a *materialized* head (view maintenance), unless the query is event-triggered (`#insert`).
- Remote heads go through a `RemoteBuffer`.
- `async` queries run on an executor, and their results come back in a later timestep.
- Facts are scheduled at `Time+1`.
- "Concurrent requests to the NameNode are handled in a serial fashion by JOL" (BOOM §3.2).

### 4.5 Timeouts, precisely

Overlog has no timeout construct. A timeout is always built from one of three mechanisms:

1. **TTL expiry** of a soft-state table. For example, `materialize(neighbor, 120, infinity, keys(2))` drops a neighbor 120 s after its last insert or refresh. Expiry fires delete listeners, so `DELTA_DELETE`-triggered or view rules can react. In P2 this happens lazily on the next table access (§2.3).
2. **Periodic check against a stored timestamp**, the idiom in Narada L1–L3 and Chord `cm1`:
   ```
   L1 neighborProbe@X(X) :- periodic@X(X, E, 1).
   L2 deadNeighbor@X(X, Y) :- neighborProbe@X(X), neighbor@X(X, Y),
        member@X(X, Y, _, YT, _), f_now() - YT > 20.
   L3 delete neighbor@X(X, Y) :- deadNeighbor@X(X, Y).
   ```
   Detection latency is at most threshold plus period. `f_now()` is read at evaluation time, which makes the rule's semantics depend on wall-clock time (a problem Dedalus addresses by making the clock an EDB relation).
3. **Tick counters** (IDODECLARE Fig. 2) build a sequence driven by a timer, which is logical time:
   ```
   timer(ticker, 1000ms);
   tick(Coordinator, TxnId, Count) :- transaction(Coordinator, TxnId, State), State == "prepare", Count := 0;
   tick(Coordinator, TxnId, NewCount) :- ticker(), tick(Coordinator, TxnId, Count), NewCount := Count + 1;
   transaction(Coordinator, TxnId, "abort") :- tick(Coordinator, TxnId, Count),
       transaction(Coordinator, TxnId, State), Count > 10, State == "prepare";
   ```
   Here `tick` is keyed on its first two columns, so the update overwrites. (The paper writes `timer(ticker, 1000ms)`. The released JOL grammar (`Core.rats`) declares a 5-argument `timer(name, physical|logical, period, ttl, delay)`, and JOL's own `test/key.olg` uses yet another form, `timer(tick,3000,0)`. Treat the paper syntax as illustrative.)

### 4.6 Deletes and updates

- `delete h(...) :- body.` removes the exact tuple `h(...)` from its (materialized) table. It cannot target events (JOL throws "trying to delete from table … ?"). Deletion is deferred to the end of the fixpoint (EVITA, P2 CommitManager). JOL additionally defers it behind all lower-stratum insertions.
- An update is key overwrite: derive a tuple with the same key and new values (`sequence(@X, NewSeq) :- refresh(@X), sequence(@X, CurSeq), NewSeq := CurSeq + 1.`). This is the only way to "mutate" a value.
- **Atomic dequeue idiom** (IDODECLARE Fig. 5): choose the minimum with an aggregate, then delete it in the same fixpoint:
  ```
  top_of_queue(Agent, min<Id>) :- stored_update_request(Agent, _, _, Id);
  begin_prepare(Agent, Update) :- stored_update_request(Agent, Update, _, Id), top_of_queue(Agent, Id);
  delete stored_update_request(Agent, Update, From, Id) :-
      stored_update_request(Agent, Update, From, Id), update_passed(Agent, _, _, Update, Id);
  ```

### 4.7 Stratification

Evita's stratification checker (`lang/olg/stratify.olg`, 5 rules; EVITA "a transitive closure program on the rule graph"), verbatim:

```
namespace stratify {
  materialize(dependency, infinity, infinity, keys(1,2,3,4)).
  materialize(strata, infinity, infinity, keys(1,2)).
  s1 ::sys::program_add(@A, Pid, Name, Rewrite, "stratify", Text, Msg, P2DL, Src) :-
        programEvent(@A, Pid, _, _, _, _, _, _, _),
        ::sys::program(@A, Pid, Name, Rewrite, Status, Text, Msg, P2DL, Src).
  s2 dependency(@A, HeadName, BodyName, Neg) :-
        programEvent(@A, Pid, _, _, _, _, _, _, _),
        ::sys::rule(@A, Rid, Pid, _, HeadPredID, _, _, _, _),
        ::sys::predicate(@A, HeadPredID, Rid, _, HeadName, _, _, _, _, _, _),
        ::sys::predicate(@A, PredID, Rid, Neg, BodyName, _, _, _, _, _, _).
  s3  strata(@A, Name, infinity) :- dependency(@A, Name, Name, true).
  s4 initStratum(@A, Name) :- dependency(@A, Head, Body, Neg).
  s5 strata(@A, Name, 0) :- initStratum(@A, Name), notin dependency(@A, Name, _, true).
  s6 strata(@A, Head, Stratum+1) :- strata(@A, Name, Stratum), dependency(@A, Head, Name, true).
}
```

Caveats:
- It only tracks `notin`, not aggregation.
- It does not distinguish event edges, even though real programs recurse *through aggregation across events*. In Chord, `lookup → bestLookupDist(min) → lookup@BI` is a cycle through an aggregate, and it is sound only because the cycle crosses a network hop, i.e. a later fixpoint.

Dedalus's "stratification modulo time" is the right formalization. Our checker must treat edges through `@next`/`@async` (sends, table deltas) as breaking strata.

---

## 5. Evaluation algorithms

### 5.1 Semi-naive (SN), as implemented in P2 (SIGMOD06 Algorithm 1; THESIS Alg. 5.1)

Delta rule form (SIGMOD06 §3.1): Δp_j^new :- p_1^old, …, p_{k−1}^old, Δp_k^old, p_{k+1}, …, p_n, b_1, …, b_m, where the p_i are recursive predicates and the b_i are base predicates. The authors note this form is logically equivalent to Δp_j^new :- p_1,…,p_{k−1}, Δp_k^old, p_{k+1},…, "and ha[s] the advantage of avoiding redundant inferences within each iteration."

```
Algorithm 1 Semi-naive (SN) Evaluation in P2
while ∃ B_k.size > 0
    ∀ B_k where B_k.size > 0:  Δp_k^old ← B_k.flush()
    execute all rule strands
    foreach recursive predicate p_j
        p_j^old ← p_j^old ∪ Δp_j^old
        B_j     ← Δp_j^new − p_j^old
        p_j     ← p_j^old ∪ B_j
        Δp_j^new ← ∅
```

Each delta rule becomes a **rule strand**: a chain of relational elements (join, select, project, aggregate) fed from a queue. Outputs "wrap back" as inputs.

### 5.2 Buffered semi-naive (BSN)

This is SN, except that "a node can start a local SN iteration at any time its local B_k buffers are non-empty. Tuples arriving over the network while an iteration is in progress are buffered for processing in the next iteration" (SIGMOD06 §3.3.1). Iterations are node-local, so no global barrier is needed.

### 5.3 Pipelined semi-naive (PSN), SIGMOD06 Algorithm 3 and THESIS Algorithm 5.3

```
execute all rules
foreach t_k ∈ derived predicate p_k:  t_k.T ← current_time();  B_k ← t_k
while ∃ Q_k.size > 0
    t_k^{old,i} ← Q_k.dequeueTuple()
    foreach delta rule execution
        Δp_j^{new,i+1} :- p_1, .., p_{k−1}, t_k^{old,i}, p_{k+1}, .., p_n, b_1, .., b_m,
             t_k^{old,i}.T ≥ p_1.T, …, t_k^{old,i}.T ≥ p_n.T, t_k^{old,i}.T ≥ b_1.T, …, t_k^{old,i}.T ≥ b_m.T
        foreach t_j^{new,i+1} ∈ Δp_j^{new,i+1}
            if t_j^{new,i+1} ∉ p_j then
                p_j ← p_j ∪ t_j^{new,i+1}
                t_j^{new,i+1}.T ← current_time()
                Q_j.enqueueTuple(t_j^{new,i+1})
```

- "To fully pipeline evaluation, we have also removed the distinctions between p_j^old and p_j in the rules. Instead, a timestamp (or monotonically increasing sequence number) is added to each tuple at arrival, and the join operator matches each tuple only with tuples that have the same or older timestamp." Timestamps are node-local, which is fine because rules are localized.
- **Theorem 1:** FP_S(p) = FP_P(p). **Theorem 2:** PSN makes no repeated inferences. For non-linear rules, only the delta rule whose input has the maximal timestamp fires (App. A).
- DECLIMP §3.4.1: "PSN makes monotonic logic embarrassingly parallel", with the sequencing borrowed from the Urhan–Franklin XJoin. This is the root of CALM.

### 5.4 Known flaws in PSN as implemented (NJWLS)

"We have found several problems with the current evaluation algorithm used, including unsound results, unintended multiple derivations of the same table entry, and divergence":

1. **Unsound with deletions.** P2 applies a received update to the local view *on receipt*, before dequeuing it. Their counter-example: `p@1 :- s@2, t@2, r@2`, `s@2 :- q@3`, `t@2 :- u@4`, with messages `ins(r)`, `del(q)`, `del(u)` in flight. `p` ends up derived with no support.
2. **Duplicate derivations.** Without the old/new (ν) distinction, `p :- t, t` becomes `ins(p) :- Δt, t` and `ins(p) :- t, Δt`, and both fire.
3. **Divergence.** With `p :- a` and `p :- p` and queue `[ins(a), del(a)]`, insert and delete of `p` propagate forever.

Their fix, **PSN^ν**, is a bag of updates with `pick` (update the ν-table), then `fire` (SN-style delta rules against ν and non-ν copies), then `update` (sync the non-ν view). It removes the FIFO-channel and timestamp assumptions. Correctness is proved for non-recursive programs, and for recursive ones given termination. They also note the centralized SN view maintenance (Gupta et al.) is only correct under a single-derivation assumption; counting is needed otherwise, and counting fails for recursive views. CACM09 says P2's count-algorithm maintenance "has subsequently been improved via the use of a compact form of data provenance encoded using binary decision diagrams" (Liu et al. ICDE 2009; not read).

**Implication for us:** do not ship PSN-with-deletes as specified. Use tick-scoped set semantics (Dedalus), so deletion is just non-persistence at the next tick, plus proper incremental maintenance inside a tick: DRed, provenance/support counting, or differential-dataflow-style multiversioned deltas.

### 5.5 Incremental view maintenance under updates (THESIS §5.4)

Three change types: insertion (handled naturally by PSN), deletion (cascaded via delete delta rules), and update (delete followed by insert). The count algorithm tracks the number of derivations. Rule-strand generation:
- **Alg. 5.4**: an insertion strand per delta rule, `Insert-Listener(Δp_k) → Join(p_j…) → Join(b…) → Project(Δp) → Network-Out`, plus a receive strand `Network-In(Δp) → Insert(Δp)`.
- **Alg. 5.5**: the same with `Delete-Listener → … → Delete`.
- **Alg. 5.6**: `Refresh-Listener` strands for soft state.
- **Alg. 5.7**: event rules, where only the event's delta rule is compiled, "since the delta predicate… is essentially a stream of update events, all other delta rules do not generate any output."

"For correctness, each strand has to execute completely before another strand is executed."

### 5.6 Rule localization rewrite

**SIGMOD06 Algorithm 2 (verbatim structure):**
```
proc RuleLocalization(R)
  while ∃ rule r ∈ R: h(@L,...) :- #link(@S,@D,...), p1(@S,..),..,pi(@S,...), pi+1(@D,...),..,pn(@D,..)
    R.remove(r)
    R.add( hS(@S,@D,..) :- #link(@S,@D,..), p1(@S,..),..,pi(@S,..). )
    R.add( hD(@D,@S,..) :- hS(@S,@D,..). )
    if @L = @D
      then R.add( h(@D,..) :- hD(@D,@S,..), pi+1(@D,..),..,pn(@D,..). )
      else R.add( h(@S,..) :- #link(@D,@S), hD(@D,@S..), pi+1(@D,..),..,pn(@D,..). )
```

**Claim 1:** every link-restricted program rewritten this way has single-node rule bodies, and all communication is derived tuples sent over links. Worked example (CACM09 form):

```
sp2  path(@Src,Dest,Path,Cost) :- link(@Src,Nxt,Cost1), path(@Nxt,Dest,Path2,Cost2),
                                  Cost=Cost1+Cost2, Path=f_concatPath(Src,Path2).
==>
sp2a linkD(@Nxt,Src,Cost) :- link(@Src,Nxt,Cost).
sp2b path(@Src,Dest,Nxt,Path,Cost) :- linkD(@Nxt,Src,Cost1), path(@Nxt,Dest,Path2,Cost2),
                                      Cost=Cost1+Cost2, Path = f_concatPath(Src,Path2).
```

After localization comes the SN rewrite. Figure 5 of SIGMOD06 shows three strands: `SP2a@S` ships links as `linkD`; `SP2b-1@Z` handles a new `path` joined with `linkD`; `SP2b-2@Z` handles a new `linkD` joined with `path`. Each sends `path` to `path.S`.

**General (non-link-restricted) localization** in P2/Evita (`localize.olg`, 28 rules) and JOL (`Rule.localize`) works like this:
- Group body predicates by locspec variable.
- Starting from the event's location, evaluate that group and project an intermediate event `r_intermediate_<event>` whose schema is everything bound so far, relocated to the next location variable, which must already be bound.
- Chain group by group. The last group projects onto the original head.
- JOL raises "Localization failed; disconnected set of body location variables" if the next location is not bound.

NR09 formalizes the precondition as **well-connectedness**: x ⇝ y if some body predicate located at x mentions y; a rule is well-connected if its body has a *source* address from which every other address is reachable. Their localization produces `β q(y_k, x, v') :- p1(x,..),..,p_{k−1}(x,..)` followed by `α h(v) :- q(y_k,x,v'), p_k(y_k,..),…`, with β = add if all prefix predicates are materialized (then q is a new table) and β = send otherwise. Materialized-head rules at a remote address become `send u(y,v) :- ...` plus `α h(y,v) :- u(y,v)`. **Rule softening** turns every materialized rule into n trigger rules on fresh update events m̂_i.

The P2 guide warns: "you cannot have distributed rules in which it is not clear how tuples at different location specifiers will 'meet.' … a rule with a right-hand side that looks like `a(@X, Y), b(@Z, Y)` will not work."

**Semantic caveat** (DECLIMP §3.5.2, BOOM §9.2): multi-location bodies read *stale snapshots* at each hop and have no defined behavior under failure ("the global database abstraction … is therefore a lie"). "In our recent Overlog code we have rarely written rules with distributed joins in the body… In Dedalus such rules are forbidden." BOOM: "we did not utilize arbitrary distributed queries… we were unsure of the semantics of such queries in the event of node failures and network partitions."

### 5.7 The P2 dataflow runtime (SOSP05 §3, THESIS ch. 4)

- **Values and tuples.** Tuples are immutable and reference-counted, and passed between elements by reference. Field 0 internally holds the tuple name.
- **Elements** have push and pull ports. Queues bridge push/pull mismatches. P2 queues *block* instead of dropping; a stalled flow is restarted by callbacks passed with each push or pull.
- **PEL**, a stack-based bytecode VM, evaluates selections, assignments and projections.
- **Joins** are stream-to-table equijoins via index lookup. The original planner supported only a stream joined with tables ("our current version of OverLog only supports equijoins of a stream and a table"). Evita added sort and merge joins. An index exists on every primary key plus secondary indexes on join keys (balanced trees in SOSP05, hash tables in THESIS).
- **Graph shape** (SOSP05 Fig. 2):
  - Input: Network-In, a big Demux on tuple name, a Dup when a tuple feeds several rules, then the rule strands.
  - Output: a RoundRobin scheduler, a Demux on "@local?", local tuples wrapped back into the input queue, remote tuples to Network-Out (marshal, congestion control, UDP transport).
  - Networking is itself built from dataflow elements.
- **Single-threaded event loop.** Each handler runs to completion (libasync). This is what gives per-fixpoint atomicity.

---

## 6. Optimizations studied (SIGMOD06 §5, THESIS ch. 8, EVITA §4)

1. **Aggregate selections** (Sudarshan and Ramakrishnan): use the running state of a monotone aggregate (`min`) to prune. For example, propagate a path only if it improves the current shortest cost for `(src,dst)`. This is *required* for shortest path to terminate on cyclic graphs. **Periodic aggregate selections** buffer improvements and send them periodically, trading convergence time for bandwidth. This is the direct ancestor of lattice-based (BloomL) monotone aggregates in recursion.
2. **Magic sets** (Supplementary Magic Sets per Ullman). Distributed example:
   ```
   #include(SP2,SP3,SP4)
   SP1-D: path(@S,@D,@D,P,C) :- magicDst(@D), #link(@S,@D,C), P = f_concatPath(link(@S,@D,C), nil).
   ```
   Evita implemented magic sets in 68 rules; `mg1`–`mg6` traverse the rule/goal graph to build adornments and `sup`/`magic_` predicates.
3. **Predicate reordering.** Left recursion `path(@S,D,..) :- path(@S,Z,..), #link(@Z,D,..)` versus right recursion `… :- #link(@S,Z,..), path(@Z,D,..)` gives **dynamic source routing vs. distance vector**: "differ only in a simple, traditional query optimization decision: the order in which a query's predicates are evaluated" (CACM09).
4. **Multi-query sharing:** query-result caching (subpaths of shortest paths are cacheable) and opportunistic message sharing (merge outbound tuples to the same destination that share attributes, with an optional 300 ms delay).
5. **Cost-based:** the neighborhood function N(X,r) decides top-down vs. bottom-up vs. a hybrid split (r_s, r_d) = argmin N(s,r_s) + N(d,r_d).
6. **Evita stages, all written in Overlog:**

   | Stage | Size |
   |---|---|
   | System R DP optimizer (join methods, interesting orders) | 51 rules / 292 LOC |
   | Cascades top-down branch-and-bound | 33 rules |
   | Equi-width histograms | 23 rules |
   | Magic sets | 68 rules / 264 LOC |
   | Localization | 28 rules |
   | Delta rewrite | 9 rules |
   | Stratification | 5 rules |
   | Wireless broadcast-vs-unicast rewrite (`a_mkset` destination sets) | 8 rules / 63 LOC |

---

## 7. Evita Raced: architecture (what a metacompiler must provide)

- **Catalog ("Metacompiler Catalog", EVITA Table 1)**: `table(table id, primary key)`, `index(index id, table id, keys, type)`, `fact(program id, table id, id, tuple)`, `program(program id, name, stage, text, depends, plan)`, `rule(program id, rule id, name, term count, head id)`, `predicate(id, rule id, table id, name, position, access method)`, `select(id, rule id, boolean, position)`, `assign(id, rule id, variable, value, position)`.
  - By convention the first body term is the event predicate and the last position is the head.
  - The source code adds `Notin`, `ECA` and `Schema` fields to `::sys::predicate(@A, PredID, Rid, Notin, Name, Tid, ECA, Schema, Pos, AM, New)`.
- **Stage API.** A stage listens on `<stage>::programEvent`, reads and rewrites catalog tables, runs to fixpoint, and signals completion by inserting a `program` tuple with `stage = <name>`. Stages are registered by inserting a program tuple with non-empty `depends`.
- **Scheduling.** A `StageLattice` relation holds a partial order of stages, with Parser as source and Installer as sink. A C++ `StageScheduler` joins each program update with the lattice and emits the next stage's `programEvent`. A Demux routes by tuple name (the paper likens this to an eddy).
- **Bootstrap** is in C++: Parser (flex/bison AST to catalog tuples), Physical Planner (a naive left-to-right translation to a textual dataflow language, P2DL), and Installer (P2DL to elements). The delta-rewrite stage is "the first stage installed following compiler bootstrap."
- **Other uses** they found: instrumentation and monitoring rewrites, pretty-printers, SecLog wrappers, stratification detection. BOOM reports the same benefit in JOL, where programs are rows in tables so metaprograms can test, optimize and rewrite them.
- **Lessons (EVITA §6)**: "The most difficult problems we faced were due to discrepancies between P2's runtime behavior and Datalog semantics… truly declarative languages with clean semantics are superior to more ad-hoc event-condition-action rule languages." "…you have to understand what happens across multiple fixpoints… reasoning about ordering among events… one possible direction is to bring concepts from temporal logic into OverLog."

---

## 8. Example programs (verbatim, with sources)

### 8.1 Shortest path, NDlog (SIGMOD06 Fig. 1)

```
SP1: path(@S,@D,@D,P,C) :- #link(@S,@D,C), P = f_concatPath(link(@S,@D,C), nil).
SP2: path(@S,@D,@Z,P,C) :- #link(@S,@Z,C1), path(@Z,@D,@Z2,P2,C2), C = C1 + C2,
                           P = f_concatPath(link(@S,@Z,C1),P2).
SP3: spCost(@S,@D,min<C>) :- path(@S,@D,@Z,P,C).
SP4: shortestPath(@S,@D,P,C) :- spCost(@S,@D,C), path(@S,@D,@Z,P,C).
Query: shortestPath(@S,@D,P,C).
```

### 8.2 Ping-pong with soft-state links (THESIS Fig. 2.5)

```
materialize(#link,10,infinity,keys(1,2)).
materialize(pingRTT,10,5,keys(1,2)).
materialize(pendingPing,10,5,keys(1,2)).
pp1 ping(@S,D,E) :- periodic(@S,E,5), #link(@S,D).
pp2 pingMsg(S,@D,E) :- ping(@S,D,E), #link(@S,D).
pp3 pendingPing(@S,D,E,T) :- ping(@S,D,E), T = f_now().
pp4 pongMsg(@S,E) :- pingMsg(S,@D,E), #link(@D,S).
pp5 pingRTT(@S,D,RTT) :- pongMsg(@S,E), pendingPing(@S,D,E,T), RTT = f_now() - T.
pp6 #link(@S,D) :- pingRTT(@S,D,RTT).
Query pingRTT(@S,D,RTT).
```

`#link` tuples expire after 10 s unless `pp6` refreshes them.

### 8.3 Routing variants (THESIS ch. 3)

```
dv1 hop(@S,D,D,C) :- #link(@S,D,C).
dv2 hop(@S,D,Z,C) :- #link(@S,Z,C1), hop(@Z,D,W,C2), C = f_compute(C1,C2).
dv3 bestHopCost(@S,D,AGG<C>) :- hop(@S,D,Z,C).
dv4 bestPathHop(@S,D,Z,C) :- hop(@S,D,Z,C),bestHopCost(@S,D,C).

-- count-to-infinity fix (split horizon + poison reverse):
#include(dv1,dv3,dv4)
dv2 hop(@S,D,Z,C) :- #link(@S,Z,C1), hop(@Z,D,W,C2), C = C1 + C2, W != S.
dv5 hop(@S,D,Z,infinity):- #link(@S,Z,C1), hop(@Z,D,S,C2).

-- dynamic source routing (left recursion):
#include(bp1,bp3,bp4)
dsr2 path(@S,D,Z,P,C) :- path(@S,Z,W,P1,C1), #link(@Z,D,C2),
                         C = f_compute(C1,C2), P = f_concatPath(P1,D).

-- link state flooding:
ls1 floodLink(@S,S,D,C,S) :- #link(@S,D,C).
ls2 floodLink(@M,S,D,C,N) :- #link(@N,M,C1), floodLink(@N,S,D,C,W), M != W.
```

The THESIS also gives policy-based routing (`f_inPath(P,W)=false`) and source-specific multicast (`joinMessage`/`forwardState`).

### 8.4 Narada mesh, executable P2 version (SOSP05 Appendix A)

```
materialize(member, infinity, infinity, keys(2)).
materialize(sequence, infinity, 1, keys(2)).
materialize(neighbor, infinity, infinity, keys(2)).
materialize(env, infinity, infinity, keys(2,3)).
E0 neighbor@X(X,Y) :- periodic@X(X,E,0,1), env@X(X, H, Y), H == "neighbor".
S0 sequence@X(X, Sequence) :- periodic@X(X, E, 0, 1), Sequence := 0.
R1 refreshEvent@X(X) :- periodic@X(X, E, 3).
R2 refreshSequence@X(X, NewSequence) :- refreshEvent@X(X), sequence@X(X, Sequence),
     NewSequence := Sequence + 1.
R3 sequence@X(X, NewSequence) :- refreshSequence@X(X, NewSequence).
R4 refresh@Y(Y, X, NewSequence, Address, ASequence, ALive) :- refreshSequence@X(X, NewSequence),
     member@X(X, Address, ASequence, Time, ALive), neighbor@X(X, Y).
R5 membersFound@X(X, Address, ASeq, ALive, count<*>) :- refresh@X(X, Y, YSeq, Address, ASeq, ALive),
     member@X(X, Address, MySeq, MyTime, MyLive), X != Address.
R6 member@X(X, Address, ASequence, T, ALive) :- membersFound@X(X, Address, ASequence, ALive, C),
     C == 0, T := f_now().
R7 member@X(X, Address, ASequence, T, ALive) :- membersFound@X(X, Address, ASequence, ALive, C),
     C > 0, T := f_now(), member@X(X, Address, MySequence, MyT, MyLive), MySequence < ASequence.
R8 member@X(X, Y, YSeq, T, YLive) :- refresh@X(X, Y, YSeq, A, AS, AL), T := f_now(), YLive := 1.
N1 neighbor@X(X, Y) :- refresh@X(X, Y, YS, A, AS, L).
L1 neighborProbe@X(X) :- periodic@X(X, E, 1).
L2 deadNeighbor@X(X, Y) :- neighborProbe@X(X), T := f_now(), neighbor@X(X, Y),
     member@X(X, Y, YS, YT, L), T - YT > 20.
L3 delete neighbor@X(X, Y) :- deadNeighbor@X(X, Y).
L4 member@X(X, Neighbor, DeadSequence, T, Live) :- deadNeighbor@X(X, Neighbor),
     member@X(X, Neighbor, S, T1, L), Live := 0, DeadSequence := S + 1, T:= f_now().
```

Note R5/R6: they rely on the per-event aggregate emitting `count = 0` on an empty join (§4.3).

### 8.5 Chord, final executable P2 version (`p2/doc/chord.olg`, verbatim rules)

This supersedes the 47-rule SOSP05 Appendix B and THESIS App. B.2, both of which contain typos (e.g. THESIS `fd1 … D = T-T1`).

```
materialize(node, infinity, 1, keys(1)).
materialize(landmark, infinity, 1, keys(1)).
materialize(finger, 180, 160, keys(2)).
materialize(uniqueFinger, 180, 160, keys(2)).
materialize(bestSucc, 180, 1, keys(1)).
materialize(succ, 30, 100, keys(2)).
materialize(pred, infinity, 1, keys(1)).
materialize(join, 10, 5, keys(1)).
materialize(pendingPing, 10, infinity, keys(3)).
materialize(fFix, 180, 160, keys(2)).
materialize(nextFingerFix, 180, 1, keys(1)).
#define SUCCESSORS 4
#define JOINRETRIES 3
#define JOINPERIOD 5
#define STABILIZEPERIOD 5
#define FINGERFIXPERIOD 10
#define PINGPERIOD 2
landmark(LOCALADDRESS, LANDMARK).
node(LOCALADDRESS, NODEID).
pred(LOCALADDRESS, "NIL", "NIL").
nextFingerFix(LOCALADDRESS, 0).
/** Lookups */
l1 lookupResults(@R,K,S,SI,E) :- node(@NI,N), lookup(@NI,K,R,E),
	bestSucc(@NI,S,SI), K in (N,S].
l2 bestLookupDist(@NI,K,R,E,a_MIN<D>) :- node(@NI,N), lookup(@NI,K,R,E),
	finger(@NI,I,B,BI), D := K - B - 1, B in (N,K).
l3 forwardLookup(@NI,a_MIN<BI>,K,R,E) :- node(@NI,N), bestLookupDist(@NI,K,R,E,D),
	finger(@NI,I,B,BI), D == K - B - 1, B in (N,K).
l4 lookup(@BI, K, R, E) :- forwardLookup(@NI, BI, K, R, E), BI != null.
/** Neighbor Selection */
n0 newSuccEvent(@NI) :- succ(@NI,S,SI).
n2 newSuccEvent(@NI) :- deleteSucc(@NI,S,SI).
n1 bestSuccDist(@NI, a_MIN<D>) :- newSuccEvent(@NI), node(@NI, N),
	succ(@NI, S, SI), D := S - N - 1.
n3 bestSucc(@NI, S, SI) :- succ(@NI, S, SI), bestSuccDist(@NI,D), node(@NI, N), D == S - N - 1.
n4 finger(@NI,0,S,SI) :- bestSucc(@NI,S,SI).
/** Successor eviction */
s1 succCount(@NI,a_COUNT<*>) :- newSuccEvent(@NI), succ(@NI,S,SI).
s2 evictSucc(@NI) :- succCount(@NI,C), C > SUCCESSORS.
s3 maxSuccDist(@NI,a_MAX<D>) :- succ(@NI,S,SI), node(@NI,N), evictSucc(@NI), D:=S - N - 1.
s4 delete succ(@NI,S,SI) :- node(@NI,N), succ(@NI,S,SI), maxSuccDist(@NI,D), D == S - N - 1.
/** Finger fixing */
f1 fFix(@NI,E,I) :- periodic(@NI,E,FINGERFIXPERIOD), nextFingerFix(@NI,I).
f2 fFixEvent(@NI,E,I) :- fFix(@NI,E,I).
f3 lookup(@NI,K,NI,E) :- fFixEvent(@NI,E,I), node(@NI,N), K:= N + (0x1I << I).
f4 eagerFinger(@NI,I,B,BI) :- fFix(@NI,E,I), lookupResults(@NI,K,B,BI,E).
f5 finger(@NI,I,B,BI) :- eagerFinger(@NI,I,B,BI).
f6 eagerFinger(@NI,I,B,BI) :- node(@NI,N), eagerFinger(@NI,I1,B,BI),
	I:=I1 + 1, K:= N + (0x1I << I), K in (N,B), BI != NI.
f7 delete fFix(@NI,E,I1) :- eagerFinger(@NI,I,B,BI), fFix(@NI,E,I1), I > 0, I1 == I - 1.
f8 nextFingerFix(@NI,0) :- eagerFinger(@NI,I,B,BI), ((I == 159) || (BI == NI)).
f9 nextFingerFix(@NI,I) :- node(@NI,N), eagerFinger(@NI,I1,B,BI), I:=I1 + 1,
	K:= N + (0x1I << I), K in (B,N), NI != BI.
f10 uniqueFinger(@NI,BI) :- finger(@NI,I,B,BI).
/** Churn Handling */
c1 joinEvent(@NI,E) :- periodic(@NI, E, JOINPERIOD, JOINRETRIES).
c2 join(@NI,E) :- joinEvent(@NI,E).
c3 joinReq(@LI,N,NI,E) :- joinEvent(@NI, E), node(@NI, N), landmark(@NI, LI), LI != NI.
c4 succ(@NI, N, NI) :- landmark(@NI, LI), joinEvent(@NI, E), node(@NI, N), LI == NI.
c5 lookup(@LI,N,NI,E) :- joinReq(@LI,N,NI,E).
c6 succ(@NI,S,SI) :- join(@NI, E), lookupResults(@NI,K,S,SI,E).
/** Stabilization */
sb0 stabilizeEvent(@NI) :- periodic(@NI, E, STABILIZEPERIOD).
sb1 succ(@NI,P,PI) :- stabilizeEvent(@NI), node(@NI,N),
	bestSucc(@NI,S,SI), pred(@SI,P,PI), PI != "NIL", P in (N,S).
sb2 succ(@NI, S1, SI1) :- stabilizeEvent(@NI), succ(@NI, S, SI), succ(@SI, S1, SI1).
sb3 pred(@SI, N, NI) :- stabilizeEvent(@NI), node(@NI, N), succ(@NI, S, SI),
	pred(@SI, P, PI), node(@SI, N1), ((PI == "NIL") || (N in (P, N1))) && (NI != SI).
/** Ping Nodes */
pp1 pendingPing(@NI, SI, E1, T) :- periodic(@NI, E, PINGPERIOD),
	succ(@NI, S, SI), E1 := f_rand(), SI != NI, T := f_now().
pp2 pendingPing(@NI, PI, E1, T) :- periodic(@NI, E, PINGPERIOD),
	pred(@NI, P, PI), E1 := f_rand(), PI != "NIL", PI != NI, T := f_now().
pp3 pendingPing(@NI, FI, E1, T) :- periodic(@NI, E, PINGPERIOD),
	uniqueFinger(@NI, FI), E1 := f_rand(), FI != NI, T := f_now().
pp4 pingResp(@RI, NI, E) :- pingReq(@NI, RI, E).
pp5 pingReq(@PI, NI, E) :- periodic(@NI, E1, 3), pendingPing(@NI, PI, E, T).
pp6 delete pendingPing(@NI, SI, E1, T) :- pingResp(@NI, SI, E), pendingPing(@NI, SI, E1, T).
/** Failure Detection */
cm1 nodeFailure(@NI,PI,E1,D) :- periodic(@NI, E, 1), pendingPing(@NI,PI,E1,T),
	T1 := f_now(), D := T1 - T, D > 20.
cm1a delete pendingPing(@NI,PI,E,T) :- nodeFailure(@NI,PI,E,D), pendingPing(@NI,PI,E,T).
cm2a deleteSucc(@NI,S,SI) :- succ(@NI,S,SI), nodeFailure(@NI,SI,E,D).
cm2b delete succ(@NI,S,SI) :- deleteSucc(@NI,S,SI).
cm3 pred(@NI,"NIL","NIL") :- pred(@NI,P,PI), nodeFailure(@NI,PI,E,D).
cm4 delete finger(@NI,I,B,BI) :- finger(@NI,I,B,BI), nodeFailure(@NI,BI,E,D).
cm6 delete uniqueFinger(@NI,FI) :- uniqueFinger(@NI,FI), nodeFailure(@NI,FI,E,D).
```

Features this one program exercises, all of which we must support:
- soft-state lifetimes and FIFO size limits;
- key overwrite;
- 160-bit modular ID arithmetic;
- ring intervals;
- per-event `min` with null-on-empty;
- `count<*>`;
- `delete` rules;
- `periodic` with repetition count;
- multi-location bodies (`sb1`–`sb3` read `pred@SI` and `succ@SI`), which need general localization;
- `#define` and `-D` macros;
- `null` comparisons.

### 8.6 Two-phase commit coordinator (IDODECLARE Fig. 1, JOL syntax)

```
peer_cnt(Coordinator, count<Peer>) :- peers(Coordinator, Peer);
yes_cnt(Coordinator, TxnId, count<Peer>) :- vote(Coordinator, TxnId, Peer, Vote), Vote == "yes";
transaction(Coordinator, TxnId, "commit") :- peer_cnt(Coordinator, NumPeers),
    yes_cnt(Coordinator, TxnId, NumYes), transaction(Coordinator, TxnId, State),
    NumPeers == NumYes, State == "prepare";
transaction(Coordinator, TxnId, "abort") :- vote(Coordinator, TxnId, _, Vote),
    transaction(Coordinator, TxnId, State), Vote == "no", State == "prepare";
transaction(@Peer, TxnId, State) :- peers(@Coordinator, Peer), transaction(@Coordinator, TxnId, State);
```

`transaction` is keyed on its first two columns. The timeout variant is shown in §4.5.

### 8.7 Paxos fragments (IDODECLARE Figs. 3–4)

```
promise(@Master, View, OldView, OldUpdate, Agent) :- prepare(@Agent, View, Update, Master),
    prev_vote(@Agent, OldView, OldUpdate), View >= OldView;
agent_cnt(Master, count<Agent>) :- parliament(Master, Agent);
promise_cnt(Master, View, count<Agent>) :- promise(Master, View, Agent, _);
quorum(Master, View) :- agent_cnt(Master, NumAgents), promise_cnt(Master, View, NumVotes),
    NumVotes > (NumAgents / 2);
```

Sizes: basic Paxos was 22 rules and 53 LOC; Multi-Paxos with liveness and catch-up was about 50 rules and 400 LOC (BOOM §4.1). Leader election (Kirsch and Amir) took 19 rules. The idioms they named are **multicast** (message plus a join with membership), **sequence** (a single-row keyed counter), **roll call**, **barrier** (a count over received messages), **voting**, **choice** (selection over an exemplary aggregate), **atomic dequeue**, and **timeout**. "Our Paxos implementation encodes safety properties declaratively, and liveness properties mechanistically."

### 8.8 NR09 ambiguity programs (verbatim; these make good semantic regression tests)

Fig. 1, event creation vs. effect:
```
materialize(neighbor, keys(1,2)).
materialize(sequence, keys(1)).
refresh(@X) :- periodic(@X, E, 3).
sequence(@X, NewSeq) :- refresh(@X), sequence(@X, CurSeq), NewSeq := CurSeq + 1.
send_updates(@X) :- refresh(@X).
update(@Y, X, S) :- send_updates(@X), neighbor(@X, Y), sequence(@X, S).
```
Question: does `update` carry the old or the new sequence number? The P2 implementation sends the **old** value, because updates apply after the step. THESIS Algorithms 5.1/5.3 suggest the new one.

Fig. 2, internal vs. external events:
```
materialize(neighbor, keys(1,2)).
materialize(store, keys(1,2)).
materialize(sequence, keys(1)).
neighbor(@X, "node1"). neighbor(@X, "node2"). neighbor(@X, "node3").
store(@X, 1). store(@X, 2). ... store(@X, 10).
sequence(@X, 0).
sequence(@X, New) :- ping(@X), sequence(@X, Old), New := Old + 1.
ping(@Y) :- broadcast(@X), neighbor(@X, Y), store(@X, _).
broadcast(@X) :- periodic(@X, E, 5, 1), X = "node1".
```
In P2, node2 and node3 reach 10, but node1 reaches **1**. node1's ten self-addressed pings are *internal* events processed in the same step against the same `sequence = 0`, while remote pings arrive one per step. This also shows that **P2 events are bags**: ten identical `ping(@Y)` derivations become ten messages. Under set semantics per timestep (Dedalus/Bloom) they collapse into one. **We must define this explicitly.**

---

## 9. The Overlog semantic problems that motivated Dedalus (consolidated)

1. **No specified update visibility.** "Overlog provided an operational model of persistent state with updates, including SQL-like syntax for deletion, and tuple 'overwrites' via primary key specifications in head predicates. But, unlike SQL, there was no notion of transactions, and issues of update visibility were left ambiguous" (DECLIMP §3.2.2). "The language descriptions give no careful specification of how and when deletions and updates should be made visible, so the third step is a 'black box'" (DEDALUS-TR §7.2). See NR09's Fig. 1 above.
2. **Internal vs. external event asymmetry**: the same rules behave differently for self-sends (NR09 Fig. 2).
3. **Mixing event, soft and hard state is ill-defined.** "What does it mean when an ephemeral body tuple 'disappears'? We would like the logged tuple in the head to remain, but it is no longer supported by an existing body fact" (DECLIMP §3.3.1). CACM09: "Mixtures of the two models become more subtle. We provided one treatment of this issue [Loo thesis], which has subsequently been revised with a slightly different interpretation [Evita]. There is still some debate." THESIS: archival rules break eventual consistency, and lifetime inversions cause oscillation.
4. **Atomic check-then-update needed an operational hack** (the Symphony max-degree race): "The check and the updates must be done in one atomic step… One solution… is to have the language runtime implement a queue of request messages at each recipient, dequeuing only one request at a time into the 'database' considered in a given Overlog fixpoint… But the use of an operational feature outside the logic is unsatisfying" (DECLIMP §3.2.2). This is P2's one-external-event-per-fixpoint rule.
5. **Reasoning across fixpoints.** Datalog semantics hold only *within* a fixpoint, while programs mean something only *across* them (EVITA §6).
6. **Runtime diverged from spec.** P2 processed multiple strata simultaneously and deleted before the end of the fixpoint (EVITA §2.1). NR09: "P2 semantics is implicitly determined by the runtime environment [12], which in turn deviates from the descriptions in the literature."
7. **PSN with deletions is unsound, duplicates derivations and can diverge** (NJWLS). The count algorithm fails for recursion.
8. **The "global database" is a lie under failure.** Distributed joins in rule bodies have no failure semantics and were avoided in practice (DECLIMP §3.5.2; BOOM §9.2). "Distributed Overlog rules induce asynchrony across nodes; hence, such rules must describe protocols to enforce distributed invariants, not the invariants themselves" (BOOM §3.3).
9. **Aggregates and state update were the main bug sources**: "Many of the bugs we encountered were due to ambiguities in the language semantics, particularly with regard to state update and aggregate functions… Overlog's support for updates has never had a formally-specified semantics" (BOOM §9.2).
10. **ECA dominance undermines declarativity** (Mao, NetDB'09, from its abstract only). Most rules in real systems are event-condition-action rather than deductive, so the benefits of declarativity are lost.
11. **Wall-clock reads** (`f_now()`) and random functions inside rules make semantics depend on evaluation timing.
12. **Syntax pain**: positional arity, eyeball unification, no modules, and verbose disjunction and conditionals (DECLIMP §3.5.1; EVITA §6; BOOM §9.2).

**What Dedalus changed** (DECLIMP §2, §3.3.1): every tuple carries a timestamp. Deductive rules act within a timestep, `@next` rules move to the successor timestep, and `@async` rules pick a nondeterministic future timestamp via `choice`. Persistence becomes an explicit inductive rule, `p(X)@next :- p(X), !del_p(X).`. Soft state is persistence with a TTL guard: `q(A,TTL,Birth)@next :- q(A,TTL,Birth), !del_q(A), now() - Birth < TTL.`. Distributed rule bodies are forbidden, and communication is only via `@async` heads.

---

## 10. Implications for bloom-remake (design guidance derived from this cluster)

1. **Use one semantic core with explicit time** (Dedalus-style ticks). Compile Overlog-style constructs to it:
   - `materialize(t, ∞, …)` becomes persistence;
   - `materialize(t, TTL, …)` becomes persistence guarded by a TTL, with refresh re-deriving the birth time;
   - `delete` becomes a `del_t` relation feeding the persistence rule;
   - key overwrite becomes a `del` of the old key-matching tuple at the next tick;
   - `periodic` and `timer` become EDB clock/timer relations injected at tick boundaries;
   - head locspec ≠ body locspec becomes `@async` send.

   This reproduces final-P2 behavior (mutations visible next fixpoint, as in `seAtomicity.olg`) while being declarative.
2. **Make batching explicit.** P2 processes one external event per fixpoint; Bloom batches everything that arrived. Offer at least batch-per-tick, and possibly a `serial`/queue construct for protocols that need one-at-a-time atomicity (the Symphony case), expressed declaratively with the Dedalus queue pattern (`qmin` plus dequeue).
3. **Define set vs. bag semantics for messages** (NR09 Fig. 2). The recommendation is set semantics within a tick, with explicit multiplicity fields when counts matter.
4. **Expire deterministically.** Evaluate expiry against a single `now` sampled per tick (EVITA step 2), not lazily on access, and generate the deletion deltas at the tick boundary.
5. **Per-event aggregates** must emit one tuple per triggering event, even when the join is empty (count 0, min/max null or absent). Choose one behavior deliberately. The P2 programs above (Narada R5/R6, Chord l3/l4) assume null or 0 is emitted; a cleaner design offers explicit `default`s.
6. **Incremental maintenance inside a tick must be correct under retraction.** Do not reproduce PSN's flaws. Semi-naive for monotone strata; counting/DRed/provenance or differential-style deltas for non-monotone and retraction cases.
7. **Localization:** support NDlog link-restricted rules and the general left-to-right chain (Evita/JOL/NR09 well-connectedness), but compile each hop to an explicit `@async` so the staleness is visible. Consider a lint that warns on multi-location bodies, since BOOM and DECLIMP report that practitioners avoided them.
8. **Static checks:** temporal stratification (edges through `@next`/`@async` break cycles); the one-event-per-rule check, if we keep that event model; the soft-state lifetime monotonicity check (head TTL ≥ body TTLs); an archival-rule warning; locspec binding (well-connectedness); head-variable safety.
9. **Metacompilation (Evita/JOL):** expose the parsed program as catalog relations (`rule`, `predicate`, `select`, `assign`, `table`, `index`) so optimizations, rewrites, stratification and monitoring can be written in the language itself. This is optional for the engine core but a stated BOOM-lineage feature.

---

## MUST-IMPLEMENT CHECKLIST

| # | Feature | Precise description | Source |
|---|---|---|---|
| 1 | Location specifiers | Exactly one `@`-marked address field per predicate. A tuple is stored at the node named by that field, and derivation of a non-local head implies a network send. | SOSP05 §2.3; SIGMOD06 Def. 1; EVITA §2.1 |
| 2 | Local vs. distributed rule classification | A local rule has the same locspec variable in every predicate, head included. All others need localization. | SIGMOD06 Def. 3 |
| 3 | Link relations and link-restricted rules (optional mode) | `#link(@S,@D,..)` literal. A non-local rule has exactly one link literal, and every other literal is located at its S or D. The mode is switchable off for full-mesh networks. | SIGMOD06 Defs. 2, 4, 5; CACM09 §2.4.1 |
| 4 | Localization rewrite (NDlog) | Algorithm 2: split into `hS`/`hD` rules so each body evaluates on one node and results travel along links. | SIGMOD06 §3.2 Alg. 2, Claim 1 |
| 5 | General localization (chain) | Group body predicates by location. Evaluate from the event's location and ship bound variables as an intermediate event to the next *bound* location. Reject disconnected bodies. | EVITA (`localize.olg`); JOL `Rule.localize`; NR09 §6 well-connectedness |
| 6 | `materialize(name, lifetime, maxSize, keys(..))` | Declare a table with TTL in seconds or `infinity`, a max size, and 1-based key positions where 1 is the locspec. An undeclared relation is an event. `keys()` means tuple-ID key. | SOSP05 §2.2; P2 `ol_parser.y`; `table2.h` |
| 7 | Typed table/event definitions (JOL/C4 style) | `define(name, keys(..), {Types})` for tables, `define(name, {Types})` for events, 0-based keys, static typing. | JOL `Core.rats`; C4 `ol_parse.y` |
| 8 | Primary-key update semantics | Same key with different values is an implicit delete of the old tuple followed by an insert of the new one, generating both deltas. | CACM09 §2.4.2; THESIS §5.4; `table2.C` |
| 9 | Soft-state refresh | Re-deriving an identical tuple resets its TTL, emits a refresh event, and is not a new insertion. | CACM09; `Table2::insert`/`updateTime` |
| 10 | TTL expiry | A tuple is removed when `now - lastRefresh > lifetime`, evaluated against one `now` per fixpoint/tick, and produces a deletion delta. | EVITA §2.2.2 step 2; `Table2::flush` |
| 11 | Max-size FIFO eviction | When the table exceeds maxSize, evict the oldest tuple by insertion/refresh time, handled like a key-overwrite displacement. | EVITA footnote 3; `Table2::flush` |
| 12 | Event (stream) relations | Zero-lifetime tuples, visible only during the fixpoint/tick that processes them and never stored. | THESIS Def. 2.8; SOSP05 §2.2 |
| 13 | `periodic(@N,E,T[,K])` | Built-in event: fires every T seconds, K times or forever, with a fresh random ID E. `periodic(@X,E,0,1)` fires once at startup. | THESIS Def. 2.9; P2 UserGuide |
| 14 | Timers (JOL) | `timer(name, physical\|logical, period, ttl, delay)`, referenced as `name(Period,TTL,Delay)`. Logical timers tick in fixpoints. | JOL `TimerTable.java` |
| 15 | At most one event per rule (or a clean replacement) | A rule body contains at most one event/delta trigger. Rules with none are materialized views, compiled to one delta rule per body table. | THESIS Def. 2.15; `ecaContext.C`; JOL `Rule.query` |
| 16 | ECA action classification | The head action is ADD (materialized), DELETE (`delete` rule) or SEND (event: local enqueue or remote transmit). | `lang/eca/ecaContext.C`; NR09 §4 (`add/delete/send/exec`) |
| 17 | `delete` rules | `delete h(..) :- body` removes exact tuples from materialized tables only, applied after the fixpoint/tick that derives them. | SOSP05 L3; EVITA §2.1 |
| 18 | Deferred, atomic mutation commit | All table inserts and deletes derived during a fixpoint become visible only after it; sends are flushed at the end (seAtomicity semantics). | EVITA §2.2.2; `scheduler.C`; `doc/seAtomicity.olg` |
| 19 | `#insert` / `#delete` event modifiers | A body predicate `t#insert(...)` or `t#delete(...)` triggers on that table's insertion or deletion deltas. | JOL `Identifier.rats` EventModifier |
| 20 | Materialized-view rules with cascaded deletion | A rule with only table predicates is maintained incrementally, and deletions of body tuples delete no-longer-supported head tuples. | EVITA `mview.olg`, `delta.olg`; THESIS §5.4; BOOM §3.1 (`fqpath`) |
| 21 | Head aggregates | `agg<X>` in head position groups by the other head fields. Required set: `min, max, count<*>, count<X>, countdistinct, sum, avg, mkset/mklist`. `topk/bottomk/limit/generic` are optional. | SOSP05; P2 `aggregates/`; JOL `Core.rats` |
| 22 | Per-event aggregate semantics | Exactly one output per event tuple, with group-by fields bound from the event, emitted even on an empty join (count 0, min/max null). | THESIS §5.5.1; `aggwrap2.C`; `aggCount.C`/`aggMin.C`; `doc/chord.olg` l3/l4 |
| 23 | Table (view) aggregates with retraction | Maintained incrementally, re-emitted on change, and able to expose the next min/max when the current extreme is deleted. | SOSP05 §3.4; JOL `Aggregation.java`; SIGMOD06 §4 |
| 24 | Aggregate on the locspec | `lookup(min<@BI>, …)` / `a_MIN<BI>` in the locspec position. The aggregate result decides the destination. | SOSP05 L3; THESIS App. B.2 |
| 25 | Stratified negation (`notin`) | `notin p(..)` is allowed only when stratified. Recursion through negation or aggregation is allowed only across time (a send or table delta). | EVITA `stratify.olg`; DECLIMP §3.4.4 |
| 26 | Assignments and selections | `Var := expr` (binds a fresh variable) and boolean selections with `== != <> < > <= >= && \|\| !`. The planner must order them by variable availability. | `ol_parser.y`; JOL `Rule.planHelper` |
| 27 | Expression language | Arithmetic `+ - * / %`, bit ops `& \| ^ ~ << >>`, `**`, ternary `?:`, list append `\|\|\|`, `null`, `true`/`false`, `infinity`, strings, vectors and lists. | Evita `olg_parser.y`; JOL `Core.rats` |
| 28 | Ring interval predicate | `X in (A,B)`, `(A,B]`, `[A,B)` and `[A,B]` with modular wraparound as defined in `oper.h`, so that `(n,n]` is the full ring. | P2 UserGuide; `p2core/oper.h` |
| 29 | 160-bit ID type | Modular 2^160 arithmetic, the literal `0x…I`, and ordering, used by Chord (`N + (0x1I << I)`, `K - B - 1`). | `p2core/ID.h`/`ID.C`; `doc/chord.olg` |
| 30 | Built-in functions | `f_now, f_rand, f_coinFlip, f_idgen, f_sha1, f_tostr, f_size`, list/path helpers (`f_init, f_concatPath, f_inPath, f_cons, f_contains, f_head, f_tail, f_isEmpty`), plus user-defined functions (UDFs). | SOSP05; THESIS ch. 3; P2 source |
| 31 | Facts | Ground facts in the program text, installed at startup (JOL schedules them at `Time+1`). | P2 grammar; JOL `runtime.olg` |
| 32 | Macros and constants | `#define NAME value` and command-line `-D` substitution; `#include(rules)`. | P2 UserGuide; THESIS §3.3.2 |
| 33 | Semi-naive evaluation | SN delta rules with the old/new distinction (no duplicate inferences) for recursive strata. | SIGMOD06 Alg. 1 |
| 34 | Pipelined / asynchronous delta processing | Process deltas as they arrive, with no global barrier. Use timestamp or sequence ordering or the ν-table scheme to avoid repeated inferences, and keep it correct under deletion (PSN^ν, not the original PSN). | SIGMOD06 Alg. 3, Thms. 1–2; NJWLS |
| 35 | Eventual consistency for monotone programs | After quiescence, distributed state equals from-scratch evaluation (the bursty model). Test this property. | SIGMOD06 Thms. 3–4 |
| 36 | Watch/trace instrumentation | Per-relation taps for insert, delete, refresh, receive, send, periodic, join and head-projection events. | P2 UserGuide §Watches; JOL `watch(t, [taeidrs])` |
| 37 | Stages / table functions / host callbacks | External functions producing tuples (`stage(...)`, JOL table functions, `async` Java calls), and host subscribe/inject APIs. | P2 UserGuide §Stages; BOOM §2.1; DECLIMP Fig. 3 |
| 38 | Durable tables | A per-table durability flag, committed atomically at the end of each fixpoint/tick. | BOOM §3.1 (Stasis); C4 `sqlite` storage |
| 39 | `async` rules | Rule evaluation off the critical path, with results inserted in a later timestep. | JOL `Driver.java`; `Core.rats` |
| 40 | Program catalog / metacompilation | Programs represented as relations (`rule`, `predicate`, `select`, `assign`, `table`, `index`, `program`) with staged rewrites written in the language. | EVITA §3; BOOM §2.1 |
| 41 | Static checks | Stratification (temporal), soft-state lifetime monotonicity (head TTL ≥ soft body TTLs), archival-rule warning, locspec well-connectedness, head-variable safety, one-event-per-rule. | EVITA; THESIS App. A.3; NR09 §4 |
| 42 | Optimizations | Aggregate selections (and periodic ones), magic sets, predicate reordering, index selection, join ordering (System R), opportunistic message batching. | SIGMOD06 §5; EVITA §4 |
| 43 | Deterministic answer to NR09's ambiguities | A documented, tested semantics for event creation vs. effect (Fig. 1) and internal vs. external events (Fig. 2). | NR09 §3 |

---

## TEST PROGRAMS

Each program is a candidate end-to-end test. "Expected" gives what the literature reports, or the property that must hold.

1. **Transitive closure / reachability** (NDlog `reachable`; BOOM Fig. 1 `path`). Expected: all reachable pairs. Under random message delay and reordering the result equals centralized Datalog (monotone, so CALM applies).

2. **Shortest path on the SIGMOD06 Fig. 2 network.** Links `e→a (1)`, `a→b (5)`, `a→c (1)`, `c→b (1)`, `b→d (1)`.
   - Expected: `shortestPath(a,b,[a,c,b],2)` replaces the earlier `[a,b],5`; `path(a,d,[a,b,d],6)` appears at iteration 2.
   - Final shortest costs: a→b 2, a→c 1, a→d 3, e→a 1, e→c 2, e→b 3, e→d 4.
   - Termination on cyclic graphs holds only with the cycle check `f_inPath` or with aggregate selection.

3. **Incremental update / bursty model** (THESIS §5.4.2 Fig. 5.7 and §3.5 Fig. 3.9; these are the thesis's own small networks, not the SIGMOD06 one).
   - Fig. 5.7: change `#link(@a,b,5)` cost to 1. It is processed as a delete plus an insert; the dependent `path` tuples are deleted and re-derived with the new cost.
   - Fig. 3.9: delete `l(@c,d,1)`. Node c derives an infinite-cost path, which propagates to a, and a recomputes `sp(@a,d,[a,b,d],3)`.
   - After quiescence, state equals from-scratch evaluation. Run with FIFO links, and also with reordering (our engine must still be correct).

4. **Distance vector with split horizon and poison reverse** (THESIS Fig. 3.4). Expected: no count-to-infinity after a link failure; convergence time proportional to the network diameter.

5. **DV vs. DSR equivalence** (THESIS Fig. 3.3 vs. Fig. 3.6). Expected: identical `bestPath` sets; only message patterns differ.

6. **Magic-sets shortest path** (SIGMOD06 §5.1.2; EVITA Fig. 8–9 topology: a 10-node chain plus clique, query to `localhost:10000`). Expected: the same answers for the magic destinations, with fewer or equal tuples sent, received and generated at every node.

7. **Ping-pong** (P2 UserGuide `pinger.olg`/`ponger.olg`). `periodic(@I,E,1,20)` gives exactly 20 pings in about 20 s and 20 pongs.

8. **Soft-state ping-pong** (THESIS Fig. 2.5).
   - `#link` survives while pongs refresh it.
   - Kill the peer: its link expires within 10 s after the last refresh.
   - `pingRTT` values are positive.

9. **Narada mesh** (SOSP05 App. A).
   - All members learn all other members, with monotonically increasing sequence numbers.
   - A dead neighbor is removed within about 20 s plus the 1 s probe interval.
   - The count-0 idiom (R5/R6) must work, which requires per-event aggregates on empty joins.

10. **Static ring lookup** (P2 UserGuide §3.1). Ring 100000@22222, 200000@11111, 300000@33333. Expected: lookup key 289383 returns `found(localhost:44444, 289383, localhost:33333)`.

11. **Dynamic and robust ring** (P2 UserGuide `dynamicRing.olg`, `robustRing.olg`).
    - The ring forms via join, stabilize and notify.
    - Kill a node: about 3 s later (MISSEDPINGS × PINGPERIOD) its predecessor declares it dead and falls back to the next `succList` entry. After stabilization the two survivors point at each other.

12. **Chord** (`doc/chord.olg`).
    - 3 nodes (IDs 0, 0x222…, 0x333…) with landmark :11111. After about 10 s the `bestSucc` chain is 0 → 0x222 → 0x333 → 0.
    - Scale-up results (SOSP05 §5): hop count averages about 0.5·log2 N; a 500-node static ring completes 96% of lookups in ≤ 6 s. Idle maintenance traffic is low (SOSP05 Fig. 3(ii)); for comparison, SOSP05 §5.1 says high-consistency DHTs typically use about 1 KB/s per node.
    - Under churn (400 nodes, session times ≥ 64 min): at least 97% of lookups are consistent. Performance was poor at ≤ 16 min sessions; this is a baseline to beat.

13. **seAtomicity** (`p2/doc/seAtomicity.olg`). `cerr` and `serr` never fire (count 0 in the same fixpoint), and `tCounter` counts from 0 to 10 across successive fixpoints. This tests deferred mutation visibility.

14. **NR09 Fig. 1.** Assert our documented choice. Under Dedalus-style semantics, `update` carries the pre-increment sequence number within the same tick.

15. **NR09 Fig. 2.** Assert our documented choice. Under set semantics, every node including node1 increments **once**. Under bag or serial semantics every node reaches 10. P2's answer (node1 = 1, others = 10) is the known-bad asymmetry.

16. **2PC** (IDODECLARE Figs. 1–2).
    - All peers vote yes: `commit`.
    - Any peer votes no: `abort`.
    - Missing votes: `abort` after more than 10 ticks.
    - Every peer eventually learns the outcome.

17. **Paxos / Multi-Paxos** (IDODECLARE §3; BOOM §4).
    - Safety: at most one value is chosen per instance.
    - A quorum requires a strict majority of promises.
    - A constrained promise carries the previously voted update.
    - With a stable leader, the log is totally ordered.
    - Leader failure leads to election and progress.

18. **Symphony degree bound** (DECLIMP §3.2.2). Two concurrent link requests at a node with 2·log n − 1 neighbors: exactly one is accepted and the degree never exceeds 2·log n. This tests atomic check-and-update.

19. **Soft-state lifetime lint** (THESIS App. A.3.1). A program whose soft head TTL is less than its body TTL must trigger a warning; at runtime it oscillates.

20. **Localization equivalence.** Chord `sb1`–`sb3` (multi-location bodies) and SP2 before and after rewrite produce the same results in a quiescent network.

21. **Evita metaprograms.** Run `stratify.olg`-equivalent rules over our program catalog. A program with `p :- notin p` gets stratum `infinity` (rejected); a legal program gets correct strata.

---

## Appendix: quick reference of first-hand code/program locations

- P2 language docs: `p2/doc/tutorial/UserGuide.tex`. Grammars: `p2/overlog/ol_parser.y`, `p2/lang/parse/olg_parser.y`.
- Evita stages: `p2/lang/olg/{delta,mview,localize,stratify,aggview1,magic,systemr,cascades,histogram,wireless}.olg`.
- P2 programs: `p2/doc/{chord,narada,gossip,symphony,ring,ring1,seAtomicity}.olg`, `p2/doc/tutorial/olg/*.olg`.
- JOL: `jol/src/jol/lang/parse/Core.rats`, `jol/src/jol/core/runtime.olg`, `jol/examples/*.olg`.
- C4: `c4/src/libc4/parser/ol_parse.y`.
- RapidNet (ns-3 NDlog descendant, seen in GitHub code search, not studied): `github.com/yakosti/rapidnet-comp/src/applications/{chord,dsr,dns,arp}/*.olg`.
