# Syntax panel: grammar and tooling judgment

Judge: grammar and tooling lens. Date: 2026-09-27.
Inputs: `proposal-a-rustlike.md` (A), `proposal-b-reactive.md` (B), `proposal-c-modern-datalog.md` (C),
`proposal-d-query.md` (D), `docs/DECISIONS.md`, FEATURES.md LANG-208, LANG-260..265, TEST-091, TEST-092.

This report judges only:

- **Grammar robustness.** Parser class (LL(k), Pratt), ambiguity, lexical hazards, error recovery.
- **Tooling fit.** Formatter, LSP (completion, hover, go-to-definition, semantic tokens), tree-sitter, diagnostics.
- **Newcomer surprise.** What a reader gets wrong on first contact. A syntax that reads correctly but means
  something else counts as a worse surprise than an unfamiliar one.
- **Evolvability.** Adding operators and modifiers without breaking parses, keyword growth, and source-level
  schema evolution (what happens to existing rules when a relation gains a column).

Semantics, CALM visibility for its own sake, and coverage of FEATURES §2 belong to the other judges. I raise
them only where they affect tooling.

Line numbers below refer to the proposal files as of this date.

---

## 1. Scores

| Proposal | Score | One line |
|---|---|---|
| **D** query/comprehension | **7.5** | Keyword-led clauses give the best recovery, formatting and completion, and record binders survive schema growth. It pays in verbosity and a very large reserved-word list. |
| **A** Rust-flavored | **7.0** | A clean LL(1) item grammar with `#[attr]` as an open extension point. It has three grammar defects of its own, rules bind names in head-first order, and positional atoms break when a relation gains a column. |
| **C** modern Datalog | **5.5** | Case-classed identifiers are a real tooling win. The Prolog end-dot and unterminated declarations are structural hazards for the LSP, the formatter and future keywords. |
| **B** reactive handlers | **5.0** | Its `;`/`}` block structure is excellent. It overloads braces, reserves the most hard keywords (and its own examples break them), derives rule ids from source position, and reads sequential when it isn't. |

Winner under this lens: **D**. The final design should not adopt D whole. §6 lists what to take from each
proposal.

---

## 2. Cross-cutting comparison

| Criterion | A | B | C | D |
|---|---|---|---|---|
| Item-level lookahead | LL(1), LL(3) at `lattice` | LL(1), four LL(2) spots | LL(2) (label) | LL(1), four LL(2) spots |
| Body parse | Pratt; atoms/guards classified by resolver | Pratt; classified by resolver | Pratt; classified syntactically (bare application = atom) | keyword-led clauses, Pratt between keywords |
| Rule terminator | `;` | `;` (statements), `}` (blocks) | Prolog end-dot `.` (whitespace-sensitive) | `;` |
| Declaration terminator | `;` or `}` | `;` | **none** | `;` |
| Finest recovery point | `,` at body depth 0 | `;` statement in a handler | `END` / `}` | each clause keyword (`from`, `join`, `where`, `let`, `select` …) |
| Hard keywords (approx.) | 61 | 73 | 75 | 119, plus 34 reserved bang words |
| Keyword as field name | raw ident `r#next` | not specified (own examples break it) | after `.` only | anywhere a field name is expected |
| Binding order vs completion | head before body (poor) | body before heads (good) | head before body (poor) | sink, then binders, then `select` (best) |
| Rule robust to column added? | positional: no; named needs `..` | named atoms open by default: yes; positional: no | positional: no; named needs `..`: no | record binders `p.f`: yes |
| Extension mechanism | `#[attr]`, open-ended, no grammar change | modifier keywords (grammar change) | contextual trailing clauses (grammar change, fragile) | contextual clauses/modifiers (grammar change) |
| Mismatched-bracket token | `x in (a, b]` | `ring (a, b]` | `ring (a, b]` | `x in (a, b]` |
| `#` | comment unless `#[` / `#![` | comment unless digit (`#12` field no.) | field numbers only (comments in `.ded` frontend) | comment unless digit |
| Non-monotone marking visible lexically | yes (`!`, bang-idents) | yes (keywords; aggregate names are resolved names) | mostly (keywords; `<=` on lattices is type-directed) | yes (`!`, bang words) |

