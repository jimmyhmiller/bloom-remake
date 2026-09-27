# Syntax panel judgment: systems-programmer ergonomics

Judge: Claude (syntax panel), 2026-09-27. Inputs: `proposal-a-rustlike.md` (A), `proposal-b-reactive.md` (B),
`proposal-c-modern-datalog.md` (C), `proposal-d-query.md` (D), `docs/DECISIONS.md`, FEATURES.md ODD-05 / LANG-008 /
§1 rule 3 (literal resend is normative).

**Lens (and only this lens).** Which syntax lets a systems programmer read, write and maintain Raft, 2PC, an HDFS
namenode, a MapReduce scheduler and a streaming dataflow engine? The focus examples are E3 (Raft election), E4 (2PC),
E6 (MapReduce with seals) and E9 (two instances plus interposition). The lens penalizes verbosity, cryptic sigils,
and designs that hide *what happens when*. I do not score semantic coverage, grammar elegance, or fidelity to
Datalog, except where they change how the code reads to this audience.

## 1. Verdict

| Proposal | Score | One line |
|---|---|---|
| **B, reactive/choreographic** | **8.0** | Reads like the protocol a systems engineer already has in their head: "on this message, do these things, each tagged with when it lands". It is the only design that makes level-triggered resend visible. |
| A, Rust-flavored relational | 6.0 | Declarations are instantly familiar and the rule kind is always the first token. But every intermediate relation needs its own far-away declaration, `!` means four different things, and one event's consequences are spread over many rules that each repeat the body. |
| C, modernized Datalog | 5.5 | The most compact rules, with good `where`/`bootstrap fresh`/input-seal ideas. But `:-`, end-dots, the `->` functional-dependency arrow, `\|>`, `\/` and `vc[]` are a steep ramp for this audience, and nothing distinguishes a once-per-event send from a resend every tick. |
| D, query/comprehension | 5.0 | Records, `$sender`, `let` views and `all x in R:` are excellent for wide tables and data-plane code. But protocol code becomes long SELECTs with a bang on nearly every line, which loses the signal in exactly the code this lens cares about. |

**Winner: B.** The final design should use B as its skeleton and graft on specific ideas from A, C and D (§6).

## 2. Method and numbers

I read all four proposals in full: philosophy, grammar highlights, the construct sections relevant to timing,
choreography, seals and interposition, the whole corpus E1–E10, and each self-critique. For E3/E4/E6/E9 I extracted
the surface code (IR listings removed) and counted it. Comments and blank lines are excluded from the counts.

| Example | Metric | A | B | C | D |
|---|---|---|---|---|---|
| E3 Raft | LOC / tokens | 74 / 1269 | 99 / **1107** | 80 / 1211 | 84 / 1250 |
| | separate intermediate declarations | 12 `scratch` | 0 (views) | 11 `scratch` | 1 (`let` infers) |
| | non-monotone markers | 12 `!` | 10 `not` | 16 `not` | 13 `!` (incl. `not!`) |
| E4 2PC | LOC / tokens | 55 / 1075 | 84 / **999** | 61 / 1038 | 80 / 1178 |
| E6 MapReduce | LOC / tokens | 45 / 757 | 35 / **409** | 29 / 425 | 44 / 476 |
| E9 interposition | LOC / tokens | 30 / 507 | 30 / 360 | 24 / **358** | 28 / 401 |

Caveats. The corpora differ in scope. A and D do full 2PC garbage collection, and B does not. D's E6 includes a
retransmitting seal/ack protocol, while B and C rely on `reliable`. A's E6 does the digest bookkeeping by hand. B's
line counts are high because it puts one statement per line inside braces, but its token counts are the lowest or
tied for lowest in all four examples. Token count tracks reading effort better than line count.

## 3. The four focus examples, side by side

### E3: Raft election

What a systems programmer needs to see: one vote per term, "persist before reply", step-down on a higher term, and
which parts fire once and which fire every tick.

- **B is the best read.** `start_election: on timed_out(t), last_log(li, lt) { let nt = t + 1; next current_term(nt);
  next voted_for(nt, self); next votes(nt, {self}); for p in Server, p != self { send request_vote{…} to p; } }` is
  the Raft paper's "on election timeout" paragraph, line for line. Each statement carries its own timing verb.
  Roles are views (`view leader(t) = eff(t), won(t)`), so step-down needs no rule and there is no role state to keep
  consistent. `become_leader: while candidate(t), votes(t, s), majority(s, Server), not won(t)` says, with `while`,
  that it is level-triggered. Costs: `view eff(t = max(t0)) { current_term(ct), let t0 = reveal(ct); heard_term(t0); }`
  is clumsy, and the handler bodies *look* sequential even though they are per-tick set operations (B §5.2.1).
