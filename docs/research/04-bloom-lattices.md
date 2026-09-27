# 04 — Lattices in the BOOM line: Bloom^L, Anna, CRDTs, Katara, Hydro `lattices`, Flo

Research report for **bloom-remake**. Written for implementers. It covers the lattice extension to
Bloom (Bloom^L), lattice composition in the Anna KVS, the CRDT literature as it relates to lattices,
Katara (CRDT synthesis), the Hydro `lattices` crate and DFIR lattice operators, the Hydroflow
delta/cumulative model, and Flo (POPL 2025). It ends with a normative design synthesis, a
**MUST-IMPLEMENT CHECKLIST**, and **TEST PROGRAMS**.

Conventions used in this document:

- "**[paper]**" = stated in the cited paper. "**[code]**" = observed in the cited source code.
- "**(analysis)**" = my own derivation or check, not stated in a source. These are marked so that
  nobody mistakes them for published claims. Where I checked an algebraic law by hand, I show the
  counterexample or the proof sketch.
- ⊔ = join / merge / least upper bound, ⊑ = lattice order, ⊥ = bottom, ⊤ = top.

---

## 0. Sources and access status

| Source | Accessed? | URL |
|---|---|---|
| Conway, Marczak, Alvaro, Hellerstein, Maier. *Logic and Lattices for Distributed Programming*, SoCC 2012 | Full text read | https://dsf.berkeley.edu/papers/socc12-blooml.pdf (also https://www.neilconway.org/docs/socc2012_bloom_lattices.pdf) |
| Same, tech report UCB/EECS-2012-167 (June 2012, earlier version) | Full text read (compared) | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2012/EECS-2012-167.pdf |
| Neil Conway, PhD thesis, *Language Support for Loosely Consistent Distributed Programming*, UCB/EECS-2014-153 | Ch. 3 (Bloom^L), §5.3 (Bloom^PO), App. A.1 read | https://www2.eecs.berkeley.edu/Pubs/TechRpts/2014/EECS-2014-153.pdf |
| Bud source (Bloom^L runtime) `lib/bud/lattice-core.rb`, `lattice-lib.rb`, `rewrite.rb`, `state.rb`, `collections.rb`, `bud.rb`, `test/tc_lattice.rb` | Read | https://github.com/bloom-lang/bud |
| bud-sandbox: `cart/cart_lattice.rb`, `cart/monotone_cart.rb`, `delivery/causal.rb`, `lattices/vc_scenario.rb` | Read | https://github.com/bloom-lang/bud-sandbox |
| The case-study page `http://db.cs.berkeley.edu/bloom-lattice` cited by the SoCC paper | **Not accessed.** The same code appears in the thesis appendix and bud-sandbox | — |
| Wu, Faleiro, Lin, Hellerstein. *Anna: A KVS for Any Scale*, ICDE 2018 | Full text read | https://dsf.berkeley.edu/jmh/papers/anna_ieee18.pdf |
| Wu, Sreekanti, Hellerstein. *Autoscaling Tiered Cloud Storage in Anna*, VLDB 2019 | Lattice-relevant parts read | http://www.vldb.org/pvldb/vol12/p624-wu.pdf |
| Anna lattice library (C++) `include/lattices/*.hpp` | Read | https://github.com/hydro-project/common (submodule of https://github.com/hydro-project/anna) |
| Sreekanti et al. *Cloudburst*, VLDB 2020 (lattice encapsulation §5.2) | §5.2 read | https://arxiv.org/abs/2001.04592 |
| Shapiro, Preguiça, Baquero, Zawirski. *A comprehensive study of Convergent and Commutative Replicated Data Types*, INRIA RR-7506 (2011) | §2–3 read (mirror copy; the HAL server blocked automated access) | https://inria.hal.science/inria-00555588 (read via https://reed.cs.depaul.edu/lperkovic/csc536/lecture10/techreport.pdf) |
| Almeida, Shoker, Baquero. *Delta State Replicated Data Types* (arXiv 1603.01529) | §4 and causal δ-CRDTs read | https://arxiv.org/abs/1603.01529 |
| Enes, Almeida, Baquero, Leitão. *Efficient Synchronization of State-based CRDTs*, ICDE 2019 | §III and appendices read | https://arxiv.org/abs/1803.02750 |
| Laddad, Power, Milano, Cheung, Crooks, Hellerstein. *Keep CALM and CRDT On*, VLDB 2023 | Read | https://arxiv.org/abs/2210.12605 |
| Laddad, Power, Milano, Cheung, Hellerstein. *Katara: Synthesizing CRDTs with Verified Lifting*, OOPSLA 2022 | §1–5 read; source `katara/lattices.py` read | https://arxiv.org/abs/2205.12425, https://github.com/hydro-project/katara |
| Hydro `lattices` crate, DFIR ops, `hydro_lang` stream types, design docs | Read at commit `9e2a120` (2026-09-24) | https://github.com/hydro-project/hydro |
| Hydro "Lattice Math" docs page | Read (from repo `docs/docs/dfir/lattices_crate/lattice_math.md`) | https://hydro.run/docs/dfir/lattices_crate/lattice_math |
| Mingwei Samuel. *Hydroflow: A Model and Runtime for Distributed Systems Programming*, UCB/EECS-2021-201 | §2–4 read | https://hydro.run/papers/hydroflow-thesis.pdf |
| Cheung, Crooks, Hellerstein, Milano. *New Directions in Cloud Programming*, CIDR 2021 | HydroLogic parts read | https://arxiv.org/abs/2101.01159 |
| Laddad, Cheung, Hellerstein, Milano. *Flo: a Semantic Foundation for Progressive Stream Processing*, POPL 2025 | Full text of arXiv v1 (Nov 2024) read; I did not compare it with the camera-ready version | https://arxiv.org/abs/2411.08274 |
| Madsen, Yee, Lhoták. *From Datalog to Flix*, PLDI 2016 | Semi-naive section and lattice semantics read | https://plg.uwaterloo.ca/~olhotak/pubs/pldi16.pdf |
| Ascent (Rust Datalog with lattices) README | Read | https://github.com/s-arash/ascent |
| Soufflé subsumption docs | Read (short page) | https://souffle-lang.github.io/subsumption |
| Kuper & Newton, *LVars* (FHPC 2013) | **Not read in full.** Only the threshold-read definition, via search summary and via Flo §6.2 | https://users.soe.ucsc.edu/~lkuper/papers/lvars-fhpc13.pdf |
| Ross & Sagiv *Monotonic aggregation* (PODS 1992); Datafun (ICFP 2016, POPL 2020); Lasp (PPDP 2015); Anna TKDE extended version | **Not read.** Mentioned only as secondary citations from papers that were read | — |

---

## 1. Executive summary (what matters for the engine and language)

1. **Generalize "relation" to "lattice".** Bloom^L replaces Datalog's single fixed lattice (sets under
   ∪) with arbitrary bounded join-semilattices. It keeps Bloom's syntax, timestep model and CALM
   analysis ([SoCC §3]).
2. **Three function classes decide both correctness and evaluation strategy.**
   - **Morphisms** (join-homomorphisms) can be evaluated on *deltas* (semi-naive).
   - **Monotone functions that are not morphisms** (Hydroflow calls these "monotone tricky", e.g.
     `size`) must be re-evaluated on the *cumulative* value, but they may appear in recursion and are
     coordination-free.
   - **Non-monotone functions** (`reveal`, negation, `lt_eq` of the receiver, `value` of a
     dominating set) require stratification, and coordination when their input arrives asynchronously.

   Sources: [SoCC §3.1, §3.2.2, §4.1], [Hydroflow thesis §2.2, §3.1].
3. **Binary operators need a per-argument notion:** a *bimorphism* is a morphism in each argument
   separately. Join, cartesian product and intersection are bimorphisms. The semi-naive rule is
   `Δout = f(Δa, B) ⊔ f(A, Δb)`, which is the symmetric hash join ([Hydro docs], [Hydroflow thesis §3.2]).
4. **Lattice values embed into relations as non-key columns under a functional dependency.** Two facts
   that agree on the key are merged column-wise with ⊔. Lattice values can never be keys
   ([SoCC §3.5.2], [Bud `collections.rb`]). Flix, Ascent and Ross–Sagiv use the same model.
5. **Threshold tests** (`gt_eq`, `contains?`, `when_true`) are how monotone growth becomes a
   discrete, once-true event without coordination. LVars, "Keep CALM and CRDT On" and Flo all make
   the same point.
6. **Composition gives safety by construction.** Complex state such as vector clocks, KVS maps, causal
   and read-committed registers are built as compositions of a few verified lattices
   ([SoCC §5], [Anna ICDE §VI]). Katara restricts CRDT synthesis to such compositions so that
   convergence is free ([Katara §5]).
7. **Beware the "dominating/lexicographic pair".** Anna's `PairLattice`, Hydro's `DomPair` and Katara's
   `LexicalProduct` are **not associative** when the first component is not a chain. Hydro documents
   and tests this. Anna and the Katara paper do not flag it. The proper lexicographic join puts ⊥ in the
   second component when the first components are incomparable ([Enes et al. App. B], analysis in §4.5).
8. **Merge must return "changed?"** (Hydro `Merge::merge -> bool`). It is the fixpoint-termination
   test and also the delta filter.
9. **Optimal deltas exist** for nearly all practical lattices (distributive + DCC):
   `Δ(a, b) = ⊔{ y ∈ ⇓a | y ⋢ b }` over the irredundant join decomposition ([Enes et al. §III]).
   Hydro's `Atomize` trait is the operational version.
10. **Flo gives the modern correctness vocabulary:** *eager execution* (determinism under incremental
    input) and *streaming progress* (no blocking on unbounded inputs), with a Bounded/Unbounded
    stream type. It models LVars (lattice collection + threshold) and DBSP (Z-sets, bilinear joins) in
    one framework.

---

## 2. Algebraic foundations (precise definitions to implement)

### 2.1 Bounded join semilattice

> "A bounded join semilattice is a triple ⟨S, ⊔, ⊥⟩, where S is a set, ⊔ is a binary operator (called
> 'join' or 'least upper bound'), and ⊥ is an element of S (called 'bottom'). The ⊔ operator is
> associative, commutative, and idempotent. The ⊔ operator induces a partial order ≤_S on the elements
> of S: x ≤_S y if x ⊔ y = y. … The distinguished element ⊥ is the smallest element in S: x ⊔ ⊥ = x for
> every x ∈ S." — [SoCC §3.1]

Bloom^L uses "lattice" to mean bounded join semilattice and "merge function" to mean least upper
bound. Anna restates the definition identically as the "ACI" properties ([Anna ICDE §III]).
Shapiro et al. define the LUB order-theoretically (Definition 2.4) and derive ACI from it
([CRDT TR §2.3.1]).

**Laws (every lattice type must satisfy all of them; the test harness must check them):**

| # | Law | Source |
|---|---|---|
| L1 | a ⊔ (b ⊔ c) = (a ⊔ b) ⊔ c | SoCC §3.1; Hydro `check_lattice_properties` |
| L2 | a ⊔ b = b ⊔ a | same |
| L3 | a ⊔ a = a | same |
| L4 | a ⊔ ⊥ = a, and `Default::default()` is ⊥ | SoCC §3.1; Hydro README "IsBot, IsTop, and Default" |
| L5 | a ⊑ b ⟺ a ⊔ b = b. Any explicit `partial_cmp` must agree with the merge-derived order | Hydro `LatticeOrd` / `NaiveLatticeOrd` / `check_lattice_ord` |
| L6 | `merge(&mut a, b)` returns `true` iff the value of `a` changed, i.e. iff b ⋢ a | Hydro `Merge` trait doc |
| L7 | ⊤, if it exists, is absorbing: a ⊔ ⊤ = ⊤. `is_top`/`is_bot` are consistent with equality | Hydro `IsTop`/`IsBot`; `check_lattice_is_top/_is_bot` |
| L8 | `PartialOrd` is reflexive, antisymmetric, transitive, and dual (a<b ⟺ b>a) | Hydro `check_partial_ord_properties` |
| L9 | Atomize: the atoms join to the original value; the atom iterator is empty iff the value is ⊥; no atom is ⊥; atoms form a *strong antichain* (pairwise meet = ⊥) | Hydro `Atomize` doc, `check_atomize_each` |
| L10 | Lattice values are immutable from the program's point of view. Methods, including merge, return new values or mutate only a private accumulator | SoCC §3.4 ("lattice elements are immutable"); Hydro merges in place into owned state |

Hydro's `NaiveLatticeOrd::naive_cmp` derives the order from merge alone:
`(a.merge(b), b.merge(a))` gives `(true,true)`→incomparable, `(true,false)`→Less,
`(false,true)`→Greater, `(false,false)`→Equal ([code `lattices/src/lib.rs`]). This is a correct
fallback comparator for user lattices that do not supply one.

### 2.2 Functions between lattices

- **Monotone function** f : S → T between posets: a ≤_S b ⇒ f(a) ≤_T f(b) [SoCC §3.1].
- **Morphism** [SoCC §3.1]: g : X → Y with **g(⊥_X) = ⊥_Y** and g(a ⊔_X b) = g(a) ⊔_Y g(b).
  The earlier TR omits the ⊥ condition, and the Hydro docs define a morphism by join-preservation
  only ("semilattice homomorphism"). Morphisms are monotone, but the converse does not hold.
  Bloom^L's formal definitions are unary, but "Bloom^L supports monotone functions and morphisms with
  any number of arguments" [SoCC footnote 2].
- **Bimorphism** [Hydro lattice-math doc; Hydroflow thesis §3.2 "split binary morphism"]:
  f : R × S → T with f(a ⊔ δa, b) = f(a, b) ⊔ f(δa, b) **and** f(a, b ⊔ δb) = f(a, b) ⊔ f(a, δb).
  The thesis points out that the "obvious" generalization f(a⊔b, x⊔y) = f(a,x) ⊔ f(b,y) is wrong: the
  cartesian product fails it because the right-hand side misses the pairs (a,y) and (b,x).