---

## 3. Grammar defects found in review

The authors did not list any of these. Each can be fixed, but the final grammar has to fix them.

**A**
1. **`as` is two things.** `PostfixOp` includes `"as" Type` (cast, line 354), and `AtomSuffix` includes
   `"as" Ident` (row binding, line 327). §2.6 (line 416) says suffixes stop the Pratt loop, but `as` is also a Pratt
   postfix operator. So `buf(..) as m` parses as a cast of the atom to type `m`. Fix: rename the row binder
   (`buf(..) @row m`, or `let m = buf(..)`), or keep `as` for casts only.
2. **`accept` is reserved (line 178), but `#[accept(external)]` uses it as an attribute path** (lines 1337,
   1352, 1935). `Attr = Path` and `Path` excludes keywords. Fix: accept any keyword inside `#[...]`.
3. **`x in (a, b]` with no introducing keyword** (lines 358, 881). The token after `in` decides between an
   interval and a parenthesized expression, and the brackets do not match. Editor bracket matching, auto-pairing,
   rainbow brackets and naive recovery all fail on it. The self-critique notes this (line 2851), but it is worse
   than A allows because there is no keyword to warn a tool.

**B**
1. **`at` is a hard keyword (line 99) but serves as a column and variable name throughout:** the built-in timer
   schema `retry(k, at: Timestamp)` (line 1122), `table deadline() -> (at: Timestamp)` (line 1621),
   `append_entries{term: at}` with `at < t` (line 1715), and E5's `at: VClock` (lines 1892–1914). The keyword list
   is too large for the authors to stay inside it. B also defines no raw-identifier or keyword-as-field escape.
2. **The brace disambiguation depends on a closed statement vocabulary** (§2.4 item 2, lines 336–345). `Path {`
   is a block if the next token is a statement keyword or `}`, and a field list otherwise. Every future statement
   form must be a new hard keyword, so the grammar cannot grow a statement without a breaking change. Braces
   already have six meanings: handler blocks, view-alternative blocks, named atoms, record literals, set literals,
   map literals, plus a lattice-lifted `{v}`.
3. **Rule identity comes from source position** (line 2435). Unlabelled handlers are numbered by position, and
   seeded choice-site ids come from those numbers. A formatter or refactoring tool that reorders handlers changes
   replay behavior. That rules out a whole class of safe tooling.
4. **Diagnostics point at rules the user never wrote** (line 2428). `outer`, `any` and nested `if` multiply one
   handler into `h.3a`, `h.3b`, …. Provenance, coverage and error spans have to map back through that expansion.
5. **The `on`/`while` event classification is global** (line 2424). An edit in one file can make a handler in
   another file an error. For an LSP that means non-local invalidation and errors far from the edit.

**C**
1. **Declarations have no terminator.** `RelDecl` (line 282), `ConstDecl` (line 251), `TimerDecl`, `ImportDecl`
   and `ProgramDecl` end wherever the next token cannot continue them. `RelDecl` ends in an open loop of
   contextual clauses (`ttl`, `max`, `accept`, `partition`, `delivery`, `resolve`, `handler`, `reserved`, …,
   lines 291–302). A rule that directly follows a declaration and whose head starts with one of those words is
   misparsed:
   ```
   scratch accept(ballot: u64, val: bytes)
   accept(b, v) :- ...            // parsed as RelClause `accept from …` → "expected `from`"
   ```
   `accept` is a natural relation name in Paxos. `max(x) :- …` cannot be told apart at all, because
   `'max' Expr` accepts `(x)`. Every contextual clause keyword added later can change how existing programs
   parse. This is the worst evolvability hazard in any of the four proposals.