- **A** reads well rule by rule (`'pick: grant(c, t) <- rv_ok(c, t), !voted_for(t, _), choose_least!(c per t);`), and
  the multi-head `delete candidate_in(t), next leader_in(t) <- won(t);` is a nice touch. But a 13-line block of
  `scratch` declarations sits 20–60 lines away from the rules that define those relations, so every new
  intermediate means editing two places. `#[key()] scratch eff_term { term: u64 }`, where an empty key means "singleton",
  is cryptic. There are 12 bangs with three different meanings.
- **C** is compact and `where` cleanly separates guards from joins. But the role state machine is four `upsert role(…)`
  rules, each with hand-written mutual-exclusion guards (`not stepped_down(), not start_election(_), not won(_)`).
  Adding a fifth transition means re-auditing all four. This is partly an authoring choice, but C's flat rule form
  offers no grouping that would make the exclusivity visible. `(current_term(x) or msg_term(x)) |> group { t = max(x) }`
  mixes pipeline and Datalog in one line.
- **D** turns Raft into a database script: `let start = from t in tick_timer, d in deadline, r in my_role, e in eff
  where now() >= d.at and r.role != Leader and not! exists (s in step_down) and not! exists (w in won) and not! exists
  (h in heard_leader) and not! exists (g in grant) select {term: e.t + 1};`. It binds a timer row `t` it never
  uses, and each nullary check costs five tokens. `send vote_reply to g.cand from g in grant` uses `g` before it is
  bound. The bangs (D §5.1.1 counts about twenty in E3; my extraction of the surface code counts 13, plus 7
  `not!`-style anti-joins) stop standing out.

### E4: Two-phase commit

- **B**: reopenable `at Coordinator { … } at Participant { … } at Coordinator { … }` sections, headed "phase 1a / 1b /
  2a / 2b", make the file read like the sequence diagram. For protocol review this is the single best readability
  feature in the four proposals. Channels declare direction (`channel vote(txn: u64, yes: bool): Participant ->
  Coordinator;`), and the ACLs are inferred from it. `announce: on inserted decision(id, d), txn(id, c, _) { … }`
  says "fire once, when the decision first becomes durable". The `verdict` view lists the three decision paths as
  alternatives. Costs: `if refused(txn) {…} if not refused(txn) {…}` is wordy, because `else` is banned after
  relational conditions (the right call, since that `else` would be a hidden anti-join). A role's state is also
  scattered across several sections, so the formatter and LSP must offer a by-role view.
- **A**: `role Coordinator { … } cluster Participant { … }` with `Node<Participant>`-typed addresses and
  `p in Participant` is clear and complete, including GC and re-announce. But the participant repeats
  `prepare(x) from c, willing(x, y)` in `'log_vote_now` and `'vote_now`, and repeats `can_commit(x, y), asked(x, c)`
  in the two `_later` rules. Logged vote and sent vote come from separately maintained bodies, which is a
  maintenance hazard that B's handler grouping removes.
- **C**: `abort: decide(x, Abort) :- (any_no(x) or timed_out(x)), not all_yes(x), not outcome(x, _).` is admirably
  short. `yes_votes[x] >= lset{ p | participant(p) }` hides set-containment polarity behind `>=` (C §5.2.6). A
  helper `scratch send_prepare` exists only so that two derivations can share one `async`. `on coordinator { }` uses
  `on` for a *location*, which collides with the most natural meaning of `on` in the other proposals.
- **D**: `all p in members(participant): y.voters.contains(p)` is the clearest spelling of "every participant voted
  yes" in the four proposals. `fail "commit decided although this participant voted no" from … select d;` is a great
  runtime assertion. But the phase logic is buried under `select {to: p, xid: o.xid}` records and 16 bangs, and it is
  the largest 2PC by tokens.

### E6: MapReduce with seals (stands in for the scheduler and the streaming engine)

- **B is clearly best.** `channel occ(…): Mapper -> Reducer reliable sealed by (split);`, then `send occ{…} to
  Reducer.by_hash(w);` and `for r in Reducer { seal occ{split} to r; }`. The **compiler** keeps the per-destination
  count digest and generates a `send after seal` violation, so a mapper that emits after punctuating fails loudly.
  On the reducer, `view split_done(s) = split_of(s, m), sealed occ{split: s} from m;` and `finish: on inserted
  all_done(), seen(…) { emit word_count(word, count(s, l, p)); }` fire exactly once. `output final` states the CALM
  class. In the whole mapper, the only seal code is one line.
