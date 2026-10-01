# SL: the language slice (working notes)

Design: `docs/design/EXTENSIONS.md`. Branch `slice-lang`, worktree `.worktrees/sl`.

## Resume here

- **State (2026-10-01):** items 1–6 done (`?`, generic functions, `while` tables, `resolve prefer`, the order of
  checks, formats). Next: item 7, the Kafka rewrite, measured.
- Update this section whenever work stops.

## Baseline (S8, merged at 69b630c)

`examples/kafka`: 5,591 lines, 4,066 code lines (no blank or comment lines). Count with:
`for f in examples/kafka/*.bls; do grep -cvE '^\s*$|^\s*//' $f; done | paste -sd+ | bc`.

## Work items, in order

1. `?` on `Option` in functions, tuple patterns in function `let`s (EXTENSIONS 2.1).
2. Generic functions with named-function arguments (2.2).
3. Guarded persistence (`table … while …`) and soft tables (2.3).
4. `upsert` into resolved tables, multi-column costs, `resolve prefer(…)` (2.4).
5. Planner: infallibility-aware ordering; `top!`, `index!` with `per`, multi-alternative `per` (2.6).
6. Formats (2.5).
7. The Kafka rewrite (2.7), measured; every S6–S8 test green.
8. Review, notes, merge.

## Findings and deviations

### Item 1: `?` (done)

- Lexer token `QUESTION`, postfix `TRYEXPR` (precedence 15), `ast::ExprKind::Try`, desugared per function body in
  `ast::desugar` into nested `match`es before name resolution (no IR change). BLS0218 for a `?` under a branch, the
  right of `&&`/`||`, a closure, a function not returning `Option`, or a rule body.
- Tuple patterns in function `let`s already worked; nothing to add.
- Tests: `bls_functions` (`v_try`, both evaluators against Rust), `blossom-front` diagnostics.

### Item 2: generic functions (done)

- Syntax: `fn f<T, U>(…)`, parameter type `fn(A, B) -> R` (a `TYPE` node with `fn`; `ast::Type::Fn`). Bounds and
  defaults on a function's type parameters, generic `extern fn`s: not implemented (LANG-180/181 errors).
- Templates (`resolve/generic.rs`): a function with type parameters or a function parameter is resolved once into
  HIR with `CallParam`/`GenericCall` nodes; its variables move out of its scope. Each call from a non-template
  makes an instance (a fresh scope, the body copied with the named functions substituted, generic calls
  instantiated recursively; a template met again on the stack is BLS0213). Instances are ordinary `HFn`s with an
  `HScheme`.
- Type checking: an instance's type parameters are fresh terms shared by its one call and its body, so they are
  inferred like rule variables; each function argument's signature is unified with its parameter's type.
  Undetermined type parameters: BLS0300 at the call, before anything in the body. `HScheme::targs` records them.
- Lowering merges instances by (template, type arguments, function arguments) into one IR function named
  `f<T…, g…>`; `HFnId → FnId` is now a map (`Lowerer::fns`, `Lowered::fn_origins`), no longer the identity.
- BLS0219: misplaced function types and function parameters, closures or generic functions as function
  arguments.
- Deviations from EXTENSIONS 2.2: a type parameter may sit only in tuples, `Option`, `Vec`, `Set`, `Map` (not in
  lattices or user types, which have no generics yet); a body's annotations cannot name a type parameter; an
  uncalled template is name-resolved but not type-checked; a type error inside an instance points into the
  template's body without naming the call.
- Tests: `bls_functions` (`v_generic`: list readers at two types, a generic helper, a function parameter passed
  on, a shared instance; both evaluators against Rust), `blossom-front` (IR merging, BLS0219, inference and
  recursion errors). Mutation-checked: dropping the signature unification, dropping the merge.

### Item 3: `table … while BODY` (done; soft tables deferred)

- Syntax: a `while BODY` relation clause (`WHILECLAUSE`, last: its body runs to `;`), tables only (BLS0106).
- Resolve: the condition is resolved as the body `p(c̄), BODY` (columns by name) into `Hir::guards`; type checking
  walks it like an invariant. Not implemented (BLS0908): with a `resolve` policy, on lattice values.
- IR: `Persistence::Frame { guard: Option<RelId> }` (serde-skipped when absent, so artifacts without guards are
  unchanged); the guard `p$keep` is a relation of the Persist construct, derived by `p$keep(x̄) :- p(x̄), BODY`;
  the validator checks the frame rule's extra literal and the guard's ownership. LDFI's `is_frame` now names the
  frame rule exactly (a guard rule shares the construct).
- Deviation: EXTENSIONS 2.3 forbade a condition reading its own table (BLS0503); nothing requires it (the frame
  rule is inductive), so it is allowed and tested.
