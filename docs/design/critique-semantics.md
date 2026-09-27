# Critique of ARCHITECTURE.md: semantic fidelity and coverage

Reviewer lens: does the architecture implement FEATURES §1 (every CR) and §3 (SEM) faithfully, does every P0/P1
feature in §2–§10 have a home, and are the analyses, LDFI and verification designed correctly? The inputs are
`docs/design/ARCHITECTURE.md` (first draft, 2026-09-27), `docs/research/FEATURES.md`, `docs/design/LANGUAGE.md`,
`docs/DECISIONS.md` and the research reports (mainly R06 for Molly and R11 for Dedalus^L).

**Method.**

- Every CR and every SEM item was traced to the IR type, planner rule, engine mechanism, oracle rule or verifier
  step that realizes it.
- The regimes of §3.4 were checked against LANGUAGE.md's normative lowerings (§7–§11): `emit` into a table is a
  deductive rule into a persistent relation, cells are read only by lookup, and soft tables and zset tables have
  explicit expansions.
- The LDFI design was checked against R06's code-level description of Molly (§3.4–§3.7, §12).
- A script listed every P0/P1 id in FEATURES §2–§10 that ARCHITECTURE.md never names. The list was then pruned by
  hand to separate ids with no home at all (Appendix C) from ids homed implicitly by the IR or by LANGUAGE.md.

**Overall.** The skeleton is right. It has one IR whose construct expansions are normative, an oracle that shares no
code with the engine, a sans-IO node shared with the simulator, and plan perturbation. Those are the correct
mechanisms for keeping a fast engine faithful to Dedalus^L, and most CRs are implemented as written.

The problems are in the fast paths and in the verification layer:

- **Engine divergence.** Four places in the incremental-regime design (§3.3–§3.5) diverge observably from naive
  per-tick Dedalus evaluation (CR-26/ENG-067). The differential suite would catch each one at run time, but the
  design itself is wrong and should be fixed on paper first.
- **LDFI.** The design is not consistent with CR-20 and cannot meet its own Molly-parity exit criterion.
- **Error handling in verifiers.** Runtime errors in the verifiers are silently absorbed as crashes.
- **Schedule pruning.** The CALM-based pruning predicate is unsound.
- **Coverage.** The core of the user's chosen Hadoop successor (lineage recovery, the stage planner) has no home.

Severity labels:

- **Must-fix:** a CR/SEM violation, an unsound verifier, or a user decision with no home.
- **Should-fix:** a normative P0/P1 item that is missing or underspecified.
- **Consider:** a clarification or a cheaper, safer alternative.

---

## Must-fix

### M1. Deletions do not interact correctly with continuing deductive support (CR-05, CR-26, ENG-062)