- **A** makes the user rebuild the seal digest: `'log_sent` duplicates `'map`'s body only to record what was sent,
  and `sent_count`, `line_count`, `split_done`, `unfinished`, `all_done` and `sealed_already` follow. That is about a
  dozen rules of plumbing, and a digest mismatch if `'map` and `'log_sent` ever diverge. A Tide author writes
  this plumbing on every shuffle edge. The reducer side (`shuffle.sealed()`, `#[final]`, the `hot_word` threshold)
  is good.
- **C** has the best *end-to-end* punctuation story: `input line(…) seal on (split)` lets the host seal the input,
  and `ready(s) :- sealed line(split: s).` carries it into the pipeline. Watermarks flowing from source to sink is
  exactly what a streaming engine needs. But the digest is hand-computed (`count{ w | … where reducer.route(w) == r }`,
  which duplicates the routing expression), and `all_in() :- not { splits(s), not sealed shuffle(split: s) }.`
  spells ∀ as a double negation.
- **D**: `channel shuffle(…) partition by hash64(bytes_of(word)) over members(reducer);` puts the partitioner on the
  edge, which is the right abstraction for a dataflow engine. The reducer is `where all m in members(mapper):
  is_sealed(got, mapper: m) group! by g.word select {…}`, which a data engineer reads cold. But the punctuation
  protocol is built by hand (a `shuffle_done` channel, an ack channel, and a receiver-side `seal got on (mapper)
  counted`), and `let fire = from g in go select {} union from t in resend_timer select {};` is an obscure idiom.

### E9: two instances plus interposition

- **B**: `interpose data.bcast as (outside, inside) { admit: on outside(id, p), p.len() <= MAX_PAYLOAD { emit
  inside(id, p); … } }` names *both* sides, and the text states that other writers of `data.bcast` (including
  `publish_bulk` above) are redirected. `membership: while peer(n) { emit data.member(n); emit control.member(n); }`
  makes the every-tick feed visible. This is the clearest interposition.
- **A** has no interposition construct. Its E9 wraps the instance and relies on the importer being the only reader of
  `bulk.deliver`. That covers metering, but not what BOOM used interposition for (LANG-008: slipping Paxos under an
  existing component, or swapping LATE into a scheduler whose inputs other code already writes). Typed generic
  instances (`ReliableBroadcast<P = Bytes, RETRY = 2s>`) are a real plus for systems code.
- **C**: `interpose data.d.bed.pipe_in as orig { … not fd.suspect(dst) }` is the most realistic systems use in the
  corpus: it suppresses retransmission to suspected peers below the retry buffer. It reaches two levels into instance
  internals, which C admits breaks encapsulation (C §5.2.12). Relation-typed module parameters (`peers: rel(n: Node)`)
  are clean.
- **D**: `interpose data.deliver as raw_data { … }` is clear, and the write-up names the surprising consequence (a
  dropped message is still marked `seen`). Relation parameters (`members: static(n: Node)`) are also good.

## 4. Per-proposal assessment

### B: reactive and choreographic blocks (8.0)

Strengths
- **Handlers group consequences by triggering event.** Raft RPCs, 2PC phases and namenode RPCs (create, addBlock,
  complete, blockReport) map one-to-one onto `on …` handlers. One body, many heads, so no body is duplicated across
  rules.
- **Timing verb on every statement** (`emit`, `next`, `send … to`, `delete`, `upsert`), with an explicit `emit` for
  "now". The kind is never implied by absence.
- **`on` versus `while`** is the only syntactic answer in the four proposals to ODD-05's normative literal resend.
  A `send` under `while` visibly re-fires every tick, and `on` requires an event atom or the compiler rejects it.
  For network code, this is the most important "what happens when" fact after the rule kind.
- **Choreography in message order** (reopenable `at Role`), typed channel direction `A -> B`, and ACLs inferred from
  the sends.
- **Role name as member set**: `p in Server`, `Server.by_hash(k)`, `majority(s, Server)`, `Participant.size`.
- **Seals the compiler maintains**: `sealed by (k)`, `seal c{k} to r;`, `sealed c{k} from m`, and a generated
  send-after-seal violation.
