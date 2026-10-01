# Blossom extensions for shorter programs (the language slice, "SL")

Status: design, 2026-10-01. Decided by the user after Slice 8 ("language slice first", then Slice 9 written in the
smaller language). Notes and the resume checklist: `docs/plan/notes/SL.md`.

## 1. Why

Bloom's promise was an order of magnitude less code for the same function. Slice 8's Kafka broker
(`examples/kafka`, about 4,100 code lines) keeps it where Datalog does the work — `raft.bls` is 370 lines against
etcd's 4,646 — and misses it elsewhere. Measured on the S8 code:

| Where the lines go | Lines | Cause |
|---|---|---|
| Wire codecs and checks | ~2,100 | sequential parsing in a function language without `?`, generics or binary formats: 287 nested `and_then`s, per-type copies of the same list reader |
| State clean-up | ~60 rules | every table's rows outlive their owner unless a `forget_`/`drop_`/`remove_` rule deletes them |
| Same-tick conflicts and their guards | several bugs, ~30 guard literals | two writers of one key in one tick is a hard error; `upsert` into a resolved table and multi-column costs are unimplemented |
| Helper views | ~30 views | the planner probes an index range only before any fallible `let`, so views are split by hand |
| Request handlers | ~300 | every API repeats decode, decide, encode, answer, dequeue, time out |

Every extension below is generic, comes from the Datalog family, lowers to the existing core IR (or a small
addition to it) and runs on both evaluators. The Kafka code is rewritten on top as the slice's acceptance test.

## 2. The extensions

### 2.1 Failure as absence in functions (`?`)

**Heritage.** In a rule body a failed match is not an error, it is no row: `let Some(x) = e` just derives nothing.
Functions should have the same failure semantics, instead of hand-threading `Option` through `and_then`.

**Syntax.** A postfix `?` on an expression of type `Option<T>` inside a function whose result type is
`Option<U>`:

```blossom
fn read_item(c: Cur) -> Option<(Item, Cur)> {
    let (slot, c) = read_u64(c)?;
    let (part, c) = read_i32(c)?;
    let (err, c) = read_i16(c)?;
    Some((Item { slot, part, err }, c))
}
```

**Semantics.** `e?` evaluates `e`; `None` makes the enclosing function return `None` at once; `Some(v)` is `v`.
Inside a closure `?` is not allowed (BLS0218: it would return from the closure, which combinators cannot express);
in a rule body `?` is not allowed either (a failed `let Some` already filters). Tuple patterns in `let` (`let (a,
c) = …`) are part of this item: today a function's `let` binds one name.

**Lowering.** Desugars in the frontend into nested `match`es before type checking finishes; no IR change.

**Diagnostics.** `?` outside an `Option`-returning function, under a branch or in a closure: BLS0218.

### 2.2 Generic functions (LANG-180, specified, unimplemented)

`fn read_array<T>(c: Cur, item: fn(Cur) -> Option<(T, Cur)>) -> Option<(Vec<T>, Cur)>` needs both type parameters
and function arguments. Type parameters are monomorphized (§5.7). Function values are restricted to *named*
functions passed as arguments and called directly, so a call graph stays first-order after monomorphization (each
instantiation with a named function is a new specialization); closures stay combinator-only. This replaces the
per-type copies (`read_all_steps`, `read_config_steps`, `read_item_steps`, …).

Generic structs and enums (LANG-023) are in scope only if the Kafka rewrite needs them.

### 2.3 Persistence with a condition (`while`) and soft tables (LANG-048)

**Heritage.** In Dedalus a table persists by a rule, `p(X)@next :- p(X), notin p$del(X)`; nothing says that rule
must be unconditional. Overlog had soft state with lifetimes. Most of the Kafka clean-up rules say "this row lives
as long as its owner": a placed produce entry as long as its request is queued, a follower record as long as this
broker leads, a leader announcement as long as the partition exists.

**Syntax.**

```blossom
table placed(c: Conn, i: u64, j: u64, …) key(c, i, j) while queued(c, i, _, _);
table behind(g: Group, f: Node<Broker>, since: Instant, out: bool) key(g, f) while leader(g, _), members(g, f);
soft table heard(n: Node) ttl 1s;                          // §7.9, as specified
```

**Semantics.** A row of a `while` table persists from tick t to t+1 only if the guard holds at t for that row (the
guard's free variables are the table's columns; it is a conjunction of atoms, negations and `where`s). Rows still
arrive through `emit`/`next`/`upsert` as before; a row whose guard fails is visible in the tick it fails and gone in
the next — exactly what a `while X, not owner(…) { delete X }` rule does today, which it replaces.

**Lowering.** The table's frame rule gains the guard: `p(X̄)@next :- p(X̄), notin p$del(X̄), guard(X̄)`. The IR's
`Persistence::Frame` gets an optional guard (validator invariant 5 checks the guarded expansion); both evaluators
already run frame rules as rules.

**Static rules.** The guard may read the table itself (the frame rule is inductive, so no same-tick cycle
arises); it is stratified like any rule body. `durable` tables may be guarded: the guard
is evaluated every tick, so a durable row whose owner is gone does not survive a restart either.

Soft tables (§7.9: TTL and `max`, deterministic expiry at tick boundaries) follow only if the Kafka rewrite needs
TTL state; until then they stay not implemented (BLS0908).

### 2.4 Resolution completions and writer precedence (LANG-117)

