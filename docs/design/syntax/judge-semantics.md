# Syntax panel judgment: semantic fidelity and analyzability

Judge: semantics lens. Date: 2026-09-27. Inputs: `docs/DECISIONS.md`; `docs/research/FEATURES.md` §1 (CR-xx), §2
(LANG), §3 (SEM), §6 (ANA) and §13 (ODD); the four proposals in `docs/design/syntax/`, each read in full.

## 1. Lens and method

This judgment asks one question: which surface syntax is the safest front end for the Dedalus^L IR? Readability,
familiarity and parser ergonomics belong to the other judges. I scored them only where they change what a program
means or what an analysis can see.

I checked five things for each proposal.

1. **Lowering.** Does each surface form reach IR rules through a local rewrite with exactly one lowering? Or does
   the meaning of a construct depend on distant context such as enclosing blocks, clause order or liveness?
2. **CR honesty.** I compared each proposal's stated expansions with the CR-xx resolutions. I spot-checked the
   expansions for soft state, choice, `seq`, ordered folds, upsert, `resolve`, seals, deltas and bootstrap line by
   line.
3. **Visibility.** Are the negative edges of SEM-021 visible in the source text, and does the compiler enforce the
   spelling? Are temporal edges (same tick, t+1, async, deferred mutation) visible?
4. **Analysis hooks.** What does the syntax give CALM (ANA-020..029), Blazes (ANA-040..047), Edelweiss
   (ANA-060..066), finality (ANA-120..122) and LDFI provenance (TEST-020..029)?
5. **Coverage.** I grepped each proposal for all 149 P0/P1 LANG ids and then checked every missing or thin hit by
   hand. I also recomputed the expected outputs of the E7 graph examples in A and C. Both are correct.

## 2. Scores

| Proposal | Score | Verdict |
|---|---|---|
| **A**, Rust-flavored relational | **8.5** | The most complete and the most faithful. The compiler enforces a bang on every negative edge, including lattice methods and Z-set reads. Its defects are small and acknowledged. |
| **C**, modernized Datalog | **8.0** | The closest to the IR, with the best vocabulary for CALM assertions and verification. Lattice polarity is invisible in the text, and three lowerings bend the spec. |
| **D**, query / comprehension | **7.0** | The strongest visual marking: bangs everywhere, and `delete! next`. Choice columns inferred from liveness and a clause-order-dependent lowering weaken its fidelity. |
| **B**, reactive handlers | **6.5** | `on`/`while` is the best single idea on the panel. But the handler model hides rule bodies, makes labels unstable, and suggests a per-event semantics that the language does not have. |

**Winner under this lens: A.** C's analysis vocabulary and B's `on`/`while` distinction should be grafted onto it
(§9).

## 3. Criterion by criterion

| Criterion | A | B | C | D |
|---|---|---|---|---|
| Directness of lowering (one surface rule → IR rules, locally) | **strong**: `head <- body;` with a leading kind keyword; multi-head is one rule per head | **fair**: each statement's body is assembled from the handler header, enclosing `if`/`for` and in-scope `let`s; `outer`/`any` multiply rules under labels the user never wrote | **strong**: `head :- body.` with a kind keyword; positional atoms put the location first, as the IR does | **fair**: clauses are processed left to right, and every non-monotone clause cuts the body into an auxiliary relation; the head is matched by field name; the chosen columns come from liveness |
| CR-xx honesty (§4) | **strong**: no defects found beyond acknowledged gaps | **fair**: soft-state staleness, LANG-190 deviation, inconsistent lattice-equality rule | **good**: seal default is unsound, `resolve` scope is extended, LANG-190 deviation | **fair**: inferred choice FD, soft-state staleness, restart behaviour of bootstrap unspecified |
| Negative edges visible | **strong**: bang enforced both ways; `delete`/`upsert` marked by keyword; Z-set views banged | **good**: a closed keyword list plus `reveal`; but `a.le(b)` and lattice `==` are inconsistent (§6) | **fair**: a keyword list, but Anti/NM lattice comparisons and methods are type-directed and unmarked | **strong**: bang on every negative edge including `delete!`, `upsert!`, `left join!`, `all!`; enforced both ways |
| Temporal edges visible | **good**: `next`, `send`, `delete`, `upsert` lead the rule; mutations do not say t+1 | **strong**: verbs plus `on`/`while`, which shows ODD-05 re-derivation and resends | **good**: `next`, `async`, `delete`, `upsert`; mutations do not say t+1 | **strong**: `into`, `into next`, `send`, `delete! next`, `upsert! next` |
| CALM / Blazes / Edelweiss hooks | **good**: seals with a producer role and digest; `#[final]`; `#[trusted]`; no `monotone` assertion | **good**: `monotone` view/handler/module; per-producer seals; event/standing classes are ARM candidates | **strong**: `monotone` on rule/block/module/output; `max{}` vs `lmax{}`; `stable fn … after t`; `threshold(…)` | **good**: `group` vs `group!`; `all` over closed domains; seals on any relation; `expect out: confluent` |
| LDFI provenance and specs | **strong**: per-rule labels; `spec` with failures, bounds, `net`, `quorum v in r { }`, `#[inductive]` | **fair**: generated labels (`h.3a`) and statement ordinals; good `check … expect` gate | **strong**: per-rule labels; `once`, `sent`, `always forall … implies`, `prove … by induction using` | **good**: `trace(r)`, `$node`/`$tick`, `check inductive { strengthen }`; auxiliary relations lengthen lineage |
| LANG P0/P1 coverage (§8) | **complete** | one P0 gap (generic `threshold`) and minor P1 gaps | several minor P1 gaps | two P1 gaps (UDAs, combiners) and minor ones |