- **Closed `view`s** declare and define in one place, with no separate `scratch` declaration.
- **Words over symbols** for non-monotone operators (`not`, `outer`, `inserted`, `reveal`), and `else` only after scalar
  conditions.
- Lowest token counts on E3, E4 and E6, tied on E9.

Weaknesses
- **Handler bodies look sequential but are per-tick set operations** (B §5.2.1). `let nt = t + 1;` followed by
  statements invites an imperative reading, and same-tick conflicting upserts (SEM-051) will surprise Go/Erlang
  programmers. The other proposals share the semantics, but B's form invites the misreading most.
- **The `emit` versus `next` trap into a negated relation** (E2's `seen`). It is shared by all four; B at least
  documents a lint.
- **`->` has two meanings**: the key/value split *and* role direction, sometimes in one declaration
  (`channel put(req: u64) -> (key: string, val: bytes): Client -> Server;`).
- The **event/standing classification is global**: changing a far-away view can turn an `on` into an error.
- **Hidden rule explosion**: `outer` and nested `if`s produce rule labels the user never wrote (`h.3a`).
- **Positional label fallback** makes seeds unstable when handlers move.
- **Named atoms are open by default**: a forgotten join field silently widens the match.
- Aggregates in the view header (`view eff(t = max(t0)) {…}`) and verbose `reveal` reads are awkward.

### A: Rust-flavored relational (6.0)

Strengths
- **Familiar declarations**: `struct`-bodied relations, generics (`ReliableBroadcast<P, const RETRY: Duration>`),
  `pub` interfaces, `use`, typed addresses `Node<Participant>`.
- **The rule kind is the first token.** `grep -E '^\s*(next|send|delete|upsert)'` lists every place where time moves.
- `send h(…) @ p <- body` says "send" plainly. Multi-head rules exist. Named struct patterns require `..`, so omitted
  fields are explicit.
- `deny` invariants with `#[on_violation(abort)]`, and `#[trusted("reason")]` / `#[nondet("reason")]`.

Weaknesses
- **Declaration tax**: E3 declares 12 scratch relations up front, away from their rules. Writability and locality
  both suffer.
- **Core semantics live in attributes**: `#[durable, key()] table`, `#[key()] scratch`, `#[seal(producers = Mapper)]
  #[fault(reliable)] channel`. Attributes read as optional metadata to a Rust programmer, yet here they carry
  durability and keys.
- **`!` is overloaded**: boolean not, anti-join, `count!`/`choose_least!` pseudo-macros ("not a macro", A §5.1.3),
  and `.leq!()` postfix. The `'label:` sigil borrows Rust's loop-label look for a different purpose.
- **Nothing distinguishes level-triggered from edge-triggered rules.** A's document never discusses ODD-05, so a
  `send` over persistent state silently re-sends every tick.
- **No event grouping**, so bodies are duplicated (E1 `'apply_del`/`'ack_del_hit` both join `del_req` with `store`;
  E4's `'log_vote_*`/`'vote_*` pairs repeat whole bodies).
- **Seal digests are the user's job** (E6 mapper bloat).
- **No interposition construct** (E9 is only a wrapper).

### C: modernized Datalog (5.5)

Strengths
- The most compact flat-rule form. **`where`** separates guards from joins. `(a or b)` works inline.
- **`bootstrap fresh { }` versus `bootstrap { }`** (first start only versus every incarnation). Raft's `current_term =
  0` needs exactly this, and nobody else spells it.
- **Input-level seals** (`input line(…) seal on (split)`, then `sealed line(split: s)`) let punctuation flow from host
  sources through the pipeline, which is the Tide watermark story.
- Relation-typed module parameters (`peers: rel(n: Node)`), `monotone` assertions on rules, modules and outputs, and
  `prove … by induction using { … }`.
- Explicit `interpose m.r as orig { … }`.

Weaknesses
- **Prolog punctuation**: `:-` and the end-dot (C admits the end-dot is error-prone, C §5.2.5). The FD arrow is in
  schemas, with `(-> term: u64)` for a singleton. There are also `|>`, `\/` (`vc[].at(self) \/ 0`), `vc[]` for a cell
  read, and `{op => Point(line)}`. Every one of these costs a systems programmer a lookup.
- **Nothing distinguishes level-triggered from edge-triggered rules** (C §5.2.16 admits newcomers will flood the
  network).
- **`delete` and `upsert` do not say "next tick"** (C §5.2.7).
- **Type-directed lattice comparison** (`>=` is sometimes ⊒, C §5.2.6) hides polarity.
- Scratch declaration per intermediate (11 in E3). Double negation for ∀. Commas versus `and` inside parentheses.
  Identifier case carries meaning (`ID` lexes as a constant).
- `on role { }` reuses `on` for location.

### D: query / comprehension (5.0)

Strengths
- **Records and field access** (`m.term`, `select {…}`) keep wide schemas maintainable: namenode inode and block
  tables with a dozen columns never suffer positional-atom drift.
- **`let x = <query>;` views infer their schema**, the lowest ceremony for intermediates in the four proposals.
- **`all p in members(role): …`** as an explicit, analyzable ∀ over closed domains.
- **`partition by hash(k) over members(role)` on the channel** with `owner(ch, k)`, which is the right shape for shuffle
  edges.
- **`fail "message" …`** assertions carry human messages. **`delete! next` / `upsert! next` spell out t+1**.
- `$sender`/`$principal` is readable. `Node@role` typed addresses. `like pipe_in` schema reuse.
- The best fit for scheduler and streaming *queries* (group-by, unnest, top-k).

Weaknesses
- **Bang saturation**: roughly one per rule in Raft and 2PC. The design's own central signal disappears in
  coordination code (D §5.1.1),
  which is the code this lens weights most. `not!`, `group!`, `left join!`, `choose!`, `resolve!`, `.inserted!`,
  `reveal!`, `fn!` and `$`-fields add up to the highest sigil density in the four proposals.
- **Verbosity**: every source needs a binder, every head is a record, and nullary checks read
  `not! exists (s in step_down)`. It has the most tokens in E3 and E4 after A's declaration overhead.
- **Sink-first, binder-later reading order**: `send vote_reply to g.cand from g in grant …` uses `g` before it is bound,
  and the reader must jump from the first line to the `select` (D §5.1.4).
- **SQL look with set semantics** (D §5.1.3). **`choose!` columns are inferred from liveness** (D §5.1.5: adding a
  field to `select` can change the winner).
- Nothing distinguishes level-triggered from edge-triggered rules, the destination is invisible at a partitioned
  `send` site, and the seal protocol is hand-built.

## 5. Hazards no proposal solves (the final design must address them)

1. **Per-tick batching.** Same-tick conflicting upserts and "one vote per term *per tick*" are invisible in all four.
   The final design should make SEM-051 a compile-time "possible conflict" warning with a two-message example, and
   the simulator's batching schedules should be on by default (TEST-003).
2. **Negating your own same-tick write** (`emit seen` plus `not seen`). It should be an **error** when the negated
   relation is written with `emit` in the same handler, not a lint. Allow an explicit override for the rare
   legitimate case.
3. **Wildcard under an aggregate collapses rows** (B E9 `total_bytes`, C §5.2.2, D §5.1.3). This needs a
   default-on lint that suggests binding the identity column.
4. **Lattice merge heads look like inserts** (A §5.1.4). No proposal marks them. Editor highlighting should mark
   merge heads, since it costs no syntax.
5. **The durability barrier is invisible at the send site.** It is correct in all four (SEM-072), but a reviewer
   cannot see "this reply waits for the fsync". The LSP should annotate `send` statements that are released after
   a durable commit.

## 6. What to steal into the final design

Base the final design on **B's handler/verb/choreography skeleton**, and graft on the following.

| # | Idea | From | Why it matters for systems code |
|---|---|---|---|
| 1 | `on EVENT, conds { stmts }` handlers; one rule per statement; shared body | B | One place per RPC or message; no duplicated bodies |
| 2 | Verbs `emit` / `next` / `send … to` / `delete` / `upsert`, always explicit | B (A, C, D agree on kind-first) | Every statement says when it lands; greppable |
| 3 | `on` (edge) versus `while` (level), compiler-checked | B | Makes ODD-05 resend visible; prevents accidental floods |
| 4 | Reopenable `at Role { }` sections, plus a formatter/LSP by-role view | B | Protocols read in message order; mitigates scattering |
| 5 | Channel direction `: A -> B` and ACL inference; role name as member set (`p in R`, `R.by_hash(k)`, `majority(s, R)`) | B | Sequence-diagram vocabulary |
| 6 | Compiler-maintained seal digests (`sealed by (k)`, `seal c{k} to r;`, `sealed c{k} from m`) and a send-after-seal violation | B | Removes about a dozen rules of error-prone plumbing per shuffle edge |
| 7 | Input-level seals (`input line(…) sealed by (split)`, read as `sealed line{split}`) | C | Watermarks flow from source to sink (Tide) |
| 8 | `partition by hash(k) over R` on a channel, plus `owner(c, k)`; keep `to` optional but LSP-annotated | D | Partitioner on the edge for shuffles |
| 9 | `interpose a.i as (outside, inside) { … }` | B | Names both sides; LANG-008 done properly |
| 10 | `where` guard clause after atoms | C | Separates joins from filters |
| 11 | `all x in R: p` quantifier (monotone only over closed domains) | D | Replaces double-negation ∀ |
| 12 | `let name = …` / `view` with **inferred** schema; explicit column types optional | D + B | Removes the declaration tax (A: 12, C: 11 per Raft) |
| 13 | `bootstrap fresh { }` versus `bootstrap { }` | C | Durable initial state set once; volatile state re-initialized after every crash |
| 14 | Generic, typed module parameters (`<P = Bytes>`, const params) and `Node<Role>` addresses | A (D `Node@role`) | Typed payloads and typed addresses catch cross-role mistakes |
| 15 | Relation-typed module parameters (`peers: rel(n: Node)`) | C, D | Membership injection without mixins |
| 16 | Named atoms must write `..` to omit fields | A, C | A forgotten join field fails loudly instead of widening the match |
| 17 | `fail "msg" …` / `invariant name: never …` with action attributes | D, B, A | Runtime assertions with readable messages |
| 18 | `upsert … resolve choose_most(col)` at the statement, as well as at the relation | A, D | The conflict policy sits where the conflict arises |
| 19 | `monotone` assertion modifier on handler, view, module or output | B, C | "I believe this is CALM" becomes a compile check |
| 20 | Mandatory labels on handlers that contain `choose`, `rand` or `seq` | B critique | Stable seeds across edits |

What to avoid:
- **Bang-as-monotonicity-marker** (A, D). It overloads `!`, looks like macros, and saturates in protocol code. Use
  reserved words (`not`, `outer`, `reveal`, `inserted`) and let the analyzer and editor highlight points of order.
- **Attributes carrying core semantics** (A's `#[durable]`, `#[key()]`). Durability and keys are keywords.
- **Prolog punctuation** (`:-`, end-dot) and one-off operators (`\|>`, `\/`, `vc[]`) (C).
- **One arrow with two meanings** (B's `->`). Keep `->` for direction and mark keys with a word, for example
  `durable table voted_for(term: u64, cand: Server) key (term);`, with `key ()` for a singleton.
- **Sink before binder** (D's `send x to g.cand from g in grant`) and binding unused timer rows.
- **`on` as a location keyword** (C's `on coordinator { }`). It collides with the handler meaning.

### Illustration (not normative): E6 with the steals applied

```blossom
choreography WordCount {
  role Mapper: cluster;
  role Reducer: cluster;
  static split_of(split: u32, mapper: Mapper) key (split);

  // B: direction and ACL; D: partitioner on the edge; B: the compiler keeps the digest
  channel occ(word: string, split: u32, line: u64, pos: u32): Mapper -> Reducer
    reliable sealed by (split) partition by hash(word);

  at Mapper {
    input line(split: u32, lineno: u64, text: string) key (split, lineno) sealed by (split);   // C
    map: on line{split, lineno, text}, (pos, w) in enumerate(words(text)) {
      send occ{word: w, split, line: lineno, pos};                  // to owner(occ, w)
    }
    // Sealedness is standing, not an event, so this is `while`. Seals are idempotent and B's lowering
    // re-sends them every tick anyway, so `while` states the real behavior.
    punctuate: while sealed line{split} {                           // C: the seal flows downstream
      for r in Reducer { seal occ{split} to r; }                    // B: digest computed for you
    }
  }

  at Reducer {
    table seen(word: string, split: u32, line: u64, pos: u32);
    output final word_count(word: string, n: u64) key (word);
    keep: on occ{word, split, line, pos} { emit seen(word, split, line, pos); }
    view all_in() = all (s, m) in split_of: sealed occ{split: s} from m;          // D: explicit forall
    finish: on inserted all_in(), seen(word, s, l, p) { emit word_count(word, count(s, l, p)); }
  }
}
```

About 20 lines of code, and every timing fact is on the page: `on` fires on an event; `while` re-fires while its
body holds; `send` is async and routed by the channel; `seal` is a punctuation whose digest the compiler owns;
`on inserted` fires once; `final` is checked. If the final design wants `on sealed …` as an edge form, it needs a
"became sealed" delta event; otherwise `while` is the honest spelling.