**Completions.** `upsert` into a table with a `resolve` policy (today: use `next`), and a resolution cost that is an
expression over several columns (today: pack them into one tuple-typed column). Deferred: the S8 Kafka code uses no
`resolve` policy, so these follow only if the rewrite needs them; until then they stay not implemented (BLS0908).

**Writer precedence.** Two rules writing one key in one tick is a hard error (SEM-050/051), and right: an accident
must not pass silently. But some programs mean it: a snapshot install resets a partition's log end in the same tick
materialization would advance it; retention moves the active segment in the tick a batch is appended. A table may
name the order in which its writers win:

```blossom
table log_end(tid: Bytes, part: i32, next: i64) key(tid, part) resolve prefer(reset_log, advance_end);
```

Among the candidates for one key in one tick (persisted, `next`, `upsert`), those written by the earliest listed
rule win; rules not listed, or two candidates from one listed rule, still conflict (the error stays the default).
**Lowering.** A new `ResolvePolicy::Prefer { rank }`: each write is staged with its handler's rank (`r$w(X̄, rank,
upsert)`, unlisted writes in `r$wx`), the least rank per key survives, and survivors apply as their verb does (then
the existing conflict checks). Implemented as LANGUAGE §10.7 describes; the candidates are one tick's writes only
(persisted rows are not ranked: an `upsert` replaces its key's row as always).

### 2.5 Formats: reversible binary grammars

**Heritage.** Prolog's definite clause grammars describe a sequence as a relation over it, and run both ways:
parse and generate. A `format` declares a byte layout once; the compiler derives the decoder and the encoder.

**Syntax.**

```blossom
format MetadataTopic(version: i16) {
    topic_id: uuid,
    name:     nullable compact_string,
    tags,
}
format MetadataRequest(version: i16) {
    topics:      nullable compact_array(MetadataTopic(version)),
    allow_auto:  bool,
    include_ops: bool               if version >= 8,
    tags,
}
```

A format is a struct (its fields, in order) plus two derived functions:

```blossom
fn MetadataRequest::decode(c: Cur, version: i16) -> Option<(MetadataRequest, Cur)>
fn MetadataRequest::encode(x: MetadataRequest, version: i16) -> Bytes
```

**The element language** is generic, not Kafka's:
- integers `i8`…`u64` (big-endian by default, `le` for little-endian), `bool`, `uvarint`, `varint` (zigzag);
- `bytes(n)` (fixed), `rest`;
- length-prefixed values and arrays, parameterized by the length's encoding and a bias: `prefixed(L, bias, F)`,
  `array(L, bias, F)`; `nullable` maps a reserved length (the bias's zero) to `None`;
- `const(v)` (written on encode, checked on decode), field conditions `if <expr over earlier fields and
  parameters>` (absent fields decode to their type's zero or the declared `= default`);
- tag-length-value sections `tlv(T, L) { 0: field: F, … }` (unknown tags skipped);
- other formats, with arguments.

Kafka's vocabulary is then a small library of format aliases in Blossom (`compact_string = prefixed(uvarint, 1,
utf8)`, `compact_array(F) = array(uvarint, 1, F)`, `tags = tlv(uvarint, uvarint) {}`, `uuid = bytes(16)`).

**Lowering.** Desugars to ordinary generated Blossom functions (2.1's `?` included) before type checking: no IR or
evaluator change. Decode of `encode(x)` is `Some((x, end))` for every value (round trip property, tested per format
with random values, on both evaluators).

### 2.6 Planner: pure work in any order, and the unfinished aggregates

Not new syntax: the planner may move a `let` or `where` whose expression it proves cannot fail (no division,
overflowing arithmetic, `error`, partial `match`, or fallible call) past others, and treats `+`/`-` on operands it
can bound as infallible for range probes, so views need not be split by hand to get an index range. Finish `top!`,
`index!` with `per`, and `per` views with several alternatives (all specified).

As built: this is a rule of the language, not a planner liberty, since moving a check changes which errors a tick
raises. LANGUAGE §9.14: once its variables are bound, every check that cannot fail runs first, then fallible filters,
then fallible bindings, each in body order; `a && b` is two checks. Both evaluators follow it, and the engine's range
probe uses every guard up to the first fallible check (a fallible guard's bound counts when it is that check: its
end is evaluated once per probe, and an end that fails is no bound, so the guard raises its own error). The
aggregates (`top!`, `index!` with `per`, multi-alternative `per`) are deferred: the Kafka code uses none.

### 2.7 Request handlers as a module

Not a language change if import (§6.5) suffices: a `Serve` module per request kind owns the queue head, the answer,
the dequeue and the timeout; the importer provides decode, decide and encode as relations. Where import falls short
inside `at` blocks or with stream parameters, those gaps are fixed generically.

## 3. Order and acceptance

1. 2.1 `?` and tuple `let`s, then 2.2 generic functions — the cheapest, and the codec rewrite needs them.
2. 2.3 guarded persistence and soft tables.
3. 2.4 resolution completions and `prefer`.
4. 2.6 planner and aggregates.
5. 2.5 formats (the largest payoff and design job).
6. The Kafka rewrite (with 2.7), measured; every S6–S8 test passes unchanged, both evaluators agree on the corpus.

Acceptance: the Kafka code at about 2,000 code lines or less (from ~4,100), the same tests green, and each feature
with its own tests on both evaluators (differential against the oracle) and its diagnostics.