## 4. Where the CR-xx resolutions come out differently

Only the rows where the proposals differ are listed. On CR-01..06, CR-09..11, CR-13, CR-15, CR-18..25, CR-28, CR-30
and CR-36..54 all four lowerings agree with §1.

| Resolution | Finding |
|---|---|
| **CR-17 / SEM-060, soft state** | **A** (§3.2) and **C** (§3.1.8) filter visible tuples against the current tick's `now`, so a visible tuple is never older than its TTL. **B** (§3.3) and **D** (§3.2.1) check the TTL only in the rule that carries a tuple to t+1, against `now` at t. Ticks are event-driven (SEM-009), so after an idle stretch of any length the first tick still shows every tuple that was alive before it. The failure detector in E8 would then report a silent peer as alive at the first tick after the stretch. B's size cap also admits a whole tick of fresh arrivals uncapped, and D evicts one tick late. This is a real defect in B and D. |
| **CR-26 / CR-45 / SEM-084, choice and site ids** | A (`choose!(y per x)`), B (`choose (x̄) -> (ȳ)`, `choose(c0)`) and C (`choose X -> Y`) all spell Ȳ. **D** infers Ȳ from the variables live after the clause (§3.8, its own weakness 5). Adding a field to `select` then changes the FD and the seeded winner, and ANA-038's D1–D5 conditions have no declared FD to reason about. On site ids: A falls back to a positional `module::rN`; B uses statement ordinals even inside labelled handlers; C and D do not say. Only B lints, and only for unlabelled handlers. SEM-084 requires site ids that survive edits, and none of the four fully meets that. |
| **CR-27, seals** | **A** names the producer role (`#[seal(key, producers = R)]`) and seals a key only when every producer has sealed it. That is sound. **B** has per-producer `sealed occ{k: v} from m`, a producer-side log that re-sends the seal until it gets through, and a "send after seal" violation. It is the most robust under loss. **C** seals a key as soon as *any one* producer seals it, unless `per producer` is written (§3.8.4). That is unsound whenever a key has several producers, and nothing checks the one-producer assumption. **D** seals any relation with a counted digest and states unanimity explicitly with `all m in members(..)`. |
| **CR-35 / ODD-06 amendment, Z-set boundary** | An edge from a Z-set stratum back into a set or lattice stratum is negative. Only **A** marks it (`distinct!`, `clamped!`, `weights!`). In B the bare atom, in C `weight n`, and in D `.distinct()` are negative edges with no marker. |
| **CR-07 / LANG-117, `resolve`** | A, B and D resolve the candidates for t+1, as LANG-117 says. **C** (§3.4.9) also resolves same-tick deductive derivations, which turns a resolved relation into a same-tick resolved view. The extension is defensible but unspecified by LANG-117, and it changes when SEM-050 fires. |
| **CR-08 / LANG-106, aggregate defaults** | All four are correct. A's `per driver` and B's `default e over atom` name the driving relation. C synthesizes the driver from the rule's other positive literals. D's correlated subqueries supply the monoid identity implicitly for `count!`/`sum!`. The explicit driver is the most analyzable, because the driver is a visible positive atom. |
| **CR-12, default fault model** | B defaults to `fair`, which matches the reasoning semantics. A defaults to `lossy_delayed` and C to `lossy`, both as simulation fault models; C says outright that the reasoning semantics stays fair. D does not state a default. |
| **CR-14, location column** | A omits the `@` column in channel atoms, so atom arity differs from the declaration. B's arrow form declares no `@` column. C keeps the location in positional body atoms (noisy but uniform with the IR). D uses a record field. All four normalize to the first IR column. |
| **CR-16, facts** | All four are correct. C is the most careful: every unannotated fact holds at every tick in every relation, so initial mutable state must go in `bootstrap`, and a `delete` of a re-asserted fact gets a warning. |
| **LANG-190, bootstrap** | A and D keep `next` inside `bootstrap` at tick 0, as LANG-190 says. B and C let `next` mean t+1 everywhere and use deductive heads for tick 0. That deviates from the text, but it is the more uniform reading: a keyword whose lowering depends on the enclosing block is context-dependent lowering. |
| **SEM-071, durable state at restart** | **C** is best: `bootstrap fresh` plus a `recovered` input, and a compile error for durable writes in a plain `bootstrap`. B warns about non-idempotent bootstrap writes into durable state. A says nothing. D defines `bootstrap` as "true only at tick 0" but relies on it re-running after a restart (E3), so its restart behaviour is unspecified. |