2. **The Prolog end-dot (lines 169–172) is whitespace-sensitive tokenization.** A `.` followed by a newline is
   `END`. While typing `votes[t].` at the end of a line to get member completion, the lexer sees a rule
   terminator, so dot-triggered completion needs a special case. A formatter may never put a line break after a
   member-access dot. `1.e5` is not a float. The self-critique (item 5) mentions the confusing errors but not the
   LSP cost.
3. **`at` is ambiguous between rule level and atom level.** `Rule = … [':-' Body] ['at' Expr] END` (line 311) and
   `AtomSuffix = … | 'at' Expr` (line 334). In `p(x) :- q(x) at 3.` the `at 3` can attach to `q(x)` or to the rule.
4. **Grouping delimiters are inconsistent.** Disjunction uses parentheses with `or`, conjunction inside
   parentheses must be `and` because `,` builds a tuple, and negated conjunction uses braces `not { … }`. Three
   delimiters for one concept (a sub-conjunction) will produce "tuple of atoms" errors (weakness 17).
5. **Polarity of `<=`/`>=` on lattices depends on the type** (line 2879). Editors cannot color those comparisons
   without full type information, and changing a column from `LMax` to `LMin` silently flips the polarity of every
   comparison on it.

**D**
1. **`let` has two jobs.** It is a view statement (`view_stmt`, line 309) and a query clause (line 321). D's own
   examples start a line with the clause form inside a query (lines 693, 780). The recovery rule in lines
   394–397, which inserts a missing `;` before a statement-initial keyword at the start of a line, would fire on
   those clauses. Fix: spell views `view name = …;`.
2. **`match e {` meets `qname {` typed records** (lines 350, 367). `match p.role { Leader => … }` parses
   `p.role { … }` as a record literal. Rust's no-struct-in-scrutinee restriction (which A adopts) is missing.
3. **`send r to g.cand from g in grant`** (line 1474) uses the binder `g` before `from` introduces it. That
   undoes D's binder-first property for the destination expression, and completion at `to g.` has nothing to
   offer. Fix: take the destination from the `@` field of the `select` record (D already allows that), and drop
   `to` or move it after `select`.
4. **`in` has three roles** (binder, membership, ring interval) and uses the same mismatched `(a, b]` as A.
5. **`choose!` infers its chosen columns from liveness** (line 2166). Editing `select` changes which columns are
   chosen, which changes the seeded priority and therefore replay. For tooling that means a formatter-safe edit
   (adding a projected field) is not a safe refactor.

---

## 4. Per-proposal judgment

### D: query / comprehension (7.5)

**Strengths**
- **Every clause starts with a reserved word** (`from join where let unnest group choose! select`), so the parser
  is LL(1) at clause level and error recovery works at clause granularity: one bad `where` loses only that clause.
  None of the other proposals recovers at a finer grain.
- **The canonical format is obvious.** One clause per line, indented under the sink, as in SQL and PRQL. A no-options
  formatter falls out of the grammar.
- **Best completion order.** The sink names the target relation first, so field names in the final `select {…}`
  can be completed from its schema. Binders (`from p in put`) come before use, so `p.` completes fields. LINQ put
  `from` first for exactly this reason. A and C put the head first, so the head's variables are unknown when the
  user types them.
- **Rules survive schema evolution.** Rows are records and fields are read by name (`p.key`), so adding a
  defaulted `since N` column breaks no rule body and no `select` that omits it. For a language with LANG-261
  field numbers and `since`, this is the most important evolvability property at the source level, and only D has
  it by default.
- **Keywords are legal as field names** (line 152): after `.`, `?.`, `$`, and before `:`. This removes most of the
  cost of a large keyword list. C does it only after `.`, A needs `r#`, and B has no escape.
- **`rule name:` is an explicit label keyword**, so labels are LL(1), unlike `ident :` in B and C or the `'`
  token in A.
- **The bang check is asymmetric in a useful way.** A missing bang is an error with a fix-it, and a superfluous
  one is only a warning (line 45). When a library method is reclassified from non-monotone to monotone, callers
  keep compiling. A's error-in-both-directions rule breaks every caller.