- **Binary monotone function**: a ⊑ b, x ⊑ y ⇒ f(a,x) ⊑ f(b,y). Equivalently, f is monotone on the
  product lattice [Hydroflow thesis §3.2; Hydro design doc 2023-08].
- **"Monotone tricky" (MTT)**: monotone but not a morphism, e.g. set cardinality:
  card({0,1}) + card({1,2}) = 4 ≠ card({0,1} ∪ {1,2}) = 3 [Hydroflow thesis §2.2].
  SoCC's version of the same example: size({1,2} ⊔ {2,3}) ≠ size({1,2}) ⊔_lmax size({2,3}) [SoCC §3.2.2].
- **Threshold query / monotone boolean query** ("Keep CALM" §3.2): Q is monotone if ∀ i ⊑ j: Q(i) ⇒ Q(j).
  A local replica is an under-approximation of global state, so a true answer is final. The
  `suspicious_activity` example returns `true` or `ABORT` ("unknown") and never `false`.
- **LVars threshold sets** (via Flo §6.2 and the LVars paper abstract; LVars full text not read): the
  threshold set T is non-empty and **pairwise incompatible** (lub of any two distinct elements is ⊤).
  A read returns the unique t ∈ T with t ⊑ current value, or blocks.

**Polarity (analysis):** in practice we need per-argument polarity: + (monotone), − (antitone),
0 (constant/ignored), ± (non-monotone). Examples:
- `x.lt_eq(y)` is antitone in x and monotone in y. Bud's code says of `lmap#lt_eq`: "For this to be a
  morphism, we require that (a) 'self' is deflationary (or fixed) (b) the input lattice value is
  inflationary (or fixed)" [code `lattice-lib.rb`].
- `difference(A, B)` is + in A and − in B.
- `is_top(x)` is monotone. `is_bot(x)` is antitone.

A composition is monotone iff every path from an asynchronous input to the output has an even
number of − edges and no ± edge. Plain monotonicity analysis (all +) is the special case Bloom^L
uses. The polarity idea generalizes Bloom^L; it is not in the papers.

### 2.3 Top, finite height, termination

- ⊤ is the value that dominates everything. Once an edge carries ⊤, nothing downstream can change.
  Hydroflow's **"⊤-stoning"** marks such an edge with a single bit and frees its state
  [Hydroflow thesis §3.3]. Examples: `Max<bool>` true, `WithTop` None, `Conflict` None.
- Termination of recursive lattice programs needs either the **ascending chain condition** (Flix
  requires "the Flix lattices be of finite height" [Flix §3]) or data that happens to converge. Bud's
  shortest paths over `lmin` terminates only because edge weights are positive. A negative cycle
  diverges (analysis). Hydroflow: "it is also possible to construct monotonic cycles that have
  unbounded outputs, such as a loop which continually increments a max-int. Currently it is up to the
  developer to avoid spinning infinitely" [Hydroflow thesis §3.4.2].
- **DCC and distributivity** (for optimal deltas): see §5.5.

### 2.4 Classification table: built-in lattice operations

"Class": M = morphism, BM = bimorphism, Mon = monotone non-morphism, Anti = antitone, NM = non-monotone.
"Src" says where the classification comes from (P = paper, C = Bud/Hydro code, A = my analysis).

| Lattice | Operation | Result | Class | Src / note |
|---|---|---|---|---|
| lbool (`Max<bool>`) | merge = ∨, ⊥ = false, ⊤ = true | | | P, C |
| lbool | `when_true { … }` | any lattice/collection (false ↦ ⊥/empty) | M | P (Table 3), C |
| lbool | `and(b)` | lbool | BM | A |
| lbool | `or(b)` | lbool | join-preserving in each arg, not ⊥-preserving | A |
| lbool | `not`, "else" branch | lbool | Anti | P footnote 3 ("an 'else' clause would test for an upper bound … non-monotonic") |
| lmax (`Max<T>`) | merge = max, ⊥ = −∞ (Bud) / `T::MIN` or `Default` (Hydro) | | | P, C |
| lmax | `gt(n)`, `gt_eq(n)` | lbool | M | P, C |
| lmax | `+(n)`, `−(n)`, n a constant | lmax | M | P (code has `+` only) |
| lmax | `min_of(n)` | lmax | M | C (declared `morph`) |
| lmax | `lt_eq(n)`, `lt(n)` | lbool | Anti | C (`lt_eq` is a plain method) |
| lmax | `a + b`, both lmax | lmax | BM (+ distributes over max; watch overflow and −∞) | A |
| lmin (`Min<T>`) | merge = min, ⊥ = +∞ | | | P, C |
| lmin | `lt(n)`, `lt_eq(n)` | lbool | M | P (code: `lt` only) |
| lmin | `+(n)`, `−(n)` | lmin | M | P (code: `+` only) |
| lmin | `a + b`, both lmin (tropical semiring) | lmin | BM | A. Shortest paths relies on this |
| lset (`SetUnion`) | merge = ∪, ⊥ = ∅ | | | P, C |
| lset | `intersect(lset)` | lset | M in each arg (BM) | P, C |
| lset | `project`/`pro(&blk)`, filter (blk returns nil to drop) | lset | M | P, C |
| lset | `product(lset)`, `eqjoin(lset, preds, &blk)` | lset | BM | P (`product`), C (`eqjoin`) |
| lset | `contains?(v)` | lbool | M | P, C |
| lset | `size` | lmax | Mon | P, C |
| lset | `group_count(cols)` | lmap of lmax | Mon | C |
| lset | `max_elem` → lmax, `min_elem` → lmin | | M (with ∅ ↦ ⊥) | A |
| lset | `nonempty` (= `size.gt_eq(1)`) | lbool | M | A |
| lset | `is_empty`, `difference(B)` in B | | Anti | A |
| lpset (non-negative numbers) | `sum` (`pos_sum` in code); SQL `SUM(DISTINCT …)` semantics | lmax | Mon | P, C (code comment: "actually computes SUM(DISTINCT ...)") |
| lbag | merge = per-element **max** multiplicity (sum is not idempotent) | | | P §3.3, C |
| lbag | `intersect` (per-element min), `project`, `contains?` | | M | P, C |
| lbag | `multiplicity(v)` (TR: `card(v)`) | lmax | M (maps ⊥ to 0, see §3.4) | P, C |
| lbag | `+(lbag)` (sum of multiplicities) | lbag | BM, not ⊥-preserving | P says morphism; A |
| lbag | `size` (sum of multiplicities) | lmax | Mon | P, C |
| lmap (`MapUnion<K,V>`) | merge: key union, values merged with V's ⊔ | | | P, C |
| lmap | `at(k)` | V (⊥_V if absent) | M | P, C |
| lmap | `key?(k)` | lbool | M | P, C |
| lmap | `key_set` | lset | M (keys mapped to ⊥ count as absent, see §7.2) | P, C |
| lmap | `intersect(lmap)`: keys ∩, **values joined** | lmap | BM. It is *not* the lattice meet | P, C, A |
| lmap | `project(&blk)` (paper), `apply_morph(sym,…)` (code) | lmap | M | P / C |
| lmap | `apply(sym, …)` with a monotone sym | lmap | Mon | C |
| lmap | `filter` (values are lbool; keep the true ones) | lmap | M | C |
| lmap | `to_collection(&blk)` | Bloom collection | M if blk is element-wise and does not `reveal` | C (declared `morph`) |
| lmap | `size` | lmax | Mon | P, C |
| lmap | `lt_eq(other)` | lbool | Anti in self, + in other | C comment |
| ldom (dominating set) | merge: union, then drop pairs whose version is strictly dominated | | | P §5.2.2, thesis App. A.1 |
| ldom | `version` (lub of versions) | version lattice | M | code declares `morph`. The paper calls it "monotone", which is weaker but still true |
| ldom | `value` (lub of concurrent values) | value lattice | NM | P |
| lcart | `is_complete` | lbool | Mon | P §6.1, C |
| lcart | `summary`, `checkout_addr` | Ruby value | declared Mon. They **raise** if the cart is incomplete: a partial function defined only on the "immutable" region above the threshold | C |
| any | `reveal` | host value | NM | P §3.2.2 |
| any | `==` against a non-⊤ value | bool | NM | A. Bud whitelists `:==` as monotone, which is unsound for lattice values |

---

## 3. Bloom^L in full detail

### 3.1 Motivation: two dilemmas [SoCC §1]

- **Type dilemma (Bloom/CALM).** Only sets were analyzable, so natural monotone constructs such as
  counters, timestamps and threshold tests were flagged as non-monotone. Example 2: a quorum vote
  counts votes and fires when `count(votes) > k`, which is clearly monotone, but `group`/`count` are
  aggregations and therefore non-monotone in set-CALM.
- **Scope dilemma (CvRDTs).** Small CRDTs are easy to verify but give weak guarantees. Large CRDTs (a
  cart) give strong guarantees but are hard to verify. CRDTs also guarantee nothing about *derived*
  state. Example 1: `Teams` derived from a `Students` set is not retracted when Bob is removed from
  `Students`.
- **Bloom^L's answer:** arbitrary lattices + monotone functions and morphisms between lattices + CALM
  across the whole program + semi-naive evaluation extended to lattices.

### 3.2 Language constructs [SoCC §3.2; thesis §3.2]

- **Declaration** in a `state` block: `lset :votes`, `lmax :cnt`, `lbool :quorum_done`, `lmap :kv_store`.
  Each declares an identifier bound to a lattice element, "initially bound to ⊥". The binding moves
  upward over time.
- **Persistence (code).** Bud lattice identifiers persist across timesteps. `LatticeWrapper#tick`
  merges the pending `<+` value into storage, never resets storage, and raises on an "orphaned delta".
  There is no scratch-lattice form in Bud [code `lattice-core.rb`]. Hydro's `lattice_fold` and
  `lattice_reduce` take a `'tick` or `'static` persistence argument (default `'tick`), so Hydro offers
  both [code DFIR `lattice_reduce.rs`].
- **Statements:** `<identifier> <op> <expression>`, with the same syntax as Bloom. If the lhs is a lattice:
  - only `<=` (merge now) and `<+` (merge at the next timestep) are allowed. The meaning is "the lhs
    identifier will take on the result of applying the lattice's least upper bound to the lhs and rhs
    lattice elements";
  - **the lhs and rhs must be the same lattice type**;
  - "Bloom^L does not support deletion (`<-` operator) for lattices";
  - `<~` does not apply to lattices directly. "Lattice elements can be embedded into facts that appear
    in channels" [SoCC §3.2.1].
- **Bootstrap:** `bootstrap do m1 <= 5 end` seeds values (e.g. `EmptyMaxMerge` in `tc_lattice.rb`).
- **Literals on the rhs:** Bud rewrites rhs literals into lambdas, so `m2 <= 6` or
  `h <= {"x" => m1}` work and are wrapped with the lhs lattice's constructor
  (`MaxConstructorImplicit`, `MapBareHashLiteral` tests) [code `rewrite.rb` `lambda_rewrite`].
- **Lattice references inside blocks:** a reference to a lattice identifier inside a collection block,
  e.g. `t1 <= t2 {|t| [t.x, cnt]}`, reads the *current value*. The rule "is akin to an implicit join
  with the lattice" and is rescanned when the lattice changes [code `rewrite.rb`
  `LatticeRefRewriter`, `UnsafeFuncRewriter` comments].

### 3.3 Lattice methods and their declared monotonicity [SoCC §3.2.2, §3.4; code]

- A lattice class declares a method with `morph :name do … end` (morphism), `monotone :name do … end`
  (monotone function), or a plain `def` (non-monotone).
- **`reveal`** exists on every lattice and is non-monotone: "once the underlying Ruby value has been
  extracted from the lattice, Bloom^L cannot ensure that subsequent code uses the value in a monotonic
  fashion."
- Non-monotone methods are handled "analogous to using a non-monotonic relational operator": the
  interpreter stratifies the program so the input is computed to completion first.
- **Bud-specific consistency rule (code):** if any lattice marks a method name as monotone (or morph),
  every other lattice defining a method with that name must mark it the same way, otherwise it is a
  `CompileError`. A method cannot be both morph and monotone. Every lattice must define `merge`
  [code `state.rb` `load_lattice_defs`, `lattice-core.rb`]. This exists because Bud's analysis works on
  **method names** (dynamic typing). A typed implementation classifies per (type, method) and does not
  need the global rule.

### 3.4 Built-in lattices: paper Table 3 vs Bud code

Paper Table 3 (layout reconstructed from the PDF), columns *Name / Description / ⊥ / Merge /
Morphisms / Monotone functions*:

```
lbool  Boolean lattice (false→true)   false      a∨b       when_true(&blk)→v
lmax   Max over an ordered domain     −∞         max(a,b)  gt(n)→lbool, gt_eq(n)→lbool, +(n)→lmax, −(n)→lmax
lmin   Min over an ordered domain     ∞          min(a,b)  lt(n)→lbool, lt_eq(n)→lbool, +(n)→lmin, −(n)→lmin
lset   Set of values                  empty set  a∪b       intersect(lset)→lset, project(&blk)→lset,
                                                          product(lset)→lset, contains?(v)→lbool     | size()→lmax
lpset  Set of non-negative numbers    empty set  a∪b       intersect, project, product, contains?     | size()→lmax, sum()→lmax
lbag   Multiset of values             empty mset a∪b       intersect(lbag)→lbag, project(&blk)→lbag,
                                                          multiplicity(v)→lmax, contains?(v)→lbool,
                                                          +(lbag)→lbag                               | size()→lmax
lmap   Map from keys to lattice vals  empty map  see text  intersect(lmap)→lmap, project(&blk)→lmap,
                                                          key_set()→lset, at(v)→any-lattice,
                                                          key?(v)→lbool                              | size()→lmax
```

Differences in the Bud code [`lattice-lib.rb`]:
- `lmax`: `gt`, `gt_eq`, `+` (numeric arg; "since bottom of lmax is negative infinity, + is a no-op"
  on ⊥), and `min_of` are morphs. `lt_eq` is non-monotone. There is no `−`.
- `lmin`: `lt` and `+` only.
- `lset`: `intersect`, `contains?`, `pro` (project/filter), `eqjoin` (morphs); `size`, `group_count`
  (monotone). There is no method named `product`: `eqjoin` without predicates computes the cartesian
  product.