## 5. Proposal A: Rust-flavored relational (8.5)

**Strengths**

- **Every negative edge carries a bang, enforced both ways by the type checker** (§1.1, bang table). This covers
  negation, aggregates, outer join, choice, order, `reveal!`, Anti/NM lattice methods (`x.leq!(y)`,
  `x.concurrent!(y)`), deltas and Z-set views. An unannotated user lattice method defaults to NM and must be called
  with a bang. Monotonicity is syntactically decidable per rule, which is the most analyzable property in the panel.
- **The lowering is direct.** A rule is `head <- body;` with the kind keyword first, and a multi-head rule is one
  rule per head. Every expansion in §3 is exact and matches CR-xx: soft state with a read-time TTL, sticky choice
  with the TEST-012 override hooks, the multi-FD greedy scan, `seq!`'s high-water mark, the `fold_ordered!` carried
  form, `upsert` through a keyed `$ups` relation (which makes SEM-051 fall out of the key), and `resolve`.
- **Seals are sound.** Producers are declared, all of them must seal, the digest is checked, and conflicting or
  exceeded digests are violations (§3.12). `c.sealed()` is a threshold, and the proposal notes correctly that the
  analyzer must treat `$sealed` as CLOSED rather than read its polarity from the lowering.
- **Explicit `per` driver** for LANG-106. The driving relation is a visible positive literal.
- **The spec language suits the verifiers.** `quorum v in r { conj }` keeps the first-order encoding in EPR (VER-008).
  There is also `net` (the network as a grow-only set), `#[inductive] invariant`, `bounds { }` for BMC, and a hard
  error for LDFI without `pre`/`post` (CR-30).
- **Choreography lowering is stated for the verifiers**: a `$role(R)` guard per rule in the single Dedalus program.
- **Coverage is complete.** An appendix maps every P0/P1 LANG id, and my grep found no gap.

**Weaknesses**

- `delete` and `upsert` carry no bang and do not say "next". A notes this itself (§5.2.1).
- Heads that merge into lattices look exactly like heads that insert (§5.1.4). The types disambiguate them for the
  analyzer but not for a reader.
- Channel atoms omit the `@` column, so their arity differs from the declaration (§5.1.5).
- Unlabelled rules get positional site ids (`module::rN`, §3.3). Reordering rules then changes seeded choices,
  which breaks replay across versions (SEM-084).
- There is no `monotone` assertion. A module cannot say "this must be CALM" and have the compiler check it.
- Bootstrap re-runs in every incarnation, with no guard or lint for durable writes (SEM-071).
- Schedule-dependent built-ins (`now()`, `tick()`, `rand()`) are unmarked (§5.2.2).
- E6's seal is sent once and relies on `#[fault(reliable)]`. There is no idiom for re-sending seals.
- Much of the semantics lives in attributes (`#[durable]`, `#[soft]`, `#[seal]`, `#[final]`, `#[exactly_once]`).
  The attributes carry meaning, not metadata, and a reader has to know that.