- **`map[…]` is a keyword-introduced map literal**, so braces keep one meaning in expressions (records).
- **Implicit columns are a separate lexical class** (`$sender`, `$principal`), so they cannot collide with user
  fields.

**Weaknesses**
- **The largest reserved-word list** (about 119, plus 34 bang words), including common nouns: `pre`, `post`,
  `old`, `values`, `record`, `rule`, `to`, `on`, `at`, `once`, `times`, `ticks`, `select`, `plain`. The
  keyword-as-field rule helps with columns, but relation names and binders still collide (`post` is a common
  relation name).
- **Verbose.** `from m in vote_reply, r in my_role where v.term == r.term …` is longer than
  `vote_reply(t, …), my_role(r, t)`. Verbosity is not a grammar defect, but long rules put more pressure on the
  formatter's wrapping rules.
- **SQL-shaped syntax with set semantics.** `select` deduplicates and `count()` counts distinct valuations (weakness
  3). A SQL user will get this wrong, and the syntax invites it.
- **Two aggregation forms** (`group! by … select` and correlated `count!(…) default`) with different empty-group
  semantics. The formatter cannot convert between them.
- **`delete! next r` and `upsert! next r`** make `next` mandatory and redundant. It is noise the parser has to
  require.
- **BANG_IDENT is adjacency-sensitive** (`x ! y` is a lexical error). This is easy in tree-sitter
  (`token.immediate`) but gives surprising errors after a copy-paste reflow.
- Defects D1–D5 from §3.

### A: Rust-flavored relational (7.0)

**Strengths**
- **Every item starts with a keyword** and every item and rule ends in `;` or `}`. Recovery synchronizes on `;`,
  `}`, item keywords and `,` at body depth 0, and every error carries the expected-token set (line 423). That
  meets TEST-091 directly.
- **`#[attr]` is the right extension mechanism.** Storage options (`#[durable]`, `#[soft(ttl=…)]`), keys, field
  tags, `since`, `reserved`, `deprecated`, resolve policies, ACLs and fault models are all attributes with one
  grammar (`Attr = Path [(args) | = Expr]`, line 238). A new modifier needs no grammar change, no new keyword and
  no formatter change, and the LSP can complete attribute names from a registry. Rust has used this mechanism the
  same way for ten years. It is the best evolvability tool in the four proposals.
- **Rust's precedence table, unchanged.** No new precedence to learn, and existing Rust tree-sitter and rustfmt
  conventions carry over. A also inherits Rust's no-struct-in-scrutinee restriction.
- **Raw identifiers (`r#next`)** give a principled escape for keyword collisions.
- **Every rule has the same shape**: `[kind] head <- body;`. The kind keyword is the first token, and multi-head
  rules and whole-relation copies are regular.
- **Bang identifiers** make non-monotone operators visible without type information, so a tree-sitter highlighter
  can color points of order.
- **`#` handling is consistent within the proposal.** `#` comments coexist with `#[` attributes, and field numbers
  are `#[tag(n)]`, so there is no `#1` versus `# 1` rule.

**Weaknesses**
- **The rule order is head-first.** Variables appear in the head before the body binds them, which weakens
  completion and gives inference-order diagnostics ("unbound in head"). Hover mitigates this but completion does
  not recover.
- **Positional atoms break when a column is added**, and named atoms need `..` (Rust pattern rules). A migration
  that adds a defaulted column forces edits to every positional use. A tool could rewrite them, but D needs no
  rewrite.
- **Overloaded symbols.** `!` means boolean not, atom negation and the bang marker. `|` means bit-or, lattice join,
  pattern alternation and closure delimiter. `..` means range, rest pattern and spread. Every error message about
  these symbols has to guess which meaning was intended.
- **`'label:` tokens.** A lone `'` confuses naive highlighters and auto-pairing (Rust lifetimes have the same
  problem). Tree-sitter handles it, but GitHub's fallback highlighting and simple editors do not.
- **`name!(…)` looks like a macro and is not one**, and it has its own argument grammar (`per`, `by`, `default`,
  `sticky`, `over`). That surprises the Rust audience A is aimed at (line 2809).