- `lpset`: subclass of `lset` that rejects negative numbers; `pos_sum` (monotone). Code comment: "for
  methods that take a user-provided code block, we need to ensure that the set continues to contain
  only positive numbers" (not enforced).
- `lbag`: stored as `Hash` element → multiplicity (> 0). Merge takes the max. `intersect` (min),
  `multiplicity`, `+`, `contains?` are morphs; `size` is monotone. The TR calls `multiplicity` `card`.
- `lmap`: keys may not be lattices, values must be. Morphs: `at(k, bottom_class)`, `filter`,
  `apply_morph`, `key?`, `key_set`, `intersect`, `to_collection`. Monotone: `apply`, `size`.
  Non-monotone: `lt_eq`. `at` on a missing key raises unless a ⊥ class is passed: "we would like to
  return some generic 'bottom' value that is shared by all lattice types. Unfortunately, such a value
  does not exist". **A typed implementation fixes this with the static value type.**
- `lbool`: `when_true` (morph).
- Bud's lattice **equality/hash/ordering** are based on `reveal`. The code warns this "isn't always
  appropriate", e.g. set lattices whose reveal order is unpredictable [`lattice-core.rb`].

**Bottom preservation (analysis).** `lbag#multiplicity` maps ⊥ (empty bag) to `lmax(0)`, which is not
⊥ = −∞. `lbag#+` maps (⊥, c) to c. These are join-preserving but not ⊥-preserving, so they are
not morphisms under the strict SoCC definition. **For semi-naive correctness only join-preservation
matters**, as long as every morphism is evaluated once on the initial full value. Bud does that: the
first iteration of each stratum scans `current_value`, and later iterations scan deltas
[code `bud.rb` `tick_internal`, `LatticeScanner#scan`]. ⊥-preservation matters only if the engine skips
evaluation on empty deltas *before* the first full evaluation.

### 3.5 User-defined lattices [SoCC §3.4]

A lattice class inherits `Bud::Lattice` and defines:
- `initialize(i)`: wraps a Ruby object. `nil` produces ⊥. Rejects bad input with `reject_input`.
- `merge(e)`: returns the lub of `self` and `e`. "The programmer must ensure that this method satisfies
  the algebraic properties of least upper bound." `e` and `self` must be instances of the same class.
- `wrapper_name :lset`: the state-block keyword.
- Any number of `morph`, `monotone` and plain methods. Elements are immutable.

Verbatim Fig. 4 (SoCC):

```ruby
class Bud::SetLattice < Bud::Lattice
  wrapper_name :lset
  def initialize(x=[])
    @v = x.uniq # Remove duplicates from input
  end
  def merge(i)
    self.class.new(@v | i.reveal)
  end
  morph :intersect do |i|
    self.class.new(@v & i.reveal)
  end
  morph :contains? do |i|
    Bud::BoolLattice.new(@v.member? i)
  end
  monotone :size do
    Bud::MaxLattice.new(@v.size)
  end
end
```

Future work stated by the authors [SoCC §8]: a test-data generation framework for merge functions,
and "a restricted DSL for implementing lattices, which would make formal verification of correctness
an easier task". Katara and Hydro's property checkers are the later answers to this.

### 3.6 Integration with set-oriented collections [SoCC §3.5]

1. **Collection → lattice (implicit fold).** "If a statement has a Bloom collection on the rhs and a
   lattice on the lhs, the collection is converted into a lattice element by 'folding' the lattice's
   merge function over the collection": each tuple goes through the lattice constructor, then the
   results are merged. Example: `votes <= vote_chn {|v| v.voter_id}` builds singleton lsets and unions
   them. (Hydro equivalent: `map(SetUnionSingletonSet::new_from) -> lattice_fold(...)`.)
2. **Lattice values embedded in collections.** Lattice elements may be column values of facts
   ("including channels and durable storage"). Semantics:
   - If several facts are derived that "differ only in their embedded lattice values", they are
     merged into a single fact with the lattice merge function. This is a key-conflict resolution
     procedure.
   - **"Lattice elements cannot be used as keys in Bloom collections."**
   - Bud's exact algorithm [code `collections.rb` `merge_to_buf`]: on inserting a
     tuple whose key matches an existing tuple, (a) an identical tuple is ignored; (b) if any non-key
     column differs and is not a lattice value on both sides, raise `KeyConstraintError`; (c)
     otherwise build a new tuple where each differing lattice column is `old.merge(new)`, and replace
     the stored tuple only if some column actually changed (`reveal` comparison).
   - Channel payloads may carry lattices: `chn <~ do_send {|t| [t.addr, m]}` sends the current `lmax`
     value (`MaxOverChannel` test). The receiver sees a tuple containing a lattice value.
3. **Lattice → collection.** `to_collection` on lmap, `when_true { [tuple] }` on lbool, and the push
   runtime's fallback: "if we're emitting outputs to a traditional Bloom collection … we simply assume
   the value embedded inside the lattice is an Enumerable that contains tuple-like values"
   [code `lattice-core.rb` `push_out`, with the comment "XXX: rethink this"].

Relation to the literature [SoCC §7]: Ross & Sagiv's monotonic aggregation also "require[s] the
lattice-valued columns to be functionally dependent on the other attributes in a predicate". Bloom^L
differs in exploiting idempotence ("gives confluence with 'at-least-once' message delivery") and in
singling out morphisms. Köstler et al.'s "reduced interpretations" and Zaniolo/Wang's LDL++ partial
aggregates are contrasted. LDL++ `choice()` is rejected because it would need coordination.

### 3.7 CALM analysis with lattices [SoCC §3.6]

- A point of order is "a program location where an asynchronously derived value is consumed by a
  non-monotonic operator". Bloom^L "simply expands the set of monotonic operations". The Bud change
  was "replac[ing] the hard-coded list of monotonic operations with a list of the monotonic methods
  defined by the lattices", about 10 LOC [SoCC §4.3].
- The formal model-theoretic semantics for Bloom^L "remains a topic for future work" [SoCC §3.6]. No
  later paper I read supplies it. Flo (§8) is the closest modern semantic foundation.
- Bud implementation detail: in `RuleRewriter` a rule is non-monotonic (`@nm = true`) if it calls a
  method that is not in `MONOTONE_WHITELIST` (`==, +, <=, -, <, >, *, ~, +@, pairs, matches, combos,
  flatten, new, lefts, rights, map, flat_map, pro, merge, schema, cols, key_cols, val_cols, payloads,
  lambda, tabname, current_value`), not a lattice morph, and not a lattice monotone function. Deletion
  (`-@`, i.e. `<-`) is non-monotone [code `rewrite.rb`]. Whitelisting `<`, `>`, `==` and `-` is
  **unsound** when they are applied to revealed lattice values (analysis).

### 3.8 Evaluation: semi-naive for lattices [SoCC §4.1; code]

The paper's scheme:
- For identifier l: Δ⁰ₗ is its value at the start of the timestep. Δʳₗ is the "new derivations for l …
  in evaluation round r". Round r evaluates statements with l bound to Δʳ⁻¹ₗ. The final value is
  ⊔ⱼ Δʲₗ.
- "This optimization cannot be used for monotone functions that are not morphisms", because
  semi-naive "effectively distribut[es] the function across the merge". For `size`, the semi-naive
  strategy would compute ⊔ⱼ size(Δʲ), "the maximum of the sizes of the incremental results", which is
  wrong.
- Implementation: "For each lattice identifier l, we record two values: a 'total' value … and a 'delta'
  value (the least upper bound of the derivations made for l in the last round). … If a statement only
  applies morphisms to lattice elements, the rewrite adjusts the statement to use the lattice's delta
  value rather than its total value."