## 6. Proposal B: reactive and choreographic blocks (6.5)

**Strengths**

- **`on` versus `while` makes ODD-05 visible** (§3.4). A level-triggered `send` over standing state is a periodic
  resend, and the keyword says so. The event/standing classification is also exactly the set of Edelweiss ARM and
  DIST-007 suppression candidates. No other proposal surfaces re-derivation.
- **Closed `view` definitions.** Every rule of a derived relation sits in one place, which helps per-relation
  stratification and CALM reports.
- **`monotone` view/handler/module is a checked promise**, and a module whose channels are all guarded gets the
  ANA-141 certificate in the build output (§3.16).
- **The seals are the most robust design** (§3.13). They are per producer (`sealed occ{split: s} from m`, which is
  Blazes `Seal_split` made operational). The producer keeps a send log, re-sends the seal until it gets through,
  and raises a "send after seal" violation.
- **`final not r(x)`** gives a first-class final-absent test (LANG-212).
- `r__prev` for delta atoms inherits the relation's durability, so recovery does not produce a burst of spurious
  `inserted` facts (§3.5).
- The instance path is part of each channel's identity and wire schema id.
- The verification gate `check ldfi | bmc | smt | sim | asp expect holds | fails` is clean.

**Weaknesses**

- **The handler form hides rule bodies.** Each statement's real body is the handler header plus every enclosing
  `if`/`for` plus every `let` in scope. `outer` and `any` multiply statements into rules labelled `h.3a`/`h.3b`,
  which the user never wrote. LDFI lineage and coverage reports name those labels (§5.2.4).
- **Labels are unstable.** Rules are numbered by statement ordinal in depth-first order even inside labelled
  handlers, and view alternatives by alternative ordinal. Reordering statements changes site ids, and with them
  seeded choices and provenance identities (SEM-084, B §5.2.6).
- **The syntax suggests a semantics the language does not have.** Handlers read as per-message, sequential code,
  while the semantics is a set per tick (CR-02, CR-03). B calls this its central trade-off (§5.2.1). E1 needs
  `index()` over same-tick puts to avoid SEM-051.
- **`emit` versus `next` into persistent state is a silent trap.** `emit seen` plus `not seen` in the same tick
  disables delivery and still stratifies (§5.2.2). Catching it needs a transitive lint.
- **Soft state is stale by one tick after idleness** (§4), and the size cap can be exceeded.
- **The lattice-read rule contradicts itself.** The table in §3.8 lists `==` on lattice values and `a.le(b)` as
  allowed exact reads, while the text below it makes any non-threshold comparison a compile error. `a.le(b)` is an
  unmarked non-monotone method call, which contradicts §1's "no function call is ever non-monotone".
- `next` inside `bootstrap` lands at tick 1, a deviation from LANG-190 that the proposal acknowledges.
- Coverage gaps: the generic `threshold(t1..tn)` (LANG-126, **P0**) is missing, the clamped Z-set view (LANG-138)
  is missing, and the bare Z-set atom is an unmarked negative edge. Role expressions under dynamic membership are
  undesigned (§5.3.8).

## 7. Proposal C: modernized Datalog (8.0)

**Strengths**

- **It is the closest to the IR.** `head :- body.` with a kind prefix, location-first positional atoms,
  Prolog-style end dots and one IR rule per surface rule. Choreographies lower to TPLP's heterogeneous-roles
  encoding, one program with a role guard per rule, and projection only drops guards (§3.7.6). That is the right
  semantic account for the verifiers.
- **`max{…}` versus `lmax{…}`** is the clearest line in the panel between non-monotone relational aggregation and
  monotone lattice folds (§3.4.4). The error message it produces ("use `lset{…}.size`") teaches CALM.
- **`monotone` on a rule, block, module or output** is checked by ANA-020/021 and names the offending keyword on
  failure.
- **`stable fn … after t`** is a declared method class for Bloom^L's "monotonic then immutable" pattern (§3.5.7). It
  gives ANA-142/143 exactly the fact they need.
- **Bootstrap and recovery are the most careful** (§3.2.6): `bootstrap fresh` with a `recovered` input, and a
  compile error for durable writes in a plain `bootstrap`.
- **The verification vocabulary is the strongest**: `once` (past time), `sent` (the network), `always forall …
  implies`, `prove X by induction using { … }`, and `faults { model: sync | async, round }` (ODD-16).
- Relation parameters (`peers: rel(n: Node)`) and the CR-16 treatment of facts are well thought through.