- Soft tables (§7.9) are deferred until the Kafka rewrite needs TTL state; they stay BLS0908.
- Tests: `bls_extensions` (each guarded table against a twin kept by the explicit clean-up rule, both evaluators,
  12 seeds; mutation: lowering without the guard fails it), `blossom-front` (a durable condition with a negation,
  a `let`, a `where`, an existential and the table itself; the misuses).

### Item 4: `resolve prefer(rule, …)` (done; the completions deferred)

- Syntax: `resolve prefer(a, b)` (a `POLICY` with names). Resolve: `HRel::prefer` (not an `HResolve`: the table keeps
  its frame rule); each `next`/`upsert` into it gets an `HRank` from its handler's label (`RuleCx::label`);
  `check_prefer` after a module's rules reports names that write nothing (BLS0411, also twice-named or empty).
- Lowering (`prefer_rels`): listed writes go to `r$w(x̄, rank, upsert)`, unlisted to `r$wx(x̄, upsert)`; the least
  rank per key survives (`r$wmin`) and goes on to `$ups` or `@next`. Conflicts among survivors are the usual
  BLSR001/BLSR002. IR: `ResolvePolicy::Prefer { rank }` on a Resolve construct.
- Deferred (Kafka uses no `resolve` policy): `upsert` into resolved tables, multi-column costs; still BLS0908.
- Tests: `bls_extensions` (each preferring table against a hand-guarded twin, upserts and `next`s, 12 seeds, both
  evaluators; mutation: max instead of min rank fails it; same-handler and unlisted conflicts are BLSR002 on both
  evaluators; one value from listed and unlisted is fine), `blossom-front` (BLS0411, BLS0106, BLS0908).

### Item 5: the order of a body's checks (done; aggregates deferred)

- Semantics (LANGUAGE §9.14): once its variables are bound, every check that cannot fail runs first, then the first
  ready fallible guard, else the first ready fallible binding, each in body order; repeat. `Expr::cannot_fail` /
  `Literal::cannot_fail` in the IR define "cannot fail" once for both evaluators (oracle `plan::flush`, engine
  `Plan::new`). The frontend splits top-level `&&` in guards into separate guards.
- Engine range probes collect bounds over every guard up to and including the first fallible check.
- Changed expectations: `engine_planner` — the range-after-fallible case now runs (the guard rejects before the
  division) and errors only for an accepted row; the two flipping-negation fixtures now negate the `let`'s output, so
  the negation still runs after the division and the S5 engine fix (`Search::fail`) stays covered (mutation-checked).
- Deferred (Kafka uses none): `top!`, `index!` with `per`, multi-alternative `per`.
- Tests: `bls_extensions` (`order.bls`: a guard after a division, a negation after a subtraction, `&&` with a
  fallible first conjunct, all without errors on both evaluators, and a fallible `let` still raising for an accepted
  row; a range probe past a fallible `let` examines < 100 of 20,000 rows). Mutation: classifying every check as
  fallible fails both.

### Item 6: formats (done)

- Syntax: `format Name(params) { [name:] element [if cond] [= default], … }` and `format name(F) = element;`
  (`FORMATITEM`/`FORMATFIELD`/…); elements are expressions (calls), so `nullable(compact_array(T(v)))`.
- `ast::format::expand` runs in `ModuleTree::load` after includes (so an alias may come from an included file):
  records become a struct, `Name::decode`/`Name::encode` (resolved as two-segment calls), generated sub-functions
  per compound element and shared helpers (`format$bytes`, `format$gather<T>`, `format$skip_tags`, …), built as AST
  (spans at the format) and desugared like written functions (`?`).
- Deviations from EXTENSIONS 2.5: call syntax (`nullable(x)`), `constant(E, v)` for `const(v)` (`const` is a
  keyword), no `le` integers and no known tagged fields yet (`tags` only skips/writes empty) — added when a use
  appears; element arguments read the parameters only (not earlier fields).
- Tests: `bls_extensions` (`formats.bls`: 60 random requests from an independent Rust encoder, at three versions —
  decoded fields, used bytes and re-encoding equal; 180 truncations and a 2^40 count decode to nothing; both
  evaluators; mutation: inverting `bool`'s encoding fails it), `blossom-front` (11 misuses, `at`, a record used from
  a rule).

### Open observations

- After item 5 the corpus is unchanged: core 195, lattices 47, async 36, net 3 passed, 0 failed.

- `kafka3::three_brokers_keep_every_acknowledged_record_under_kill_9_and_partitions` failed its idle-CPU check
  (a broker over 2 s of CPU in 4 s idle) once during a full `cargo test --workspace`, and passed alone (72 s). Load
  sensitive; not caused by SL (no Kafka program uses the new features yet).