What the Bud code actually does [`lattice-core.rb`, `bud.rb`]:
- `LatticeWrapper` keeps `@storage` (total), `@delta` (the previous round's new derivations),
  `@new_delta` (this round's derivations, merged), and `@pending` (from `<+`).
- `tick_deltas` merges `@new_delta` into storage, sets `@delta = @new_delta`, and returns whether
  storage changed. The per-stratum fixpoint loop repeats until no merge target reports a change
  (`until fixpoint … fixpoint = false if t.tick_deltas`).
- `LatticeScanner#scan(first_iter)` pushes `current_value` on the first iteration of a stratum (or
  when `disable_lattice_semi_naive` is set) and `current_delta` otherwise.
- `PushApplyMethod` (one per method call in the dataflow):
  - if the method is a **morph**, it applies it to the incoming value (a delta) directly;
  - if it is **monotone/other**, it merges the incoming value into a cached receiver (`@recv_cache`)
    and applies the method to the whole accumulated value;
  - for lattice-valued **arguments** it caches the merged argument (`@input_caches`). For morphs, when
    an argument delta arrives it computes `recv_cache.meth(arg_delta)`, and when a receiver delta
    arrives it computes `recv_delta.meth(arg_total)`. This is the bimorphism rule
    Δ = f(ΔA, B) ⊔ f(A, ΔB).
- The Δ that flows is **not minimized**: it is the lub of everything derived into `@new_delta` in the
  round, which may overlap the total. Correct by idempotence, but wasteful.

Performance [SoCC §4.2, Fig. 5]: transitive closure over DAGs with ~log₂n out-edges per node. Naive
Bloom^L was much slower. Semi-naive Bloom^L ≈ set-based Bloom with similar derivation counts. Bloom
pulled ahead on large inputs because "the lset merge function allocates a new object … In contrast,
Bloom collections are modified in-place". The paper plans in-place updates when safe.
**Lesson: in-place merge into owned accumulators, which Hydro's `Merge` does, is required for
performance.**

Implementation size [SoCC §4.3]: Bud was about 7300 LOC. Bloom^L added under 1000 (300 LOC of
built-in lattices, 250 LOC of lattice query-plan elements), with no changes to the core fixpoint loop.

### 3.9 Case studies and verbatim programs

**Fig. 2, non-monotone quorum (Bloom):**
```ruby
QUORUM_SIZE = 5
RESULT_ADDR = "example.org"
class QuorumVote
  include Bud
  state do
    channel :vote_chn, [:@addr, :voter_id]
    channel :result_chn, [:@addr]
    table   :votes, [:voter_id]
    scratch :cnt, [] => [:cnt]
  end
  bloom do
    votes      <= vote_chn {|v| [v.voter_id]}
    cnt        <= votes.group(nil, count(:voter_id))
    result_chn <~ cnt {|c| [RESULT_ADDR] if c >= QUORUM_SIZE}
  end
end
```

**Fig. 3, monotone quorum (Bloom^L):**
```ruby
class QuorumVoteL
  include Bud
  state do
    channel :vote_chn, [:@addr, :voter_id]
    channel :result_chn, [:@addr]
    lset  :votes
    lmax  :cnt
    lbool :quorum_done
  end
  bloom do
    votes       <= vote_chn {|v| v.voter_id}
    cnt         <= votes.size
    quorum_done <= cnt.gt_eq(QUORUM_SIZE)
    result_chn  <~ quorum_done.when_true { [RESULT_ADDR] }
  end
end
```

**KVS (Figs. 6–7) and quorum client (thesis App. A.1):**
```ruby
module KvsProtocol
  state do
    channel :kvput, [:reqid, :@addr] => [:key, :val, :client_addr]
    channel :kvput_resp, [:reqid] => [:@addr, :replica_addr]
    channel :kvget, [:reqid, :@addr] => [:key, :client_addr]
    channel :kvget_resp, [:reqid] => [:@addr, :val, :replica_addr]
  end
end
class KvsReplica
  include Bud
  include KvsProtocol
  state { lmap :kv_store }
  bloom do
    kv_store   <= kvput {|c| {c.key => c.val}}
    kvput_resp <~ kvput {|c| [c.reqid, c.client_addr, ip_port]}
    kvget_resp <~ kvget {|c| [c.reqid, c.client_addr, kv_store.at(c.key), ip_port]}
  end
end
```
The thesis version passes `val_class` in `kvget` so that `at(key, val_class)` can build ⊥, and adds
`ReplicatedKvsReplica` (anti-entropy: `repl_propagate <~ kvrepl {|r| [r.target_addr, kv_store]}`,
`kv_store <= repl_propagate {|r| r.kv_store}`) and `QuorumKvsClient`. The quorum client uses
`put_reqs <= kvput_response {|r| [r.reqid, Bud::SetLattice.new([r.replica_addr])]}` in a table keyed by
`reqid` with an embedded lset, then `r.acks.size.gt_eq(@w_quorum_size).when_true { [r.reqid] }`. This
is **the embedded-lattice + threshold pattern**.

**Vector clocks** [SoCC §5.2.1]: `lmap` from node id to `lmax`. "The merge function provided by lmap
achieves the desired semantics." Ve < Ve′ ≡ ∀x[Ve(x) ≤ Ve′(x)] ∧ ∃y[Ve(y) < Ve′(y)]. The paper
attributes the lattice structure of vector time to Mattern.

**Dominating set `ldom`** [SoCC §5.2.2; thesis Listing A.1, verbatim core]:
```ruby
class DomLattice < Bud::Lattice
  wrapper_name :ldom
  def initialize(i=nil)   # Hash: version-lattice => value-lattice
    ...
  end
  def merge(i)
    i_val = i.reveal
    return i if @v.nil?
    return self if i_val.nil?
    rv = {}
    preserve_dominants(@v, i_val, rv)
    preserve_dominants(i_val, @v, rv)
    wrap_unsafe(rv)
  end
  morph :version do
    compute_reconcile
    @reconcile.first unless @reconcile.nil?
  end
  def value
    compute_reconcile
    @reconcile.last unless @reconcile.nil?
  end
  private
  def preserve_dominants(target, other, rv)
    target.each_pair do |k1, val|
      # A key/value pair is included in the result UNLESS there is another key
      # in the other input that dominates it. ...
      next if other.keys.any? {|k2| k2.merge(k1) == k2 && k1 != k2}
      rv[k1] = val
    end
  end
  def compute_reconcile
    return if @v.nil? or @reconcile
    @reconcile = [@v.keys.reduce(:merge), @v.values.reduce(:merge)]
  end
end
```
Semantics: an antichain of ⟨version, value⟩ pairs (a Dynamo-style multi-value register). Server
cases: V_U > V_S replaces, V_U < V_S keeps V_S, incomparable keeps both. `version` is a morphism,
`value` is not monotone. The KVS replica needs **no code changes**: clients store single-pair ldoms
and the lmap merge does the rest. Quorum reads take the lub of R responses; read repair writes the lub
back. The KVS is under 100 lines of Ruby+Bloom^L; ldom is 50 more. Voldemort's vector clock class
alone is 216 lines of Java [SoCC §5.2.3].

Edge case (analysis): if the two inputs contain the **same version with different values**,
`preserve_dominants` writes `rv[k1]` twice and the last write wins, which is non-commutative. This
requires unique versions per write. Clients must increment their own entry on every write.

**Monotone shopping cart** [SoCC §6, Fig. 10; bud-sandbox `cart/`]:
```ruby
module MonotoneReplica
  include CartProtocol
  state { lmap :sessions }
  bloom do
    sessions <= action_msg do |m|
      c = LCart.new({m.op_id => [ACTION, m.item, m.cnt]})
      { m.session => c }
    end
    sessions <= checkout_msg do |m|
      c = LCart.new({m.op_id => [CHECKOUT, m.lbound, m.addr]})
      { m.session => c }
    end
    response_msg <~ sessions do |session, cart|
      cart.is_complete.when_true {
        [cart.checkout_addr, session, cart.summary]
      }
    end
  end
end
```
lcart semantics (from `cart_lattice.rb`): a map from op-id to `[ACTION_OP, item, mult]` or
`[CHECKOUT_OP, lbound, addr]`. There is at most one checkout. All ids must lie in
`[lbound, checkout_id]`; any other input is rejected with an exception, because a violation "likely
indicates a logic error". Merge unions the maps and **raises if the same id maps to different values**
(Point/Conflict semantics). `is_complete` holds when a checkout exists and every id in
`lbound..ubound` is present. `summary` sums multiplicities per item, keeps the positive ones, and
raises if the cart is incomplete. Discussion points:
- The design needs single-client knowledge of the op range.
- Non-monotone intermediate states are hidden inside the lattice.
- "monotonic-then-immutable" values are a general pattern.
- Code comment: "we will send an unbounded number of response messages for each complete cart",
  because the persistent lmap re-fires the `<~` rule every tick.

**Causal delivery** (bud-sandbox `delivery/causal.rb`, Schiper–Eggli–Sandoz):
```ruby
bloom :update_vc do
  next_vc <= my_vc
  next_vc <= pipe_in { {ip_port => my_vc.at(ip_port) + 1} }
  next_vc <= buf_chosen { {ip_port => my_vc.at(ip_port) + 1} }
  next_vc <= buf_chosen {|m| m.clock}
  my_vc <+ next_vc
end
bloom :outbound_msg do
  chn <~ pipe_in {|p| [p.dst, p.src, p.ident, p.payload, next_vc, ord_buf]}
  ord_buf <+ pipe_in {|p| {p.dst => next_vc} }
  pipe_sent <= pipe_in
end
bloom :inbound_msg do
  recv_buf <= chn
  buf_chosen <= recv_buf {|m| m.ord_buf.at(ip_port, Bud::MapLattice).lt_eq(my_vc).when_true { m } }
  recv_buf <- buf_chosen
  pipe_out <= buf_chosen {|m| [m.dst, m.src, m.ident, m.payload]}
  ord_buf <+ buf_chosen {|m| m.ord_buf}
end
```
This uses nested lattices (`lmap` of `lmap` of `lmax`), `<+` on lattices, lattices in channel columns,
and a non-monotone `lt_eq` test that is correctly stratified. The code notes that deleting obsolete
`ord_buf` entries "would make ord_buf not a lattice".

**Shortest paths with an embedded lmin** (bud `test/tc_lattice.rb`, verbatim):
```ruby
class ShortestPathsL
  include Bud
  state do
    table :link, [:from, :to, :c]
    table :path, [:from, :to, :next_hop] => [:c]
    table :min_cost, [:from, :to] => [:c]
  end
  bloom do
    path <= link {|l| [l.from, l.to, "direct", Bud::MinLattice.new(l.c)]}
    path <= (link * path).pairs(:to => :from) do |l,p|
      [l.from, p.to, l.to, p.c + l.c]
    end
    min_cost <= path {|p| [p.from, p.to, p.c]}
  end
end
```
The test asserts that all three collections are in stratum 0: recursion through lattice arithmetic
needs no stratification. It also checks exact outputs for acyclic and cyclic inputs (see TEST PROGRAMS).

### 3.10 Known limitations of Bloom^L that later work addressed

| Limitation | Where stated | Later work |
|---|---|---|
| No formal (model-theoretic) semantics | SoCC §3.6 | Flo (eager execution / streaming progress), Hydro stream types |
| Programmer must prove ACI for custom lattices | SoCC §1, §8 | Hydro `test::check_*`, Katara synthesis + SMT, Hydro Verus proofs for commutativity/idempotence |
| Immutable values cause copying, which is slow | SoCC §4.2 | Hydro in-place `Merge::merge(&mut self, …) -> bool` |
| No generic ⊥ in a dynamically typed language (`lmap#at` needs a class) | code | typed lattices (Hydro `Default` = ⊥) |
| Method-name-based monotonicity, `==` whitelisted | code | typed per-operator properties (Hydro `monotone = proof`) |
| Garbage collection of lattice state | SoCC §8 (GLB across replicas idea) | Edelweiss (Bloom, not lattices); Anna delete-with-timestamps; tombstone lattices in Hydro; ⊤-stoning |
| "Monotonic-then-immutable" values | SoCC §8 | Flo `fixed`/bounded streams; LVars freeze (not read in full) |
| Persistent lattices re-fire async rules every tick | code comment in `monotone_cart.rb` | Hydro `state[items]` emits only changed items; delta edges |
| Escrow (bounded decrements) | SoCC §7 ("currently exploring") | not addressed in anything I read |

**Bloom^PO** (thesis §5.3, a side note): `po_table` and `po_scratch` collections carry monotonicity
constraints (the second column is smaller in some partial order). They support **universal
constraint stratification** (Ross), which allows cycles through negation that respect a data-level
partial order, e.g. a part hierarchy or causal-history DAG. The strata are manually declared
(`stratum 0 do … end`). Evaluation is "stratified enumeration" over poset strata stored in a
"stratified graph". This belongs to the stratification cluster but is lattice/poset-adjacent.

---

## 4. Anna: lattice composition for consistency levels

### 4.1 Architecture relevant to us [Anna ICDE §IV–V, §VII]

- **Coordination-free actors:** one thread per core, private state (a lattice map), no shared memory.
  "Lattices do not change the above discussion; any shared-memory lattice implementation is subject to
  the same synchronization overheads."
- **Multi-master replication with epoch multicast (gossip).** Each actor serves puts and gets locally
  and appends to a *changeset*. At the end of each multicast epoch it sends the changeset to the other
  replicas of each key and merges incoming multicasts.
- **Merge-at-sender:** by associativity, a burst u₁…uₙ on a hot key can be sent as one merged value:
  ⊔(…⊔(⊔(s,u₁),u₂)…,uₙ) = ⊔(s, ⊔(…⊔(u₁,u₂)…,uₙ)).
- GET reads a single replica; staleness is bounded by the multicast period. PUT merges into one
  replica.
- **DELETE** is a PUT with an empty value. Memory is freed only "when the minimum timestamp within
  the vector-clock [of latest-heard timestamps from all actors] becomes greater than the DELETE's
  timestamp". This relies on ordered point-to-point channels. For vector-clock consistency levels the
  actor first asks the other replicas for the key's vector clock ([Anna ICDE §VII]). This is a
  **coordinated GC protocol layered on lattices**.

### 4.2 Consistency via lattice composition [Anna ICDE §VI]

- Worker state is `MapLattice<Key, ValueLattice>`, following Conway.
- "Simple eventual consistency" means ad hoc user merge functions. Anna guarantees only convergence
  for these.
- **Causal consistency** (Fig. 2 of the paper): the vector clock is a `MapLattice<proxy_id,
  MaxIntLattice>`. The value is a `PairLattice` merged "in lexicographic order": if P.a ⊐ Q.a take P;
  if Q.a ⊐ P.a take Q; if incomparable, (P.a ⊔ Q.a, P.b ⊔ Q.b). Proxies do read-modify-write,
  incrementing their own entry.
- **Read committed** (coordination-free definition from HAT/Bailis): the vector clock is replaced by a
  `MaxIntLattice` transaction timestamp ("larger timestamp wins"). Equal timestamps mean the same
  transaction and invoke the value merge. Dirty reads are prevented by buffering a transaction's
  writes at the client proxy until commit. For multi-statement SQL transactions:
  "a nested PairLattice of (transaction timestamp, command number), both being MaxIntLattices".
- **Read uncommitted** uses the same composition as RC without client buffering. **Item-cut isolation**
  uses the same composition with a client-side cache of values already read in the transaction.
- Fig. 3 of the paper lists LOC per level (Lattice/Server/Client proxy) for Causal, RU, RC, Item Cut,
  Monotonic Reads, Monotonic Writes, Writes Follow Reads, Read Your Writes and PRAM. The ICDE text
  **does not describe** the compositions for the session guarantees beyond the LOC table.
- Transaction IDs are "generated by concatenating a unique actor sequence number with a local
  timestamp".
- The VLDB 2019 paper states that Anna stores data in **last-writer-wins lattices by default** and that
  lattices "can be composed to offer the full range of coordination-free consistency guarantees".
- **Cloudburst §5.2:** Python values are wrapped in an Anna **LWW lattice** whose timestamp is
  "generated … by concatenating the local system clock and the node's unique ID", which makes ties
  impossible. In causal mode each key goes in a **causal lattice** (vector clock + dependency set +
  value). Concurrent versions merge by pairwise max of clocks and set union of dependencies and
  values. Readers get one deterministic tie-break version while the cache keeps all concurrent
  versions.

### 4.3 Anna's C++ lattice library [code `hydro-project/common/include/lattices`]

| Class | Merge | Notes |
|---|---|---|
| `Lattice<T>` | abstract `do_merge` | `reveal()`, `merge(T)`, `merge(Lattice)`, `assign`. Equality by reveal |
| `BoolLattice` | `element \|= e` | ⊥ false |
| `MaxLattice<T>` | keep the larger | Bug: `int current = this->element;` truncates non-int T. Has `add` and `subtract` (non-destructive) |
| `SetLattice<T>` | insert all | `size()→MaxLattice<unsigned>`, `intersect`, `project(pred)` (a filter) |
| `OrderedSetLattice<T>` | same over an ordered set | |
| `MapLattice<K,V>` | per-key `V::merge`, insert if absent | `intersect` merges both values (same as Bud), `project`, `contains→BoolLattice`, `key_set`, `at`, **`remove`** (non-monotone, used for GC) |
| `LWWPairLattice<T>` | take incoming if `p.timestamp >= this.timestamp` | **`>=` makes merge non-commutative on equal timestamps with different values.** Relies on unique timestamps (clock ‖ node id) |
| `PriorityLattice<P,V,Compare>` | keep the pair with the smaller priority (`Compare = less`) | default priority `INT_MAX`. Equal priorities keep self, so ties are non-commutative too |
| `SingleKeyCausalLattice<T>` | merge VCs; if merged == incoming VC then assign the incoming value, else if merged ≠ previous VC then merge values | the dominating-pair rule with a vector-clock key |
| `MultiKeyCausalLattice<T>` | same, plus `dependencies: MapLattice<Key, VectorClock>` assigned or merged alongside the value | used by Cloudburst/HydroCache |

### 4.4 Lessons for our design

1. Consistency levels are **lattice compositions plus client-proxy protocol**: buffering writes for
   RC, caching reads for item-cut. The server stays a lattice map. Our language should express a
   "KVS with consistency level X" as a library that picks the value lattice and a client module.
2. A timestamp domain must be totally ordered and **tie-free**: (clock, node_id), or Lamport
   (counter, node_id).
3. Merge-at-sender batching falls out of associativity. The runtime should always pre-merge outgoing
   lattice messages per destination and per key within a send epoch.
4. GC of lattice state (deletes) needs a separate, coordinated (or stability-based) protocol.

### 4.5 The dominating/lexicographic pair is not a lattice in general (analysis + sources)

Define DomPair merge as in Anna's causal lattice, Hydro `DomPair`, and Katara `LexicalProduct`:
(a₁,b₁) ⊔ (a₂,b₂) = (a₁,b₁) if a₁ ⊐ a₂; (a₂,b₂) if a₂ ⊐ a₁; otherwise (a₁⊔a₂, b₁⊔b₂).

**Counterexample (analysis).** Take the key lattice = sets and x = ({1},p), y = ({2},q),
z = ({1,2},r).
- (x ⊔ y) ⊔ z = ({1,2}, p⊔q) ⊔ ({1,2}, r) = ({1,2}, p⊔q⊔r).
- x ⊔ (y ⊔ z) = x ⊔ ({1,2}, r) = ({1,2}, r).

These differ whenever p⊔q ⋢ r, so associativity fails.

- Hydro documents this: "Note that this is not a proper lattice, it fails associativity. However it
  will behave like a proper lattice if `Key` is a totally ordered lattice or a properly formed vector
  clock lattice. The exact meaning of 'properly formed' is still TBD, but each node always
  incrementing its entry for each operation sent should be sufficient." Its unit test asserts that
  `check_lattice_properties` **panics** for DomPair [code `dom_pair.rs`].
- The Katara paper (§5.1) states that LexicalProduct "respects the lattice axioms". Its code
  (`katara/lattices.py`) implements exactly the rule above. That holds when the first component is a
  chain (e.g. `MaxInt` clocks, which is how Katara uses it) but not in general.
- **The proper lexicographic product** (order: a₁ < a₂, or a₁ = a₂ ∧ b₁ ≤ b₂) has join
  (a₁ ⊔ a₂, ⊥_B) when a₁ and a₂ are incomparable. Enes et al. App. B Fig. 13 shows this: the join of
  ⟨{a},·⟩ and ⟨{b},·⟩ is ⟨{a,b}, ∅⟩. They also note that the lexicographic product "with an arbitrary
  first component" is **not distributive**, so optimal deltas are not unique. "Fortunately, the
  typical use of lexicographic products to design CRDTs is with a chain (total order) as the first
  component."
- **For "keep concurrent values" semantics** use the antichain of maximal elements (Bloom^L `ldom`,
  Shapiro's MV-Register, Enes' M(P) construct, δ-CRDT `Causal<DotFun>`). These are proper lattices.
- **Reachable-state argument (analysis):** DomPair behaves associatively on reachable states if no
  write carries a clock *exactly equal* to the join of two incomparable clocks. Every write increments
  its writer's own entry past the join it read, so the only elements with that clock are merge results.
  Katara's synthesized `invariant*`/`orderWithState*` is the kind of mechanism that could discharge
  this obligation formally.

**Requirement:** our built-in `DomPair`/`Lex` must either (a) require a chain-typed key at the type
level, or (b) implement the proper lexicographic join, with a separate `MVReg`/`Antichain` type for
multi-value semantics. The property checker must include a DomPair-like negative test.

---

## 5. CRDTs and their relation to Bloom^L lattices

### 5.1 Definitions [CRDT TR §2]

- Eventual convergence (Def. 2.3). *Safety*: equal causal histories imply equivalent abstract states
  ("all query operations return the same values"). *Liveness*: every update eventually reaches every
  replica.
- **CvRDT (state-based):** the payload is a join semilattice, `merge(x,y) = x ⊔ y`, and updates move
  upward ("monotonic semilattice"). It needs `compare(x,y)` = x ≤ y and merge always enabled.
  Prop. 2.1: replicas converge given payload transmitted "infinitely often between pairs of replicas
  over eventually-reliable point-to-point channels". It tolerates loss, reordering and duplication.
- **CmRDT (op-based):** reliable broadcast in a delivery order <d (causal suffices). Concurrent ops
  commute. Commutativity (Def. 2.6) includes preservation of source preconditions. Prop. 2.2 gives
  convergence. The TR shows state- and op-based are mutually emulable (Specs 3–4).
- The TR's related-work section on Bloom: "Monotonic logic is more restrictive than our monotonic semilattice. Thus,
  Bloom does not support remove without synchronisation." (That was written about pre-Bloom^L Bloom.)
- "Keep CALM" §2.2: a CmRDT log is itself a CvRDT (a grow-only set of DAG edges of operations), so
  op-based CRDTs "are arguably a specific form of a CvRDT for partially-ordered logs", i.e. a gossip
  compression technique.

### 5.2 The CRDT catalog as lattice compositions (for the standard library)

| CRDT (source) | Lattice composition | Query and its class |
|---|---|---|
| G-Counter (TR Spec 6; δ paper Fig. 2) | `MapUnion<ReplicaId, Max<u64>>`. Increment = local entry + 1. δ-mutator returns `{i ↦ m(i)+1}` | `value = Σ entries`: Mon (not M) |
| PN-Counter (TR Spec 7) | `Pair<GCounter, GCounter>` | `P − N`: NM (neither monotone nor antitone overall) |
| Non-negative counter (TR §3.1.4) | not achievable without a local invariant (per-replica P[g]−N[g] ≥ 0) or synchronization (escrow) | — |
| LWW-Register (TR Spec 8) | `Lex<Max<(ts,node)>, Point/any>`: chain key, so this is a proper lattice | `value`: NM |
| MV-Register (TR Spec 10; δ paper Fig. 14) | antichain of (value, VV) / `Causal<DotFun<V>>` | `value` returns the set of concurrent values: NM as a register read. The version part is M |
| G-Set (TR Spec 11) | `SetUnion<T>` | `lookup(e)`: M |
| 2P-Set (TR Spec 12; Katara Fig. 2) | `Pair<SetUnion<T>, SetUnion<T>>` (A, R). Remove requires a prior add | `lookup = e∈A ∧ e∉R`: NM ("Potato and Ferrari") |
| U-Set (TR Spec 13) | 2P-Set with unique elements, op-based | |
| LWW-element-Set, PN-Set (TR §3.3) | map element → LWW flag / counter | |
| OR-Set (TR Spec 15, op-based) / Add-Wins set (δ paper Fig. 15) | `Causal<DotMap<E, DotSet>>` | `elements`: NM |
| Enable-Wins Flag (δ paper Fig. 13) | `Causal<DotSet>`. enable = new dot + remove existing dots via context | `read = store ≠ {}`: NM |
| 2P2P-Graph, Add-only monotonic DAG, Add-Remove Partial Order, RGA, continuum sequence, OR-Cart (TR Specs 16–21) | graph/sequence CRDTs (op-based in the TR) | beyond Bloom^L's scope; candidate library items |

Causal δ-CRDT lattice [δ paper Fig. 12] (for a `Causal` combinator), where c is the causal context
(set of dots, compressible to a version vector under causal anti-entropy):
- DotSet: (s,c) ⊔ (s′,c′) = ((s ∩ s′) ∪ (s \ c′) ∪ (s′ \ c), c ∪ c′)
- DotFun: (m,c) ⊔ (m′,c′) = ({k ↦ m(k) ⊔ m′(k) | k ∈ dom m ∩ dom m′} ∪ {(d,v) ∈ m | d ∉ c′} ∪ {(d,v) ∈ m′ | d ∉ c}, c ∪ c′)
- DotMap: (m,c) ⊔ (m′,c′) = ({k ↦ v(k) | k ∈ dom m ∪ dom m′ ∧ v(k) ≠ ⊥}, c ∪ c′), where v(k) = fst((m(k),c) ⊔ (m′(k),c′))

"A dot present in a causal context but not in the corresponding dot store means that the dot was
present in the dot store, some time the past, but has been removed meanwhile." Removal therefore
needs **no tombstones**. It is the mechanism for monotone "removes".

### 5.3 δ-CRDTs [δ paper §4]

- **Delta-mutator** m^δ: S → S. **Delta-group**: a delta or a join of delta-groups. A δ-CRDT is
  (S, M^δ, Q) where a state transition is X′ = X ⊔ m^δ(X) or X′ = X ⊔ D.
- Decomposition: m(X) = X ⊔ m^δ(X). Deltas should be **minimal** (no redundant information). "A full
  state can be seen as a special (extreme) case of a delta-group."
- Without causal requirements, delta-groups may travel over lossy/reordering/duplicating channels.
  For causal consistency: **delta-intervals** Δᵃ'ᵇ plus the **causal delta-merging condition** (only
  join Δᵃ'ᵇ into a state that already reflects the first a deltas).
- **Relation to semi-naive (analysis):** a δ-mutator plays exactly the role of a semi-naive delta. In
  both, correctness needs only X ⊔ Δ = X′, and "minimality" is an efficiency property. Our engine's
  delta propagation and CRDT anti-entropy should be **the same mechanism**, with deltas shipped over
  channels.

### 5.4 Queries over CRDTs ["Keep CALM and CRDT On"]

- CRDTs guarantee state convergence but "offer no APIs (or guarantees!) for visibility into the
  state". Queries are "no safer to use than arbitrary queries executed directly on the underlying
  state".
- **Potato/Ferrari:** checkout = A − R on a 2P-Set can run before the Ferrari removal arrives.
- **Monotone queries are exactly the queries that are safe to run locally** (via CALM). Example:
  `suspicious_activity` (count of large gift-card txns > 50 → `true`, else ABORT) and
  `|A| + |R| > 100` are safe.
- For non-monotone queries: coordinate with a quorum (write-one/read-all, majority/majority,
  write-all/read-one), or accept stale local reads.
- The authors propose a SQL dialect over semilattices where monotonicity is syntactically visible. They
  are "currently working on a similar formalization for extending relational algebra to
  semi-lattices".
- **Implication:** our language should let users write CRDT queries as rules and have the compiler
  classify them (local vs needs-coordination) automatically. That is Bloom^L's CALM analysis applied
  to queries.

### 5.5 Join decompositions and optimal deltas [Enes et al.]

- **Join-irreducible** (Def. 1): x = ⊔F ⇒ x ∈ F. ⊥ is never join-irreducible.
- **Join decomposition** (Def. 2): D ⊆ J(L) with ⊔D = x. **Irredundant** (Def. 3): no proper subset
  joins to x.
- In a distributive lattice with DCC every element has a **unique irredundant decomposition** ⇓x
  (Prop. 1). With a finite ideal: ⇓x = max{ r ∈ J(L) | r ⊑ x } (Prop. 2, Birkhoff).
- **Optimal delta:** Δ(a,b) = ⊔{ y ∈ ⇓a | y ⋢ b }. It satisfies Δ(a,b) ⊔ b = a ⊔ b and is minimum
  among all c with c ⊔ b = a ⊔ b. Optimal δ-mutator: m^δ(x) = Δ(m(x), x).
- **Decomposition rules** (App. C): chain c ↦ {c}; A×B ↦ ⇓a×{⊥} ∪ {⊥}×⇓b; C⊠A (lex with chain first)
  ↦ ⇓c × ⇓a; linear sum Left/Right ↦ tagged decompositions; U↪A (maps) ↦ {k ↦ v | v ∈ ⇓f(k)};
  P(U) ↦ singletons; M(P) (maximal elements) is also listed.
- **Composition closure (Table III):** A×B, C⊠A, A⊕B, P(U), U↪A and M(P) preserve DCC and
  distributivity. **A⊠B with a non-chain A is not distributive.**
- **Hydro's `Atomize` is the operational form of ⇓** (strong antichain of non-⊥ atoms that join back to
  the value) [code `lib.rs`].

---

## 6. Katara: synthesizing CRDTs with verified lifting [OOPSLA 2022]

### 6.1 Specification

- Input: a sequential data type (C/C++, lowered via LLVM) given as `init_state`, a state transition
  `st(s, o)` and `query(s, q)`, plus a user **operation ordering** `opOrder(o₁,o₂)` ("returns true when
  a call to o₂ is allowed to occur after a call to o₁"). This resolves non-commuting operations.
  Remove-before-insert yields a G-Set. Insert-before-remove yields a 2P-Set.
- Optional **timestamps** (Lamport): opOrder(o₁,o₂) ≜ (o₁.t < o₂.t) ∨ (o₁.t = o₂.t ∧ opOrder_orig(o₁,o₂)).
  `opPrecondition(o) = o.t > 0` excludes degenerate timestamps.
- Correctness: the CRDT and the sequential type answer every query identically after any in-order
  sequence of operations. This is sufficient because CRDT operations commute, so distributed
  executions flatten to some sequence.

### 6.2 Verification conditions (SMT via Rosette/Metalift)

- A bisimulation-style **`equivalent*(s, s*, q)`**: the initial states are equivalent; `st` preserves
  equivalence; equivalent states give equal query results.
- The ordering is enforced by a synthesized **`orderWithState*(s*, o)`** invariant (e.g. "an insert is
  in-order when the set of removed elements is empty").
- **Bounded operation logs** σ prune candidates quickly (lio(σ) and lc(s*,σ) side conditions). The
  unbounded conditions are verified after that.

### 6.3 Search space: lattice compositions only (§5.1)

- Grammar: `latticeList ::= latticeType | FreeTuple(latticeType, latticeList)`;
  `latticeType ::= OrBool | NegBool | MaxInt | Set(type) | Map(type, latticeType) |
  LexicalProduct(latticeType, latticeType)`; `type ::= Bool | Int`.
- Specialized integer types prune the grammar: `OpaqueInt` (no arithmetic), `ClockInt` (only
  comparison), `EnumInt` (only equality), `NodeIDInt`.
- Code ⊥ values [code `katara/lattices.py`]: MaxInt ⊥ = 0 (and validity `v ≥ 0`), OrBool ⊥ = false,
  Set ⊥ = empty, Map ⊥ = empty, LexicalProduct ⊥ = (⊥, ⊥).
- **Key design: `st*(s*, o) = merge*(s*, f*(o))`.** Only f* is synthesized. "This choice grants us
  monotonicity, commutativity, associativity, and idempotence entirely for free." Queries are
  unconstrained.
- **Non-idempotent operations** (counters): with a flag, f* may read the current state and node id,
  but may only write the portion owned by `currentNodeID` (a map keyed by node id). Queries may reduce
  over node-keyed maps with `+`, `∨`, `∧`, or lattice join. "The merge function remains correct since
  a node can never receive new information about the portions of state it owns through gossip."
- The initial state is synthesized and may differ from ⊥ (e.g. `(0, true)` for an enabled-by-default
  flag over `LexicalPair<ClockInt, OrBool>`).

### 6.4 Implications for us

1. Adopt the **"update = merge with an operation-derived lattice value"** discipline for any stateful
   update in the language. Arbitrary assignment is non-monotone and flagged, as in HydroLogic's `:=`.
2. Offer a **restricted lattice-definition DSL** built from verified constructors (products, maps,
   sets, chains, lexicographic with chain key, antichains, causal dot stores). Its ACI properties
   follow by construction; composition rules are checked by induction, as Anna argues.
3. **Single-writer sub-state** (node-owned map entries) is the standard way to get non-idempotent
   updates (counters) inside a lattice. We should provide it as a library pattern: `Owned<NodeId, L>`.
4. Katara-style synthesis (sequential spec → CRDT) is a stretch goal. It needs an SMT backend.

---

## 7. Hydro `lattices` crate and DFIR (Rust reference implementation)

Commit `9e2a120` of https://github.com/hydro-project/hydro. Crate docs: https://hydro.run/rustdoc/lattices/.

### 7.1 Traits [code `lattices/src/lib.rs`]

```rust
// signatures abbreviated from lattices/src/lib.rs
pub trait Merge<Other> {
    /// Must be associative, commutative, idempotent. Returns true iff self changed.
    fn merge(&mut self, other: Other) -> bool;
    fn merge_owned(mut this: Self, delta: Other) -> Self where Self: Sized { Self::merge(&mut this, delta); this }
}
pub trait LatticeOrd<Rhs = Self>: PartialOrd<Rhs> {}          // marker: PartialOrd is the lattice order
pub trait NaiveLatticeOrd<Rhs = Self> { fn naive_cmp(&self, other: &Rhs) -> Option<Ordering>; } // sealed, derived from merge
pub trait LatticeFrom<Other> { fn lattice_from(other: Other) -> Self; } // between representations of the same lattice, recursive
pub trait IsBot { fn is_bot(&self) -> bool; }
pub trait IsTop { fn is_top(&self) -> bool; }
pub trait Atomize: Merge<Self::Atom> { type Atom: 'static + IsBot; type AtomIter: Iterator<Item = Self::Atom>; fn atomize(self) -> Self::AtomIter; }
pub trait DeepReveal { type Revealed; fn deep_reveal(self) -> Self::Revealed; }
pub trait LatticeMorphism<LatIn> { type Output; fn call(&mut self, lat_in: LatIn) -> Self::Output; }
pub trait LatticeBimorphism<LatA, LatB> { type Output; fn call(&mut self, lat_a: LatA, lat_b: LatB) -> Self::Output; }
pub fn closure_to_morphism(...)   // "Does not check for correctness."
pub fn closure_to_bimorphism(...)
pub trait Lattice: Sized + Merge<Self> + LatticeOrd + NaiveLatticeOrd + IsBot + IsTop {} // sealed alias
// Also: Semiring<T>: Addition + Multiplication + Zero + One (semiring_application.rs: BinaryTrust ({0,1},∨,∧), Multiplicity (N,+,*))
```

- `Merge<Other>` is **heterogeneous**. A `SetUnionSingletonSet<T>` delta merges directly into a
  `SetUnionHashSet<T>` accumulator, so deltas need no allocation. This matters for performance.
- `Default::default()` must be ⊥ (README).
- `algebra.rs` provides property checkers for monoid, semigroup, semiring, ring, group, distributivity,
  absorbing element, identity, associativity, and so on (used for semiring work).

### 7.2 Lattice types [code `lattices/src/*.rs`, README]

| Type | Domain / merge | ⊥ / ⊤ | Notes |
|---|---|---|---|
| `Max<T>` | max, requires `T: Ord` | ⊥ = `Default` for bool (false) and char ('\0'). `ord.rs` doc: "the Default::default() value for numeric type is MIN, not zero" | `Max<bool>` = lbool. `Max<()>` is both ⊥ and ⊤. `T: Ord` excludes raw `f64` (NaN) |
| `Min<T>` | min | ⊥ = MAX. `Min<bool>` ⊥ = true | order reversed: "0 is greater than 1" |
| `SetUnion<Set>` | union | ⊥ = empty, no ⊤ | representations: HashSet, BTreeSet, Vec, `ArraySet<N>`, `SingletonSet`, `OptionSet`. `CartesianProductBimorphism`. Atomize = singletons |
| `MapUnion<Map>` | key union; per-key merge; **incoming ⊥ values are filtered out** | `is_bot` = all values ⊥ | representations: HashMap, BTreeMap, VecMap, ArrayMap, SingletonMap, OptionMap. `KeyedBimorphism<MapOut, B>` lifts a bimorphism per key ("KeyedBimorphism<…, CartesianProduct<…>> is a join") |
| `UnionFind<Map>` | partitions: merge unions sets that share elements | ⊥ = all singletons | `union(a,b)` = merging an atom (a,b) |
| `VecUnion<Lat>` | index-wise merge, longer length wins | ⊥ = empty | like `MapUnion<usize, Lat>` without gaps |
| `WithBot<L>` | `Option<L>`, None = new ⊥ | | gives a ⊥ to lattices lacking one (e.g. `Conflict`, `Point`) |
| `WithTop<L>` | `Option<L>`, None = new ⊤ | | adds a ⊤. (The doc comment line "compares as less than" is a copy-paste slip; the code makes None greater) |
| `Pair<A,B>` | component-wise | (⊥,⊥) | `PairBimorphism` builds pairs. Called "coordinatewise order" in the thesis |
| `DomPair<K,V>` | dominating pair (see §4.5) | | **not a lattice**; documented and tested |
| `Conflict<T>` | equal stays, unequal → None (⊤ "conflict") | no ⊥ (`is_bot` always false) | "wrap non-lattice (scalar) data into a lattice" |
| `Point<T, Provenance>` | panics on unequal merge | | a provenance type parameter prevents mixing |
| `()` | unit lattice | both ⊥ and ⊤ | |
| `SetUnionWithTombstones<Set, TS>` | set ∪ set, tombstones ∪ tombstones, then set = set − tombstones | | invariant: an item is never in both. Tombstone sets: `RoaringTombstoneSet` (u64 bitmap), `FstTombstoneSet<String>`, HashSet |
| `MapUnionWithTombstones` | analogous for maps | | "if the user knows that keys will be created and deleted strictly sequentially … a single integer" |
| `ght::*` (Generalized Hash Trie) | trie lattice for relations (from the Wang/Willsey/Suciu Free Join work) | | `GhtCartesianProductBimorphism`, `GhtKeyedBimorphism` (join). A factorized representation. Relevant to fast joins |

### 7.3 Test utilities [code `lattices/src/test.rs`]

`check_all(items)` runs: `check_lattice_ord` (PartialOrd agrees with naive_cmp),
`check_partial_ord_properties`, `check_lattice_properties` (ACI over all triples),
`check_lattice_is_bot`, `check_lattice_is_top`, `check_lattice_default_is_bot`, `check_atomize_each`,
`check_lattice_morphism(f, items)` (f(a⊔b) = f(a)⊔f(b)), and `check_lattice_bimorphism(f, items_a,
items_b)`. `cartesian_power` enumerates tuples. **We need the same harness plus randomized
(property-based) generation.**

### 7.4 DFIR lattice operators [code `dfir_lang/src/graph/ops`]

- `lattice_fold::<'tick|'static, Lat>(init)` ≡ `fold(init, Merge::merge)`. It may accumulate into a
  different type than its input.
- `lattice_reduce::<'tick|'static>()` ≡ `reduce(Merge::merge)` (same type, no ⊥ needed). The default
  persistence is `'tick`.
- `state::<Lat>()`: **two output ports**. `[items]` emits "the input items that actually changed the
  lattice state (deltas)". `[state]` emits the accumulated value. This is the StateMergeOp of the
  thesis.
- `lattice_bimorphism(func, #lhs_state, #rhs_state)`: "The function must be a lattice bimorphism for
  both (LhsState, RhsItem) and (RhsState, LhsItem)". This is the semi-naive symmetric join over
  lattice state.
- `_lattice_fold_batch` (fold, released on a signal), `_lattice_join_fused_join`, and `join_fused`
  (per-key fold/reduce fused into a join).
- `FloType` operator categories (`Source`, `Windowing`, `WindowingLazy`, `WindowingEager`,
  `Unwindowing`) come from the Flo nesting semantics.

### 7.5 The Hydroflow model (2021 thesis): delta vs cumulative edges

- **Delta edges** carry any lattice elements whose join is the logical value; they are ideally small and
  non-redundant. **Cumulative edges** carry a sequence in which each element dominates the previous
  ("a path through the lattice partial ordering"). A cumulative edge is a subtype of a delta edge
  (correct but inefficient), not the reverse.
- **StateMergeOp:** delta in → cumulative out, plus minimized delta out. For sets the minimized delta
  is δx′ = δx \ x.
- **Morphisms** may run on either kind of edge. **MTT functions** need cumulative input. **Split
  binary morphisms** (bimorphisms) need cumulative + delta for both inputs. Their dataflow "is
  identical to symmetric hash join".
- **"Cumulative edges are not allowed to cross node boundaries"**, because the network reorders.
  Convert to deltas, send, and re-merge on the other side.
- **⊤-stoning.** **Taint:** operators downstream of non-monotone ones are tainted (unresolved points
  of order). Options: coordinate, a single node, or "memories, guesses and apologies".
- **Bundles:** MapUnion partitions any lattice by key. DomPair is a "forgetful" map "partitioning
  through time".
- **Representation independence:** a lattice type is a label paired with a physical representation
  (HashSet, BTreeSet, Vec, array, Single, Option). Deltas therefore cost no more than ordinary
  dataflow elements.
- Garbage collection "is non-monotonic and will require coordination" but can happen in the
  background.

### 7.6 Lattice-flow stream types (design docs 2023-07/08)

Flow kinds: `SeqFlow<*, T>` (ordered sequence), `LatticeFlow<Lat>` (= delta),
`CumuLatticeFlow<Lat>`, and an early `DiffLatticeFlow`. Operator rules from the 2023-08 table:
- `map`: `SeqFlow` any fn; `CumuLatticeFlow` needs `MonotonicFn`; `LatticeFlow` needs `Morphism`.
- `filter`: on `CumuLatticeFlow` needs `MonotonicFn(&Lat) -> Max<bool>`. On `LatticeFlow`: "Nope —
  no meaningful filter morphisms exist. Use map (convert atoms to bot) instead."
- `lattice_fold::<Lat2>()`: `LatticeFlow<Lat1>` → `CumuLatticeFlow<Lat2>`.
- Binary: `lattice_binary_map(f)` on two cumulative flows needs a binary monotone fn.
  `lattice_cross_join(f)` on two delta flows needs a **BinaryMorphism**. A keyed join on
  `MapUnion`s lifts a bimorphism per key.
- `union` of cumulative flows yields a non-cumulative flow.
- Casts: Seq→Lattice is not allowed. Lattice→Cumu is not allowed without merge. Cumu→Lattice is allowed.
- "Sending bottom ⊥ through a [lattice flow] stream should have the exact same behavior as sending
  nothing through."

### 7.7 Current Hydro (`hydro_lang`) stream properties [code]

- `Stream<T, Loc, Bound, Order, Retries>`. Bound ∈ {`Bounded`, `Unbounded`}; Order ∈ {`TotalOrder`,
  `NoOrder`}; Retries ∈ {`ExactlyOnce`, `AtLeastOnce`}. Other live collections: `Singleton`,
  `Optional`, `KeyedStream`, `KeyedSingleton`.
- `fold`/`reduce`/`for_each` on a `NoOrder` stream require a **commutativity proof**. On
  `AtLeastOnce` they require an **idempotence proof**: `q!(|acc, x| …, commutative =
  manual_proof!(/** … */), idempotent = manual_proof!(/** … */))`. Proofs are either `ManualProof` or
  machine-checked **Verus** proofs (`VerusCommutativeProof`, built by macros that symbolically run the
  closure body in both orders).
- `monotone = proof` on fold produces a **monotone Singleton** (e.g. `count()` on an unbounded stream
  returns `Singleton<usize, L, B::StreamToMonotone>`). `threshold_greater_or_equal(threshold)`
  requires `B: IsMonotonic` and emits the threshold once it is crossed.
  `map(…, order_preserving = proof)` preserves monotonicity.
- **Lesson:** ACI splits into separately-checkable properties. Commutative means order-insensitive,
  idempotent means duplicate-insensitive, associative means batchable, monotone means early outputs
  and thresholds are allowed. A lattice has all four. **Our type system should track these per stream
  or relation and per function.**

---

## 8. Flo (POPL 2025): semantic foundation for progressive streams

Read from arXiv v1 (https://arxiv.org/abs/2411.08274).

- **Collection language** L_C = (C, ++, E_C, T_C, ⟦⟧, ⌊⌋, type, fix). `++` is concatenation (it
  need not be monotone, commutative or associative). fixed(c) ≜ ∀c′. c ++ c′ = c. ∅ is a right
  identity. `fix` maps a value to an equivalent fixed one.
- **Stream types** (τ, B|U) with subtyping (C,B) ≤ (C,U). "Operators can only block on bounded
  streams, and must always make progress with respect to unbounded streams."
- **Operators** have small-step semantics (I, e) →δ (I′, e′, O′). →O must be **confluent**,
  type-preserving, and decreasing in a finite downward-closed partial order ≺, which gives a unique
  stuck state (Lemma 3.1).
- **Eager execution (Def. 3.1):** processing part of the input and then receiving Δ reaches the same
  stuck state as having Δ from the start.
- **Streaming progress (Def. 3.3, via Output Maximality 3.2):** with the bounded inputs fixed, the stuck
  outputs are maximal (fixing the unbounded inputs would only *fix* the outputs, not change them), and
  the bounded outputs are fixed.
- Graph composition (sequence `;`, parallel `|`) preserves determinism, eager execution and streaming
  progress (Lemmas 4.2–4.4).
- **Nested streams:** `nest(g)` with `read_defer(k, v)`/`write_defer(k)` for loop-carried state. The
  inner outputs must be bounded. Write keys are used linearly (substructural context W).
- **LVars in Flo (§6.2).** LVar<L> = (value, fixed-flag). (v, false) ++ v₂ = (v ⊔ v₂, false);
  (v, true) ++ v₂ = (v, true); ⊗ fixes the value. `fold_lattice(f)` maps S<T> → LVar<U> and preserves
  boundedness.
  - A naive `to_sequence` on an LVar **violates eager execution**, because the output depends on
    scheduling.
  - Waiting for `fixed` satisfies eager execution but **violates streaming progress** on unbounded
    input, so it must be typed bounded-only.
  - The safe operator is `thresh(t₁, …)`: it emits tᵢ when v ⊔ tᵢ = v. The side condition requires
    distinct thresholds to be pairwise incompatible (the typing rule's text was garbled in
    extraction; this matches the LVars definition).
- **DBSP in Flo (§6.3).** Z-sets with `+` as ++. Join is incremental because it is **bilinear**:
  (a+a′)⋈(b+b′) = a⋈b + a′⋈b + a⋈b′ + a′⋈b′. Flo notes this is "exactly the property we need to prove
  eager execution". This is the group-valued analogue of the bimorphism rule, and it supports
  retractions.
- **Why it matters for us:** (1) a clean criterion for which operators the compiler may run eagerly
  and incrementally (eager execution) versus which must wait for sealed input (bounded); (2) lattices
  (monotone) and Z-sets (retractions) coexist in one model; (3) "threshold on an unbounded lattice" is
  *the* safe way to leave the lattice world.

---

## 9. Related Datalog-with-lattices engines (for the "fastest engine" goal)

- **Flix (PLDI 2016).** Relations are declared `lat Name(key…, LatticeType)`. The lattice is the last
  attribute, and each key tuple ("cell") holds one lattice value ("compact"). Monotone filter
  functions (→ bool, false ⊑ true) and **strict and monotone transfer functions** ("strictness ensures
  that when a function is applied to ⊥ it returns ⊥"). Lattices must have **finite height**. No
  negation (in 2016).
  **Semi-naive for lattices:** "the incremental relation ΔPᵢ contains every ground atom from Pᵢ′
  which is strictly greater than the ground atom for the same cell in Pᵢ", so Pᵢ′ = ΔPᵢ ⊔ Pᵢ. Also:
  "we must compute the least upper bound … A(Odd) ⊔ A(Even) = A(⊤), and re-evaluate the third rule
  under x ↦ ⊤", i.e. rules see the joined cell value, not individual facts.
- **Ascent (Rust macro Datalog, CC 2022).** `lattice name(K1, …, L)`: "when a new lattice fact
  (v₁…vₙ₋₁, vₙ) is discovered, and a fact (v₁…vₙ₋₁, v′ₙ) is already present … vₙ and v′ₙ are joined".
  Example (verbatim):
  ```rust
  ascent! {
     lattice shortest_path(i32, i32, Dual<u32>);
     relation edge(i32, i32, u32);
     shortest_path(x, y, Dual(*w)) <-- edge(x, y, w);
     shortest_path(x, z, Dual(w + l)) <--
        edge(x, y, w),
        shortest_path(y, z, ?Dual(l));
  }
  ```
  According to a search-result summary of the CC'22 paper (I did not read the paper itself), it reports
  performance comparable to Datafrog and Soufflé and about two orders of magnitude faster than Flix on
  a re-implementation of the Rust borrow checker.
- **Soufflé subsumption:** `A(x) <= A(y) :- body.` deletes dominated tuples (the dominated head is on
  the left). This is lattice-like compaction by deletion. A lattice extension to Soufflé exists as a
  thesis (not verified as merged).
- **Takeaway for our engine (analysis):** the "compact cell" model (Flix, Ascent, Bloom^L embedded
  columns) plus a delta of "cells that strictly increased, carrying their new value" is the standard
  and proven design. Two refinements: (a) Hydro-style in-place `merge -> bool` to detect strict
  increase cheaply; (b) optionally ship Enes-style minimal deltas for set-like cell values (e.g. a
  per-key lset) so rules that are morphisms in that column process only new atoms.

---

## 10. Design synthesis for bloom-remake (normative; derived from the sources above)

### 10.1 Data model

1. **Lattice types** are first-class: built-ins (§11), constructors, and user-defined types written in
   a restricted DSL or as Rust impls of a `Merge`-like trait with declared ⊥ and optional ⊤, order,
   and atomize/delta functions.
2. **Lattice-valued relations:** `rel path(from: Node, to: Node, hop: Node) -> cost: Min<u64>`. The key
   is all non-lattice columns (a functional dependency) and there is one lattice value per key.
   Allowing several lattice columns is equivalent to one `Pair`. Deriving a fact with an existing key
   merges into it. Two differing *non-lattice* non-key values under the same key is a **hard error**
   (Bud raises `KeyConstraintError`). Lattice-typed columns may not be keys, join keys or group-by
   keys, and may not be compared with `==` except through declared monotone predicates. Enforce this
   in the type checker.
3. **0-ary lattice relations** (`lmax cnt;`) are Bloom^L lattice identifiers.
4. **Persistence:** the default for lattices is persistent, matching Bloom^L/Dedalus: implicit
   `l@t+1 ⊒ l@t`. Also support tick-scoped (`scratch`) lattices, which reset to ⊥ each tick. They are
   not monotone across time, so the CALM analysis treats them like scratches.
5. **Temporal ops:** `<=` merge in the same tick; `<+` merge next tick; no `<-` for lattices (removal
   is expressed with tombstone/causal lattices); `<~` only through channels whose tuples embed lattice
   values. The receiver merges on key collision.
6. **Normalization:** a map entry whose value is ⊥ is the same as an absent entry (Hydro filters ⊥ on
   merge; δ-CRDT DotMap drops ⊥).

### 10.2 Function/operator typing

- Each built-in operation declares, per argument, **M / BM-component / Mon / Anti / NM**.
- User functions over lattices declare their class. Morphism and monotone claims are checked by
  property tests (Hydro `check_lattice_morphism`) and optionally proven (Verus/SMT, Katara-style).
- Element-wise functions over set-like lattices (`project`, `filter`, `flat_map`) are **automatically
  morphisms**, provided the closure is deterministic and does not `reveal` lattice state (Bud
  `UnsafeFuncRewriter` rejects "unsafe" functions).
- Threshold operators (`gt_eq`, `contains`, `key?`, `is_top`, `when_true`, a generic
  `threshold(t₁..tₙ)`) are the bridge from lattices to discrete facts.
- `reveal` and any NM operation make a rule non-monotone: stratify it, and flag a point of order if
  its input depends on a channel.

### 10.3 Stratification with lattices

- Edges through M, BM, Mon and threshold operations are **positive** and may be recursive, as the
  Bud `ShortestPathsL` test asserts (everything in stratum 0).
- Edges through Anti/NM operations (including `reveal`, `lt_eq` on the receiver, `not`, set
  difference, ldom `value`) are **negative**. Cycles through them are rejected, exactly like negation
  and aggregation in Bloom [SoCC §3.2.2].
- Termination: warn (or require an explicit opt-in) for recursion through lattices without ACC (e.g.
  `Min<i64>` with subtraction). Provide a configurable fixpoint-iteration bound that raises a **hard
  error** when exceeded; it must never silently truncate.

### 10.4 Evaluation algorithm (semi-naive over lattices)

State per lattice relation R: `total[R]` (key → value), `delta_prev[R]` (key → value that strictly
increased in the previous round, carrying the new or minimized value), `delta_next[R]`.

```
for each stratum S (in order):
  round 0: evaluate every rule in S on totals (first_iter semantics: morphisms see full values)
           merge results into delta_next via merge_into(total, key, val):
               changed = total[key].merge(val)            // Hydro-style in-place merge -> bool
               if changed: delta_next[key].merge(minimize(val, old_total[key]))   // minimize optional
  loop:
    swap(delta_prev, delta_next); clear(delta_next)
    if all delta_prev empty: break
    for each rule r in S:
       for each body atom Aᵢ over a relation/lattice with nonempty delta, where r is
           (M or BM) in Aᵢ's lattice column / set membership:
             evaluate r with Aᵢ := delta_prev, Aⱼ (j<i) := total_new, Aⱼ (j>i) := total_old  // standard SN term split
       for rules that are Mon (not M) in some lattice argument whose input changed:
             re-evaluate r on totals of those arguments (reactive / cumulative); merge the output
    (morphism outputs merge into delta_next through merge_into as above)
```

Notes:
- The correctness argument for a morphism f: f(total_old ⊔ Δ) = f(total_old) ⊔ f(Δ), and
  f(total_old) is already merged [SoCC §4.1]. For a bimorphism the standard SN term expansion holds in
  each argument [Hydro docs; Hydroflow thesis §3.2]. For Mon functions, recomputing on the total is
  required [SoCC §4.1: the size example].
- Duplicated or overlapping deltas only waste work. Correctness needs merge idempotence
  [SoCC §4.1; Hydroflow §3.1].
- Fixpoint detection uses the merge's `changed` bit (L6) [Hydro; Bud `tick_deltas`].
- **Across ticks:** Bloom/Dedalus semantics re-derive facts from persistent lattices every tick. For
  sinks that are idempotent (lattice merges), the engine may evaluate only new deltas from tick to
  tick. For **async sends from persistent state**, semantically the message is "sent each tick".
  Recommended: dedup at the sender by keeping a per-(channel, dest) cumulative "already-sent" lattice
  and sending only Δ(new, sent). This is sound for lattice-valued payloads by idempotence (analysis;
  motivated by the `monotone_cart.rb` "unbounded responses" comment and Hydroflow's delta edges).
- **Physical representation** is separate from lattice type: singleton, option and array deltas;
  hash/btree/trie totals. Heterogeneous `Merge<Other>` avoids conversion [Hydro README;
  Hydroflow §4.1].
- **Joins on lattice-embedded relations** use indexes on key columns only. Lattice columns are
  payload.

### 10.5 Distribution

- Channels carry deltas, never "cumulative" sequences that rely on order [Hydroflow §3.1].
- Merge-at-sender batching per destination and key [Anna §V-A].
- Anti-entropy/gossip for replicated lattice state is `kv_store <= repl_propagate {|r| r.kv_store}`
  (whole state) or delta-interval based [δ-CRDT §5].
- Timestamps: always totally ordered and tie-free ((counter, node_id)) [Cloudburst §5.2].

### 10.6 Correctness tooling

- A property-test harness equivalent to Hydro `test::check_all` for every lattice (built-in and
  user-defined), morphism and bimorphism.
- A known-bad test: DomPair with a set key must fail associativity.
- Optional SMT/Verus proof obligations for user lattices, in the spirit of Katara and Hydro Verus
  proofs.

---

## 11. MUST-IMPLEMENT CHECKLIST

### A. Lattice algebra core
1. **Lattice trait**: in-place `merge(&mut self, other) -> changed: bool` (ACI), `bot()` (= default), `is_bot`, optional `top()/is_top`, `partial_cmp` agreeing with merge — [Hydro lattices `lib.rs`; SoCC §3.1].
2. **Heterogeneous merge** between representations of the same lattice (singleton/option/array delta into hash/btree total) and `lattice_from` conversions — [Hydro README "LatticeFrom", thesis Table 2].
3. **Merge-derived comparison fallback** (`naive_cmp`) for user lattices without an explicit order — [Hydro `NaiveLatticeOrd`].
4. **Atomize / join decomposition** for set-like lattices (non-⊥ strong-antichain atoms that join back to the value) — [Hydro `Atomize`; Enes et al. §III-A].
5. **Optimal delta** Δ(a,b) = ⊔{y∈⇓a | y⋢b}, at least for SetUnion, MapUnion, Pair, chains and chain-keyed Lex — [Enes et al. §III-B, App. C].
6. **Morphism and bimorphism function kinds** with the law f(a⊔b)=f(a)⊔f(b), per-argument for bimorphisms — [SoCC §3.1; Hydro lattice-math doc].
7. **Per-argument monotonicity classes** (M, BM, Mon, Anti, NM) on every lattice operation, used by the analysis and evaluator — [SoCC §3.2.2, Table 3; Hydroflow MTT §2.2; polarity is analysis].
8. **Property-check harness** (ACI, ⊥ identity, order agreement, partial-order laws, is_bot/is_top, atomize, morphism, bimorphism) with a DomPair negative test — [Hydro `test.rs`, `dom_pair.rs` test].

### B. Built-in lattices and their operations
9. **Max<T> / lmax** (T: Ord; ⊥ = MIN/−∞): `gt`, `gt_eq`, `+n`, `−n`, `min_of` (M); `lt`, `lt_eq` (Anti) — [SoCC Table 3; Bud `MaxLattice`].
10. **Min<T> / lmin** (⊥ = MAX/+∞): `lt`, `lt_eq`, `+n`, `−n` (M); `gt`, `gt_eq` (Anti) — [SoCC Table 3; Bud `MinLattice`; Hydro `Min`].
11. **Bool / lbool** = Max<bool> (⊥ false, ⊤ true): `when_true` (M), `and` (BM), `not` (Anti) — [SoCC Table 3; Hydro `Max<bool>`].
12. **SetUnion / lset**: `contains` (M), `intersect` (BM), `project/map/filter/flat_map` (M), `product/eqjoin(preds)` (BM), `size` (Mon), `group_count` (Mon) — [SoCC Table 3; Bud `SetLattice`; Hydro `SetUnion`].
13. **lpset** (non-negative numbers) with `sum` = SUM(DISTINCT) (Mon), closed under `project` — [SoCC §3.3; Bud `PositiveSetLattice`].
14. **lbag** (merge = per-element max multiplicity): `multiplicity` (M), `intersect` (M), `contains` (M), `+` (BM), `size` (Mon) — [SoCC §3.3; Bud `BagLattice`].
15. **MapUnion / lmap** (values are lattices, ⊥ entries = absent): `at(k)` returning ⊥_V (M), `key?` (M), `key_set` (M), `map_values/apply_morph` (M), `apply(monotone)` (Mon), `filter` on bool values (M), `intersect` = keys∩ with values joined (BM), `to_collection` (M), `size` (Mon), `lt_eq` (Anti/+) — [SoCC Table 3; Bud `MapLattice`; Hydro `MapUnion`].
16. **Pair / product** (component-wise) with projections (M) and pairing (BM) — [Hydro `Pair`, `PairBimorphism`; Katara FreeTuple; Enes A×B].
17. **Lexicographic pair with chain-typed key** (proper lattice; incomparable keys → (k⊔k′, ⊥)); LWW register on it with (ts, node_id) keys — [Enes App. B; CRDT TR Spec 8; Cloudburst §5.2].
18. **DomPair** kept only as an explicitly unsafe type, or rejected, with documentation of its non-associativity — [Hydro `dom_pair.rs`; §4.5 analysis].
19. **Antichain / dominating set (ldom, MV-register)**: merge keeps the non-dominated ⟨version,value⟩ pairs; `version` (M), `value` (NM) — [SoCC §5.2.2; thesis A.1; CRDT TR Spec 10].
20. **WithBot, WithTop, Conflict, Point, Unit, VecUnion, UnionFind** — [Hydro `lattices` crate].
21. **Tombstone lattices** (SetUnionWithTombstones, MapUnionWithTombstones with pluggable tombstone sets, e.g. roaring bitmaps) — [Hydro `set_union_with_tombstones.rs`, `tombstone.rs`].
22. **Causal (dot-store) lattices**: `Causal<DotSet|DotFun<V>|DotMap<K,·>>` with a causal context compressible to a version vector, yielding EW-flag, MV-register and AW-set — [δ-CRDT paper Figs. 10–15].
23. **Vector clock** = MapUnion<NodeId, Max<u64>> with happens-before/concurrent tests (the tests are Anti/+) — [SoCC §5.2.1; Anna ICDE §VI-C].
24. **CRDT library**: G-Counter (MapUnion<Node,Max>, `value` Mon), PN-Counter, G-Set, 2P-Set, OR/AW-Set, LWW-Register, MV-Register, EW/DW flags — [CRDT TR §3; δ-CRDT §6].

### C. Language integration
25. **Lattice declarations** (0-ary identifiers and lattice-valued relations), initialized to ⊥ — [SoCC §3.2].
26. **Merge statements** `<=` and `<+` into lattices; forbid `<-` on lattices; require the same lattice type on both sides — [SoCC §3.2.1].
27. **Embedded lattice columns** under an FD: merge on key collision, hard error on a non-lattice conflict, lattices never keys — [SoCC §3.5.2; Bud `merge_to_buf`].
28. **Collection → lattice implicit fold** (construct singleton, merge all) — [SoCC §3.5.1].
29. **Lattice → collection** via `when_true`, `to_collection`, and threshold operators producing tuples — [SoCC Fig. 3/10; Bud `to_collection`].
30. **Channels carrying lattice values** (merge on receive; re-send is idempotent) — [SoCC §3.2.1, §5.1; Bud `MaxOverChannel` test].
31. **User-defined lattices** with a declared merge, ⊥, and per-method classes (morph/monotone/plain); immutable semantics — [SoCC §3.4].
32. **`reveal`** as the explicit non-monotone escape hatch — [SoCC §3.2.2].
33. **Persistent (default) and tick-scoped lattices** — [Bud `LatticeWrapper#tick` (persistent); DFIR `'tick`/`'static`].
34. **Typed ⊥** so `at(k)` on a missing key returns ⊥_V without runtime class arguments — [Bud `lmap#at` comment].

### D. Evaluation
35. **Semi-naive over lattices:** morphism rules on deltas, first round on full values, fixpoint via the merge changed-bit — [SoCC §4.1; Bud `tick_internal`, `LatticeScanner`].
36. **Bimorphism delta rule** Δ = f(ΔA, B) ⊔ f(A, ΔB) (symmetric join over lattice state) — [Bud `PushApplyMethod`; Hydroflow §3.2; DFIR `lattice_bimorphism`].
37. **Monotone-tricky functions** re-evaluated on cumulative values whenever an input changes — [SoCC §4.1; Hydroflow §2.2].
38. **Flix-style compact cells:** the delta is the set of cells whose value strictly increased — [Flix §3 semi-naive].
39. **In-place merges** (no copying on merge) — [SoCC §4.2 performance note; Hydro `Merge`].
40. **Delta minimization** (StateMergeOp minimized-delta output; the `state[items]` port) — [Hydroflow Fig. 3; DFIR `state`].
41. **Stratification**: Anti/NM lattice operations are negative edges; recursion through M/BM/Mon is allowed — [SoCC §3.2.2; Bud `tc_lattice.rb` stratum-0 assertions].
42. **Termination guard**: detect non-ACC recursion (warn) and a hard error on exceeding an iteration bound — [Flix finite-height requirement; Hydroflow §3.4.2].
43. **⊤ short-circuit (⊤-stoning)**: once an input is ⊤, stop maintaining state for it — [Hydroflow §3.3].

### E. Analysis
44. **Lattice-aware CALM analysis**: points of order are async inputs reaching NM/Anti operations; the monotone operator list is extended by lattice method classes — [SoCC §3.6].
45. **Taint propagation** of non-monotonicity through the dataflow graph for diagnostics — [Hydroflow §3.4.1].
46. **Query classification for CRDT reads** (local-safe monotone/threshold vs needs quorum) — ["Keep CALM and CRDT On" §3].
47. **Stream properties** Bounded/Unbounded, ordered/unordered, exactly-once/at-least-once, with fold legality requiring commutativity/idempotence/monotonicity claims — [hydro_lang `boundedness.rs`, `stream/mod.rs`, `properties`; Flo §3.3].
48. **Eager-execution/streaming-progress checks**: blocking operators only on bounded inputs; LVar-style threshold for unbounded lattices — [Flo Defs. 3.1, 3.3; §6.2].

### F. Distribution and storage
49. **Delta shipping over channels**; cumulative values never cross nodes as ordered sequences — [Hydroflow §3.1].
50. **Merge-at-sender batching** per key and destination — [Anna ICDE §V-A].
51. **Anti-entropy** replication of lattice state (full state or delta intervals) — [thesis App. A.1 `ReplicatedKvsReplica`; δ-CRDT §5].
52. **Consistency-level library** via lattice composition + client protocol (causal, RC, RU, item-cut; LWW default) — [Anna ICDE §VI; VLDB 2019 §3].
53. **GC protocols** for lattice state (deletes via timestamps + min-of-heard-clocks; tombstones) as explicitly coordinated components — [Anna ICDE §VII; SoCC §8].

### G. Stretch
54. **Restricted lattice DSL + automatic ACI proofs** (SMT/Verus) — [SoCC §8; Katara §4–5; hydro_lang Verus proofs].
55. **CRDT synthesis from sequential specs + opOrder** (Katara) — [Katara].
56. **Z-set (group) collections** alongside lattices for retraction-capable incremental views (bilinear join) — [Flo §6.3].

---

## 12. TEST PROGRAMS

Each test lists its source and the expected behavior. "Fuzz" means: run under randomized message
delay, reordering and duplication, and require that every run converges to the same final state or
produces the same externally visible outputs.

1. **QuorumVoteL** (SoCC Fig. 3). Feed `vote_chn` with voter ids, including duplicates, in random
   order. Expect exactly one `result_chn` fact at `RESULT_ADDR`, emitted as soon as ≥5 *distinct*
   voters are seen and never before. CALM analysis reports **no points of order**. The Fig. 2 Bloom
   version must be flagged non-monotone at `group`/`count`.
2. **SimpleMax / MaxOfMax / EmbedMax / EmptyMaxMerge / MaxConstructorImplicit / MaxOverChannel**
   (bud `tc_lattice.rb`). E.g. `done <= m.gt(12)` becomes true once any input > 12 and stays true.
   EmptyMaxMerge: `m1 <= m2` with m2 = ⊥ leaves m1 = 5. Channel test: the received `lmax` in
   `chn_log` equals the sender's value at send time.
3. **ShortestPathsL** (bud tests). Input `link` = [a,b,11],[a,b,10],[a,c,15],[b,c,20],[b,c,21],[b,d,30],[c,d,5],[d,e,10].
   Expect `min_cost` = {(a,b,10),(a,c,15),(a,d,20),(a,e,30),(b,c,20),(b,d,25),(b,e,35),(c,d,5),(c,e,15),(d,e,10)}
   and all of `link`, `path`, `min_cost` in stratum 0.
   Cyclic input [a,b,20],[a,b,21],[b,a,5],[b,a,8],[b,c,10],[b,c,12],[a,c,35],[d,a,15],[d,b,5]:
   expect `min_cost` = {(a,a,25),(a,b,20),(a,c,30),(b,a,5),(b,b,25),(b,c,10),(d,a,10),(d,b,5),(d,c,15)}.
   Also run `ShortestPathsVariant` (path extended at the end).
4. **Negative-cycle lmin shortest path** (analysis). Must hit the iteration bound and raise a hard
   error, or be rejected statically as non-ACC recursion.
5. **MaxCapacityPaths** (bud tests; `lmax` with `min_of`): widest path per pair.
6. **AllPathsL / AllPathsImplicitProject / AllPathsEqJoin** (bud tests; pure lset transitive closure with
   `eqjoin`). Expected: the transitive closure with summed costs.
7. **Semi-naive equivalence** (SoCC §4.2, Fig. 5 workload): transitive closure over DAGs with n nodes
   and ~log₂n out-edges. The naive and semi-naive lattice versions must match the set-based Datalog
   version exactly. Derivation counts should be comparable to set semi-naive.
8. **size-in-recursion regression** (SoCC §4.1): a rule `cnt <= s.size` where s grows over several
   rounds. Semi-naive must give size(total), not max of delta sizes.
9. **Map tests** (bud: SimpleMap, MapIntersect, MapAt, MapApply, MapFromCollection, MapToCollection).
   E.g. `s1 <= m4.apply(:size).apply(:gt_eq, 2).filter.key_set` yields the keys whose sets have ≥2
   elements. `at` on a missing key yields ⊥.
10. **Set/bag/sum tests** (bud: SetProduct, SetEqjoin, SetSimpleGroupCnt, CollectionToSet,
    SetToCollection, SetToChannel, NotInToLattice, SimpleSum, SimpleBag). Bag merge is max of
    multiplicities; `+` sums them.
11. **Lattice-embedded key-constraint semantics**: inserting two tuples with the same key and
    different lattice values merges them. Different non-lattice values raise a key-constraint error.
    A lattice value in a key column is a type error. `MaxErrors`: merging a non-comparable value into
    lmax is a type error.
12. **KVS** (SoCC Figs. 6–7; thesis App. A.1). Puts of lattice values to one replica, then
    `cause_repl` to another: the replicas converge (equal `kv_store`). Gets return the merged value
    or ⊥. **QuorumKvsClient**: the write returns after W acks; the read returns the lub of R responses.
    Fuzz: any order of puts and replications converges.
13. **Versioned KVS with ldom + vector clocks** (SoCC §5.2). Two clients write concurrently
    (incomparable clocks): a read returns version = lub and value = user-merge of both. A write that
    has seen both dominates and replaces both. Stale (dominated) writes are ignored. Read repair
    stores the lub. Equal-version/different-value input must be rejected.
14. **Monotone shopping cart** (SoCC Fig. 10; bud-sandbox `cart/`). Actions and checkout are routed
    to random replicas in random order. Every replica that becomes complete emits the **identical**
    summary. Incomplete carts emit nothing. Two different checkout messages, or an op id outside
    [lbound, checkout], cause an error. With our sender-side dedup (§10.4), each replica sends at most
    one response per distinct summary.
15. **Causal delivery** (bud-sandbox `delivery/causal.rb`, `lattices/vc_scenario.rb`). Three nodes with
    injected delays. Messages are delivered in causal order, and the scenario that sends message 3
    before message 1 is delayed rather than delivered out of order. `vc_scenario` without the causal
    module must detect the violation (`lt_eq(my_vc).when_true`).
16. **Anna consistency lattices.** (a) LWW with (ts,node) keys converges under fuzz. (b) LWW with bare
    `>=` timestamps and equal ts must fail the commutativity property check. (c) Causal pair: a
    dominated write is discarded and a concurrent write is merged. (d) DomPair with a SetUnion key:
    `check_lattice_properties` must fail (x=({1},p), y=({2},q), z=({1,2},r)). The proper Lex with a
    chain key must pass. (e) RC via client-buffered transactions: no dirty reads (uncommitted writes
    never visible).
17. **CRDT catalog property tests.** Run `check_all` on samples of each lattice: G-Counter, PN-Counter
    (the value is not monotone), G-Set, 2P-Set, AW-Set via DotMap<E,DotSet>, EW-Flag, MV-Register via
    DotFun. Fuzz convergence with random delta-group delivery. The causal delta-merging condition
    must hold for the causal variants.
18. **Keep CALM queries.** On a 2P-Set, `checkout = A − R` is classified as needing coordination;
    `suspicious_activity` (count>50 → true/ABORT) and `|A|+|R| > 100` are classified local-safe.
    Potato/Ferrari: with a delayed remove, the local non-monotone checkout may be wrong; the monotone
    version never retracts.
19. **Katara 2P-Set** (Katara Fig. 2). Implement as `st*(s, (add,v)) = s ⊔ (add ? ({v},{}) : ({},{v}))`
    and `query = v ∈ s₁ \ s₂`. It must agree with the sequential set on all sequences where removes
    follow adds (random sequence tests).
20. **Hydro-derived tests.** `lattice_bimorphism` with `CartesianProductBimorphism` over states
    {0,1,2} and {3,4}: output = all 6 pairs regardless of arrival interleaving. `state[items]` emits
    only changed items. `lattice_reduce` of Max over [1..5] = Max(5).
21. **Flo LVar threshold.** `fold_lattice` sum over an unbounded input; `thresh(7)` emits 7 exactly once
    regardless of batch boundaries (Hydro `monotone_fold_threshold` test: fold over [1..6],
    `threshold_greater_or_equal(7)` → 7). A `to_sequence`/`reveal` on an unbounded lattice must be
    rejected by the type checker. Threshold sets that are not pairwise incompatible must be rejected.
22. **Stratification negative tests.** `reveal`, `lt_eq` on a growing receiver, or ldom `value` inside a
    recursive cycle must be rejected at compile time.
23. **Merge-at-sender equivalence** (Anna §V-A). Sending n updates individually vs pre-merged must give
    identical receiver state; batching reduces message count.
24. **Persistence semantics.** A persistent lattice keeps its value across ticks. A tick-scoped lattice
    resets to ⊥. `<+` applies at t+1 (Bud `PendingLatticeMerge` test). A lattice with only bootstrap
    facts and no rules keeps its value (Bud `BootstrapNoRules`).

---

## 13. Open questions and items not verified

- The Bloom^L model-theoretic semantics was never published, as far as the sources I read show. Our
  semantics will need to be defined, most naturally as Dedalus with lattice-valued relations plus Flo's
  eager-execution criterion. That is a design task, not a research finding.
- I did not read LVars, Datafun, Lasp, Ross & Sagiv, or Anna's TKDE extension in full. Claims about
  them above are limited to what was quoted by papers I did read (or a search abstract, which is
  noted).
- The Flo typing rule for `thresh` was partly garbled in PDF extraction. I restated it using the LVars
  pairwise-incompatibility definition, which the Flo text says it models.
- Hydro's `WithTop` doc comment says the new top "compares as less than", but the code implements
  greater. I reported what the code does.