**Weaknesses**

- **Lattice polarity is invisible in the text** (§3.5.4, C §5.2.6). `a <= b` means ⊑ on lattices and is antitone in
  `a`; `x >= 5` is a threshold on `LMax` but antitone on `LMin`; `a.concurrent_with(b)` is NM. None of these is
  marked. That contradicts C's own claim that every non-monotone operator comes from one short keyword list.
- **The seal default is unsound** when a key has several producers (§4, CR-27).
- **`resolve` is extended to same-tick deductive derivations** without LANG-117 backing (§4).
- `delete` and `upsert` do not say t+1, and `next` inside `bootstrap` lands one tick later (a deviation from
  LANG-190).
- `#` is not a comment in `.bls` files, a deviation from LANG-208 (P0) that C records as D1.
- **Coverage gaps**: the grammar has no `extern type` (LANG-027), although the coverage table claims one; no
  `readonly` (LANG-051); no `keys`/`values`/`payloads`/`rename` projections (LANG-094); no Z-set view modes
  (LANG-138); and a `weight n` read is an unmarked negative edge.
- Site ids for unlabelled rules are unspecified.
- `localize` is restricted to insert-only bodies.

## 8. Proposal D: query / comprehension (7.0)

**Strengths**

- **Bangs mark everything and the marking is enforced both ways** (§1.3). That includes `delete! next`,
  `upsert! next`, `left join!` and `all!` over open domains. "A rule without `!` is monotone" is literally true.
- **Mutations say when they happen** (`delete! next`, `upsert! next`). No other proposal makes the t+1 timing of
  deletion and upsert visible.
- **`all x in r: e` is monotone, and needs no bang, exactly when `r` is static or sealed on the quantified key.**
  That is the closed-domain ∀ that CALM and finality depend on. It makes "every producer sealed" (E6) and "every
  participant voted yes" (E4) monotone by construction.
- **`group by` (lattice folds only) versus `group!` (relational)** is the same monotone/non-monotone split as C's
  `lmax{}`/`max{}`.
- **Seals work on any relation**, with a counted digest, a persisted sealed flag, and "broken" and "conflict"
  violations (§3.13). The reducer-side seal on `got(mapper, …)` is exactly what Edelweiss join reclamation
  (ANA-063) needs.
- **`expect out: confluent;` in a spec asserts an ANA-029 certificate.** That puts CALM results in CI.
- Implicit `$sender`/`$principal`/`$session` fields keep SEM-091's columns apart from the payload.

**Weaknesses**

- **The chosen columns are inferred from liveness** (§4, D §5.1.5). This is a semantic defect in a P0 feature
  (LANG-108): the FD is not stated, and seeded outcomes change when unrelated `select` fields change.
- **Lowering depends on clause order.** Every non-monotone clause cuts the body into an auxiliary relation, so
  where a `where` or `let` sits relative to `group!` or `choose!` changes the IR. The auxiliary relations also
  lengthen lineage, and D says nothing about collapsing them in explanations.
- **Soft state is stale by one tick after idleness, and eviction is one tick late** (§4). The hidden `$birth: lmax`
  column is nonetheless an elegant way to express "re-derivation refreshes" (SEM-060).
- `bootstrap` is "true only at tick 0" while E3 relies on it re-running after a restart, so restart behaviour is
  unspecified (SEM-071).
- **Coverage gaps**: user-defined aggregates exist only as `extern` declarations, with no in-language
  init/step/merge/finish (LANG-105); there are no derived combiners (LANG-112); there is no `sum_values`
  (LANG-282); the Z-set view modes are unbanged negative edges; and no default fault model is stated.
- Partitioned channels lower to `hash % C` (§3.11), not rendezvous hashing, so ownership shifts on any membership
  change.
- A Raft election has about twenty bangs, so the bang stops standing out in coordination code (D §5.1.1).

## 9. Gaps shared by all four

1. **The Blazes `Rep` annotation (ANA-041, P1) has no syntax anywhere.** `Seal_key` is covered by each proposal's
   seal declaration, but no proposal can say "this stream is replicated". Grey-box annotations (ANA-044, P2) are
   also absent.
2. **Site ids for unlabelled rules are not stable** (SEM-084). A, B and D fall back to positions, and C does not say.
3. **Schedule-dependent built-ins are unmarked.** `now()`, `tick()`, `rand()`, `choose_rand` and sticky choice make
   outputs schedule-dependent (SEM-087). A and C say so in prose only.