- **Repeated variables join.** In Rust a repeated binding is an error or a shadow, so a Rust reader brings the wrong
  intuition (line 2798).
- **Attribute noise on evolved schemas**: `#[key] #[tag(1)] key: String, #[tag(3), since(7)] ver: u64 = 0`.
- Defects A1–A3 from §3.

### C: modern Datalog (5.5)

**Strengths**
- **Identifier case is classified by the lexer** (`lower`, `Upper`, `CONST`, lines 99–115). A lowercase name in a
  pattern is always a variable, and a constant can never be mistaken for a binding. This is decided before name
  resolution, so tree-sitter can color variables, constants and types correctly with no semantic pass, and parse
  errors can say "this is a variable" before resolution runs. It is the strongest purely lexical idea in the four
  proposals.
- **`#` has one meaning** (field numbers). `#` comments live only in the `.ded` frontend, which is where LANG-208's
  need actually comes from (deviation D1). One sigil, one meaning, no lexer rule.
- **The smallest rule core**: `[kind] head :- body.`. Datalog, Soufflé and Molly readers can read it with no
  surprise at all.
- **Keywords are allowed as member names after `.`** (line 137).
- **Named mode is strict**: a bare identifier must be a field, and omitted fields need `..`. Typos fail instead of
  silently widening the match. This gives good diagnostics, though it hurts evolution (below).
- **Aggregates are an open family** (`name{ e | body }`). A user-defined aggregate needs no grammar change.
- **The whole grammar is LL(2) with no backtracking.**

**Weaknesses**
- **The end-dot** (defect C2) hurts completion, formatting and error messages. It is the only whitespace-sensitive
  token rule in any of the four proposals.
- **Unterminated declarations with open trailing clauses** (defect C1) make item boundaries depend on a
  contextual-keyword vocabulary that will grow.
- **Head-first rule order**, with the same completion problem as A.
- **Positional atoms and strict named atoms both break when a column is added.** C's strictness is good for typos
  and bad for evolution, and the proposal does not address the conflict.
- **Case carries meaning** (weakness 4). Acronyms lex wrong (`IO`, `ID` become `CONST`), and renaming a constant to
  a type changes its token class. Newcomers from Rust or Go will not expect case to be syntax.
- **Surprise for mainstream programmers**: `:-`, end-dot, `and` inside parentheses versus `,` outside, and
  `\/`/`⊔` as a token.
- **Two aggregation syntaxes** (`|> group by` and `agg{…}`) (weakness 1).
- Defects C3–C5 from §3.

### B: reactive and choreographic blocks (5.0)

**Strengths**
- **The most block-structured of the four**: every statement is `;`-terminated and every block `}`-closed, so one
  bad statement cannot swallow a module (lines 357–360). Handlers are natural fold regions and outline entries.
- **Body before heads.** `on body { emit …; send …; }` binds variables before the consequences use them, which is
  good for completion (as in D).
- **Verbs lead every statement** (`emit next send delete upsert`), so the rule kind is the first token, and
  greppable.
- **Closed `view` definitions.** Every rule of a view is in one place, so go-to-definition returns one site and
  the formatter has one block to lay out.
- **The `(key) -> (vals)` schema split** puts the key visibly into the declaration grammar, not into an attribute.
- **`monotone` is a checkable modifier** on a view, handler or module, so a tool can enforce CALM claims inside a
  region.

**Weaknesses**
- **The most dangerous newcomer surprise of the four.** Handlers read as sequential per-message code and are
  set-at-a-time per tick (weakness 1, "the angle's central trade-off"). `emit` versus `next` into state that the
  same handler negates is a silent bug (weakness 2). The syntax reads right and means something else.
- **The largest hard-keyword set that has no escape** (73 hard keywords, about 75 contextual), and the authors'
  own examples break it (defect B1).
- **Braces have six or seven meanings**, and the disambiguation depends on a closed statement vocabulary (defect
  B2). This is an evolvability tax on every future statement form.