**Where.** §3.4 segment table ("*persistent*: everything any rule writes into a persistent relation, whatever the
rule's regime … removed only by deletion statements … a Counted rule's Δ⁻ is ignored"), §3.9 `apply_staged`.

**Problem.** LANGUAGE §8.2 lowers `emit r(…)` into a table to a *deductive* rule `r(A…) :- W.` next to the frame
rule `r(X̄)@next :- r(X̄), notin r$del(X̄).` In Dedalus the deductive rule fires again at t+1 whenever its body still
holds, so a deletion at t has no lasting effect on a tuple that is still derived.

The engine puts a Standing or Counted rule's output into the persistent segment. A staged `$del` then kills it at
the boundary, and Standing continuation never re-derives it, because no input changed.

```blossom
table active(id: u64);            // no delete path: Growing
table shown(id: u64);             // has a delete path
show: while active(id) { emit shown(id); }      // Standing (monotone over Growing input)
hide: on hide_req(id) { delete shown(id); }
```

Dedalus: `shown(7)` holds at t+1 after `hide_req(7)` at t, because `active(7)` still derives it. Engine: `shown(7)`
is absent from t+1 on. The same happens with a Counted writer, with `upsert`'s implicit deletions, and with
`resolve`. This is exactly the "semi-naive with deletions" pattern that ENG-062 forbids. It is hidden because the
writer's regime is chosen from its *inputs* (Growing), while the *head* shrinks.

Inductive writers are handled correctly (§3.5 cancels `del(t) ∩ V(t)`). The deductive case has no equivalent.

**Proposed change.** Give each persistent head that has a deletion path two supports:

- F, the frame support: the persistent segment. Deaths apply only here.
- D, the current deductive support: the outputs of the Standing and Counted rules that write the head, kept in a
  standing or weighted segment.

The normative equations (add them to §3.5):

```
r(t)   = F(t) ∪ D(t)
F(t+1) = ((F(t) ∪ D(t)) ∖ del(t)) ∪ V(t)          -- V = the inductive contributions
```

Incremental realization, O(|Δ|) per tick:

- A tuple enters F only when it *leaves* D, in the Δ⁻ of the writing rule at tick u. It is inserted into F, visible
  from u, unless it is in `del(u−1)`.
- The boundary keeps a small tombstone set `del(t) ∩ D(t)` until D has been recomputed at t+1.
- Heads without a deletion path keep today's fast path (straight into F).

Transient writers are unaffected. Add this to §0.2 as the concrete form of ENG-003/004 for heads that shrink. Add a
corpus case built on the example above, and a perturbation that forces these writers to `Recompute`.

### M2. `$now` is not treated as time-varying (CR-17, CR-18, ENG-074, SEM-083)

**Where.** §3.2 (`time_varying: contains $rand/$tick/choose_rand sites`), §3.4 (the Standing condition "No
time-varying built-ins"), §3.4 dirty scheduling.

**Problem.** `$now` changes every tick, but only `$rand`, `$tick` and `choose_rand` count as time-varying. So a rule
such as

```blossom
while deadline(id, d) where d < now() { emit overdue(id); }   // deadline: a Growing table
```

is classified Standing (all positive atoms Growing, guard monotone). It is evaluated once, when `deadline(id, d)`
arrives, and never again as time advances, so `overdue(id)` never appears.

The same applies to every soft-table lowering (LANGUAGE §7.9: `heard$b(N; B) :- …, B := LMax::of($now)` and the
read-time TTL `$now - T < TTL`) and to logical timers (`N := $tick / 5, T := $now`). FEATURES ENG-074 itself omits
`now()`. SEM-087 does list it as schedule-dependent, so this is a gap in FEATURES that the architecture inherited.

**Proposed change.**

- Define the time-varying scalars as `$now`, `$tick`, `$rand*` and `choose_rand`. `$incarnation` belongs in the
  list for completeness; it only changes when the engine is rebuilt.
- A rule that mentions any of them anywhere (atom, guard, bind or head) is never Standing or Counted; it is Transient.
- A construct whose expansion mentions one (SoftTable, LogicalTimer) marks its stratum `time_varying`.
- Record this in §0.2 as a refinement of ENG-074.
- A later optimization may treat `d < $now` as a threshold over the non-decreasing per-incarnation clock. It must
  still dirty the stratum on every tick in which the stratum's input is non-empty.

### M3. Lookup literals never produce Δ versions (CR-11, SEM-031, SEM-103, LANG-280, ENG-146)

**Where.** §3.3 ("Take a rule with k body *atoms*…"), §3.4 (Standing and Counted versions are over atoms), §3.7
("Binders: `Bind`, `Lookup`").

**Problem.** Semi-naive versions, Standing continuation from `TickNew` and Counted delta queries are all rooted at
body *atoms*. A `Lookup` (`V = r[k̄]`) is a binder, so a change to the looked-up cell never re-fires the rule. Cells
are read *only* by lookup (LANGUAGE §7.13), and lattice folds in expressions lower to lookups (LANGUAGE §11.7):

```ir
fold$4be1(T; {V}) :- vote(T, V).
quorum_ok(T) :- term(T), S = fold$4be1[T], $size(S) >= QUORUM.
```

`term` is a Growing table, so `quorum_ok` is Standing. When the fold for T crosses QUORUM at tick 5, no atom of the
rule has a Δ, no version runs, and `quorum_ok(T)` is never derived.

If `vote` is a channel, the fold is tick-local but read by lookup. The Transient condition ("some positive *atom* is
TickLocal") does not trigger either, so the rule is still planned Standing and reads stale values. In-stratum
lattice recursion through lookups terminates early, with a fixpoint that is too small.

**Proposed change.** Treat a `Lookup` as an occurrence of its relation for versioning.

- A Δ version is rooted at the changed cells of the looked-up relation (the change log of §3.3). It probes the
  remaining literals by the bound key.
- For a 0-ary cell, the Δ version is "the cell changed: re-run the rule".
- Because a lookup of an absent key yields ⊥ and a non-strict morphism may map ⊥ to a non-⊥ value (ENG-146), the
  "absent → present" transition is part of the Δ.
- Classify a Lookup of a TickLocal or Shrinking relation like a positive atom of that class (Transient or Counted).
  In Counted, a cell change is −(k̄, old) + (k̄, new).
- The growth-class fixpoint and the trigger sets must include lookup edges (`EdgeKind::Lookup` already exists).

### M4. The DBSP boundary is underspecified: weighted heads and CR-35 enforcement (CR-26, CR-35, LANG-138, LANG-158, ENG-070)

**Where.** §2.4 (`RelClass::Weighted`), §2.5 (`HeadMode::ZAdd`), §3.4 (the weighted segment holds "Counted outputs,
with derivation counts"), §4.6 (`WeightedStore` holds "Counted outputs and zset tables"), §5.4 (frame kind
`7 ZBatch`).

**Problems.**

1. **Weighted heads under re-derivation.**
   - LANGUAGE §12 makes a `zset table` persistent and additive: `emit` adds w now, `next` adds w at t+1. Under CR-26
     a level-triggered writer adds w *every tick*, as in `while assigned(n, job) { emit load(n); }`. The oracle
     would do this.
   - The planner would classify the writer Standing (Growing inputs, monotone) and add w once. That is a
     divergence at the exact place the lens names.
   - The IR also never states the transition for weighted relations: sum over distinct *body valuations* (not head
     tuples) per tick, integrated across ticks, with zero weight meaning absent.
2. **Two kinds of weight in one store.** The internal derivation counts of the Counted regime (presence = w > 0 for
   a *set* relation) and user zset tables (signed weights that mean something) share `WeightedStore` with no stated
   invariant separating them.
3. **CR-35 is not enforced below the frontend.**
   - The IR validator (V1–V8) has no rule saying that a group-typed column or a `Weighted` relation on a channel
     requires `wrapper: Some(..)`.
   - Admission accepts `ZBatch`/`GDelta` frames on any channel.
   - DIST-007 resend suppression and delta shipping are not excluded for group payloads.
   - LANGUAGE's BLS0307 is the only guard. The validator is meant to be the backstop (§2.9), and here it is not.
4. **ZBoundary is not negative for CALM.** ENG-070 makes edges out of a Z-set stratum negative "for SEM-021 **and
   ANA-022**". The draft uses `ZBoundary` only for stratification rejection. Its polarity for ANA-020/022 and its
   OPEN class for ANA-120 (SEM-017) are unstated.
5. **W3 `unwrap` needs `(old, new)`.** ENG-070 requires the merge to return both for each changed key.
   `LatticeOps::join_into` returns only `Joined`.

**Proposed change.**

- Add to §2.4 the normative weighted transition: `Z(t) = Z_carried(t) + Σ_{ν ⊨ body, emit} w(ν)` and
  `Z_carried(t+1) = Z(t) + Σ_{ν ⊨ body, next} w(ν)`. The oracle implements exactly this.
- Reject level-triggered writers into `zset`/`bag` heads: a ZAdd statement must be event-driven. This needs a new
  BLS code and a LANGUAGE amendment. An unbounded per-tick accumulation is almost certainly a bug. The alternative
  is to plan ZAdd-headed rules as Transient over all valuations every tick. Pick one; either is correct, and
  "Standing" is not.
- Keep the two weight kinds separate: `WeightedStore<Derivations>`, which asserts w ≥ 0 (a negative weight is an
  `InternalError`), and `WeightedStore<UserZ>`.
- Add validator rule **V9**: a group/ring-typed column, or a `Weighted` relation, on a channel requires a wrapper.
  Add an admission rule: `ZBatch`/`G*` kinds are accepted only on channels whose schema declares that wrapper.
  Neither DIST-007 nor SEM-109 applies to wrapped group channels.
- `ZBoundary` edges get polarity ± in ANA-021. Weighted relations are OPEN for ANA-120 unless sealed.
- `join_into` gets an optional out-parameter carrying the replaced value, used by the W3 `unwrap`.

### M5. LDFI crash semantics, the hazard encoding and the Molly-parity target are inconsistent (CR-20, CR-21, CR-22, TEST-021, TEST-027, BENCH-130..136)

**Where.** §6.3, §8.1–§8.4, §8.7, §11.5, §14.2 (the M4 exit criterion).

**Problem 1: the encoding is incomplete for the adopted crash semantics.**

- CR-20 adopts the paper's semantics: a crashed node fires no rules from its crash tick. A receiver that crashes
  before a message arrives therefore never derives anything from it.
- The encoding puts crash variables only on message leaves, as `K(from, t)`, and creates them "only for senders".
- So "crash the receiver before it receives" is never hypothesized. Theorem B.2 (completeness) then fails for our
  own semantics.

**Problem 2: CR-20 contradicts the Molly verdicts the corpus requires.**

- Molly's *code* keeps a crashed node computing and receiving; only its outgoing clocks are removed (R06 §3.4(4)).
- The published verdicts in BENCH-130..132 come from that code.
- With the corpus specs used verbatim, `deliv_assert.ded`'s `missing_log` has no crash filter. Take `replog` at
  6/3/1, where the expected result is no CE:
  - crash c at tick ≤ 2;
  - under CR-20, c never logs "hello", so `missing_log(c, hello)` holds;
  - `post(b, hello)` fails while `pre(b, hello)` holds;
  - the result is a CE.
- FEATURES is internally inconsistent here: TEST-021 says "crashes follow SEM-070", while BENCH-130 requires Molly's
  verdicts. The architecture claims both.

**Problem 3: "identical sets of minimal counterexamples" (§8.7, §11.5, M4) is ill-defined and unreachable.**

- R06 §12 says explicitly that *verdicts* are the parity target and that counts are a performance target.
- Molly uses point crash literals `C(n,t)` with exactly-one constraints and then `minimalSets`. That keeps crash
  times that are not minimal in Appendix B's sense.
  - Example: a goal supported by v's sends at ticks 1 and 3.
  - Molly keeps both `{O(v,c,1), C(v,2)}` and `{O(v,c,1), C(v,3)}`.
  - Under Appendix B only the crash at 3 is minimal, because crash(v,3) removes a subset of the clocks crash(v,2)
    removes.
- Our order-variable encoding computes Appendix-B minimality (which is correct), so it yields a different set.
- Molly's reported counterexamples are also bad *runs*, whose fault sets are seeded supersets, not minimal
  falsifiers.

**Proposed change.**

- Add `FailureSpec.crash_view: CrashView { Frozen, MollyContinue }`.
  - `Frozen` is the default (CR-20).
  - `MollyContinue`: a crashed node keeps receiving and running its local deductive and inductive rules, and sends
    nothing from its crash tick. It changes nothing that another node can observe; it differs only in what *spec*
    rules see of crashed nodes.
  - The `.ded` compatibility profile sets `MollyContinue`.
  - Record this in DECISIONS.md as a scoped refinement of CR-20/TEST-021, because it resolves a conflict inside
    FEATURES.
- Hazard encoding under `Frozen`:
  - add a premise `Alive { node, tick }`, with hazard `K(n, t)`, to every **non-persistence** firing at (n, t);
  - persistence and identity frames carry the frozen state and get no Alive premise;
  - create crash variables for every node that has a firing in the lineage, from its first firing;
  - drop omissions subsumed by a receiver crash at ≤ send tick + 1.
- Under `MollyContinue` the encoding is exactly Molly's: sender-side crashes only.
- Parity becomes:
  1. an identical CE/no-CE verdict for every configuration in BENCH-130..134 and BENCH-137;
  2. run counts ≤ BENCH-136;
  3. where a corpus item states falsifier sets (BENCH-135's Netflix toy, BENCH-300..312), equality of the
     **Appendix-B-minimal falsifier sets**, defined over removed clock facts (crash(n,t) removes n's clocks at ≥ t
     and, under `Frozen`, n's firings at ≥ t).
- Golden files store those normalized sets and never raw Molly output. Reword the M4 exit criterion accordingly.

### M6. Lineage is incomplete under incremental regimes (TEST-023, TEST-026, ENG-112)

**Where.** §4.9 (Tier C records "every distinct firing"), §8.2 (only "persistence chains stay as frame firings").

**Problem.** Molly's lineage, and TEST-023's "every alternative firing", is per tick: a fact at tick t is explained
by firings *at t*. The draft's regimes do not re-fire recurring derivations:

- Standing outputs are derived once;
- Counted outputs change only on Δ;
- carried contributions are applied as deltas;
- natives replace their expansions.

So Tier C has no firing at EOT for, say, a tick-local view `r(x) :- s(x)` over a persistent `s`. The ProvGraph
builder then either fails ("no firing explains r(x)@EOT", Molly's own error) or anchors the support at the wrong
tick. Under `Frozen` crashes (M5) the tick of each firing matters for the `Alive` premise.

Two related gaps:

- A Counted firing that stops holding (a premise died) is indistinguishable in the log from one that still holds.
- The hidden sender column is dropped when no rule reads it (§2.4). The ProvGraph still needs one receive record per
  (sender, send tick), because each sender's copy is an alternative support (SEM-091, TEST-023).

**Proposed change.**

- **P0: an LDFI plan profile.** When Tier C capture is on for LDFI, the planner uses `Transient`/`Recompute` for
  every rule and turns natives off, so the expansion runs literally and every recurring firing is logged per tick.
  Molly-scale programs make the cost irrelevant, and the plan-perturbation suite already exercises this path.
- **P1: interval-stamped firings.** Each logged firing carries [first tick, end tick):
  - Standing: open-ended;
  - Counted: closed by the Δ⁻ that removed a premise;
  - carried: closed by the contribution's removal.

  ProvGraph instantiates a firing at tick t lazily when t lies in its interval.
- Receive records are always per (sender, send tick), whatever the storage projection.

### M7. Runtime errors are absorbed as crashes inside the verifiers (ARCH-20, SEM-032, CR-53)

**Where.** ARCH-20, §3.9, §5.1. Nothing in §6, §8 or §9 says what the simulator, LDFI or BMC do with a `TickError`.

**Problem.** ARCH-20's rationale, "every analysis, certificate and LDFI verdict already assumes crashes are
possible", is sound for *production*. As a verification semantics it hides bugs:

- a deterministic BLSR001 that halts every node makes `pre` false, and LDFI reports a vacuous pass;
- an error-crash counts against, or silently exceeds, `max_crashes`;
- BMC certifies "holds within bounds" over runs in which the program had no meaning (CR-53).

**Proposed change.** State in §6.1 that under `blossom-sim`, LDFI, BMC and the oracle a `TickError` is a **verdict
of its own**:

- `SimError::ProgramError { node, tick, error, reproducer }`;
- an LDFI result of "fails: program error";
- a BMC result of "fails".

It is never a crash fault. `NodePolicy::on_tick_error` applies only in `blossom-runtime`. The differential oracle
must raise the same error at the same tick (error equality is part of ENG-067).

### M8. The CALM schedule-pruning predicate is unsound (TEST-003, ANA-039, VER-002)

**Where.** §6.2: a channel is branching when "the receiver's consumer reaches a non-monotone operator, a choice, or
a schedule-dependent site **before persistent state or a lattice**". §9.1 CALM-POR reuses it.

**Problem.** TEST-003 permutes every delivery that reaches a non-monotone consumer. Cutting the search at persistent
state or a lattice drops exactly the programs where that state is later read non-monotonically:

```blossom
table got(k: u64);
on put(k) { emit got(k); }                              // channel → persistent state (the draft stops here)
while got(1), not got(2) { send alarm(1) to MONITOR; }  // non-monotone read of that state
```

If `put(1)` and `put(2)` arrive together, no alarm is sent. If they are split across two ticks, an alarm is sent.
The draft delivers `put` canonically, so the simulator and BMC never see the alarm, and BMC's "holds within bounds"
is wrong. The cut seems to have been borrowed from ANA-024 (guarded asynchrony), which is a different question: two
streams meeting.

**Proposed change.** `branching(c)` holds iff some **same-node** path from c's receive side reaches one of:

- a SEM-021 negative edge (negation, non-lattice aggregate, outer, order, choice, exact lattice read, ZBoundary);
- a seed-dependent or schedule-dependent site (ANA-039);
- a time-varying read.

The path may go through deductive, inductive, persistence and lattice-merge edges; async edges are excluded,
because the next node's own pruning covers them. Add the example above to the `async` corpus as a pruning
regression test.

### M9. The user's chosen Hadoop successor has no home for its core (DECISIONS ODD-19 (d); FLAG-121, 123, 124, 132; DIST-045)

**Where.** §1.5 systems list, §14.2 M7, §14.3.

**Problem.** DECISIONS.md fixes the successor as a *lineage dataflow engine* with lineage-based recovery and
watermarks as seals. The draft's systems are `boomfs`, `boommr`, `objectstore` and `tide`. Several P1 items have no
home:

- FLAG-121, the stage planner (narrow versus wide dependencies);
- FLAG-123, recovery from lineage (`derived_from`, recursive recomputation, checkpointing by cost);
- FLAG-124, deterministic speculation with an attempt-keyed idempotent commit;
- FLAG-132, CALM-minimized fault tolerance (replay deterministic operators, log only nondeterministic events at
  points of order);
- DIST-045, distributed provenance.

FLAG-123 also conflicts with ODD-08 (b) as the draft realizes it: engine provenance is off in production, but
lineage recovery needs `derived_from` there.

**Proposed change.**

- Add `systems/boom2` (M1: FS2 plus the lineage batch engine), with the stage planner and the recovery rules as
  Blossom modules.
- Make `derived_from` **program-level, partition-granularity lineage**: ordinary relations written by the stage
  rules, not Tier B/C. It then works with provenance off.
- Add `blossom-prov::distributed` for DIST-045 (ExSPAN-style by-reference `prov`/`ruleExec` relations keyed by
  tuple and rule hashes, opt-in).
- For FLAG-132, specify the causal log as the TEST-010 *Minimal* trace restricted to nondeterministic sources at
  ANA-022 points of order, plus an ABS barrier implemented as a seal-driven native.
- Put FLAG-120..124 into M7's deliverables and exit criteria explicitly (M1a includes FS2).

---

## Should-fix

**S1. Upsert and resolve conflicts are detected one tick late (SEM-051, CR-07).** §3.5 and §3.9 raise BLSR002 "at
the tick boundary", which means during tick t+1's `apply_staged`. In the normative expansion (LANGUAGE §8.2) the
conflict is a key violation of the keyed scratch `r$ups`, inside tick t's fixpoint. So the oracle errors at t, while
the engine commits t and releases its outbox, then errors at t+1. Detect the conflict in tick t's temporal phase,
before `finish_tick`.

**S2. DIST-007 resend suppression has to be a rewrite the oracle sees (ODD-05, ENG-067).** §3.5 applies ARM-gated
suppression inside the engine. The oracle resends every tick, so the "exact outbox" comparison would fail, or be
quietly weakened. Express suppression as a `blossom-rewrite` pass: a `sent$` shadow relation plus a guard, checked
by VER-016. Both the oracle and the engine then evaluate the rewritten program.

**S3. SEM-109 is missing: delta shipping needs its side condition.** The wire has `LDelta` (§5.4) and §5.7 lists
DIST-006 tags, but nothing restricts delta shipping to channels whose every consumer is a join-morphism into
persistent state. Add that analysis and let the plan choose the frame kind per channel.

**S4. Counted rules over tick-local inputs need the previous tick's contents.** The Counted rule needs integrated
state I_j and a change batch D_j for every input. A negated TickLocal atom falls into Counted (it is neither
positive-TickLocal nor Standing), but the transient segment is truncated at tick start, so I_j is gone. Simplest
fix: any rule with a TickLocal or time-varying literal, positive or negated, is Transient. Counted then reads only
persistent inputs.

**S5. The tick counter is not durable across incarnations (SEM-001, SEM-071; LANGUAGE §15.1).** Recovery boots at
"last_tick + 1", but ticks without a durable delta are never written to the WAL. After a restart the node reuses
tick numbers it already executed. That breaks message identity (sender, send tick), trace keys (node, tick),
LDFI's (from, to, send tick) variables and LANGUAGE's "`tick()` is durable across incarnations". Keep a tick
high-water mark in META with reserve-ahead (write "reserved up to T+N" before passing T), and boot at the reserved
bound.

**S6. "An idle stretch equals empty ticks" is false for this runtime (SEM-009, CR-12, ODD-04 (c), TEST-004).**

- **Why the claim fails.** An empty tick fires level-triggered sends (ODD-05's literal resend) and advances `$tick`
  and `rand` keys. Lazy ticks do neither.
- **Consequences.** TPLP fair runs, which CR-12 uses for reasoning, require every node to transition infinitely
  often. The `ExhaustiveScheduler` only ticks nodes that have an event, so BMC explores fewer runs than the
  normative semantics.
- **Fix.**
  - Add ODD-04 (c)'s per-node heartbeat (`NodeConfig::heartbeat: Option<Duration>`).
  - Add heartbeat transitions to the exhaustive scheduler and the confluence tester, as TEST-004 already demands.
  - Add a lint for level-triggered sends in deployed programs that no timer or event gates.
  - Qualify LANGUAGE §15.3: the equivalence holds only for nodes whose empty tick is a no-op.

**S7. The BMC visited set can merge states that are not equivalent (VER-002, ENG-120).** `WorldDigest` is "state
modulo time plus the in-flight multiset". It must also cover:

- timer deadlines relative to now;
- native state that is not stored in relations (seal counts, wrapper contexts, `seq` high-water marks, soft
  deadlines);
- in-flight earliest arrival times relative to now;
- crash status;
- for nodes whose plan has time-varying sites, the tick and the incarnation, because `rand` and `choose_rand` are
  keyed by them.

Otherwise two states with different futures share a hash and BMC prunes real behaviour.

**S8. The LDFI negative-support details are incomplete (CR-31, TEST-025, TEST-143, ENG-116).**

- Implement Molly's time filter for possible causes: time(z) < time(n), or equal only along a purely deductive path.
  Without it the over-approximation is still sound, but BENCH-136's run counts are at risk.
- The hazard table (§8.3) covers only `notin`. Exact lattice reads (TEST-143), non-lattice aggregates and choice
  also need conservative negative support. A fault can *add* a contributor or a lower-priority candidate
  (ENG-116's refined mode), and ignoring that breaks completeness for native Blossom programs. Keep Molly's
  conjunctive aggregates under the `.ded` profile.

**S9. LDFI and the simulator ignore channel fault models (LANG-155).** A `#[fault(reliable)]` channel should have no
omission variables, only crash-induced loss. `lossy-forever` and `lossy` should be distinguished in the simulator.
Today every channel is droppable.

**S10. Certificates are missing normative distinctions (CR-29, ANA-029, ANA-141, ANA-143).** The certificate
record (§7.1) has to report:

- "confluent" and "consistent under fair runs" as separate results (CR-29; BENCH-091 must fail to certify
  consistency);
- exactly one of ANA-029's verdicts;
- ANA-143's "confluent but not certified".

ANA-141 needs a `join_prime: bool` on threshold operations in `LatOpDecl`: a non-join-prime threshold over an
ephemeral lattice fails the certificate (BENCH-302).

**S11. Finality has no construct, although the text says it does (ANA-121, LANG-212, SEM-016, SEM-017).**

- The ANA-121 row calls it "a native `Finality` construct", but `ConstructKind` and the natives table (§3.6) have
  none.
- Finality status (`provisional`, `final_present`, `final_absent`) is observable output on subscriptions and
  channels. So it needs a normative expansion (the M⁻/M⁺ bounds programs of ANA-121, with seals making relations
  CLOSED) that the oracle evaluates. Otherwise ENG-067 does not cover it.
- State SEM-017's rule (reject `final` on NEVER-FINAL outputs) in the ANA-120 row.

**S12. The SEM-036 wrapper identity is underspecified.** Dots must be `(origin, incarnation, seq)`. Reusing a dot with
a different payload must raise a hard `Conflict`, which needs a payload fingerprint per dot inside the unacked
window and a `TickError` variant (neither LANGUAGE nor the draft has a BLSR code for it). `seq` must be durable or
incarnation-scoped.

**S13. Dynamic membership breaks the "static" assumptions (CR-16, DIST-042, SEM-080).**

- `node_dir` and `R$members` are `Static`, and Standing allows `notin` over static relations. With DIST-042 they
  change at runtime, so they must be classified Shrinking, or every membership change must force `Recompute` of
  their dependents.
- `TraceEvent` has no directory or membership event, so replay of such runs is incomplete. Add one.

**S14. Semantic cross-checks are missing (SEM-023, SEM-108, BENCH-312, TEST-012, TEST-015).** Add to §11:

- a declarative-versus-operational differential runner: clingo `pure^L(P)` models against the ultimate models of
  exhaustive simulation, on the corpus (the BENCH-312 guard for Theorem 4^L);
- an oracle that computes its own stratification instead of trusting `blossom-analysis`, plus a perturbation
  across valid stratifications (SEM-023);
- the exhaustive priority-permutation mode (TEST-012);
- the k-order shuffle checks (TEST-015). Today it appears only as a risk-table mention.

**S15. There is no procedure for computing ultimate models (SEM-044, SEM-107, TEST-005, TEST-088, VER-015).**
Confluence and finality tests compare ultimate models, but no procedure produces them for runs that never quiesce
(timers, heartbeats). Define one:

- stop at quiescence; or
- detect a lasso via `WorldDigest` repetition (which needs S7) and take the facts that hold around the loop;
- compare lattice limits through threshold facts;
- otherwise report "inconclusive", never "confluent".

**S16. The spec engine is not O(Δ) as claimed.** `r$log(Node, x̄, Tick)` gains a row for every fact on every tick
it holds, which is O(state) per step. Encode trace relations as intervals (birth and death, reusing ENG-029
history) and define spec atoms over them. Alternatively, drop the O(Δ) claim.

**S17. Per-occurrence polarity needs to be stated (SEM-102).** ANA-002 rejects SCCs that contain "NM/Anti
LatticeOp" edges. Say that an edge's class is the *composition* along the occurrence's expression path to the head
or a guard: Anti∘Anti is Mon, anything with NM is NM, and a threshold under `not` is Anti. Otherwise the analysis
either rejects valid programs or misses exact occurrences.

**S18. Several P1 items have no home (see Appendix C).** Proposed homes:

| Item | Home |
|---|---|
| LANG-202 catalog relations | generated `static` relations from `front::lower` |
| TEST-082 input generation | a SAT model finder in `blossom-testkit::inputgen`, using rustsat |
| TEST-084 implementation equivalence | the differential runner |
| VER-020 trusted-module interface checks | `blossom-verify::trusted`: sim plus LDFI on the declared interface spec |
| TEST-007 stochastic delivery at quiescence | a scheduler mode |
| ENG-066 invalidation sets | `blossom-plan` |
| ENG-147 aggregate semimodules | `blossom-prov` |
| LANG-113 estimators | `blossom-std`, plus FLAG-135's seeded random block order in the scheduler |
| LANG-028 blobs | content-addressed handles; bytes in the object store; never interned |
| ANA-033 CRDT query classification | `blossom-analysis` |
| SEM-045 the published `pure^L(P)` | `docs/design/SEMANTICS.md`, plus the ASP encoder |
| SEM-052 (P1, but its frontend LANG-221 is P2) | the Upsert native's "emit both deltas" mode, so the P1 item does not depend on the P2 frontend |

---

## Consider

- **C1.** Define the iteration bound in Kleene rounds (semi-naive iteration i equals naive iteration i), so the
  oracle and the engine agree on *whether* BLSR007 fires. Treat a disagreement as a test-configuration error, not
  as a divergence.
- **C2.** Soft expiry never wakes a node on its own. State this, and point ANA-006 at soft tables whose expiry must
  trigger sends but that have no timer.
- **C3.** Specify blob handles as content hashes, so canonical order and fingerprints never depend on arrival
  (SEM-088).
- **C4.** On a fault, decide whether ticks that committed but have not yet been released (pipelined commit) release
  their outbox before the halt. Releasing is the more faithful choice.
- **C5.** `.ded` frontend: state that tick 0 runs with no events (Molly facts are `@k` with k ≥ 1). Say how the
  location column of *local* relations is handled: stripped and re-added as `Node` in `r$log`.
- **C6.** `ChooseSpec` for `Rand` should spell out SEM-085's key: node seed, incarnation, tick.
- **C7.** FOL/EPR translation: model choice sites as uninterpreted functions constrained only by the FD, and model
  `$now` and timers as arbitrary non-decreasing inputs.
- **C8.** Note that SEM-043's fourth network mode (non-causal replay) is homed at DIST-025 (P2).
- **C9.** `Head.args` ("every column except col 0") and `Atom.args` ("direction-form channels omit col 0") disagree
  for column-form channels. Pick one rule.
- **C10.** A carried segment of a *tick-scoped lattice* needs replace semantics per key at the boundary, not join.
- **C11.** Have `xtask` generate a per-id coverage table (id → crate/module → milestone) for PLAN.md. §14.1 is only
  per area, and DECISIONS requires every feature to be tracked.
- **C12.** Receive-side SEM-050 conflicts between two senders abort the receiver's tick, so a buggy or hostile peer
  can halt a node. Extend ANA-007 to warn about channels whose key excludes the sender when there are several
  senders.
- **C13.** Add a testkit check that calls each `extern fn` twice per input. A host function that claims purity but
  is impure makes the engine and the oracle diverge in ways that are hard to diagnose.

---

## Appendix A. CR-by-CR fidelity

| CR | Status | Note |
|---|---|---|
| 01 | ok | IR is the semantics; the oracle evaluates expansions |
| 02 | ok | Batch per tick; leftovers trigger another tick |
| 03, 04, 06 | ok | |
| 05 | **broken for deductive writers** | M1; the inductive case is correct |
| 07 | ok, timing off | S1 |
| 08, 09, 10 | ok | |
| 11 | **broken via lookups** | M3 |
| 12 | partial | Fairness and heartbeats (S6); non-causal replay P2 (C8) |
| 13 | ok | C5 |
| 14, 15 | ok | C9 |
| 16 | partial | S13 |
| 17 | **broken** | M2 (`$now`); the lowering itself is fine |
| 18, 19 | ok | |
| 20 | **inconsistent with the parity target; encoding incomplete** | M5 |
| 21, 22 | ok | |
| 23 | ok | Lookup and ⊥ interplay: M3 |
| 24, 25 | ok | |
| 26 | **violated in four places** | M1–M4; S2 |
| 27, 28 | ok | |
| 29 | **missing** | S10 |
| 30 | ok | |
| 31 | partial | S8 |
| 32 | partial | FLAG-107/108 live in systems; M9 |
| 33 | ok | |
| 35 | **partial** | M4 |
| 36 | ok | S11 |
| 40, 41, 45, 46 | ok | |
| 50, 51, 52 | ok | S17 |
| 53 | ok | C1, M7 |
| 54 | ok | |

## Appendix B. SEM items needing action

| SEM | Issue |
|---|---|
| 001, 071 | Tick durability across incarnations (S5) |
| 009, 040 | Idle is not equivalent to empty ticks; fairness (S6) |
| 016, 017 | Finality construct; NEVER-FINAL rejection (S11) |
| 023 | No test (S14) |
| 032, 103 | Iteration-bound agreement (C1); errors in verifiers (M7) |
| 036 | Dot identity and Conflict (S12) |
| 044, 107 | Consistency versus confluence (S10); ultimate-model extraction (S15) |
| 045, 106, 108 | Published `pure^L(P)`; BENCH-312 harness (S14, S18) |
| 050, 051 | Upsert timing (S1); receive-side conflicts (C12) |
| 052 | Home independent of the P2 Overlog frontend (S18) |
| 070 | Crash semantics in LDFI (M5) |
| 083 | Broken by M1–M4 as designed |
| 102 | Composition rule (S17) |
| 104 | Weighted transition undefined (M4) |
| 109 | Missing (S3) |

## Appendix C. P0/P1 ids in FEATURES §2–§10 with no home

The script found 400 P0/P1 ids that ARCHITECTURE.md never names. 169 of them are BENCH (§11), which is outside this
lens. Most of the rest are homed *implicitly*: LANG spelling and statics in LANGUAGE.md and `blossom-front`, and SEM
mechanics in IR types and the oracle. C11's generated table should list those explicitly. The ids below have **no
home or no design** in the draft.

- **No home.**
  - LANG-028 (blob storage and streaming), LANG-113, LANG-202, LANG-205 together with VER-020.
  - ENG-066, ENG-147.
  - DIST-045.
  - ANA-033.
  - TEST-082, TEST-084.
  - SEM-045, SEM-052 (see S18).
  - FLAG-121, 123, 124, 132 (M9).
- **Named only inside a range, with no design text.**
  - TEST-004 (heartbeat exploration, S6), TEST-007, TEST-015 (a risk-table mention only).
  - ENG-146's lookup rule (M3).
- **Homed but semantically incomplete** (covered above).
  - ENG-069 is not stated explicitly. It holds by construction of the Standing condition, but should be stated.
  - ENG-074 (M2), SEM-109 (S3), ANA-121 (S11), ANA-141 (S10), TEST-023 (M6), TEST-025 and TEST-143 (S8),
    TEST-003 (M8).
- **LIB and FLAG.** Homed at crate level (`blossom-std`, `systems/*`). The exceptions are the M9 items, and FS2
  (FLAG-120), which should be named explicitly as part of `boomfs` or `boom2`.
- **Priority inversions to record in PLAN.md.**
  - SEM-011 (P1) depends on LANG-072 (P2).
  - SEM-052 (P1) depends on LANG-221 (P2).
  - FLAG-107 (P0) depends on DIST-011 (P1). It is needed only once the shuffle is pipelined (M1b).