4. **Lattice merges look like inserts** in every head form. Only C's `lset{…}` fold expressions and D's `group by`
   make a fold visible, and only in expression or clause position.
5. **Role expressions under dynamic membership** (`route`, `size`, `majority`, typed `@` columns) need an epoch
   argument that no proposal spells. A comes closest with `C::membership(n, epoch)`.
6. **Compiler-generated relations in provenance.** Only B says that generated relations are collapsed in
   explanations. LDFI lineage (TEST-023) needs every generated relation tagged as provenance-transparent in the IR.

## 10. What the final design should take

The recommended base is **A's rule form and bang discipline**, with these additions.

1. **From D: make mutations say both facts.** Spell deferred mutations so that they show the negative edge and the
   t+1 timing, for example `delete! next r(…)` and `upsert! next r(…)`, or A's keywords with a bang. This closes
   A's footnote "every negative edge has a `!`, except …".
2. **From C: split relational aggregates from lattice folds.** Keep relational aggregates (`count!`, `max!`) and
   monotone lattice folds (`lset{ x | … }`, `lmax{ … }`) visibly different. Lattice folds need no bang and can be
   used in expressions.
3. **From B and C: a checked `monotone` modifier** on rules, blocks, modules and outputs. Failure names the offending
   construct, and a `monotone` module whose channels are guarded prints the ANA-141 certificate.
4. **From B: `on`/`while` as an analysis surfaced in syntax.** Adopt B's event/standing classification. Require an
   explicit marker on `send` and `next` rules whose body has no event atom (level-triggered resend, ODD-05), and warn
   when an event-driven rule is written with it. These rules are also the ARM/DIST-007 candidates.
5. **From C: `stable fn … after t`** as a declared method class for monotone-then-immutable reads, next to A's
   `#[threshold]`.
6. **From C: `bootstrap fresh` plus a `recovered` input**, with a compile error for durable writes in a plain
   `bootstrap`.
7. **From B and C: keep `next` meaning t+1 everywhere, including in `bootstrap`,** and write tick-0 initialization
   with deductive heads. Record this in DECISIONS.md as a clarification of LANG-190. It removes a context-dependent
   lowering.
8. **From A and C: spell the chosen columns in every choice** (`choose!(y per x)` or `choose X -> Y`). Reject D's
   inferred Ȳ.
9. **Stable site ids.** Require a label on every rule that contains a seed- or schedule-dependent site: an error
   under `--strict`, a warning otherwise. Derive the ids of other unlabelled rules from a canonical hash of the
   normalized rule rather than from its position.
10. **Seals, combining A, B and D.** A seal declaration names the key *and* the producer role (A); unanimity is
    required unless the analysis proves one producer per key. The digest is mandatory. The producer keeps a send
    log, re-sends the seal until it gets through, and raises "send after seal" (B). Receivers get overflow and
    conflict violations (A, D). Seals can apply to local relations as well as channels (D). Add D's rule that
    `all x in r: e` is monotone exactly when `r` is closed.
11. **Soft state from A and C, with D's refresh mechanism.** Filter visibility against the current tick's `now` (A,
    C) and express refresh as D's hidden `$birth: lmax` merge. Evict in the same tick that the table exceeds `max`.
12. **From A: bang the Z-set → set/lattice read** (`distinct!`, `clamped!`, `weights!`), as the ODD-06 amendment
    requires.
13. **From A: an explicit `per` driver** for LANG-106 defaults.
14. **Specs combining A, B, C and D**: A's `quorum v in r { }` and `net`; C's `once`, `sent`, `always forall …
    implies` and `prove … by induction using { … }`; B's and C's `expect holds | fails`; D's `expect out: confluent`
    certificate assertion.
15. **From B: delta atoms inherit durability.** The `r_prev` relation behind `inserted`/`deleted` has the relation's
    own durability.
16. **Fill the shared gaps.** Add a `#[rep]`/`replicated` stream annotation (ANA-041). Add a
    `provenance_transparent` flag on every compiler-generated IR relation. Add an optional determinism marker for
    schedule-dependent outputs, enforced through ANA-039 under `--strict` rather than through the bang, because
    schedule dependence is a determinism class and not a monotonicity class.
17. **From A and C: lower choreographies to one program with a `$role` guard per rule**, and treat projection as
    guard elimination. That is what the verifiers need to see.