- **Position-derived rule ids** (defect B3), so the formatter and refactorings cannot reorder safely.
- **Hidden rule explosion** (defect B4) and **non-local classification errors** (defect B5) make diagnostics and
  incremental LSP analysis harder.
- **The ternary `? :`** reuses `:`, which already marks labels, types and map entries.
- **`#` versus `#12`** is the same lexer rule as D's.
- **Reopenable `at Role` sections** scatter a role's state (weakness 5). The proposal asks the formatter to offer
  two groupings, which rules out a canonical format.

---

## 5. Evolvability in detail

**New operators and modifiers.** Ranked: A (attributes, no grammar change) > D (keyword clauses, cheap once
editions exist) > C (open aggregate family, but trailing contextual clauses are fragile) > B (every new statement
must be a hard keyword because of the brace rule).

**Schema evolution at the source level (LANG-261 `since`).** Ranked: D (record binders) > B (named atoms open by
default, but positional atoms break) > A (positional atoms break, named need `..`) > C (both forms break by
design). The final design should make named or record access the default for any relation whose schema is
versioned (durable relations, channels and interfaces), and allow positional atoms only for short, unversioned
`scratch` relations.

**Versioning syntax.** All four have `program X version N`, field numbers, `since`, `reserved`, `deprecated`,
`semantics_changed`, `migrate from N` and per-channel translation. Differences:
- Field-number spelling: A uses `#[tag(n)]` (verbose and uniform). B uses a prefix `#1 key: T`. C and D use a
  postfix `key: T #1`. The postfix form keeps the name first, which aligns columns and makes names easier to
  grep. Use it, with `#` meaning nothing else in `.bls` (C's choice).
- Translation blocks: A and D use `emit c to N` / `accept c from N` (matching LANG-263's spelling). B and C use
  `translate c to/from N`. `translate` is one keyword instead of two, and it frees `emit`/`accept` for other uses
  (B already uses `emit` as a verb, and `accept from` is the ACL clause in all four). Use `translate`.

**Language-level evolution.** None of the four proposals has a mechanism for adding keywords after 1.0. The
final design needs one: an **edition** in the program header (`program kv version 3 edition 2026;`, defaulting to
the compiler's current edition, with the edition recorded in `schema.lock`). New reserved words land only in a new
edition, and an automatic `blossom fix --edition` rewrites collisions to raw identifiers.

---

## 6. Ideas to take into the final design

Each entry names its source and what it fixes.

**Grammar skeleton**
1. **(A, B, D) `;`-terminated items and rules, `}`-closed blocks, and no whitespace-significant tokens.** Reject
   C's end-dot and C's unterminated declarations.
2. **(D) Keyword-led clauses and binder-first order in rule bodies**, or at minimum a body-first form. If the final
   design keeps a Datalog-style `head <- body` core for brevity, it should also accept the clause form, and the
   LSP should complete head variables from the body (the parser already builds both).
3. **(D) `rule name:` as the label syntax** (LL(1)). Reject `'name:` (A) and `ident :` (B, C).
4. **(A) `#[attr]` as the single open-ended home for modifiers**: storage options, channel fault models, partition
   hints, resolve policies, `nondet("why")`, `unsafe_ungated("why")`, `trusted`, and evolution metadata other than
   the field number. Keywords are reserved for things that change the shape of the grammar (rule kinds, clause
   heads, item kinds). Also accept keywords inside attribute paths (fixes A2).
5. **(A) Rust's precedence table, unchanged, and Rust's no-struct-in-scrutinee restriction** (fixes D2).
6. **(D) `map[…]` for map literals**, so braces mean only blocks and records. Reject B's set and map brace
   literals, and use `set[…]` if a set literal is needed.

**Lexical**
7. **(C) `#` means field number only in `.bls`.** `#` comments stay in the `.ded` frontend (LANG-208 is satisfied
   there). This removes the `#1` versus `# 1` lexer rule in B and D.
8. **(D) Reserved words are legal wherever a field name is expected**, and **(A) raw identifiers `r#kw`**
   everywhere else. Together they end the keyword-collision problem (fixes B1).
9. **(C, as a lint) Case conventions for binders, constants and types**, enforced as warnings, with one hard
   rule: a `const` must be `SCREAMING_CASE`, so a lowercase pattern identifier is always a binding (A's version).
   This keeps most of C's lexical-coloring benefit without the acronym trap.
10. **Intervals without mismatched brackets.** Use range operators such as `a <..= b` and `a ..< b` (open or
    closed at each end), wrapping modulo the `Ring` type (affects A, B, C and D alike).
11. **(A, D) Bang identifiers `name!(`**, if the final design keeps bangs, lexed as one token only when the `!` is
    adjacent and not followed by `=`. Use D's asymmetric check: a missing bang is an error with a fix-it, a
    superfluous bang is a warning.

**Schema evolution and identity**
12. **(D) Name- or record-based access by default for versioned relations**, and **(B) open named atoms** (omitted
    fields are wildcards) together with **(C) strict field-name checking** (unknown field names are errors). Adding
    a defaulted column must not break any existing rule.
13. **(B) The key/value split in the schema** (`(key) -> (vals)` or D's `=>`), as grammar, not attributes. It is
    the one piece of schema metadata every reader needs.
14. **Stable rule identity.** A seeded choice site, and any rule whose id reaches provenance, must carry an
    explicit label; an unlabelled choice site is a compile error, not a lint. Ids are `module::label`, never
    ordinals, and the formatter never reorders rules or items. This fixes B3, and the same gap exists in A (line
    750) and D (ordinals inside a rule).
15. **(D, amended) `choose!` names its chosen columns explicitly** (`choose! per (k) of (v)`). Fixes D5.
16. **`translate c to/from N`** (B, C) for cross-version channel translation. Postfix `#n` field numbers (C, D).
17. **An `edition` in the program header** (new; none of the four has it).

**Tooling deliverables the grammar should be designed for from day one**
18. **A lossless concrete syntax tree** (rowan-style, with trivia kept) shared by the compiler, the formatter and
    the LSP. Recovery nodes (`ERROR`, `MISSING`) must be first-class so the LSP works on broken code.
19. **One canonical formatter with no options** (gofmt/rustfmt-style), specified alongside the grammar. D's
    clause-per-line layout is the model for rule bodies.
20. **A tree-sitter grammar kept in the repository** and tested against the same conformance corpus as the
    recursive-descent parser. Any construct tree-sitter cannot express without an external scanner is a grammar
    smell. Today that would flag C's end-dot and B's brace disambiguation.
21. **Stable diagnostic codes** (`BLS0123`) with spans, expected-token sets (A), fix-its (D), and a
    `blossom explain` entry per code (TEST-091).
22. **Keep atom/guard/function classification in the resolver**, not the parser (all four agree). The parser stays
    context-free, and the resolver can say "`kv` is a relation, used here as a function" (B §2.4).
23. **Semantic tokens for points of order** (TEST-092) computed from the IR, so non-monotone sites are highlighted
    even where the surface marks them only by type (C5).

---

## 7. Verdict

Under the grammar and tooling lens, **D** is the strongest base. Its keyword-led clause structure gives
clause-level error recovery, an obvious canonical format, the best completion order, and rules that keep working
when a relation gains a column. **A** is a close second, and its `#[attr]` mechanism is the best tool in any
proposal for growing the language without touching the grammar. It should be adopted whichever surface wins. **C**
has the best lexical idea (identifier classes decide roles) and the two worst structural choices (the end-dot and
unterminated declarations). **B** has excellent block structure, but its brace overloading, keyword explosion,
position-derived rule ids and sequential-looking semantics make it the weakest base for tooling.

The strongest combination for tooling is D's clause bodies and record binders, A's attributes, raw identifiers,
precedence table and `;` discipline, C's single-meaning `#` and case lint, and B's `;`/`}` handler blocks and
closed `view`s, plus the three things none of them has: editions, bracket-safe intervals, and label-only rule
identity.
