# Blossom Language Reference

Status: **normative**. Version 1 of the surface language, edition 2026. Date: 2026-09-27.

This document defines the Blossom surface language: its lexical structure, its complete grammar, its type system,
the meaning of every construct as an exact lowering to the Dedalus^L core IR, and the static rules and
diagnostics a conforming compiler implements. It is the input to the parser, the resolver, the type checker, the
lowering pass, the formatter and the language server.

Normative inputs, in order of precedence: `docs/DECISIONS.md`; `docs/research/FEATURES.md` §1 (the CR-xx
resolutions) and §3 (SEM); then the LANG, ANA, TEST and VER items of FEATURES.md. Nothing here overrides a CR-xx.
Where this document refines a LANG item (for example LANG-190's timing of `<+` in bootstrap), §22.3 lists the
refinement and DECISIONS.md records it.

The record of how this design was chosen is kept in `docs/design/syntax/`: four proposals (A: Rust-flavored
relational, B: reactive and choreographic blocks, C: modernized Datalog, D: query/comprehension) and three
judgments (ergonomics, semantics, tooling). §1.1 summarizes the decision and §22 explains every rejected
alternative. The example programs E1–E10 are in `examples/` and conform to this document exactly; they are
parser, type checker and end-to-end tests.

Contents

1. The decision and the design principles
2. Lexical structure
3. Grammar
4. The core IR and lowering conventions
5. Types
6. Programs, files, modules and choreographies
7. Relation declarations
8. Rules: handlers, statements, views, bootstrap and facts
9. Rule bodies
10. Aggregation, choice, numbering and folds
11. Lattices, weighted collections and groups
12. The operator × collection legality matrix
13. Stratification, time, monotonicity and determinism as the user sees them
14. Distribution: channels, roles, partitioning, seals, finality
15. Time, timers and randomness
16. Functions and host interop
17. Invariants, specs and verification
18. Security: principals, senders, ACLs, sessions
19. Versions, schema evolution and migrations
20. Diagnostics
21. Compatibility frontends
22. Rejected alternatives and refinements of FEATURES.md
- Appendix A. LANG coverage
- Appendix B. The built-in library
- Appendix C. Reserved words

Conventions. "Must", "must not", "is an error" and "is rejected" are requirements on programs and compilers. Code
in `blossom` blocks is surface syntax; a block that contains `…` or starts with a statement, a literal or a
schematic placeholder is a fragment of a larger item, and every complete block parses as written. Code in `ir`
blocks is the Dedalus^L IR of §4. Diagnostic codes `BLSnnnn` are defined in §20. `Rnn §x` refers to section `x` of
research report `nn`, not to this document.

---

## 1. The decision and the design principles

### 1.1 The decision

**Base design: proposal B's reactive/choreographic skeleton, rebuilt on proposal A's typed foundations.**

A Blossom program is written as *handlers*: `on EVENT { consequences }` and `while CONDITION { consequences }`,
where every consequence is a statement that starts with a verb naming its Dedalus rule kind (`emit`, `next`,
`send`, `delete`, `upsert`, `seal`). Derived, tick-local relations are closed `view`s with inferred schemas.
Multi-role protocols are written in one module, in message order, with reopenable `at Role { … }` sections.
That skeleton is B's, which the ergonomics judge chose (8.0): it groups every consequence of one message under one
body, which is how Raft, 2PC, an HDFS namenode or a MapReduce scheduler are specified in their papers, and it is
the only one of the four proposals that makes Dedalus's re-derivation of rules over persistent state (ODD-05)
visible in the syntax, through `on` (edge-triggered) versus `while` (level-triggered).

Under that skeleton sits proposal A's foundation, which the semantics judge chose (8.5): Rust-shaped items,
declarations, generics, types and expressions; `#[attributes]` as the one open-ended home for metadata; a
compiler-enforced bang (`name!(…)`, `.name!(…)`) on every non-monotone *operator*; declared producers and
unanimity for seals; soft state with a read-time TTL; banged Z-set views; the `quorum`/`sent` spec vocabulary; and
the choreography-as-one-program encoding for the verifiers.

The rebuild removes every defect the three judges found in B:

| Defect found in B | Resolution here |
|---|---|
| Handler bodies were assembled from enclosing `if`/`for`/`let`; `outer` and `any` multiplied statements into rules the user never wrote (`h.3a`) | The header of a handler lowers to **one** materialized, provenance-transparent relation `H$when`; every statement is exactly one IR rule reading it (§8.1). `outer` and `any` expand inside `H$when`, never per statement. |
| `let` statements made handlers read like sequential code | There are no `let` statements. `let` is a body literal and belongs to the header (§8.2, BLS0102). |
| `emit` versus `next` into a relation the same handler negates was a silent bug | It is an error (BLS0506), with a fix-it to `next`. |
| Rule identities came from source positions, so reordering changed seeds and replay | Identities are labels or content hashes, never ordinals. Every seeded site must sit under a user label (BLS0600, §4.3). |
| Braces had six or seven meanings | Braces delimit blocks, struct literals and keyword-introduced groups only. Set and map literals are `set[…]` and `map[k => v]`. Struct literals are forbidden where a block may follow (Rust's rule, §3.4). |
| A large escape-free keyword list that its own examples broke (`at`) | 52 hard keywords; everything else is contextual; keywords are legal wherever a field name is expected; raw identifiers `r#kw` everywhere else (§2.3). |
| `->` meant both "key/value split" and "role direction" | Keys are a `key(…)` clause; `->` means only direction. |
| Soft-state TTL checked only when carrying to t+1 (stale after idleness) | Visibility is filtered against the current tick's `now`; eviction happens in the tick the table exceeds `max` (§7.9). |
| Lattice `a.le(b)` and `==` were unmarked | Every non-monotone lattice operation needs a bang; `==` on lattice values is an error (§11.4). |
| No generic `threshold`, no clamped Z-set view | Both exist (§11.6, §11.10). |
| `next` in bootstrap landed one tick late while the text said tick 0 | `next` means t+1 everywhere; bootstrap initial state is written with `emit`, which holds in the boot tick (§8.4, §22.3). |

**Grafts from the runners-up.**

| From | Idea | Where |
|---|---|---|
| A | Rust items: `struct`, `enum`, `type`, `fn`, `impl`, `const`, `use`, `pub`, generic modules `M<P>` | §5, §6 |
| A | `#[attr]` for metadata, with keywords accepted inside attribute paths | §2.5, §3 |
| A | Rust's precedence table and its no-struct-in-scrutinee rule | §3.3, §3.4 |
| A | Raw identifiers `r#kw`; `const` names must be SCREAMING_CASE | §2.3, §2.6 |
| A | The bang discipline for non-monotone operators, enforced both ways | §11.4, §13.2 |
| A | Typed addresses `Node<Role>` | §5.3 |
| A | Seals with a declared producer set, unanimity by default, digest conflict and overflow violations | §14.4 |
| A | Soft state with a read-time TTL filter | §7.9 |
| A | Banged Z-set views `distinct!`, `clamped!`, `weights!` (ODD-06 amendment) | §11.10 |
| A | `quorum v in R { … }` in specs, the network as `sent` | §17.3 |
| A, C | Choreographies lower to one program with a `$role` guard per rule; projection is guard elimination | §6.10 |
| C | `where` separates guards from joins | §9.5 |
| C | `bootstrap fresh { }` versus `bootstrap { }`, and the `recovered()` input | §8.4 |
| C | Input-level seals, so punctuation flows from host sources through the pipeline | §14.4 |
| C | Relation-typed module parameters `peers: rel(n: Node)` | §6.4 |
| C | `max!` (relational aggregate) versus `lmax{…}` (monotone lattice fold) | §10.1, §11.7 |
| C | `stable fn … after t` for "monotone, then immutable" | §11.8 |
| C | `prove … by induction using …`, fault models `model: sync \| async`, `round` | §17 |
| C | Postfix field numbers `name: T #n`; strict field names in named atoms | §7.1, §9.2 |
| B, C | A checked `monotone` modifier on views, handlers, modules and choreographies | §13.5 |
| B | Closed `view`s; reopenable `at Role { }`; channel direction `: A -> B` with inferred ACLs; role name as member set | §8.3, §6.10, §7.7 |
| B | Compiler-maintained seal digests with a producer send log, idempotent resend and "send after seal" | §14.4 |
| B | `interpose a.i as (outside, inside) { … }` | §6.9 |
| B | `final not r(…)`; delta relations inherit durability; `translate c to N` | §14.5, §9.10, §19 |
| B, C | `check ldfi \| bmc \| smt \| sim \| asp expect holds \| fails` | §17.5 |
| D | Inferred-schema views (no declaration tax) | §8.3 |
| D | `forall D { C }`, monotone exactly when its domain is closed | §9.8 |
| D | `partition by e` on a channel, so a shuffle `send` names no destination | §14.3 |
| D | Reserved words legal wherever a field name is expected | §2.3 |
| D | Asymmetric bang check: missing is an error with a fix-it, superfluous is a warning | §11.4 |
| D | `expect confluent(out)` certificate assertions | §17.2 |
| D | Invariant messages (`invariant name "message": never …`) | §17.1 |
| tooling judge | Editions, bracket-safe intervals (`a<..=b`), label-only rule identity, a lossless CST and stable diagnostic codes | §3.5, §9.4, §4.3, §20 |
| semantics judge | Fills for gaps no proposal covered: `#[replicated]` (Blazes Rep, ANA-041), provenance-transparent generated relations, `#[deterministic]` outputs | §14.1, §4.1, §13.6 |

### 1.2 Design principles

1. **A handler is a rule body with several heads, and nothing more.** `on B { S1; S2; }` means "for every
   valuation of `B` in this tick, each `Si` holds with its own timing". Statements are an unordered set of
   consequences. There is no control flow, no mutation and no sequencing inside a block. The header is lowered once
   (§8.1); each statement is one IR rule.
2. **The verb is the time.** `emit` holds now, on this node. `next` holds at t+1. `send` arrives at a later tick
   at the destination. `delete` and `upsert` take effect at t+1. A reader never infers a rule kind from context.
3. **Edge and level look different.** `on` requires an event in its header and fires once per event. `while` fires
   at every tick in which its header holds, which is Dedalus's literal re-derivation (CR-26, ODD-05). A `send`
   under `while` is a periodic resend and reads like one.
4. **Every point of order is visible and checked.** Non-monotone *operators* carry a bang (`count!`, `choose!`,
   `reveal!`, `.leq!()`), enforced in both directions. Non-monotone *literals* are introduced by a closed list of
   keywords (`not`, `outer`, `inserted`, `deleted`, `forall` over an open domain) and the two deferred mutations
   (`delete`, `upsert`). `grep` over those finds every candidate point of order; the points-of-order report
   (ANA-022) is authoritative.
5. **The surface hides nothing from the analyzer.** Rule bodies contain no closures and no host code (LANG-002).
   Every construct has one exact lowering to the IR, and the IR is all the analyses, the simulator and the backends
   see (ENG-001).
6. **Declarations say the semantics in words; attributes say metadata.** Storage class, durability, keys,
   partitioning, seals and resolution policies are keywords and clauses. Fault models, ACLs, evolution metadata,
   plan hints and accepted-nondeterminism reasons are `#[attributes]`.
7. **Identity is stable under editing.** Rule and site ids come from labels and content hashes, never from source
   positions. The formatter never reorders items or statements.
8. **Tools first.** Every item starts with a keyword or a label; every statement ends in `;`; every block ends in
   `}`; there are no whitespace-significant tokens. The grammar is LL(1) except for a few fixed-lookahead decisions,
   all listed in §3.4. Classification of atoms, guards and function calls happens in the resolver, so the parser is
   context-free and works on incomplete code.

### 1.3 A short tour

```blossom
program kvs version 1;                                   // a program root (§6.1)

role Client: external;                                   // clients are sessions, not nodes (§18.4)
role Server;                                             // one process

channel put(id: u64, key: String, val: Bytes): Client -> Server;   // direction; ACL inferred (§7.7)
channel put_ok(id: u64): Server -> Client;

at Server {                                              // rules and state placed on Server (§6.10)
    durable table store(key: String, val: Bytes) key(key);          // WAL-logged, committed before sends

    apply: on put(id, key, val) from c, choose_most!((c, id, val) per key) {   // one winner per key per tick
        upsert store(key, val);                          // replaces the key's row at t+1
    }
    ack: on put(id, _, _) from c {
        send put_ok(id) to c;                            // released only after the tick's fsync (SEM-072)
    }
}
```

The handler `apply` lowers to two IR rules: the header, materialized once with its seeded choice, and the one
statement (§8.1). The handler `ack` is edge-triggered on the `put` message; had its header read only persistent
state, the compiler would have demanded `while` (BLS0504).

---

## 2. Lexical structure

### 2.1 Source files

- Source is UTF-8. The extension is `.bls`. Identifiers and keywords are ASCII; string literals and comments may
  contain any Unicode.
- A file is a module (§6.1). A file whose first item is `program NAME version N;` is a *program root*.
- Line terminators are LF or CRLF. Whitespace (space, tab, CR, LF, form feed) separates tokens and is otherwise
  insignificant. There is no layout rule and no whitespace-sensitive token.

### 2.2 Comments (LANG-208)

| Form | Meaning |
|---|---|
| `// …` to end of line | line comment |
| `/// …` | doc comment, attached to the next item, field, variant, statement or spec member |
| `//! …` | inner doc comment, attached to the enclosing file or item |
| `/* … */` | block comment; nests |
| `# …` to end of line | line comment, **unless** the `#` is immediately followed by `[` (attribute), `![` (inner attribute) or an ASCII digit (field number, §7.1) |

The `#` rule keeps Dedalus/Molly-style comments working in pasted code and makes `#!/usr/bin/env blossom` a
comment. `#1` is always a field number; write `# 1st` (with a space) for a comment that starts with a digit. The
formatter rewrites `#` comments to `//`.

### 2.3 Identifiers and keywords

```
Word        = [A-Za-z_][A-Za-z0-9_]*
IDENT       = Word that is not a hard keyword and is not "_"      |   "r#" Word   (raw identifier)
BANG_IDENT  = Word "!"   where the "!" is immediately followed by "(" or "{" (the bracket is a separate token)
              and Word is not a hard keyword (a hard keyword followed by "!(" is BLS0004)
```

**Hard keywords** (52) are never identifiers, except written raw (`r#next`) or where a field name is expected
(below):

```
as bootstrap channel choreography const delete else emit enum extern false fn for if impl import in include
input interpose invariant lattice let loopback match migrate module next not on output override param program
protocol pub scratch seal self send spec static struct table translate true type upsert use view where while
```

**Contextual keywords** are ordinary identifiers except in the grammatical position the grammar names them in
(written `'word'` in the grammar of §3). Examples: `role`, `at`, `cell`, `timer`, `fact`, `key`, `ttl`,
`partition`, `from`, `to`, `per`, `forall`, `outer`, `inserted`, `nodes`, `check`. Appendix C lists them all with
their positions. A variable may be called `key` or `at`; a relation may be called `deleted` or `role`.

**Keywords as field names.** Wherever the grammar expects a field name (`FieldName` in §3: a column or struct field
declaration, a named argument `name: e`, a struct-literal field, a member after `.`, a relation path segment), any
hard keyword is accepted as an ordinary name: `table started(at: Instant, table: u64)`, `r.output`,
`emit kv(type: t)`. Punning (`r(type)`) is not available for hard keywords; write `r(type: x)`.

**Raw identifiers.** `r#` followed by a word makes that word an identifier anywhere: `let r#match = 3`. The
edition mechanism (§3.5) rewrites new keywords that collide with existing names to raw identifiers.

**Reserved names.** Identifiers containing `$` cannot be written; the IR uses them for generated relations (§4.1).
The lattice fold names `lset lmax lmin lbool lmap lbag lpset` and the literal constructors `set` and `map` may not
be declared as types, relations or functions (BLS0201).

### 2.4 Literals

| Literal | Examples | Type |
|---|---|---|
| decimal integer | `42`, `1_000_000` | from context; otherwise `i64` |
| hex, binary integer | `0xff`, `0b1010`, `0x1_0000` | from context; otherwise `i64` |
| suffixed integer | `7u8`, `3i32`, `0u64`, `2u128` | the suffix |
| modular id (LANG-026) | `0x1f3aI` | `Mod<N>`, with `N` from context (error if none) |
| float | `1.5`, `2e-3`, `1.0f64`, `6.02e23` | `f64` |
| duration | `250us`, `100ms`, `2s`, `1.5s`, `5m`, `1h`, `3d`, `10ns` | `Duration` |
| string | `"a\n\"b\""`, `r"raw"`, `r#"raw with "quotes""#` | `String` |
| interpolated string | `f"{n} item{s} left"`, `f"{x:.2}"`, `f"{{literal braces}}"` | `String` |
| byte string | `b"abc"`, `br"raw"` | `Bytes` |
| boolean | `true`, `false` | `bool` |
| unit | `()` | `()` |

Rules:
- Escapes: `\n \r \t \\ \" \' \0 \u{HEX}`. Any other escape is BLS0005. A raw string ends at `"` followed by as
  many `#` as it began with.
- A number immediately followed by letters that are neither an integer suffix (`u8 … u128`, `i8 … i128`), `f64`,
  the modular suffix `I` (hex only) nor a duration unit (`ns us ms s m h d`) is BLS0003 ("unknown numeric suffix
  `kb`"), never an identifier.
- A `.` begins a float's fraction only when a digit follows it. `1..5` is `1`, `..`, `5`.
- After the token `.`, a run of digits is an integer (tuple index), so `p.1.0` is `p . 1 . 0`.
- **Interpolated strings** (S16, docs/design/SUGAR.md §1): `f"…"` holds `{expr}` holes, any expression; a `:` outside
  the hole's brackets starts its spec, of which `.N` (N digits after the point, for an `f64`) is the only one
  (BLS0435). `{{` and `}}` are literal braces, a lone `}` is BLS0005, and the escapes are a string's. The value is the
  text and the holes joined with `++`, each hole converted with `to_string` (with `.N`, `to_fixed(N)`), so a hole of
  a type without one is that method's error.
- There is no `null`: absent values are `None` (`Option<T>`, LANG-025).
- A string is accepted where a `Node` or `Principal` is expected only in `fact`s and `static` configuration inside
  specs and deployments; rules never build node names from strings.

### 2.5 Punctuation, operators, attributes, field numbers

```
#!["   #["   <..=   ..=   <..   ::   ..   ->   =>   ==   !=   <=   >=   <<   >>   &&   ||   **   ++
(  )  [  ]  {  }  ,  ;  :  .  @  =  <  >  +  -  *  /  %  &  |  ^  ~  !  _
```

Tokens are matched longest first. `!` appears only inside `BANG_IDENT`, in a bang method (`.name!(`) and in `!=`;
a lone `!` is BLS0100 (boolean negation is `not`). `_` is the wildcard pattern.

`FIELD_NUM` is `#` immediately followed by decimal digits (`#3`); it appears only after a column or struct field
type and after an enum variant (§7.1, §19.2).

An attribute is `#[path(args)]` or `#[path = expr]` before an item, field, variant, statement or spec member, and
`#![…]` at the top of a file. Attribute paths accept keywords (`#[accept(external)]`). Attributes carry metadata:
fault models, ACLs, evolution markers, plan hints, accepted-nondeterminism reasons, lint levels and host bindings.
An unknown attribute is BLS0210. Appendix C lists the built-in attributes.

### 2.6 Naming conventions

The convention is Rust's: `snake_case` for relations, variables, fields, functions, labels and instance aliases;
`CamelCase` for types, lattices, modules, choreographies, protocols, roles and enum variants; `SCREAMING_CASE` for
constants, parameters and spec node names. Violations are warnings (BLS1001), with one hard rule: a `const`, a
`param`, a module value parameter and a spec node name **must** be SCREAMING_CASE (BLS0211). A lowercase
identifier in a pattern position is therefore always a variable, and an uppercase one is always a constant, a
type, a role, a variant or a node name. A variable whose name starts with `_` (`_id`) is a named variable (it is
part of the valuation, §10.1) whose non-use is not warned about.

---

## 3. Grammar

### 3.1 Notation

The grammar is EBNF: `{ x }` is zero or more, `[ x ]` is optional, `( … )` groups, `|` separates alternatives,
`(* … *)` is a comment. `"x"` is a hard keyword or punctuation token. `'x'` is a **contextual keyword**: an `IDENT`
token spelled `x` (not written raw), recognized only at that position. Upper-case names (`IDENT`, `BANG_IDENT`,
`INT_LIT`, `FLOAT_LIT`, `DURATION_LIT`, `STRING_LIT`, `BYTES_LIT`, `FIELD_NUM`) are tokens from §2.

`Body⁰`, `Expr⁰` and `AtomLit⁰` are the same productions parsed in a **no-struct context** (§3.4 rule 4).
`Expr≥n` is an expression parsed at precedence level `n` or tighter (§3.3). A trailing `,` is accepted in every
comma-separated list that is closed by a bracket.

### 3.2 The complete grammar

```ebnf
(* ======================================================================== files *)
File            = { InnerAttr } [ ProgramHeader ] { Item } EOF ;
ProgramHeader   = "program" IDENT 'version' INT_LIT [ 'edition' INT_LIT ] ";" ;

(* ======================================================================== attributes *)
InnerAttr       = "#![" AttrBody "]" ;
OuterAttrs      = { "#[" AttrBody { "," AttrBody } [ "," ] "]" } ;
AttrBody        = AttrPath [ "(" [ AttrArg { "," AttrArg } [ "," ] ] ")" | "=" Expr ] ;
AttrPath        = AnyWord { "::" AnyWord } ;
AttrArg         = AnyWord "=" Expr | Expr ;
AnyWord         = IDENT | HardKeyword ;
FieldName       = IDENT | HardKeyword ;

(* ======================================================================== items *)
Item            = OuterAttrs [ "pub" ] ItemKind ;
ItemKind        = UseItem | ImportItem | IncludeItem | ConstItem | ParamItem
                | TypeAlias | StructItem | EnumItem | FnItem | ExternItem | ImplItem
                | LatticeTypeItem | AggregateItem | ServiceItem
                | ModuleItem | ProtocolItem | RoleItem | AtSection
                | RelDecl | CellDecl | TimerDecl | ViewDecl | HandlerItem
                | BootstrapItem | FactItem | InvariantItem
                | InterposeItem | BlockItem | OverrideItem | AclItem
                | SnapshotItem | MigrateItem | TranslateItem | SpecItem | TreeItem ;
TreeItem        = 'tree' IDENT "{" { IDENT RelPath "(" IDENT { "," IDENT } ")" ";" } "}" ;  (* §8.2 *)
FragmentItem    = 'fragment' IDENT "(" [ IDENT ":" Type { "," IDENT ":" Type } ] ")" Children ;  (* §8.2 *)

UseItem         = "use" UseTree ";" ;
UseTree         = IDENT { "::" IDENT }
                  [ "::" "*" | "::" "{" [ UseTree { "," UseTree } [ "," ] ] "}" | "as" IDENT ] ;
ImportItem      = "import" SimplePath [ GenericArgs ]
                  [ "(" [ NamedArg { "," NamedArg } [ "," ] ] ")" ]
                  "as" IDENT [ 'with' "(" RoleBind { "," RoleBind } [ "," ] ")" ] ";" ;
NamedArg        = IDENT "=" Expr ;
RoleBind        = IDENT "=" IDENT ;                                   (* template role = importer role *)
IncludeItem     = "include" ( SimplePath | STRING_LIT ) ";" ;
SimplePath      = IDENT { "::" IDENT } ;
ConstItem       = "const" IDENT ":" Type "=" Expr ";" ;
ParamItem       = "param" IDENT ":" Type [ "=" Expr ] ";" ;

(* ======================================================================== types and values *)
TypeAlias       = "type" IDENT [ Generics ] "=" Type ";" ;
StructItem      = "struct" IDENT [ Generics ]
                  ( "{" [ FieldDecl { "," FieldDecl } [ "," ] ] "}"
                  | "(" [ Type { "," Type } [ "," ] ] ")" ";" ) ;
FieldDecl       = OuterAttrs FieldName ":" Type [ FIELD_NUM ] [ "=" Expr ] ;
EnumItem        = "enum" IDENT [ Generics ] "{" [ Variant { "," Variant } [ "," ] ] "}" ;
Variant         = OuterAttrs IDENT
                  [ "(" [ Type { "," Type } [ "," ] ] ")" | "{" [ FieldDecl { "," FieldDecl } [ "," ] ] "}" ]
                  [ FIELD_NUM ] ;
Generics        = "<" [ GenericParam { "," GenericParam } [ "," ] ] ">" ;
GenericParam    = IDENT [ ":" Type { "+" Type } ] [ "=" Type ] ;
GenericArgs     = "<" [ GenericArg { "," GenericArg } [ "," ] ] ">" ;
GenericArg      = IDENT "=" Type | INT_LIT | Type ;
Type            = 'unsafe' Type                                       (* LANG-136: DomPair only *)
                | "(" [ Type { "," Type } [ "," ] ] ")"                (* tuple; () is unit *)
                | "fn" "(" [ Type { "," Type } [ "," ] ] ")" "->" Type (* a function parameter's type, §16.1 *)
                | SimplePath [ GenericArgs ] ;

(* ======================================================================== functions *)
FormatItem      = 'format' IDENT [ "(" [ FormatParam { "," FormatParam } ] ")" ]
                  ( "=" Expr ";" | "{" [ FormatField { "," FormatField } [ "," ] ] "}" ) ;   (* §16.7 *)
FormatParam     = IDENT [ ":" Type ] ;
FormatField     = [ FieldName ":" ] Expr [ "if" Expr ] [ "=" Expr ] ;
FnItem          = [ FnClass ] FnSig BlockExpr ;
FnClass         = 'morphism' | 'bimorphism' | 'monotone' | 'antitone' | 'threshold' | 'stable' ;
FnSig           = "fn" IDENT [ Generics ] "(" [ FnParam { "," FnParam } [ "," ] ] ")" "->" Type
                  [ 'after' IDENT ] ;                                 (* `after` only with 'stable' *)
FnParam         = "self" | Expr⁰ ":" Type ;                            (* the expression is a pattern *)
BlockExpr       = "{" { "let" Expr [ ":" Type ] "=" Expr ";" } Expr "}" ;
ExternItem      = "extern"
                  ( [ FnClass ] FnSig "=" STRING_LIT ";"
                  | "table" "fn" IDENT ParamList "->" ParamList "=" STRING_LIT ";"
                  | "type" IDENT [ Generics ] "=" STRING_LIT ";"
                  | "lattice" IDENT [ Generics ] "=" STRING_LIT
                    ( ";" | "{" { OuterAttrs [ FnClass ] FnSig ";" } "}" ) ) ;
ParamList       = "(" [ Param { "," Param } [ "," ] ] ")" ;
Param           = FieldName ":" Type ;
ImplItem        = "impl" [ Generics ] Type [ "for" Type ] "{" { OuterAttrs FnItem } "}" ;
LatticeTypeItem = "lattice" IDENT [ Generics ]
                  ( "=" Type ";" | "{" [ FieldDecl { "," FieldDecl } [ "," ] ] "}" ) ;
AggregateItem   = 'aggregate' IDENT [ Generics ] ParamList "->" Type "{" { AggMember } "}" ;
AggMember       = "type" 'State' "=" Type ";" | IDENT "=" Expr ";" ;  (* init, step, combine, finish *)
ServiceItem     = 'service' IDENT ParamList "->" ParamList ";" ;

(* ======================================================================== modules and roles *)
ModuleItem      = [ 'monotone' ] ( "module" | "choreography" ) IDENT [ Generics ] [ ModParams ]
                  [ ":" Type { "+" Type } ] "{" { Item } "}" ;
ModParams       = "(" [ ModParam { "," ModParam } [ "," ] ] ")" ;
ModParam        = IDENT ":" ( 'rel' ParamList | Type [ "=" Expr ] ) ;
ProtocolItem    = "protocol" IDENT [ Generics ] [ ":" Type { "+" Type } ] "{" { Item } "}" ;
RoleItem        = 'role' IDENT [ ":" ( 'process' | 'cluster' | 'external' ) ] ";" ;
AtSection       = 'at' IDENT "{" { Item } "}" ;
InterposeItem   = "interpose" RelPath "as" "(" IDENT "," IDENT ")" "{" { Item } "}" ;
BlockItem       = 'block' IDENT "{" { Item } "}" ;
OverrideItem    = "override" ItemKind ;                   (* a labelled HandlerItem, a ViewDecl or a BlockItem *)
AclItem         = 'acl' RelPath 'accept' "(" [ Expr { "," Expr } [ "," ] ] ")" ";" ;

(* ======================================================================== relations *)
RelDecl         = { RelMod } RelKind IDENT
                  ( "(" [ ColDecl { "," ColDecl } [ "," ] ] ")" | 'like' RelPath )
                  { RelClause } ";" ;                     (* each clause kind at most once *)
RelMod          = 'durable' | 'soft' | 'sealed' | 'zset' | 'bag' | 'final' ;
RelKind         = "table" | "scratch" | "channel" | "input" | "output" | "static" | "loopback" ;
ColDecl         = OuterAttrs [ "@" ] FieldName ":" Type [ FIELD_NUM ] [ "=" Expr ] ;
RelClause       = ":" IDENT "->" IDENT                                (* channel direction *)
                | 'key' "(" [ FieldName { "," FieldName } [ "," ] ] ")"
                | 'ttl' Expr | 'max' Expr                             (* soft tables *)
                | 'range' "(" FieldName ")"                           (* range tables *)
                | 'resolve' Policy
                | 'partition' 'by' Expr [ 'over' RelPath ]
                | 'sealed' 'by' "(" [ FieldName { "," FieldName } [ "," ] ] ")" [ 'producers' RelPath ]
                | "while" Body                                        (* tables; last, §7.2 *)
                | 'exactly_once' "(" IDENT ")" ;
Policy          = ( 'choose' | 'choose_rand' ) [ 'sticky' ]
                | ( 'choose_least' | 'choose_most' ) "(" Expr ")"
                | 'prefer' "(" IDENT { "," IDENT } ")"                 (* tables, relation level *)
                | 'merge' ;
CellDecl        = { 'durable' | "scratch" } 'cell' IDENT ":" Type ";" ;
TimerDecl       = 'timer' IDENT ( 'every' Expr [ 'ticks' ] [ 'times' Expr ] | 'once' [ 'after' Expr ] ) ";" ;
RelPath         = FieldName { "." FieldName } ;

(* ======================================================================== rules *)
ViewDecl        = [ 'monotone' ] "view" IDENT "(" [ ViewCol { "," ViewCol } [ "," ] ] ")"
                  ( "=" Body ";" | "{" { Body ";" } "}" ) ;
ViewCol         = IDENT [ ":" Type ] [ "=" Expr ] ;                   (* the Expr is a head aggregate call *)
HandlerItem     = [ IDENT ":" ] [ 'monotone' ] ( "on" | "while" ) Body⁰ Block ;
Block           = "{" { Stmt } "}" ;
Stmt            = OuterAttrs ( VerbStmt | IfStmt | ForStmt | CallStmt ) ;
CallStmt        = Element ;                                           (* a fragment call, §8.2 *)
VerbStmt        = "emit" Target [ 'weight' Expr ] End
                | "next" Target [ 'weight' Expr ] End
                | "send" Target [ 'to' Expr ] End
                | "delete" Target End
                | "upsert" Target [ 'resolve' Policy ] End
                | "seal" Head [ 'to' Expr ] ";" ;
Target          = Head | RelPath Element ;                            (* the second: a tree statement, §8.2 *)
End             = ";" | Children ;                                    (* a tree statement's element ends itself *)
IfStmt          = "if" Body⁰ Block [ "else" ( IfStmt | Block ) ] ;
ForStmt         = "for" Body⁰ Block ;
Head            = RelPath "(" [ Arg { "," Arg } [ "," ] ] ")" ;
Element         = ElemName [ "[" [ Arg { "," Arg } ] "]" ] [ "(" [ Arg { "," Arg } [ "," ] ] ")" ] ( Children | ";" ) ;
ElemName        = IDENT { ( "-" | "." ) IDENT } ;                     (* `font-face`; `a.rel` for a child head *)
Children        = "{" { Element | ChildIf | ChildFor | VerbStmt | Expr [ ";" ] } "}" ;   (* the Expr: content *)
ChildIf         = "if" Body⁰ Children [ "else" ( ChildIf | Children ) ] ;
ChildFor        = "for" Body⁰ Children ;
BootstrapItem   = "bootstrap" [ 'fresh' ] Block ;
FactItem        = 'fact' Head [ "@" Expr ] [ 'from' Expr ] [ 'at' 'tick' Expr ] ";" ;
InvariantItem   = "invariant" IDENT [ STRING_LIT ] ":" 'never' Body ";" ;

(* ======================================================================== bodies *)
Body            = Literal { "," Literal } [ "where" Expr { "," Expr } ] ;
Literal         = "not" ( "{" Body "}" | Literal )
                | "let" Expr "=" Expr                                (* pattern = expression *)
                | 'outer' AtomLit
                | 'inserted' AtomLit | 'deleted' AtomLit
                | 'sealed' AtomLit
                | 'final' [ "not" ] AtomLit
                | 'per' AtomLit                                       (* views only *)
                | 'any' "{" Body { ";" Body } [ ";" ] "}"
                | 'forall' AtomLit⁰ "{" Body "}"
                | 'ever' AtomLit | 'sent' AtomLit                     (* spec bodies only *)
                | 'quorum' IDENT "in" RelPath "{" Body "}"            (* spec bodies only *)
                | AtomLit ;
AtomLit         = Expr { AtomSuffix } ;
AtomSuffix      = 'from' Expr≥5 | 'principal' Expr≥5 | 'weight' Expr≥5
                | "@" Expr≥5                                          (* specs and localized handlers *)
                | 'at' 'tick' Expr≥5 ;                                (* specs *)

(* ======================================================================== expressions *)
Expr            = Prefix { BinaryOp Prefix | "as" Type } ;            (* precedence climbing, §3.3 *)
Prefix          = "not" Expr≥4
                | ( "-" | "~" ) Expr≥14
                | "|" [ Expr { "," Expr } ] "|" Expr                  (* closure: function bodies only *)
                | Postfix ;
BinaryOp        = "||" | "&&" | "==" | "!=" | "<" | "<=" | ">" | ">=" | "in"
                | ".." | "..=" | "<.." | "<..=" | "|" | "^" | "&" | "<<" | ">>"
                | "+" | "-" | "++" | "*" | "/" | "%" | "**" ;
Postfix         = Primary { "." FieldName [ "(" Args ")" ] | "." BANG_IDENT "(" Args ")"
                          | "." INT_LIT | "(" Args ")" | "[" Expr "]" | "?" } ;
Primary         = INT_LIT | FLOAT_LIT | DURATION_LIT | STRING_LIT | BYTES_LIT
                | "true" | "false" | "self" | "_"
                | "(" ")" | "(" Expr ")" | "(" Expr "," [ Expr { "," Expr } [ "," ] ] ")"
                | "[" [ Expr { "," Expr } [ "," ] ] "]"                (* Vec literal *)
                | 'set' "[" [ Expr { "," Expr } [ "," ] ] "]"
                | 'map' "[" [ Expr "=>" Expr { "," Expr "=>" Expr } [ "," ] ] "]"
                | FoldName "{" Expr [ "=>" Expr ] "|" Body "}"          (* lattice fold; `|` ends the element *)
                | BANG_IDENT "(" BangArgs ")"
                | "if" Expr⁰ BlockExpr "else" ( Primary | BlockExpr )  (* the Primary is another `if` *)
                | "match" Expr⁰ "{" [ MatchArm { "," MatchArm } [ "," ] ] "}"
                | PathExpr [ StructLit ]                              (* StructLit: never in no-struct contexts *)
                | FString ;
FString         = 'f"' { FSTRING_TEXT | "{" Expr [ ":" FSTRING_SPEC ] "}" } '"' ;  (* §2.4 *)
PathExpr        = IDENT { "::" ( IDENT | GenericArgs ) } ;
StructLit       = "{" [ FieldInit { "," FieldInit } [ "," ] ] "}" ;
FieldInit       = FieldName ":" Expr | FieldName | ".." Expr ;
FoldName        = 'lset' | 'lmax' | 'lmin' | 'lbool' | 'lmap' | 'lbag' | 'lpset' ;
MatchArm        = Expr [ "if" Expr ] "=>" Expr ;                      (* the first Expr is a pattern *)
Args            = [ Arg { "," Arg } [ "," ] ] ;
Arg             = ".." [ Expr | Record ] | ( FieldName | PropName ) ":" Expr | "*" | Expr ;
Record          = "{" [ ( FieldName | PropName ) ":" Expr { "," ( FieldName | PropName ) ":" Expr } [ "," ] ] "}" ;
PropName        = IDENT { "-" IDENT } | STRING_LIT ;                  (* `stroke-width`, `"aria-label"` *)
BangArgs        = [ BangArg { "," BangArg } ] { BangClause } ;
BangArg         = "*" | Expr ;
BangClause      = 'per' Expr | 'by' OrderKeys | 'default' Expr | 'least' Expr | 'most' Expr
                | 'sticky' | 'durable' | 'release' ;
OrderKeys       = "(" OrderKey { "," OrderKey } [ "," ] ")" | OrderKey ;
OrderKey        = Expr [ 'asc' | 'desc' ] ;

(* ======================================================================== evolution and snapshots *)
MigrateItem     = "migrate" 'from' INT_LIT [ 'down' ] "{" { Item } "}" ;
TranslateItem   = "translate" RelPath ( 'to' | 'from' ) INT_LIT "{" { Item } "}" ;
SnapshotItem    = 'snapshot' IDENT 'of' RelPath 'at' 'progress'
                  ( 'every' Expr 'upto' Expr | "(" [ Expr { "," Expr } [ "," ] ] ")" )
                  [ 'mode' IDENT ] [ 'estimate' Expr ] ";" ;

(* ======================================================================== specs *)
SpecItem        = "spec" IDENT
                  [ "for" SimplePath [ GenericArgs ] [ "(" [ NamedArg { "," NamedArg } [ "," ] ] ")" ] ]
                  "{" { SpecMember } "}" ;
SpecMember      = OuterAttrs
                  ( 'nodes' IDENT { "," IDENT } ";"
                  | 'assign' IDENT "=" "[" [ IDENT { "," IDENT } [ "," ] ] "]" ";"
                  | 'faults' OptBlock
                  | "include" SimplePath ";"
                  | 'liveness' IDENT ":" 'eventually' Body 'within' Expr 'ticks' 'after' ( 'eff' | Expr ) ";"
                  | 'prove' IDENT 'by' 'induction' [ 'using' IDENT { "," IDENT } ] ";"
                  | 'expect' ( 'confluent' | 'deterministic' ) "(" RelPath ")" ";"
                  | 'check' ( 'ldfi' | 'bmc' | 'smt' | 'sim' | 'asp' ) [ OptBlock ]
                    [ 'expect' ( 'holds' | 'fails' ) ] ";"
                  | ConstItem | FactItem | ViewDecl | InvariantItem ) ;
OptBlock        = "{" [ IDENT ":" Expr { "," IDENT ":" Expr } [ "," ] ] "}" ;
```

Where an item may appear is a static rule, not a grammar rule (§6.2, BLS0110): for example `role` and `at` only
inside a `choreography` or a multi-role root, handlers not inside a `protocol`, and `spec` only at file level.

### 3.3 Operator precedence

Loosest first. This is Rust's table, with `not` as a low-precedence boolean prefix (as in Python), `in`, range
operators that bind tighter than comparisons (so `x in lo..hi` needs no parentheses), `**` and `++`.

| Level | Operators | Associativity |
|---|---|---|
| 1 | `\|\|` | left |
| 2 | `&&` | left |
| 3 | prefix `not` | — |
| 4 | `==` `!=` `<` `<=` `>` `>=` `in` | none: `a < b < c` is BLS0103 |
| 5 | `..` `..=` `<..` `<..=` | none |
| 6 | `\|` | left |
| 7 | `^` | left |
| 8 | `&` | left |
| 9 | `<<` `>>` | left |
| 10 | `+` `-` `++` | left |
| 11 | `*` `/` `%` | left |
| 12 | `**` | right |
| 13 | `as` (postfix type cast) | left |
| 14 | prefix `-` `~` | — |
| 15 | postfix `.f` `.m(…)` `.m!(…)` `.0` `(…)` `[…]` `?` | left |

`if … { } else { }` and `match` are primaries (there is no `?:`). `|` is bitwise or on integers; it is never a
lattice join (joins are `a.join(b)`, §11.4). Inside a fold's element (`lset{ e | … }`) a top-level `|` ends the
element; parenthesize a bitwise or there.

### 3.4 Disambiguation rules

The grammar is LL(1) except at these points. Each is decided with a fixed, small lookahead and no backtracking.

1. **Labels.** At item level, `IDENT ":"` is a label and must be followed by `on`, `while` or `monotone`
   (BLS0105). No other item starts with an identifier.
2. **Contextual item keywords.** At item level an identifier is a contextual keyword when the grammar allows it
   there and the next token fits: `role IDENT`, `at IDENT {`, `cell`, `durable cell`, `timer`, `fact`, `service`,
   `aggregate`, `snapshot`, `block`, `acl`, the relation modifiers, `monotone`, and a function class followed by
   `fn`. `scratch cell` is a cell and `scratch IDENT(` is a table (two tokens).
3. **Contextual literal prefixes.** In a body, `outer`, `inserted`, `deleted`, `sealed`, `per`, `ever` and `sent`
   are keywords when the next token is an identifier; `final` when the next token is an identifier or `not`; `any`
   when the next token is `{`; `forall` when the next token is an identifier or `(`; `quorum` when the next two
   tokens are an identifier and `in`. Otherwise they are identifiers (`deleted(k)` is an atom of a relation named
   `deleted`).
4. **No-struct contexts.** A path followed by `{` is a struct literal only if the tokens after `{` are `}`, or
   `..`, or a field name followed by `:`, `,` or `}` (three tokens of lookahead), **and** the parser is not in a
   no-struct context. The no-struct contexts are: a handler header (`on`/`while` … `{`), the body of an `if` or
   `for` statement, a `forall` domain, the condition of an `if` expression, a `match` scrutinee, and a function
   parameter pattern. There, a struct literal must be parenthesized (Rust's rule). Lattice folds (`lset{`) and bang
   calls are recognized by their head token and are allowed everywhere.
5. **Named arguments and generic arguments.** Inside `(…)` of a call or head, `FieldName ":"` starts a named
   argument; inside `<…>`, `IDENT "="` starts a named generic argument. `::` is one token, so `a::b` is never a
   named argument.
6. **Bang-call clauses.** Inside `BANG_IDENT(…)`, the contextual words `per by default least most sticky durable
   release` begin clauses when they appear where an argument would start or after a complete argument.
7. **`>>` in types.** In a type context the parser splits `>>` into two `>` tokens (`Map<K, LMax<u64>>`).
8. **Expression statements do not exist.** Inside a block every statement starts with a verb, `if` or `for`, so a
   block is never confused with an expression.

The **resolver**, not the parser, classifies (§9.1):
- a body literal `Expr` as an atom, a generator, a membership test, a choice/order filter or a guard;
- `a.b(…)` as an instance relation `a.b`, a struct field call, or a method call;
- a bare name as a variable, a constant, a relation (all-wildcard atom), a role or a node name;
- `r(x)` in a named-mode atom as a pun.
This keeps the parser context-free and lets the language server parse incomplete code. The resolver's errors
("`kv` is a relation, but it is used as a function", BLS0202) are more useful than parse errors.

### 3.5 Recovery, formatting and editions

- **Concrete syntax tree.** The parser produces a lossless CST (every token and all trivia) shared by the
  compiler, the formatter and the language server. `ERROR` and `MISSING` nodes are first-class, so an editor works
  on broken code.
- **Recovery.** Synchronization points are `;`, the `}` that closes the current block, and any hard keyword that
  starts an item at the beginning of a line. Inside a body, `,` at depth 0 is a synchronization point, so one bad
  literal does not lose the rest of the header. A missing `;` before an item keyword on a new line is inserted and
  reported (BLS0101). Every parse error carries its span and the set of expected tokens.
- **Formatter.** One canonical format, no options. It never reorders items, statements, literals or alternatives
  (rule identity does not depend on order, but reviews do). One statement per line; headers longer than the line
  width break after commas with a continuation indent; `where` starts a line when the header breaks.
- **Tree-sitter.** A tree-sitter grammar is kept in the repository and tested against the same corpus as the
  parser (`examples/` plus the grammar tests). Contextual keywords are handled with tree-sitter's keyword
  extraction; no external scanner is needed.
- **Editions.** `program … edition N` selects the language edition; the default is the compiler's current
  edition, which is recorded in `schema.lock`. New hard keywords are added only in a new edition, and
  `blossom fix --edition N` rewrites every colliding identifier to a raw identifier. The first edition is 2026.

---

## 4. The core IR and lowering conventions

Every construct in §6–§19 is defined by its lowering to the Dedalus^L core IR (ENG-001; CR-01, CR-50). The IR is the
*meaning*: the engine must be observationally identical to naive per-tick evaluation of the lowered program
(CR-26, ENG-067). Engines are free to implement constructs natively (persistence as storage, ENG-003; choice as an
argmin index, ENG-068; `index!` by sorting, ENG-072).

### 4.1 IR notation

```ir
decl table   store(key: String, val: Bytes) key(key) durable     // set relation; key before the flags
decl scratch votes$now(term: u64; LSet<Node>)                    // lattice-valued: key columns ; value lattice
decl channel vote(term: u64, granted: bool) dir(Server -> Server)

h(t̄) :- b1, …, bn.            // deductive: same node, same tick
h(t̄)@next :- b.               // inductive: same node, tick t+1; evaluated once on the completed fixpoint
h(@D, t̄)@async :- b.          // async: delivered to node D at a later tick (destination written first)

r(t̄)            positive atom, local to the executing node (the location column is implicit, CR-14)
notin r(t̄)      negation (range-restricted)
X := e          binding;   e   a guard
r(k̄; V)         lattice-valued relation: generator over non-⊥ cells (in a body) or merge (in a head)
V = r[k̄]        lattice lookup: the cell value, ⊥ if absent (LANG-280)
c(t̄ | S, P)     a received channel tuple with its implicit sender S and principal P (SEM-091)
h(ḡ, agg<X>)    head aggregate, GROUP BY ḡ over the rule's distinct valuations (§10.1)
```

- IR variables are capitalized (`K`, `V`); surface variable `key` becomes `Key`.
- Built-in IR relations and scalars: `boot()` and `recovered()` (§8.4), `R$members(N)` for each role `R`
  (§6.10), `$dir(Node, Addr, Principal, Role)` (§18.1); scalars `$self`, `$now`, `$tick`, `$incarnation` (§15).
- **Generated relations** have `$` in their names (`store$del`, `apply$when`, `occ$out`), which no surface name
  can contain, so a lowering never captures a user name. Every generated relation is marked
  **provenance-transparent** in the IR: explanations (TEST-050), LDFI lineage (TEST-023) and coverage reports
  collapse it into the surface construct that produced it, and report surface labels, never generated ones.
- Instance relations are prefixed by their instance path: `chat.data.msg` (§6.5).
- `persist r` abbreviates the frame rule `r(x̄)@next :- r(x̄), notin r$del(x̄).` (LANG-065).

### 4.2 The tick, briefly

A tick is TPLP's local transition (SEM-002): (1) apply the deletions, then the insertions, staged for this tick,
clear tick-local relations and ingest the batch (delivered messages, timer events, host inputs, the `now` and
randomness samples); (2) compute the stratified deductive fixpoint; (3) evaluate the inductive rules (staging
t+1) and the async rules (filling the outbox); (4) commit durable state; (5) release the outbox; (6) run host
callbacks. Every read in a tick sees the state of that tick; every mutation lands at t+1 (CR-04, SEM-004). Each
tick ingests a batch and every relation is a set within a tick (CR-02, CR-03).

### 4.3 Rule identity and site ids (LANG-068, SEM-084)

Rule identity feeds provenance, tracing, coverage, plan hints, override, and the site ids of seeded operators. It
never depends on source order.

| Construct | IR rule id |
|---|---|
| handler header | `M::L$when`, where `L` is the handler's label, or `h#XXXXXXXX` (the first 8 hex digits of the SipHash of the handler's normalized header text) if it has none |
| statement | `M::L/verb:target` (for example `M::grant_vote/send:vote`); if two statements of one handler share verb and target, each adds `#` and the hash of its normalized text |
| `if`/`for` block | `M::L$if#XXXXXXXX` / `M::L$for#XXXXXXXX`, hashed over the block's normalized condition |
| view alternative | `M::V` for a single-alternative view, `M::V#XXXXXXXX` for each alternative of a multi-alternative view |
| bootstrap statement | `M::bootstrap/verb:target[#hash]` (`M::bootstrap_fresh/…` for `bootstrap fresh`) |

`M` is the module path including the instance path (`chat.data`). Normalization is the formatter's canonical
printing with comments removed, so reformatting never changes an id.

**Seeded sites.** A `choose!`, `choose_least!`, `choose_most!`, `choose_rand!` or `seq!` site, and a relation
`resolve` policy, gets the site id `M::N::op#k`, where `N` is the enclosing handler's **label** or the enclosing
**view name**, `op` is the operator name and `k` is its ordinal among operators of the same name *within that one
header, block condition or view body*. A seeded site in an unlabelled handler or in a multi-alternative view is
BLS0600: its id would not survive edits. A `resolve` policy's site is `M::relation::resolve`. Labels are unique per
module (BLS0201). Editing a labelled body changes only that body's hashes; editing elsewhere changes nothing.

---

## 5. Types

### 5.1 Scalar types (LANG-022, LANG-026–028)

| Type | Values | Notes |
|---|---|---|
| `bool` | `true`, `false` | |
| `u8` `u16` `u32` `u64` `u128`, `i8` `i16` `i32` `i64` `i128` | integers | arithmetic is checked: overflow, division by zero and out-of-range casts are hard runtime errors (BLSR004), never wrap-around; `wrapping_add` and friends are explicit functions |
| `f64` | IEEE 754 doubles | totally ordered by IEEE `totalOrder` for canonical order; **not** usable as the element of `LMax`/`LMin` (BLS0312) |
| `String`, `Bytes` | UTF-8 text, byte strings | |
| `()` | unit | |
| `Duration`, `Instant` | nanosecond durations and instants | `Instant - Instant = Duration`, `Instant ± Duration = Instant` |
| `Mod<N>` | `N`-bit modular ids (LANG-026) | modular `+ - << >>`; ring intervals (§9.4) |
| `Blob` | a handle to an out-of-line byte stream (LANG-028) | bytes move through host handlers (§16.6) |
| `Node`, `Node<R>` | routable node addresses | §5.3 |
| `Session` | an external client session | §18.4 |
| `Principal` | an authenticated identity (SPIFFE id) | §18.1 |

**`f64` (LANG-022).** Floats are IEEE 754 binary64 with round-to-nearest-even, made deterministic:

- **Canonical values.** Every `f64` a program computes (a literal, a constant, arithmetic, a cast, a library call,
  `rand_float`) is canonical: zero has one sign (`-0.0` is `0.0`) and NaN one bit pattern. So `==` and the
  comparisons, which use the values' total order (as joins and keys do), agree with IEEE on numbers; NaN equals
  itself and sorts above `+∞`.
- **Arithmetic** `+ - * / %` and unary `-` are IEEE (`%` is the truncated remainder, the dividend's sign); a
  division by zero gives `±∞`, an invalid operation NaN — never a runtime error. Both operands are `f64`: an integer
  literal is not a float (`x * 2` with `x: f64` is a type error; write `2.0`), and integers convert with `as`.
- **Casts.** `n as f64` is the nearest double. `x as i64` (any integer type) truncates toward zero; NaN, `±∞` or a
  value out of the type's range is BLSR004, as for integer casts.
- **The library** offers only correctly rounded operations, so results are the same on every platform: `abs`,
  `min`, `max`, `clamp`, `x.sqrt()`, `x.floor()`, `x.ceil()`, `x.round()` (half away from zero), `x.trunc()`, and
  `x.to_string()` (the shortest decimal that reads back as `x`, without an exponent: `1`, `0.1`, `-2.5`, `NaN`,
  `inf`). Transcendental functions wait for a deterministic implementation.
- **Aggregates.** `min!`, `max!`, `count!` and the others that do not add accept `f64`; `sum!` over `f64` is refused
  (BLS0908): float addition is not associative, so the sum would depend on the evaluation order.

### 5.2 Compound types (LANG-023, LANG-025)

- Tuples `(A, B, …)`, with `.0`, `.1` access and destructuring patterns; `()` is unit.
- `Vec<T>` (literal `[a, b]`), `Set<T>` (literal `set[a, b]`), `Map<K, V>` (literal `map[k => v]`). Values are
  immutable; `m.insert(k, v)` returns a new map. Iteration order is canonical (LANG-118).
- `Option<T>` with `Some(x)` and `None`. There is no null and no nil padding (CR-28).
- `struct Name<T> { field: T, … }` and tuple structs `struct Name(A, B);`, with structural equality, the canonical
  order of §5.5, field access `s.f` and struct literals `Name { f: e, g }` (with punning and `..base`: last, of the
  struct's type, evaluated once, it gives the fields not written).
- `enum Name<T> { A, B(T), C { f: T } }`. Every enum that reaches a channel, a durable relation or an interface must
  have exactly one variant marked `#[unknown]` (BLS0308): a value from a newer program version decodes to it,
  keeps its bytes, and is re-encoded unchanged (LANG-261). Variants are encoded by stable number (`#n`), never by
  index.
- `type Name<T> = Type;` is an alias. `extern type Name = "rust::Path";` is an opaque host type (§16.3).

### 5.3 Locations

`Node` is a routable location. Inside `at R { … }`, `self` has type `Node<R>`, and a role name used where a type is
expected denotes `Node<R>` (`Node<Server>`). `Node<R>` is a subtype of `Node`; a `Node` is converted to `Node<R>`
only by membership (`n in R` binds `n: Node<R>`). A channel's direction fixes the type of its sender (`from s`:
`Node<Src>`, or `Session` if the source role is `external`) and of its `to` expression. `Session` is not a `Node`:
it is the identity of one external client connection, valid only for replies (§18.4). `Principal` is distinct from
both (LANG-240).

How roles are inferred (subtyping as in MLsub, Dolan and Mycroft, POPL 2017; joins and meets as in TAPL §16.3):
- **Merges take the join.** The value of `if`/`match`, a collection's elements, `push`/`concat`, `unwrap_or` and a
  fold's accumulator is one of several values, so its type is their least upper bound: `if c { a } else { b }`
  with `a: Node<R>` and `b: Node` is a `Node`, and `Node<R>` with `Node<S>` joins to `Node`. Collections are
  covariant (values are immutable): `Vec<Node<R>>` is a subtype of `Vec<Node>`.
- **Conjunctions take the meet.** A rule body is a conjunction, so a rule variable is in every column that binds it
  and equal to what `let` binds it to: its type is the greatest lower bound of those. `==` as a conjunct of a rule
  body (not under `not`, `any` or `forall`, and not inside an expression) makes its two sides one value, so it
  narrows both. Nothing else narrows: a negated atom says nothing of the variables it tests, and in a function
  `a == b` is a comparison (there is no flow typing: `b` stays `Node` in the branch where `a == b` holds).
- **Requirements are checked, never inferred from.** A column written, a channel's `to` expression, a function's
  parameters and result, a struct field or variant payload and a `let x: T` annotation each require their declared
  type: a `Node` where a `Node<R>` is required is BLS0300. A function's parameters have exactly their declared types.
- **An inferred view column** holds the join of what its alternatives and writers put there.

### 5.4 Lattice, weighted and group types

Lattice types are ordinary types whose values carry a join: the built-in catalog is in §11.5 and user lattices in
§11.8. `ZSet<T>` (ℤ-weighted multisets), `Z`, `Zn<N>` and user `impl Group`/`impl Ring` types are group types
(§11.10). A group type is never a lattice (LANG-142, CR-35).

### 5.5 Canonical order (LANG-024, SEM-088)

Every type has one total **canonical order**, used by `<` on values, by `index!`, `top!`, `collect!`,
`percentile!`, tie-breaking in `choose*!`, by host callbacks, `stdout` and dumps (LANG-118). Numbers compare
numerically; `false < true`; `String` and `Bytes` compare lexicographically by bytes; tuples and structs compare
field by field in declaration order; enums compare by variant number, then payload; `None < Some(x)`; `Vec`
compares lexicographically, `Set` and `Map` as their sorted sequences; `Node` by node id; `Duration` and `Instant`
numerically. Lattice values compare by their canonical form (this is for deduplication and ties only; it is not
the lattice order). Canonical order never uses intern ids, hashes or arrival order (SEM-088).

### 5.6 Type inference (LANG-021, CR-28)

- Relation, channel, interface, cell and struct schemas are declared. View schemas are inferred from their
  alternatives (annotations optional). Function signatures are declared.
- Variables are typed by unification across all their occurrences in one rule body, head and block. A type error
  lists every piece of conflicting evidence with its span (BLS0300). An arity mismatch is BLS0301.
- An unsuffixed integer literal takes its type from context; with none, `i64`. There is no implicit numeric
  widening; convert with `as`.
- **Lattice lifts.** Where the expected type is a lattice, a value is lifted into it: `T` into `LMax<T>`,
  `LMin<T>` or `LPoint<T>`; `bool` into `LBool`; `Set<T>` (for example `set[v]`) into `LSet<T>`; `Map<K, V>` into
  `LMap<K, L>` when `V` lifts into `L`. The expected type comes from a head column, a typed `let`, a function
  parameter or a struct field. `LMax::of(x)`, `LSet::of(x)` and the other constructors of §11.5 are the explicit
  forms.
- **The non-⊥ refinement.** A variable bound by a generator atom over a lattice column (`votes(t, s)`) is known to
  be non-⊥ (SEM-101 N4). `reveal!` of a known non-⊥ `LMax<T>` or `LMin<T>` has type `T`; of any other value of those
  types, `Option<T>` with `None` for ⊥ (§11.4). The refinement follows direct variable binding only.
- Comparison operators need both operands of one type. `==` and `!=` on lattice values are BLS0305.

### 5.7 Generics

`struct`, `enum`, `type`, `fn`, `lattice`, `module`, `choreography` and `protocol` take type parameters
`<T, U: Bound = Default>`. Generics are monomorphized before lowering. Bounds are protocols (on module-typed
parameters, §6.4) and the built-in traits `Lattice`, `Group` and `Ring`. Every type is `Eq`, `Hash` and canonically
ordered, so no bound is needed for those.

---

## 6. Programs, files, modules and choreographies

### 6.1 Files and programs (LANG-001, LANG-260)

- Every file is a module. Its path is its path relative to the crate root (the directory of the program root),
  with `/` written `::`: `std::bcast::reliable` is `std/bcast/reliable.bls` (or `…/reliable/mod.bls`). `std::…` is
  the standard library.
- `program NAME version N [edition E];` makes a file a **program root**. The file's items form the root module,
  which is what the program deploys. `NAME` and `version` key the schema lock (LANG-260). A program root's `pub`
  items can still be `use`d by other files, and a spec may target it by its module path (E10).
- Items, statements, alternatives and literals are unordered sets (LANG-001): reordering never changes meaning.

### 6.2 Items and where they may appear

| Item | File/program root | `module` | `choreography` | `at R` section | `protocol` | `spec` |
|---|---|---|---|---|---|---|
| `use`, `import`, `include`, `const`, `param`, types, `fn`, `extern`, `impl`, `lattice`, `aggregate` | ✓ | ✓ | ✓ | `import` only | `const`, types | `const` |
| relations, cells, timers, `service` | ✓ | ✓ | channels and `static` only (the rest inside `at`) | ✓ | `input`, `output` only | — |
| views, handlers, `bootstrap`, `invariant`, `interpose`, `block`, `override`, `acl`, `snapshot` | ✓ | ✓ | inside `at` | ✓ | — | views, invariants |
| `role`, `at` | ✓ (multi-role root) | — | ✓ | — | — | — |
| `module`, `choreography`, `protocol`, `spec`, `migrate`, `translate` | ✓ | — | — | — | — | — |
| `fact` | ✓ | ✓ | ✓ | ✓ | — | ✓ |

An item in the wrong place is BLS0110. In a module that declares roles, every view, handler, bootstrap, invariant
and non-shared relation must be inside an `at` section (BLS0408); channels, `static` relations, types, constants
and functions may be shared (declared outside every `at`).

### 6.3 Visibility and interfaces (LANG-003)

A module's interface is its `input` and `output` relations, and nothing else: an importer may write an instance's
inputs and read its outputs (BLS0203 otherwise). `pub` applies to types, functions, constants, lattices,
aggregates, modules, choreographies and protocols, making them visible to `use`; `pub` on a relation is BLS0110
(interfaces are the only connection points). A spec may read every relation of its target, private or not,
because it observes a trace, not an interface.

### 6.4 Constants and parameters (LANG-010)

```blossom
const QUORUM: u64 = 3;                  // compile-time constant, folded
param RETRY: Duration = 2s;             // deploy-time: `blossom run --param RETRY=500ms`, or the deployment spec
pub module ReliableBroadcast<P>(RETRY: Duration = 1s, peers: rel(n: Node)): Broadcast<P> { … }
```

- `const` is substituted before lowering. `param` becomes an IR constant bound at deployment and recorded in the
  trace header (TEST-010). Both must be SCREAMING_CASE (BLS0211).
- **Module parameters** are declared in `(…)` after the name:
  - a **value parameter** `NAME: Type [= default]` is a per-instance constant;
  - a **relation parameter** `name: rel(col: T, …)` is bound at import to a relation of the importer with the same
    column types (BLS0205). The instance may read it and never write it (BLS0406). Lowering substitutes the bound
    relation's name. A relation parameter bound to a `static` or `sealed` relation is closed for `forall`
    (§9.8) and for finality (ANA-121).
- **Type parameters** go in `<…>`. A type parameter bounded by a protocol (`module Multicast<D: Delivery>`) is a
  module parameter: the body may `import D as d;`, and the importer chooses the implementation
  (`import Multicast<D = ReliableDelivery> as mc;`). This is LANG-006's composition-time choice.

### 6.5 Import creates instances (LANG-004)

```blossom
import ReliableBroadcast<Bytes>(RETRY = 2s) as data;
import ReliableBroadcast<String>(RETRY = 200ms) as control;
emit data.members(n);                   // write an instance input
on data.deliver(o, id, p) { … }         // read an instance output
```

`import M<T…>(K = v, …) as a;` creates an independent instance: `M` is monomorphized with its type arguments,
its value and relation parameters are substituted, and every relation of `M` is renamed `a.r` in the flat IR. Two
imports are two disjoint instances; reusing an alias is BLS0201; nested instances are `a.b.r`. The instance path is
part of every channel's identity and wire schema id (DIST-003), so `data.msg` frames can never be decoded as
`control.msg`. Instances bootstrap in the same boot-tick fixpoint as their importer (§8.4). The IR of an import is
the renamed rules; there is nothing else.

`use a::b::{C, D};` brings names into scope without creating anything. `use a::b;` brings the module `b`.

### 6.6 Include (LANG-005)

`include M;` copies the items of module `M` into the current module flat, in one namespace, as Bloom's `include`
does: `M`'s interfaces become this module's interfaces. Duplicate names are errors (BLS0201). `include "file.bls";`
is textual and resolves relative to the including file. `include "file.ded";` compiles the file through the Molly
frontend (§21) and includes the resulting relations and rules; their schemas are inferred from the `.ded` rules.

### 6.7 Protocols (LANG-006)

```blossom
pub protocol Broadcast<P> {
    input bcast(id: MsgId, payload: P);
    input members(n: Node);
    output deliver(origin: Node, id: MsgId, payload: P) key(origin, id);
    output bcast_done(id: MsgId);
}
pub module ReliableBroadcast<P>(RETRY: Duration = 1s): Broadcast<P> { … }
```

A protocol declares interfaces (and may declare constants and types) and nothing else. `module M: P` makes `P`'s
interfaces `M`'s interfaces; `M` need not redeclare them, and a redeclaration must be identical (BLS0206). `M` may
not add interfaces that none of its protocols has (BLS0206). A protocol produces no IR; it is a catalog entry.

### 6.8 Named blocks and override (LANG-007)

A labelled handler, a view, and a `block NAME { items }` are each a named rule block. In a module that `include`s
another, `override` replaces the included block of the same name:

```blossom
module QuietBroadcast {
    include ReliableBroadcast;
    table muted(n: Node);
    override retransmit: on retry, outbox(dst, id, payload), not muted(dst) {
        send msg(self, id, payload) to dst;
    }
}
```

The included block's rules are dropped and the new ones added. A same-name block without `override`, or an
`override` with nothing to replace, is BLS0207.

### 6.9 Interposition (LANG-008)

```blossom
interpose data.bcast as (outside, inside) {
    admit: on outside(id, p) where p.len() <= MAX_PAYLOAD {
        emit inside(id, p);
        emit accepted(id, p.len());
    }
    refuse: on outside(id, p) where p.len() > MAX_PAYLOAD {
        emit rejected(id, p.len());
    }
}
```

`interpose a.i as (outside, inside) { … }` routes one interface of an instance through the importer's rules.
`outside` is always what the rest of the program sees; `inside` is always what the component sees.
- For an **input** `a.i`: every write to `a.i` anywhere in the program (including by the importer's own handlers)
  is redirected to `outside`; the component reads `inside`, which only the interposition block writes.
- For an **output** `a.i`: the component's writes go to `inside`; every reader of `a.i` outside the block reads
  `outside`, which only the block writes.

Lowering is renaming. For the input case above:

```ir
data.bcast$outside(Id, P) :- publish(Id, P).                          // was: data.bcast(Id, P) :- publish(Id, P).
data.bcast(Id, P) :- M::admit$when(Id, P).                            // `inside` is the instance's real input
M::admit$when(Id, P) :- data.bcast$outside(Id, P), $len(P) <= 65536.
accepted(Id, S) :- M::admit$when(Id, P), S := $len(P).
```

An interface may be interposed once per program (BLS0208). Only interface relations can be interposed, so an
internal refactor of the component cannot break the importer. This is BOOM's LATE, Paxos-insertion and metering
use (LANG-008).

### 6.10 Choreographies and roles (LANG-009, LANG-153, LANG-243)

A module with roles contains the rules of several locations. It is written either as a `choreography` item (a
reusable template) or directly at a program root (a deployed multi-role program).

```blossom
role Client: external;          // clients: sessions, not nodes; no rules may be placed there
role Coordinator;               // `process` (the default): exactly one node
role Participant: cluster;      // SPMD: one or more nodes running the same projection

channel prepare(txn: u64): Coordinator -> Participant;      // direction; shared declaration
at Coordinator { … }            // items placed on Coordinator
at Participant { … }
at Coordinator { … }            // sections reopen, so the protocol reads in message order
```

- **Placement.** Relations, cells, timers, views, handlers, bootstraps and invariants inside `at R` live on role
  `R`. Types, constants, parameters, functions, channels and `static` relations declared outside every `at` are
  shared. A channel's send side belongs to its source role and its receive side to its destination role: a `send`
  of `c: A -> B` must be placed at `A`, and an atom of `c` read at `B` (BLS0404).
- **Role expressions.** A role name used as a value is its member set: `p in R` is a generator (binding
  `p: Node<R>`) or a membership test; `R.size()` is its cardinality; `R.route(k)` is the member that owns key `k`
  under rendezvous hashing over the canonically ordered members (every node computes the same owner);
  `majority(s, R)` is the quorum threshold of §11.6. Membership is static per deployment (ODD-21 (c)); dynamic
  membership is the epoch-sealed library of DIST-042, whose epoch member relations are passed to modules as
  relation parameters.
- **External roles** hold no rules. A channel whose source is an external role carries a `Session` sender; a
  channel to an external role is egress-only and its `to` expression must be a `Session` (§18.4).
- **ACLs** are inferred: a channel accepts frames only from the roles that `send` into it (LANG-242 P0, ODD-33).
  A channel whose source role is `external` is open to that role's sessions; declaring the direction is the
  explicit opening ANA-105 asks for.

**Deployment.** A program root with no roles is single-location: every node of the deployment runs it. A program
root with roles is deployed by assigning nodes to its roles (`blossom deploy --role Server=n1,n2,n3`, or `assign`
in a spec). A `choreography` template is deployed by importing it into a multi-role root and binding each of its
roles to a root role of the same kind:

```blossom
role App: external;
role Coord;
role Worker: cluster;
import TwoPhaseCommit(TIMEOUT = 2s) as tpc with (Client = App, Coordinator = Coord, Participant = Worker);
```

Every template role must be bound (BLS0206). Several template roles may bind one root role only if their kinds
match. A single-location module is placed on a role by importing it inside that role's `at` section.

**Projection.** For each non-external role `R`, the compiler builds one program from: every item placed at `R`; the
shared items those rules reference; the send side of each channel whose source is `R` and the receive side of each
channel whose destination is `R`; and `R'$members` for every role `R'` the rules mention. A cluster role's
projection runs on every member.

**Meaning.** The Dedalus meaning of a choreography is **one** program in which every rule placed at `R` carries
the guard `$role(R)` (TPLP's heterogeneous-roles encoding, R02 §4.2), and `Node<R>` is a sort. Projection is guard
elimination, so it is correct by construction. The verifiers (VER-006, VER-003) and the simulator see the guarded
single program:

```ir
tpc.start$when(Id, C) :- $role(Coordinator), tpc.begin(Id | C, _), notin tpc.txn$p1(Id).
tpc.start$for#5d02c4e1(Id, C, P) :- $role(Coordinator), tpc.start$when(Id, C), Participant$members(P).
tpc.prepare(@P, Id)@async :- $role(Coordinator), tpc.start$for#5d02c4e1(Id, C, P).
tpc.first_vote$when(Txn, Co) :- $role(Participant), tpc.prepare(Txn | Co, _),
                                notin tpc.prepared$p1(Txn), notin tpc.decided$p1(Txn).
```

---

## 7. Relation declarations

### 7.1 The general form (LANG-020, LANG-121, LANG-261)

```blossom
[#[attrs]] {modifier} kind name(column, …) {clause} ;
[#[attrs]] {modifier} kind name like other {clause} ;
column = [#[attrs]] [@] name: Type [#n] [= default]
```

- **Columns** are named and typed. `#n` pins the column's stable field number (otherwise the compiler assigns one
  and records it in `schema.lock`, §19). `= default` is the value a reader uses when the column is absent, which
  every column added after version 1 must have (with `#[since(N)]`).
- **Keys.** By default every non-lattice column is a key. `key(a, b)` makes exactly those columns the key;
  `key()` makes the relation a singleton (a register). A key column may not be lattice-typed (BLS0304). Two
  distinct tuples with one key in one tick are a runtime error (SEM-050, BLSR001) unless every differing column is
  lattice-typed, in which case the lattice columns merge (CR-07, CR-51). A relation with lattice columns is
  lattice-valued (SEM-100): its value is the product of its lattice columns, keyed by its key.
- **`like other`** reuses another relation's columns, types, field numbers and key (LANG-020).
- **Clauses** are listed in §3.2; each kind of clause may appear once (BLS0106) and only on the kinds that accept
  it (§12). Their meaning is given where each is introduced.

IR: `decl <class> name(k̄, v̄) key(k̄) [flags]`, or `decl <class> name(k̄; L)` when lattice-valued.

### 7.2 `table` (LANG-040, LANG-065)

A persistent relation. Lowering adds the frame rule and a deletion relation read only by it:

```ir
decl table link(src: Node, dst: Node, cost: u64) key(src, dst, cost)
decl scratch link$del(src: Node, dst: Node, cost: u64)          // written by `delete` and `upsert`
link(S, D, C)@next :- link(S, D, C), notin link$del(S, D, C).
```

`emit` into a table inserts now and persists from now on; `next` inserts at t+1. The explicit Dedalus form
(`while p(x), not p_del(x) { next p(x); }` over a `scratch p`) is accepted and is recognized as the same storage
(LANG-065).

**Persistence with a condition: `while`.** A table may persist its rows only while a condition holds for them
(EXTENSIONS 2.3), which states once that a row lives as long as its owner instead of in a clean-up rule:

```blossom
table placed(c: Conn, i: u64, j: u64) while queued(c, i, _);
table follower(g: Group, f: Node) while leader(g, self), members(g, f);
```

The condition is a body over the table's columns, by name (so they are named as variables, BLS0106) (relation atoms, negations, `let`s and a `where`); its
other variables are existential. A row persists from tick t to t+1 only if the condition holds for it at t — a row
whose condition fails is visible in that tick and gone in the next, exactly as with `while p(x̄), not <condition>
{ delete p(x̄); }`, which it replaces. Writes (`emit`, `next`, `upsert`, `delete`) are unchanged. The condition may
read the table itself, and is stratified like any rule body. `while` comes last among the declaration's clauses
(its body runs to the `;`), applies to tables only (BLS0106), and is not implemented on tables of lattice values.
On a table with a `resolve` policy (§10.7) it keeps a persisted row from being a candidate for the next tick. A
`durable` table may have one: the condition is evaluated every tick, so a row whose
owner is gone does not survive a restart either. Lowering — the frame rule gains a guard relation of the same
construct:

```ir
decl scratch placed$keep(c: Conn, i: u64, j: u64)
placed$keep(C, I, J) :- placed(C, I, J), queued(C, I, _).
placed(C, I, J)@next :- placed(C, I, J), notin placed$del(C, I, J), placed$keep(C, I, J).
```

### 7.3 `durable` (LANG-044, SEM-072)

`durable table`, `durable cell` and `durable soft table` add no rules. The IR declaration carries `durable`: the
tick's staged changes to the relation are appended to the write-ahead log and fsynced in step 4 of the tick, before
the outbox is released in step 5. So a `next voted_for(t, c)` and a `send vote(…)` in the same handler are ordered
by the durability barrier: the vote is on disk before the reply leaves. Durable relations are reloaded at restart
(SEM-071). Their columns are locked in `schema.lock` with field numbers, and their schema hash is recorded in every
WAL segment and checkpoint (DIST-081).

### 7.4 `scratch` (LANG-041)

A tick-local relation with no frame rule: empty at the start of every tick. `next s(…)` into a scratch appears only
in the next tick. A `scratch` is *open*: any handler may write it. Prefer a `view` (closed, schema inferred, §8.3)
for derived relations, and use `scratch` when several handlers contribute or when the value is staged with `next`.

### 7.5 `static` (LANG-045, CR-16)

Base facts that hold at every tick: configuration, membership, topology. Rows come from `fact` items (§8.4) and from
the deployment configuration under the relation's name. No statement may write a static relation (BLS0400). IR:
`decl static r(…)`; each fact is a bodiless rule.

### 7.6 `input` and `output` (LANG-003, LANG-043, LANG-067, LANG-185, LANG-206, LANG-212)

Tick-local interface relations; the catalog records their direction.
- A module never writes its own `input` (BLS0406). The host inserts into a program's inputs for a *future* tick only
  (LANG-067). An importer writes an instance's inputs in the current tick (`emit data.members(n)`), which is how
  modules compose.
- `output` relations are written by the module and read by the importer or the host. The host subscribes to each
  tick's full contents or only to deltas (LANG-185).
- `final output r(…)` is accepted only if ANA-120 classifies `r` as POS-, NEG-, TOP-, THRESH-, FINITE- or
  SEALED-final (BLS0705); every emitted row then carries `final_present`, and the output can report
  `final_absent` (LANG-212, §14.5).
- `#[atomic]` on an output releases it only after the tick's state updates are visible to snapshot reads
  (LANG-206). `#[handler("rust::path")]` calls a host handler per emitted row after the tick commits (LANG-186).
  `#[nondet("reason")]` exports accepted nondeterminism through the interface (LANG-204).

### 7.7 `channel` (LANG-042, LANG-150, LANG-155)

```blossom
channel vote(term: u64, granted: bool): Server -> Server;               // direction form: destination implicit
channel pipe(@dst: Node, src: Node, id: u64, payload: Bytes);           // column form: one `@` column
#[fault(reliable)] channel occ(word: String, split: u32, line: u64, pos: u64): Mapper -> Reducer
    partition by word
    sealed by (split);
```

- A channel is an asynchronous relation. Only `send` writes it; on the receiving node its contents are tick-local.
- **Destination.** In the direction form the destination is not a column: `send c(…) to d` supplies it and the
  receiver never sees it (it is `self`). In the column form exactly one column is marked `@`; `send c(…)` supplies
  it as that column and writing `to` is BLS0403. Either way the IR normalizes the destination to the first column
  (CR-14): `vote(@D, T, G)@async`. Atoms of a channel have exactly its declared columns.
- **Direction** `: Src -> Dst` names roles in a multi-role module (required there) or is `Node -> Node` (the
  default) in a single-location module. It types the sender and the destination (§5.3) and places the two sides
  (§6.10).
- **Sender and principal.** A received tuple carries two implicit columns, bound with the atom suffixes
  `from s` and `principal p` (§18.2). They are not payload and cannot be forged; when no rule reads them they are
  projected away, so identical facts from different senders merge (SEM-091).
- **Keys.** The channel key (all columns by default, or `key(…)`) is checked at the sender, per tick (SEM-050).
- **Lattice columns** merge at the sender, per (destination, key, tick), and within one delivered batch; nothing
  merges across ticks without a persistent sink (CR-52, SEM-105).
- **Clauses and attributes.** `partition by` (§14.3), `sealed by` (§14.4), `exactly_once` (§11.10);
  `#[fault(…)]` (§14.2), `#[accept(…)]` (§18.3), `#[replicated]` (§14.1).

### 7.8 `loopback` (LANG-046)

A channel whose destination is always `self`, delivered through the network path, so a tuple sent at tick t arrives
at a later tick. `send retry_later(i);` takes no `to` (BLS0403 otherwise). IR: `retry_later(@$self, I)@async :- …`.
To merely request another tick without the network path, write `next localtick();` (§7.15).

### 7.9 `soft table` (LANG-048, CR-17, SEM-060, SEM-061)

```blossom
soft table heard(n: Node) ttl TTL max 4096;
```

A tuple is visible while its birth is less than `ttl` before the tick's sampled `now`. Re-deriving a tuple resets
its birth (a refresh, not an insertion). When more than `max` tuples are alive, the oldest are evicted, ordered by
(birth, canonical order), **in the same tick**. Expiry is deterministic at tick boundaries against the tick's
`now`, never lazy on access (CR-17). An expiry is a deletion delta, so `deleted heard(n)` is exactly the expiry
event. Lowering (writes of `heard` target `heard$b`; a `next heard(…)` targets `heard$n`):

```ir
decl scratch heard$b(n: Node; LMax<Instant>)                    // born or refreshed in this tick
decl scratch heard$s(n: Node; LMax<Instant>)                    // carried storage (explicit persistence below)
heard$b(N; B) :- <body of each write>, B := LMax::of($now).
heard$b(N; B) :- heard$n(N), B := LMax::of($now).
heard$n(N)@next :- <body of each `next heard(n)`>.
heard$all(N; B) :- heard$s(N; B).                               // merge: the latest birth wins
heard$all(N; B) :- heard$b(N; B).
heard$live(N, T) :- heard$all(N; B), T := reveal(B), $now - T < TTL.        // read-time TTL (CR-17)
heard$rank(N, I) :- heard$live(N, T), I = index<by (T, N) desc>.            // newest first (§10.5)
heard(N) :- heard$rank(N, I), I < 4096.                                     // evict beyond `max` now
heard$s(N; B)@next :- heard(N), heard$live(N, T), B := LMax::of(T), notin heard$del(N).
```

A soft head re-derived every tick from a soft body is refreshed every tick the body is visible, which realizes
SEM-061's cascaded refresh under Dedalus re-derivation. ANA-006 warns when a head's TTL is shorter than a soft body
atom's TTL. `max` is optional (no eviction bound). `durable soft table` makes `heard$s` durable.

### 7.10 `sealed table` (LANG-049)

Writable only by `bootstrap` statements (BLS0401 otherwise); `delete` and `upsert` are rejected. After the boot
tick the whole relation is sealed:

```ir
config(K, V)@next :- config(K, V).                                // no $del: deletions are rejected
config$sealed() :- notin boot().                                  // CLOSED for ANA-121 from the next tick on
```

### 7.11 `range(col)` tables (LANG-050)

`table acked(src: Node, seq: u64) range(seq);`: every column is a key, and the integer column `seq` is stored as
disjoint `[lo, hi]` buckets per value of the other columns. The semantics are those of `table`; `delete` and
`upsert` are rejected (range collections are never reclaimed).

### 7.12 `zset table` and `bag table` (LANG-138)

Weighted collections; §11.10.

### 7.13 `cell`: 0-ary lattice relations (LANG-120, LANG-128, LANG-280)

```blossom
cell clock: VClock;                 // persistent, starts at ⊥
scratch cell seen_now: LSet<u64>;   // ⊥ at the start of every tick (CR-24)
durable cell term: LMax<u64>;       // persistent and WAL-logged
```

A cell is Bloom^L's lattice identifier: `decl table clock(; VClock)` with the implicit identity rule
`clock(; X)@next :- clock(; X).` (SEM-104); a `scratch cell` has no identity rule. It is written with exactly one
argument (`emit clock(v);` merges now, `next clock(v);` merges at t+1) and read **only** by lookup: the cell's name
used as an expression is `clock[]`, ⊥ until something is merged in (LANG-280).

### 7.14 Timers (LANG-172, LANG-173)

`timer beat every 1s;` declares an event relation `beat(count: u64, at: Instant)`; `timer wake every 10ms while
waiting;` fires only while the view or table `waiting` holds (§15.2).

### 7.15 Built-in relations (LANG-046, LANG-051, LANG-052, LANG-202, LANG-240, LANG-243)

| Relation | Kind | Meaning |
|---|---|---|
| `boot()` | event | the first tick of every incarnation (§8.4) |
| `recovered()` | event | holds in the boot tick iff durable state was reloaded |
| `stdin(line: String)` | event | lines read from stdin since the last tick |
| `stdout(line: String)` | channel to the host | `send stdout(s);` writes at the end of the tick, in canonical order (LANG-118) |
| `halt(kill: bool)` | output | `emit halt(false);` stops the node at the end of the tick; `true` also stops the process |
| `localtick()` | scratch | `next localtick();` requests another tick (a staged change, SEM-009) |
| `session_open(s: Session, p: Principal, at: Instant)`, `session_closed(s: Session, reason: String)` | event | external sessions, on roles that receive from an external role (§18.4) |
| `node_dir(node: Node, addr: String, principal: Principal, role: String)` | static | the node directory (LANG-240) |
| `catalog.rule`, `catalog.depends`, `catalog.stratum`, `catalog.schema`, `catalog.interface` | static | the compiled program's catalog, after `use std::catalog;` (LANG-202) |

`#[readonly] table r(…)` is a host-maintained persistent relation: the host writes it between ticks and rules may
only read it (LANG-051). File sources are table functions (§16.2).

### 7.16 Plan hints (LANG-053)

`#[materialize]` and `#[recompute]` on a view or scratch choose incremental maintenance or recomputation. They
never change meaning.

---

## 8. Rules: handlers, statements, views, bootstrap and facts

### 8.1 Handlers

```blossom
[#[attrs]] [label:] [monotone] on    HEADER { STATEMENTS }
[#[attrs]] [label:] [monotone] while HEADER { STATEMENTS }
```

A handler is a rule body with several heads. For every valuation of `HEADER` in a tick (a body, §9), each statement
holds with its own timing. The statements form an unordered set; there is no control flow or sequencing. `on`
requires an event in the header and fires once per event; `while` fires at every tick in which the header holds
(§8.5).

**Lowering (normative).** The header is materialized once as a provenance-transparent scratch relation; each
statement is exactly one IR rule that reads it; each nested `if`/`for` block adds one more materialized relation.

```blossom
receive: on msg(origin, id, payload) from s {
    send ack(origin, id) to s;
    if not seen(origin, id) {
        emit deliver(origin, id, payload);
        next seen(origin, id);
    }
}
```

```ir
M::receive$when(Origin, Id, Payload, S) :- msg(Origin, Id, Payload | S, _).
ack(@S, Origin, Id)@async :- M::receive$when(Origin, Id, Payload, S).
M::receive$if#1c9e0b2a(Origin, Id, Payload, S) :- M::receive$when(Origin, Id, Payload, S), notin seen(Origin, Id).
deliver(Origin, Id, Payload) :- M::receive$if#1c9e0b2a(Origin, Id, Payload, S).
seen(Origin, Id)@next :- M::receive$if#1c9e0b2a(Origin, Id, Payload, S).
```

- `H$when` has one column per named variable of the header, in order of first occurrence. A choice literal, `outer`,
  `any`, `forall` or `not { … }` in the header expands inside the definition of `H$when` (§9), never per statement,
  so every statement sees the same valuations and the same choices.
- A head argument that is not a variable becomes a fresh IR variable bound by `:=` in the statement's rule.
- `H$when` is a scratch defined only by the header (one rule, or one per alternative of an `outer` or `any`), so
  inlining it into each statement rule is observationally identical; engines may do so (common-subexpression
  elimination is the reverse). A choice in the header stays computed once, in its own generated relations (§10.4).
- Provenance, tracing and coverage report the surface label and statement (§4.3).

### 8.2 Statements (LANG-060–068)

| Statement | Meaning | Bloom | IR head |
|---|---|---|---|
| `emit r(a…);` | holds now, on this node; into a lattice, merges now | `<=` | `r(A…) :- W.` |
| `next r(a…);` | holds at t+1; into a lattice, merges at t+1 | `<+` | `r(A…)@next :- W.` |
| `send c(a…) to d;` | arrives at `d` at a later tick | `<~` | `c(@D, A…)@async :- W, D := d.` |
| `delete r(a…);` | the exact tuple is absent from t+1, unless also inserted for t+1 (insert wins, CR-05) | `<-` | `r$del(A…) :- W.` |
| `upsert r(a…);` | at t+1 the key's rows are replaced by this tuple | `<+-` | below |
| `seal c(k: v) [to d];` | punctuation: no more `c` tuples with this key (§14.4) | — | §14.4 |

`W` is the materialized header or block relation. **Upsert** lowers through a keyed scratch, so two different
upserts to one key in one tick violate that scratch's key, which is exactly SEM-051 (BLSR002), and the error names
both source statements:

```ir
decl scratch store$ups(key: String, val: Bytes) key(key)       // r's key: the SEM-051 check happens here
store$ups(K, V) :- W.
store$del(K, V0) :- store$ups(K, _), store(K, V0).
store(K, V)@next :- store$ups(K, V).                            // insert wins if V0 == V (CR-05)
```

`upsert r(…) resolve POLICY;` replaces the SEM-051 error for that statement's conflicts with a declared choice
(§10.7). `emit`/`next` into a `zset` or `bag` table take `weight w` (default 1, §11.10).

**Heads.** A head lists every column positionally, or by name (`r(term: t, granted: true)`); a named head must give
every column that has no default (BLS0303), and `..` is not allowed in a head. Arguments are expressions over bound
variables (BLS0500). A head argument may be a head aggregate (`count!(*)`), grouping by the other head arguments
(§10.1).

**`if` and `for`.** `if BODY { … }` and `for BODY { … }` conjoin `BODY` to the enclosing condition for the
statements inside; they differ only in intent (`for` usually binds new variables, `if` usually only filters). A
nested block is lowered to its own relation (`H$if#…`, `H$for#…`). `else` is legal only when the `if` body is a
single scalar guard over already-bound variables, with no atom, lattice operand or bang (BLS0409): its negation is
then a selection, not an anti-join. Write `if not r(x) { … }` for the relational case.

**Child heads, spreads and trees** (S16; docs/design/SUGAR.md has the rationale and examples). Sugar for writing
many related rows; each lowers to ordinary statements of the statement's verb in the enclosing block, so nothing
else in the language sees it.

- *Child heads.* `emit order(id: o, customer: c) { line(sku: s, qty: 2); }`: the block after a head holds child heads
  (named arguments; `if`/`for` blocks of them; more children). A child inherits, from its enclosing heads (nearest
  first), every column it does not give whose name and type match one of theirs; a column neither given nor
  inherited is BLS0303. Positional arguments in a child head are BLS0303.
- *Spreads.* `..{name: value, …}` as a head's last argument writes one row per field into the relation's last two
  columns (a `String` name, a value); `..m` (a `Map<String, T>`) one row per entry. A value is converted with
  `to_string` when the column is a `String`.
- *Trees.* `tree T { node r(id, parent, pos, kind); props p(id, name, value); content c(id, value); }` names the
  relations a tree is written to, their columns in each role's order (BLS0434 when the shapes do not fit: ids of one
  type, an integer position). `emit T kind[meta](props) { children }` then writes elements: a node row each (its
  parent the enclosing element's id, `""` at the root; its position its slot among its siblings, counting through
  `if` blocks, or `[pos: e]`), a props row per property (dashed or string names allowed, values converted with
  `to_string` into a `String` column), and a content row for a bare-expression child (at most one). An element's id
  is `[id: e]`, else derived: the parent's id, `/`, the kind, `.`, the slot, and `[k]` with `[key: k]`. An element
  inside a `for` with neither an id nor a key is BLS0430; a key beside an id BLS0431.
- *Fragments.* `fragment f(x: T, …) { items }` names a group of statements and tree elements; a call `f(e, …)` stands
  where a statement or a tree element can. Its meaning is its items in a block of their own whose body binds each
  parameter to its argument (typed: the argument flows into `T`), and in which only the parameters and the
  fragment's own variables are visible: a caller's variable never joins a fragment's (hygiene). Called inside a tree,
  its elements are the enclosing element's children, in the call's slots; called elsewhere, a tree element in it is
  BLS0432. A fragment that calls itself is BLS0433; a parameter that is not a variable name BLS0436.

**No `let` statements.** A `let` is a body literal and belongs in the header or in an `if`/`for` body
(`on timed_out(t), let nt = t + 1 { … }`). A `let` statement is BLS0102. This keeps blocks from reading as
sequential code.

### 8.3 Views (LANG-047, LANG-053)

```blossom
view pending(id) = outbox(_, id, _);                          // one alternative
view grant(c, t) {                                            // several alternatives: a union
    fresh_grant(c, t);
    rv_ok(c, t), voted_for(t, c);
}
view eff(t = max!(x)) { current_term(x); heard_term(x); }     // an aggregate column
monotone view reach(a, b) { edge(a, b, _); reach(a, c), edge(c, b, _); }
```

A view is a **closed**, tick-local, deductive relation: every rule that defines it is in its declaration, and no
statement may write it (BLS0406). Its column types are inferred from the alternatives (annotate with `c: T` when
inference needs help or to force a lattice type, `d: LMin<u64>`). Every alternative must bind every non-aggregate
column (BLS0500). Views may be recursive (CR-11). With a lattice-typed column, a view is a tick-scoped lattice
relation, and alternatives merge per key (CR-24).

```ir
decl scratch grant(c: Node<Server>, t: u64)                    // schema inferred from the alternatives
grant(C, T) :- fresh_grant(C, T).
grant(C, T) :- rv_ok(C, T), voted_for(T, C).
eff$u(X) :- current_term(X).
eff$u(X) :- heard_term(X).
eff(max<X>) :- eff$u(X).
```

A view with aggregate columns first unions its alternatives into `v$u` over the variables that occur in **every**
alternative, then aggregates once (§10.1). A view that contains a seeded site (§4.3) or a `per` driver (§10.2) must
have exactly one alternative (BLS0600). `monotone view` is checked (§13.5).

### 8.4 Bootstrap and facts (LANG-069, LANG-190, SEM-012, SEM-071, CR-13, CR-16)

```blossom
bootstrap fresh { emit current_term(0); }                  // first start only
bootstrap { emit deadline(now() + rand_range(ELECTION_MIN, ELECTION_MAX, ("boot", 0u64))); }
fact edge(1, 2, 4);
```

- `boot()` holds in the first tick of **every incarnation**: tick 0 of a fresh node (SEM-012), and the first tick
  after each restart, after durable relations have been reloaded (SEM-071). `recovered()` holds in that tick iff
  durable state was reloaded. The tick counter is durable node metadata and keeps counting across incarnations.
- `bootstrap { … }` is a handler whose header is `boot()`. It re-initializes volatile state after every restart.
  Writing a durable relation in it is BLS0402, because the write would collide with reloaded state.
- `bootstrap fresh { … }` is a handler whose header is `boot(), not recovered()`: it runs only on a node's very
  first start, and is where durable initial values go (Raft's `current_term = 0`).
- `emit` in a bootstrap block holds in the boot tick; `next` holds at t+1 (tick 1 of a fresh node). `next` means
  t+1 everywhere, so LANG-190's "a `<+` in bootstrap takes effect at tick 0" is written `emit` (§22.3).
- All bootstrap statements of a program and its instances run in the same boot-tick fixpoint. An importer's
  bootstrap may read what an instance's bootstrap derives in that tick; stratification orders them, and no other
  order is observable (LANG-190's "imported modules bootstrap first", §22.3).
- `fact r(…);` asserts a row of a `static` relation that holds at every tick of every node running the module
  (CR-16). A fact into any other kind of relation is BLS0405: initial state of a mutable relation goes in
  `bootstrap`. In a spec, `fact r(…) @ n at tick k;` is a timestamped input event at node `n` (LANG-069, §17.2).

```ir
current_term(0) :- boot(), notin recovered().
deadline(D) :- boot(), D := $now + $rand_range(150ms, 300ms, ("boot", 0)).
edge(1, 2, 4).
```

### 8.5 Edge and level: the event/standing classification

Dedalus re-derives every rule at every tick in which its body holds (CR-26), so a rule over persistent state fires
again and again, and a `send` over persistent state is a periodic resend (ODD-05's normative literal semantics).
Blossom makes the difference visible.

**Event relations.** A relation is an *event* relation if it holds only in ticks caused by an event:
- channels (on the receiving side), loopbacks, `input`s, timers, `boot()`, `recovered()`, `stdin`,
  `session_open`, `session_closed`, service results;
- a view whose every alternative has a positive event literal;
- a `scratch` or `output` (including an instance's output, classified inside the instance) that has at least one
  writing statement and whose every writing statement is event-driven;
- an interposition's `outside`/`inside` pair of an input interface.

Everything else is *standing*: tables of every flavor, statics, cells, relation parameters, role membership and
`sealed` tests. The classification is the greatest fixpoint of these rules over the whole program.

A **positive event literal** is a positive atom of an event relation (not under `not`, not `outer`), a delta literal
(`inserted r(…)`, `deleted r(…)`), or an `any` whose every alternative has one. A statement is event-driven if its
handler's header has one; bootstrap statements are event-driven (`boot()` is an event).

**The rule.** An `on` handler must have a positive event literal in its header (BLS0504, an error). The message
explains that the handler would fire at every tick its header holds, suggests `while`, and prints the chain of
definitions that made each header relation standing (for example "`data.bcast_done` is standing: it is written by
`complete` (e02:61), which is `while mine(id), …`"). A `while` handler with an event literal is a warning (BLS0505,
"write `on`"). The two lower identically; the keyword is a checked statement of intent.

Level-triggered `send` statements are the resend sites; the compiler lists them, and they are exactly the
candidates for sender-side suppression, which the runtime applies only where ARM proves the receiver idempotent
(DIST-007, ANA-061).

### 8.6 Further static rules for handlers and views

- **Self-negation (BLS0506).** A handler that writes `r` with `emit` and tests `r` negatively (`not r(…)`,
  `not { … r … }`) in its header or in one of its `if`/`for` conditions is an error: the test sees the handler's
  own write in the same tick, which almost always meant `next`. The fix-it rewrites the `emit` to `next`.
  `#[allow(self_negation)]` on the statement overrides it for the rare legitimate case.
- **Seeded sites need stable ids (BLS0600)**, §4.3.
- **Range restriction (BLS0500, ANA-001)**: every variable of a head, a negated literal, a guard, a `to` or a
  `weight` must be bound by a positive literal, a `let`, a generator or an atom suffix of the same body.
- **Possible key conflicts (BLS1003, ANA-007)**, on by default: two statements, or one statement over several
  valuations, that may insert or upsert different values for one key in one tick. The warning includes a
  two-message example and suggests `choose_most!`, a lattice, or `resolve` (the Go/Erlang programmer's first
  surprise, CR-02).
- **Wildcard under an aggregate (BLS1004)**, on by default (§10.1).

---

## 9. Rule bodies

A body is a comma-separated conjunction of literals, optionally followed by `where` and guards. The planner orders
literals by the variables they need (LANG-085); textual order within a body carries no meaning.

### 9.1 Literals and their classification

The parser reads most literals as expressions (§3.4); the resolver classifies them:

| Surface | Classified as | Edge (§13.2) |
|---|---|---|
| `r(args)`, `a.r(args)` where `r` is a relation | atom (generator) | positive |
| `r` (a bare relation name) | atom with every column a wildcard | positive |
| `pat in e`, `pat` has an unbound variable | generator over `e` (§9.4) | positive |
| `x in e`, `x` bound | membership test (§9.4) | positive (a lattice `contains` is a threshold) |
| `choose!(…)`, `choose_least!(…)`, `choose_most!(…)`, `choose_rand!(…)`, `argmin!(…)`, `argmax!(…)`, `top!(…)`, `limit!(…)` | choice or order filter (§10.3, §10.4) | negative |
| `distinct!(z(…))`, `clamped!(z(…), n)`, `weights!(z(…), w)` | Z-set view (§11.10) | negative |
| any other expression of type `bool` | guard | positive (it only filters) |
| an expression of type `LBool` | threshold guard (`when_true`) | positive |
| `not L`, `not { B }` | negation (§9.3) | negative |
| `let p = e` | binding (§9.4) | none |
| `outer r(…)` | left outer join (§9.6) | negative |
| `inserted r(…)`, `deleted r(…)` | delta (§9.10) | negative |
| `sealed c(k: v) [from m]` | seal test (§14.4) | positive (a CLOSED threshold) |
| `final r(…)`, `final not r(…)` | finality test (§14.5) | positive (finality is monotone) |
| `per r(…)` | aggregate driver (§10.2) | positive |
| `any { B1; B2; … }` | disjunction (§9.7) | as its alternatives |
| `forall D { B }` | universal quantifier (§9.8) | positive iff `D` is closed |
| `ever …`, `sent …`, `quorum …`, `@ n`, `at tick k` | spec-only (§17.3) | — |

A name that resolves to a function is a call; `kv(k)` where `kv` is a relation used in expression position (for
example as an argument) is BLS0202.

### 9.2 Atoms (LANG-080, LANG-081, LANG-086)

```blossom
store(k, v)                          // positional: every column, in declaration order
log(i, t, _)                         // `_` is a wildcard
limit(k, k)                          // a repeated variable is an equality join
store(k, b"x")                       // a constant selects (LANG-081)
log(i + 1, t, _)                     // an expression over bound variables is an equality test
request_vote(term: t, ..)            // named: `field: pattern`; `..` ignores the other columns
vote(term, granted: true)            // named mode: a bare name is a pun (`term: term`)
entry(Entry { term, .. })            // struct patterns destructure a column
data.deliver(o, id, p)               // an instance's interface
```

- **Positional atoms** give every declared column (BLS0301). Channel atoms have exactly the declared columns (a
  direction-form channel has no destination column).
- **Named atoms.** An atom is in *named mode* if any argument is `field: pattern` or `..`. In named mode a bare
  identifier is a pun, an unknown field is BLS0302, and omitting a column requires `..` (BLS0302), except that a
  column declared `#[since(N)]` may always be omitted, so adding a defaulted column to a relation breaks no existing
  rule (LANG-261). A forgotten join field therefore fails loudly unless `..` says it was meant.
- **Patterns in arguments.** An argument is a pattern: a variable binds (or joins, if already bound), `_` matches
  anything, a literal or constant selects, and tuple, enum-variant and struct patterns destructure. An argument
  that is an expression over bound variables (`i + 1`) is an equality test.
- **Lowering.** Named atoms become positional IR atoms with fresh anonymous variables for omitted columns; each
  non-variable argument becomes a fresh variable plus an equality or pattern test:

```ir
log(J, T, _), J == I + 1                   // log(i + 1, t, _)
request_vote(T, _, _ | _, _)               // request_vote(term: t, ..), a received channel atom
```

### 9.3 Negation and anti-joins (LANG-082, LANG-083)

```blossom
not seen(o, id)                      // whole-tuple anti-join
not store(key, _)                    // key anti-join: `_` is existential
not { hits(k, h) where h > 10 }      // anti-join with a predicate; `h` is local to the braces
```

Every variable of a negated atom other than `_` must be bound by a positive literal of the enclosing body
(ANA-001, BLS0500). Variables first bound inside `not { … }` are existential and invisible outside it. Lowering:

```ir
fresh(M) :- msg(M), notin seen(M).
store$p1(K) :- store(K, _).                            // projection for the existential wildcard
remove_missing$when(Id, Key, C) :- del(Id, Key | C, _), notin store$p1(Key).
not$3a1f(K) :- kv(K, _), hits(K, H), H > 10.           // the outer bindings that make `{ … }` true
cold$when(K) :- kv(K, V), notin not$3a1f(K).
```

`not` applied to a scalar guard (`not (x > 3)`) is plain boolean negation and adds no negative edge; applied to an
`LBool` threshold it is antitone and is a negative edge.

### 9.4 `let`, generators, membership, ranges and intervals (LANG-085, LANG-088, LANG-090, LANG-092, LANG-026)

- `let p = e` binds the fresh variables of pattern `p` to `e`. A refutable pattern filters:
  `let Some(x) = parse_u64(s)` keeps the valuations where `e` matches. `let` never shadows: re-binding a bound
  variable is BLS0501 ("write `x == e`"). IR: `X := e` or a pattern test `Some(X) := e`.
- `pat in e` with an unbound variable in `pat` is a **generator**. `e` may be:
  - a `Vec`, `Set` or `Map` value (a map yields `(k, v)` pairs), in canonical order: `(p, w) in words(t).enumerate()`;
  - a range with bound endpoints: `i in lo..hi`, `i in lo..=hi`;
  - a role: `p in Server` (binds `p: Node<Server>`);
  - a unary relation: `n in peers` (the atom `peers(n)`);
  - a set-like lattice (`LSet`, `LPSet`, `LBag`): `v in s` (a morphism: Bloom^L `to_collection`, LANG-123);
  - a range scan of a keyed relation: `(i, t, c) in log[lo..=hi]` (§9.9);
  - a table function call with bound inputs: `(n, line) in lines(path)` (§16.2).
  The generator's input must be bound (a binding pattern, LANG-092, BLS0500). IR: `$member(E, P)`,
  `$range(Lo, Hi, I)`, `R$members(P)`.
- `x in e` with `x` bound is a **membership test**: `R$members(X)` for a role, an atom for a unary relation, the
  monotone `contains` threshold for a set-like lattice, and a guard for a value collection or a range.
- **Ranges and ring intervals.** `a..b` is [a, b), `a..=b` is [a, b], `a<..b` is (a, b) and `a<..=b` is (a, b].
  On `Mod<N>` values the four forms wrap around the ring, so `n<..=n` is the whole ring (Chord's
  `x in (a, b]` is written `x in a<..=b`). On other integers they do not wrap, and an empty range is empty. The
  operators never produce mismatched brackets.

### 9.5 `where` (guards)

`where` separates guards from joins: after it come only expressions (BLS0507 if a relation atom appears there).
Guards may also be written as ordinary literals before `where`; the two are equivalent.

```blossom
view rv_ok(c, t) =
    request_vote(t, li, lt) from c, eff(t), last_log(mi, mt)
    where lt > mt || (lt == mt && li >= mi);
```

### 9.6 `outer` (left outer join, LANG-087)

```blossom
read: on get(id, key) from c, outer store(key, val) {         // val: Option<Val>
    send get_resp(id, key, val) to c;
}
```

Every variable first bound by an `outer` atom has type `Option<T>`; either all of them are `Some` or all are `None`.
`outer` expands inside the header relation, never per statement:

```ir
store$p1(K) :- store(K, _).
M::read$when(Id, Key, C, Some(Val)) :- get(Id, Key | C, _), store(Key, Val).
M::read$when(Id, Key, C, None)      :- get(Id, Key | C, _), notin store$p1(Key).
get_resp(@C, Id, Key, Val)@async :- M::read$when(Id, Key, C, Val).
```

### 9.7 `any` (disjunction, LANG-089)

`any { B1; B2; … }` holds when some alternative holds. A variable used outside the `any` must be bound in every
alternative (BLS0500). It lowers to one rule per alternative for the enclosing relation (`H$when` or the view),
never per statement:

```ir
M::h$when(T) :- request_vote(T, _, _ | _, _).
M::h$when(T) :- vote(T, _ | _, _).
```

Scalar conditional *values* are `if c { a } else { b }` and `match`, which are pure expressions (IR `ite`) and need
no rule split. A `match` must cover every value of its scrutinee (BLS0314, naming a value no arm takes); an arm with
a guard covers nothing. The names in an arm's pattern are the arm's own: in a function body they bind afresh,
shadowing outer ones, as `let` does there; in a rule a name the rule already binds is BLS0501, since comparing and
rebinding would both be plausible readings (name the arm's variable differently and compare in a guard).

### 9.8 `forall` (universal quantification)

```blossom
view all_yes(id) = txn(id, _, _), forall p in Participant { p in yes_from[id] };
view all_done() = forall split_of(s, _) { split_done(s) };
```

`forall D { B }` holds when `B` holds for every valuation of the domain literal `D`. Variables first bound by `D`
(and inside `B`) are local; variables bound outside are correlated and must be bound by the enclosing body. The
domain is an atom or a membership generator. Lowering (Ō the correlated variables, X̄ the domain's own):

```ir
fa$9c1e(Ō, X̄) :- D', B'.                               // domain valuations that satisfy B
fa$9c1e$miss(Ō) :- D', notin fa$9c1e(Ō, X̄).             // some domain valuation fails B
… :- …, notin fa$9c1e$miss(Ō).
```

`forall` is **monotone exactly when its domain is closed**: a role's membership, a `static` relation, a `sealed`
table after bootstrap, a relation parameter bound to one of those, a view over only those, or a partition guarded
by a `sealed` test. Then ∀ over a fixed domain of a monotone predicate cannot flip from true to false, so the
analysis treats the literal as a positive threshold (ANA-065's CLOSED rule) and it is not a point of order. Over an
open domain `forall` is a point of order (a negative edge), reported as such; it is never silently monotone.

### 9.9 Lookups and range scans (LANG-091, LANG-129, LANG-280)

- On a lattice-valued relation, `r[k̄]` (every key column bound) is the cell's value, ⊥ of the value type when the
  cell is absent: `p in acked[id]`, `yes_from[id]`. It never fails. A cell's name is the lookup `c[]`.
- On a keyed set relation, `r[k̄]` is the tuple of its value columns (or the single value column), and the literal
  fails when the key is absent: it is a join. `let (t, _) = log[pi]`.
- `r[lo..hi]` (any range form of §9.4) on the first key column is a range scan, used as a generator:
  `(i, t, c) in log[next..=last]`. It lowers to the atom plus range guards, planned as an index range query.

### 9.10 Delta literals (LANG-071)

`inserted r(…)` holds for tuples present in this tick and absent in the previous one; `deleted r(…)` for tuples
present in the previous tick and absent now. They work on any relation:

```ir
decision$prev(I, D)@next :- decision(I, D).            // one shadow per delta-read relation
… :- decision(I, D), notin decision$prev(I, D), …      // inserted decision(i, d)
… :- decision$prev(I, D), notin decision(I, D), …      // deleted decision(i, d)
```

`r$prev` has `r`'s durability. For a durable relation, deltas after a restart are relative to the last committed
tick, so recovery produces no burst of spurious `inserted` facts and no delta is observed twice. For a fresh node,
every tuple at tick 0 is `inserted`. Both forms read `r` exactly, so both are negative edges (§13.2), and over
asynchronous inputs they are schedule-dependent (SEM-087). Delta literals are event literals (§8.5).

### 9.11 Sender and principal (LANG-241)

`c(…) from s` binds the transport-authenticated sender of a received channel tuple (a `Node<Src>`, or a `Session`
for an external source); `principal p` binds its principal. Both are allowed only on channel and loopback atoms
(BLS0212); they are the trailing IR columns of §4.1 and are projected away when unread (SEM-091). §18.2.

### 9.12 Expressions (LANG-084, LANG-180)

Expressions are pure. They use the precedence of §3.3, the canonical order for `<`, `++` for string and `Vec`
concatenation, checked arithmetic, `if`/`match` values, field access, tuple indexing, struct literals, casts with
`as`, calls of pure functions (§16.1) and methods of the built-in library (Appendix B). `now()`, `tick()`,
`random()` and `rand(k…)` read the tick's samples (§15). There are no closures in rule bodies (LANG-002): closures
exist only inside `fn`, `impl` and `aggregate` bodies. Lattice operations and their classes are in §11.4.

### 9.13 Projections (LANG-094)

`r.keys(k̄)` is an atom over the projection of `r` onto its key columns and `r.values(v̄)` onto its value columns; for
a column-form channel `c.payloads(…)` drops the `@` column. A renaming is a view. `schema_of(r)` is a compile-time
constant describing `r`'s columns, usable in `const` items.

### 9.14 Evaluation order and runtime errors

A body's meaning is a set of valuations; its order matters only for which runtime errors (checked arithmetic,
BLSR004; `error(…)`, BLSR010; …) a tick raises. A valuation of the positive atoms then meets the other literals —
negations, lookups, `let`s, generators and guards — each once its variables are bound, in this order:

1. every ready check that **cannot fail**, in body order: negations, lookups, and guards and `let`s whose expression
   is built only from variables, constants, comparisons, boolean and bitwise connectives, construction, field
   access and `if` over those;
2. then the first ready fallible **filter** (a guard) in body order, or else the first ready fallible **binding** (a
   `let` or a generator); then again from 1.

A `where a && b` is two guards. So a filter protects every expression it can: an expression is evaluated only for
valuations that every filter able to run before it accepts, wherever it is written, and an error is raised only
for such a valuation.

```blossom
view ratio(k, r) = pair(k, a, b), let r = a / b where b != 0;     // never divides by zero
view gap(k, d) = pair(k, a, b), not done(k), let d = a - b;       // `not done(k)` runs before the subtraction
```

Both evaluators follow this order, and the engine uses it: a guard bounding a column of an atom not yet joined
narrows that atom to a range scan (§9.9) whenever only checks that cannot fail run before the guard.

---

## 10. Aggregation, choice, numbering and folds

### 10.1 Head aggregates (LANG-100–104, CR-08, CR-09)

An aggregate is a bang call in a head: a view's aggregate column (`view load(w, n = count!(t)) = …`) or an argument
of a statement head (`emit word_count(w, count!(*));`). The non-aggregate head terms are the GROUP BY.

**Input.** An aggregate folds over the **set of distinct valuations** of the body (CR-03): for a statement, the
valuations of its materialized header or block relation (every named variable); for a view, the valuations, over
all alternatives, of the variables that occur in every alternative. Wildcards are not variables. So
`sum!(n)` over `partial(split, word, n)` adds `n` once per distinct `(split, word, n)`, and two splits reporting the
same partial count both contribute. **An empty group produces no row** (CR-08); defaults are §10.2. Every
non-lattice aggregate is a negative edge (CR-09); monotone aggregation is written with lattices (§11.7).

| Aggregate | Result |
|---|---|
| `count!(*)` | the number of distinct valuations in the group |
| `count!(e)` | the number of distinct values of `e` in the group (Datalog's `count<X>`) |
| `sum!(e)`, `avg!(e)` | the sum / mean of `e` over the group's valuations |
| `min!(e)`, `max!(e)` | the least / greatest `e` in canonical order |
| `collect!(e [by keys])` | a `Vec` of `e` over the valuations, ordered by (keys, canonical) (LANG-102, LANG-118) |
| `collect_set!(e)`, `collect_map!(k, v)` | a `Set`; a `Map` (two values for one key is BLSR005) |
| `bool_and!(c)`, `bool_or!(c)` | conjunction, disjunction (LANG-103) |
| `percentile!(p, e)` | the nearest-rank percentile in canonical order (LANG-104) |
| `index!(…)`, `seq!(…)`, `fold!(…)`, `reduce!(…)` | §10.5, §10.6 |
| `ola_sum!(…)`, `ola_count!(…)`, `ola_avg!(…)` | §10.9 |
| `name!(…)` for a user aggregate | §10.8 |

**BLS1004** (on by default) warns when a `_` in the body drops a column that is not functionally determined by the
named variables, under a `count!(*)`, `sum!` or `avg!`: two contributions that differ only in that column collapse.
Bind the column to a named variable (`_id` if unused, §2.6).

Lowering follows Molly's split, so body variables that do not reach the head cannot change the grouping:

```ir
M::finish$when(W, S, L, P) :- all_done(), seen(W, S, L, P).
word_count(W, count<(S, L, P)>) :- M::finish$when(W, S, L, P).      // count!(*) grouped by w
eff$u(X) :- current_term(X).                                        // view eff(t = max!(x)) { … }
eff$u(X) :- heard_term(X).
eff(max<X>) :- eff$u(X).
```

### 10.2 Defaults and drivers (LANG-106, ODD-03)

`per r(…)` in a (single-alternative) view marks the **driver**: the view produces one row per driver tuple even
when the rest of the body matches nothing. The driver's variables must include every grouping column, and any other
driver variable must be functionally determined by them (BLS0511). With a driver, `count!`, `sum!`, `collect!`,
`collect_set!`, `collect_map!`, `bool_or!` (false), `bool_and!` (true) and `fold!` (its `init`) use their identity;
every other aggregate needs `default e` inside its parentheses. A view with no grouping columns may use
`default e` without a driver (the driver is the unit relation).

```blossom
view acks_so_far(r, n = count!(a)) = per pending(r), ack(r, a);
view best_bid(i, p = max!(b default 0)) = per item(i), bid(i, _, b);
view metered(total = sum!(size default 0)) = accepted(_id, size);
```

```ir
acks_so_far$u(R, A) :- pending(R), ack(R, A).
acks_so_far$a(R, count<A>) :- acks_so_far$u(R, A).
acks_so_far(R, N) :- acks_so_far$a(R, N).
acks_so_far$ak(R) :- acks_so_far$a(R, _).
acks_so_far(R, 0) :- pending(R), notin acks_so_far$ak(R).
metered$a(sum<Size>) :- accepted(Id, Size).                         // over distinct (Id, Size)
metered(T) :- metered$a(T).
metered$ak() :- metered$a(_).
metered(0) :- notin metered$ak().
```

### 10.3 Exemplary and order filters (LANG-093, LANG-103, LANG-104, LANG-118)

These are body literals; each keeps a subset of the body's valuations and is a negative edge.

- `argmin!(c per g)`, `argmax!(c per g)`: the valuations whose `c` is least (greatest) within group `g`, **every
  tie** included (`per` omitted: one global group).
- `top!(k by keys per g)`: the first `k` valuations per group in (keys, canonical) order; `by l desc` gives the
  largest.
- `limit!(k per g)`: the first `k` valuations per group in canonical order.

Ties are always broken by the canonical order of the whole valuation, so the result is deterministic; ANA-038's D2
lint reports where a tie-break actually happens ("ties broken by canonical order").

```ir
best_hop$m(A, C, min<Cost>) :- via_cost(A, C, Via, Cost).          // argmin!(cost per (a, c))
best_hop(A, C, Via) :- via_cost(A, C, Via, Cost), best_hop$m(A, C, Cost).
```

### 10.4 Choice (LANG-108, LANG-114–116, CR-45, SEM-084–087)

```blossom
choose!(c per t)                     // one c per t per tick: the least seeded priority
choose!((c, id, v) per key most (c, id))   // greedy: the greatest cost first, then the seeded priority
choose_most!((c, id, val) per key)   // shorthand for choose!((c, id, val) per key most (c, id, val))
choose_least!(c per t)               // shorthand for choose!(c per t least c)
choose_rand!(n)                      // one uniformly random n per tick, per node (no `per`: one global group)
choose!(y per x sticky)              // keep last tick's choice while it is still a candidate (LANG-115)
likes(a, b), choose!(b per a), choose!(a per b)             // several choices: a greedy matching (LANG-116)
```

`choose!(Ȳ per X̄ …)` is a body literal that enforces the functional dependency X̄ → Ȳ (plus the cost, for
`least`/`most`) over the body's candidate valuations **in this tick**: node and tick are always in the determinant,
and a tick-free FD cannot be written (SEM-085). Variables of the body that are in neither X̄ nor Ȳ are not
constrained; list them in Ȳ to choose whole valuations. Semantics (CR-45):

- `choose!` picks, per X̄, the candidate with the least priority `(PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ)`, where `σc` is
  the choice seed shared by all nodes (ODD-38), so nodes that see the same candidates agree.
- `least c` / `most c` order by the cost `c` first, then the priority, then canonical order (GZ01's greedy choice).
  When the cost is unique per group the seed never matters (ANA-038 D5).
- `choose_rand!` keys the PRF with the node seed, the incarnation and the tick: a fresh draw every tick
  (schedule-dependent, SEM-087).
- `sticky` keeps the previous tick's choice while it is still a candidate, carrying a `held` relation with `@next`;
  `sticky durable` makes `held` durable (`choose!(y per x sticky durable)`). Protocol decisions that must be
  permanent are persisted explicitly, not made sticky (LANG-115).
- Several choice literals in one body are one multi-FD site: candidates are scanned greedily in seeded-priority
  order and each is accepted iff it is consistent with every FD given those already accepted (LANG-116).
- Overrides (TEST-012) beat a sticky keep, which beats priority; an override naming a non-candidate is a hard error.

Restrictions: a choice site may not lie on a same-tick recursive cycle (SEM-086, BLS0503); lattice-typed variables
may not be in Ȳ (BLS0604); the site must be in a labelled handler or a single-alternative view (BLS0600). Every
output reached by a choice is labeled with its nondeterminism class unless ANA-038 proves it forced or local
(LANG-108).

Lowering of `fresh_grant` (site `M::fresh_grant::choose#0`):

```ir
fg$cand(T, C) :- rv_ok(C, T), notin voted_for$p1(T).                           // the candidates (X̄ = T, Ȳ = C)
fg$pmin(T, min<P>) :- fg$cand(T, C), P := $prio("M::fresh_grant::choose#0", (T), (C)).
fg$chosen(T, C) :- fg$cand(T, C), fg$pmin(T, P), P == $prio("M::fresh_grant::choose#0", (T), (C)).
fresh_grant(C, T) :- rv_ok(C, T), notin voted_for$p1(T), fg$chosen(T, C).
```

`$prio(site, X̄, Ȳ)` is `(PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ)`; `least c` replaces it by `(c, $prio(…))` and `most c` by
`(neg(c), $prio(…))` in the canonical order; `choose_rand!` uses `$rprio`, which adds `σ_node`, the incarnation and
the tick. The sticky expansion (R12 §5.5) adds:

```ir
s$keep(X, Y) :- s$held(X, Y), s$cand(X, Y), notin s$forced(X).
s$kept(X) :- s$keep(X, _).
s$pminf(X, min<P>) :- s$cand(X, Y), notin s$kept(X), P := $prio(site, X, Y).
s$fresh(X, Y) :- s$cand(X, Y), notin s$kept(X), notin s$forced(X), s$pminf(X, P), P == $prio(site, X, Y).
s$chosen(X, Y) :- s$keep(X, Y).
s$chosen(X, Y) :- s$fresh(X, Y).
s$chosen(X, Y) :- s$ovr(X, Y).
s$held(X, Y)@next :- s$chosen(X, Y).
```

(`s$ovr`/`s$forced` come from the simulator's `__choice` override input and are empty in production.) The
multi-FD scan is `s$acc := fold_ordered(∅, s$step, s$cand order by ($prio(site, C̄), C̄))` over the whole candidate
tuple C̄, followed by `… :- …, $member(s$acc, C̄)` (§10.6).

### 10.5 Numbering: `index!` and `seq!` (LANG-097, LANG-098, CR-46)

Both are head-only (a view column or a statement head argument), because they number the **deduplicated head
tuples**, not body rows.

- `index!([by keys] [per g])`: the dense 0-based rank of each head tuple within its group, in (keys, canonical)
  order, recomputed every tick. Over a persistent input it re-ranks every tick; ANA-011 lints it (BLS0601).
- `seq!([by keys] [per g] [durable] [release])`: each distinct head tuple gets the next number the first tick it
  appears (within that tick, in (keys, canonical) order); numbers are never reused. `release` drops a tuple's
  number when the tuple leaves (the number is still never reused). The state must be `durable` if the numbers reach
  a `send` or an `output` (ANA-011, BLS0602). A `seq!` site needs a stable id (BLS0600).

```ir
// view slot(p, i = index!()) = client_req(p), is_leader();
slot$h(P) :- client_req(P), is_leader().
slot$lt(P, P2) :- slot$h(P), slot$h(P2), (P2) <c (P).         // <c: canonical order, `by` keys first
slot$a(P, count<P2>) :- slot$lt(P, P2).
slot(P, I) :- slot$a(P, I).
slot$ak(P) :- slot$a(P, _).
slot(P, 0) :- slot$h(P), notin slot$ak(P).
// seq!(durable) at site s (R12 §5.9):
s$h(R) :- <head tuples>.
s$assigned(R, I)@next :- s$assigned(R, I).                     // durable
s$has(R) :- s$assigned(R, _).
s$new(R) :- s$h(R), notin s$has(R).
s$nrank(R, J) :- <index of s$new by (keys, canonical)>.
s$ncount(count<R>) :- s$new(R).
s$hwm(0) :- boot(), notin recovered().
s$hwm(H) :- s$hwm$p(H).
s$hwm$p(H + C)@next :- s$hwm(H), s$ncount(C).                   // durable
s$ncountk() :- s$ncount(_).
s$hwm$p(H)@next :- s$hwm(H), notin s$ncountk().
head(R, I) :- s$h(R), s$assigned(R, I).
head(R, H + J) :- s$nrank(R, J), s$hwm(H).
s$assigned(R, H + J)@next :- s$nrank(R, J), s$hwm(H).
```

The engine sorts instead of evaluating the quadratic reference; ENG-067 checks that the two agree (ENG-072).

### 10.6 Ordered folds and `reduce!` (LANG-109, LANG-110)

`fold!(init, step, elem by keys)` left-folds the pure function `step(acc, elem)` over the group's valuations in
(keys, canonical) order. It is a head aggregate: an empty group has no row (the aggregate form). With a `per`
driver (§10.2), an empty group yields `init`, which is LANG-110's carried form:

```blossom
fn apply(s: KvState, cmd: Cmd) -> KvState { s.apply(cmd) }
view next_state(s2 = fold!(s, apply, cmd by i)) = per sm(s), to_apply(i, cmd);
while next_state(s2) { next sm(s2); }
```

```ir
f$rk(I, Cmd, K) :- to_apply(I, Cmd), K = index<by (I)>.
f$acc(0, S) :- sm(S).
f$acc(K + 1, S2) :- f$acc(K, S), f$rk(I, Cmd, K), S2 := apply(S, Cmd).
f$n(count<(I, Cmd)>) :- to_apply(I, Cmd).
next_state(S2) :- f$acc(N, S2), f$n(N).
f$nk() :- f$n(_).
next_state(S) :- sm(S), notin f$nk().
```

The recursion through `f$acc` is positive and bounded by the row count, so it is accepted (SEM-020). A carried fold
over a persistent input re-applies every row every tick, as Dedalus requires; ANA-011 lints it (BLS0601), and the
idiom is to fold only new rows (`last_applied < i <= commit`). `reduce!(f, elem)` is `fold!` without an `init` over a
non-empty group; it is evaluated in canonical order unless `f` is declared `#[commutative, associative]` and the
claim is proved (TEST-087).

### 10.7 Resolution policies (LANG-117, ODD-02 (c))

Without a policy, two tuples with one key at one tick are an error (SEM-050), and two different upserts to one key
in one tick are an error (SEM-051). A policy replaces the error with a declared, deterministic choice.

```blossom
durable table current_term(t: u64) key() resolve choose_most(t);     // relation level: terms only grow
durable table reg(k: String, ts: (u64, Node), v: Bytes) key(k) resolve choose_least(ts);   // LWW over a tie-free ts
upsert kv(k, v, order) resolve choose_most(order);                   // statement level
```

- **Relation level** (`key(…) resolve P`): the candidates for a key are every tuple that would be present at t+1:
  persisted tuples not deleted, `next` inserts and upserts. `P` picks one. Same-tick `emit`s remain subject to
  SEM-050.
- **Statement level** (`upsert … resolve P`): the candidates are that tick's upserts to the key.
- Policies: `choose`, `choose sticky` (first writer wins), `choose_rand`, `choose_least(e)`, `choose_most(e)` (`e`
  over the relation's columns), and `merge` (only when every non-key column is a lattice, where it is the default
  anyway). Site id `M::relation::resolve` (§4.3). A resolved relation may not lie on a same-tick cycle (SEM-086).

```ir
reg$cand(K, TS, V) :- reg(K, TS, V), notin reg$del(K, TS, V).
reg$cand(K, TS, V) :- reg$n(K, TS, V).                          // every `next`/`upsert` into reg targets reg$n
reg$cmin(K, min<TS>) :- reg$cand(K, TS, V).
reg(K, TS, V)@next :- reg$cand(K, TS, V), reg$cmin(K, TS).      // replaces reg's frame rule
```

**Writer precedence: `resolve prefer(rule, …)`.** Two handlers writing one key in one tick is an error, and an
accident must not pass silently; but some programs mean it — a snapshot install resets a partition's log end in the
tick materialization would advance it. A table may name, by handler label, whose writes win (EXTENSIONS 2.4):

```blossom
table log_end(tid: Bytes, part: i32, next: i64) key(tid, part) resolve prefer(reset_log, advance_end);
```

Among one tick's `next` and `upsert` writes to a key, those of the earliest listed handler survive; the others are
dropped. Survivors then apply as their verb does, so two different values from one listed handler, or a write by a
handler the list does not name next to any other value, still conflict (SEM-050, SEM-051). `emit` and `delete` are
not arbitrated. The table keeps its frame rule (it is not a resolved table: persisted rows are not candidates, and
an `upsert` still replaces its key's row). Each name must label a handler of the module that writes the table with
`next` or `upsert`, once, and label only one handler (BLS0411); `prefer` needs a key and plain values. Lowering — writes are staged with their
handler's rank (unlisted ones apart), and the least rank per key goes on:

```ir
log_end$w(T, P, N, R, U) :- <body of each listed write>, R = <its handler's position>, U = <is an upsert>.
log_end$wx(T, P, N, U)   :- <body of each unlisted write>.
log_end$wmin(T, P, min<R>) :- log_end$w(T, P, N, R, U).
log_end$ups(T, P, N) :- log_end$w(T, P, N, R, true), log_end$wmin(T, P, R).     // then as any upsert
log_end$ups(T, P, N) :- log_end$wx(T, P, N, true).
log_end(T, P, N)@next :- log_end$w(T, P, N, R, false), log_end$wmin(T, P, R).   // and as any `next`
log_end(T, P, N)@next :- log_end$wx(T, P, N, false).
```

### 10.8 User-defined aggregates and combiners (LANG-105, LANG-112)

```blossom
#[commutative, associative]
aggregate mean(x: f64) -> f64 {
    type State = (f64, u64);
    init = (0.0, 0);
    step = |s, x| (s.0 + x, s.1 + 1);
    combine = |a, b| (a.0 + b.0, a.1 + b.1);
    finish = |s| if s.1 == 0 { 0.0 } else { s.0 / (s.1 as f64) };
}
view avg_latency(svc, m = mean!(ms)) = sample(svc, _id, ms);
```

`type State`, `init` and `step` are required; `combine` is required when the aggregate is declared
`#[commutative, associative]`; `finish` defaults to the identity. The claims are checked by the law and shuffle
harnesses (TEST-015, TEST-087): a refuted claim is BLS0704; a claim that is only tested lets the engine combine in any
order but not upgrade retries (ANA-015). Without proved commutativity and associativity the aggregate is evaluated
as `fold!` in canonical order, so it is still deterministic. From `combine` the compiler derives the sender-side
partial aggregate for partitioned channels (LANG-112): a lattice merge gives `merge`, a commutative monoid a partial
fold, `avg!` and variance the moment tuple `(n, Σx, Σx²)`; holistic aggregates get none. Non-idempotent partials
cross a channel only through an `exactly_once` channel (§11.10, DIST-011).

### 10.9 Estimators and quorum sugar (LANG-111, LANG-113)

- `ola_sum!(x, conf)`, `ola_count!(conf)` and `ola_avg!(x, conf)` return `(estimate, lo, hi)` over block-level
  samples, with the large-sample CLT interval; an optional last argument `Interval::Hoeffding(lo, hi)` selects the
  conservative Hoeffding interval. `scale_by_progress!(e)` is HOP's scale-up, always labeled biased. Every estimator
  is implicitly `#[nondet("progressive")]` and states its assumptions in the catalog (LANG-113).
- `majority(s, R)`, with `s` a set-like lattice of nodes and `R` a role or a closed unary relation, is the
  threshold |s ∩ R| > |R| / 2 (§11.6). The verifier maps it to a quorum sort with the intersection axiom (VER-008).

### 10.10 Progressive snapshots (LANG-139)

```blossom
snapshot wc_snap of word_count at progress every 0.1 upto 0.9 mode committed_only estimate scale_by(coverage);
```

A `snapshot` item publishes `wc_snap(point, actual_progress, class, attempt, value)` each time the producers' mean
progress (a threshold on the progress lattice, producers not yet heard from counting as 0) crosses a point: `every Δ
upto p` or an explicit list `(p1, p2, …)`. `mode` is `committed_only` (the default) or `include_tentative`;
`estimate` names an estimator (`scale_by(progress)`, `scale_by(coverage)`). It lowers to a `reveal!` of the source
gated by the threshold, is implicitly `#[nondet("progressive")]`, and carries the ANA-036 class; class-L snapshots are
certified to be a chain of lower bounds (CR-33).

---

## 11. Lattices, weighted collections and groups

### 11.1 Lattice-valued relations (LANG-120, LANG-121, LANG-128, CR-24, CR-50, CR-51)

A relation with a lattice-typed column is lattice-valued (SEM-100): its key is its key columns (by default every
non-lattice column; a lattice column is never a key, a join key or a group key, BLS0304), and two derivations with
the same key merge instead of conflicting. Non-lattice payload columns outside the key still obey SEM-050.

- **Persistence.** A lattice `table` or `cell` persists by default (CR-24): the IR gives it the implicit identity
  rule `r(K̄; X)@next :- r(K̄; X).` (SEM-104), so its value only grows (SEM-030). A `scratch` table, a `scratch
  cell`, and a view with lattice columns are tick-scoped: ⊥ at the start of every tick.
- **No retraction.** `delete` and `upsert` on a lattice-valued relation are BLS0410 (LANG-284). A lattice is reset
  monotonically by raising an epoch: `Lex<LMax<u64>, L>` discards the old component when the epoch grows (§11.9).
- **⊥-normalization** (SEM-101). A cell whose value is ⊥ is absent. Writing ⊥, merging ⊥ or sending ⊥ is a no-op:
  no message, no lineage leaf, no wake-up.

### 11.2 Writing lattices (LANG-122, LANG-123)

`emit r(k̄, v)` merges `v` into the cell now (Bloom `<=`); `next r(k̄, v)` merges at t+1 (Bloom `<+`). Both sides
have the same lattice type after lifting (§5.6). A collection becomes a lattice by the implicit fold: every
valuation contributes a singleton and the key merges them.

```blossom
count_vote: on vote(t, true) from v { emit votes(t, set[v]); }       // set[v] lifts into LSet<Node<Server>>
emit carts(s, Cart { ops: map[op => LPoint::of(CartOp { item, delta })] });   // a product literal
```

```ir
votes(T; {V}) :- M::count_vote$when(T, V).                            // ⊔ under key (T)
votes(T; X)@next :- votes(T; X).                                      // the implicit identity rule
```

A struct literal of a product lattice (§11.8) may omit fields; the omitted fields are ⊥ (`Cart { expect:
LPoint::of(n) }`). This is the only place a struct literal may omit fields.

### 11.3 Reading lattices (LANG-280, LANG-129)

- **Generator.** A positional or named atom `votes(t, s)` ranges over the non-⊥ cells only (SEM-101 N4) and binds
  `s` to the cell's value; `s` is known non-⊥ (§5.6).
- **Lookup.** `votes[t]` with `t` bound is the value, ⊥ if the cell is absent (N5). A cell is read only by lookup:
  its name used as an expression (`clock`) is `clock[]`.
- **Typed ⊥.** `m.at(k)` on a missing key is ⊥ of the value lattice (LANG-129).

### 11.4 Operation classes and the bang rule (LANG-125, LANG-126, LANG-127, SEM-102)

Every lattice operation has a class for each argument: morphism (M), bimorphism (BM), monotone (Mon), antitone
(Anti) or non-monotone (NM). R04 §2.4 is normative for the built-ins; §11.5 lists them. The syntax follows the
class:

- **M, BM, Mon operations and thresholds have no bang**: `m.at(k)`, `s.intersect(t)`, `s.size()`, `x + 1` on an
  `LMax`, `a.join(b)`, `x >= c` on an `LMax`, `x in s`.
- **Anti and NM operations need a bang**: `a.leq!(b)`, `vc.concurrent!(w)`, `s.is_empty!()`, `s.difference!(t)`,
  `x.val!()` on a `Lex`, `reveal!(x)`. A user method or function with no class (§11.8) is NM.
- **The check is asymmetric** (the tooling judge's refinement of A's rule): a missing bang is BLS0700, an error
  with a fix-it that inserts it; a superfluous bang is BLS0701, a warning, so reclassifying a library method as
  monotone never breaks callers.
- **Comparisons with a scalar** are allowed only in the threshold direction: `x >= c` and `x > c` on `LMax`,
  `x <= c` and `x < c` on `LMin`, with `c` a scalar over bound variables. The other direction is BLS0306, which
  suggests `reveal!(x) <= c` (an exact read) or `not (x > c)`. Two lattice values are compared with methods
  (`a.leq!(b)`), because comparison is antitone in one argument.
- **`==` and `!=` on lattice values** are BLS0305 (Bud's whitelisting of `==` is unsound for lattices, R04 §2.4).
- **Defaults are joins.** `x.join(c)` with a constant is the monotone way to give a lattice read a default
  (`clock.at(self).join(0)`, LANG-283). There is no `??` operator.
- **`reveal!(x)`** is the one exact read (LANG-127). It is deep: nested lattices reveal to plain values. Its type
  `R(L)`: `LBool` → `bool`; `LMax<T>`, `LMin<T>`, `LPoint<T>` → `Option<T>`, or `T` when `x` is known non-⊥;
  `LSet<T>`, `LPSet<T>` → `Set<T>`; `LBag<T>` → `Map<T, u64>`; `LMap<K, L>` → `Map<K, R⁺(L)>`, where `R⁺` is the
  non-⊥ reveal (map entries are never ⊥, so `VClock` reveals to `Map<Node, u64>`); a product lattice → the struct
  of its fields' reveals; `Lex<K, L>` → `(R(K), R(L))`. Inside a method of a lattice's own `impl`, `reveal!` is
  free: the analysis trusts the method's declared class, which is a checked claim (§11.8).

The polarity analysis of SEM-102 classifies every occurrence: an occurrence is monotone if every path from it to
the head or a guard has an even number of Anti operations and no NM operation; otherwise it is exact, and an exact
occurrence is a negative edge (§13.2). The bang makes every such occurrence visible in the source.

### 11.5 The built-in lattices (LANG-124, LANG-130–134, LANG-136, LANG-281, LANG-282)

| Type | ⊥ | Merge | Monotone operations (no bang) | Bang operations |
|---|---|---|---|---|
| `LBool` | `false` | or | as a guard (`when_true`); `a.and(b)` BM; `a.or(b)` | `a.not!()` Anti |
| `LMax<T>` (`T` totally ordered, not `f64`) | adjoined −∞ (ODD-50) | max | `x >= c`, `x > c` thresholds; `x + c`, `x - c` M; `x.min_of(c)` M; `a + b` BM | `a.leq!(b)`, `a.lt!(b)` |
| `LMin<T>` | adjoined +∞ | min | `x <= c`, `x < c` thresholds; `x + c` M; `a + b` BM (tropical) | `a.leq!(b)` |
| `LSet<T>` | ∅ | ∪ | `s.contains(x)` / `x in s` thresholds; `x in s` generator M; `s.intersect(t)`, `s.product(t)` BM; `s.map(f)`, `s.filter(p)` M (with morphism `f`/pure `p`); `s.size()` Mon (→ `LMax<u64>`); `s.nonempty()` threshold; `s.min_elem()` M (→ `LMin`), `s.max_elem()` M (→ `LMax`) | `s.is_empty!()` Anti; `s.difference!(t)` Anti in `t`; `s.leq!(t)` |
| `LPSet<T>` (non-negative numbers) | ∅ | ∪ | as `LSet`; `s.sum()` Mon (SUM DISTINCT) | as `LSet` |
| `LBag<T>` | ∅ | per-element max multiplicity | `b.multiplicity(x)` M (→ `LMax<u64>`); `b.contains(x)`, `b.intersect(c)` M; `a + b` BM; `b.size()` Mon | — |
| `LMap<K, L>` | empty (⊥ entries absent) | key union, values joined | `m.at(k)` M (⊥ if absent); `m.has_key(k)` threshold; `m.key_set()` M (→ `LSet<K>`); `m.intersect(n)` BM (values joined); `m.map_values(f)` M; `(k, v) in m` generator M; `m.size()` Mon; `m.sum_values()` Mon when values are `LMax<N ≥ 0>` (LANG-282) | `m.leq!(n)` |
| product lattices (`lattice X { … }`), `LPair<A, B>` | all fields ⊥ | fieldwise | field access M | — |
| `Lex<K, L>` (LANG-131; `K` a chain) | (⊥, ⊥) | lexicographic; incomparable keys give (k ⊔ k′, ⊥) | `x.key()` M | `x.val!()` NM |
| `LDom<V, L>` (LANG-132; MV-register) | ∅ | keep the pairs no other pair dominates | `x.version()` M | `x.value!()` NM |
| `LPoint<T>` | ⊥ | x ⊔ x = x; merging two different values is a hard `Conflict` error (BLSR006) | `x.get()` threshold (→ `Option<T>`) | — |
| `LConflict<T>` | ⊥ | two different values merge to ⊤ | `x.get()`, `x.is_top()` thresholds | — |
| `LWithBot<L>`, `LWithTop<L>` | added ⊥ / ⊤ | as `L` | `x.is_top()` threshold; `L`'s operations | — |
| `LUnit` | the only value | — | — | — |
| `LVec<L>` | `[]` | pointwise; the longer wins | `v.at(i)` M | — |
| `LUnionFind<T>` | all singletons | union of classes | `u.same(a, b)` threshold | — |
| `LTombSet<T>`, `LTombMap<K, L>` (LANG-133) | ∅ | union of adds and of tombstones | `x.added(e)`, `x.removed(e)` thresholds | `x.live!(e)` NM |
| `Causal<DotSet>`, `Causal<DotFun<V>>`, `Causal<DotMap<K, L>>` (LANG-134) | empty | causal join | `x.context()` M (→ `VClock`) | `x.read!()` NM |
| every lattice | | | `a.join(b)` BM; `x.join(c)` M | `reveal!(x)`; `x.is_bot!()` Anti |

Prelude aliases: `type VClock = LMap<Node, LMax<u64>>;` (LANG-130; its happens-before and concurrency tests are
`a.leq!(b)`, `a.lt!(b)`, `a.concurrent!(b)`); `type Ballot = Lex<LMax<u64>, LMax<Node>>;`;
`type Lww<T> = Lex<LMax<(u64, Node)>, LPoint<T>>;`. Constructors: `LMax::of(x)`, `LMin::of(x)`, `LSet::of(x)`,
`LMap::of(k, v)`, `LPoint::of(x)`, `Lex::of(k, v)`, `LBool::of(b)`, and `X::bot()` for every lattice.
`DomPair<K, V>` is not associative (CR-25) and exists only as `unsafe DomPair<K, V>` in a type, which the compiler
reports (LANG-136, BLS0706).

### 11.6 Thresholds (LANG-126, LANG-111, LANG-212)

A threshold is a monotone map into `bool` (or `Option<T>` whose `Some` values never change): once true, true in
every larger state. Thresholds are the only monotone way to read a plain value out of a lattice; as guards they are
positive literals and are THRESH-final (ANA-120).

- The comparisons of §11.4, `contains`, `x in s`, `has_key`, `nonempty`, `is_top`, `get`, an `LBool` used as a guard.
- `threshold(x, t1, …, tn)` returns `Option<u32>`, the index of the one `ti` that `x` has reached (`x ⊒ ti`). The
  `ti` must be pairwise incompatible (their join is ⊤ or a Conflict), which the compiler checks for constants and
  the law harness for the rest (BLS0707); so the result never changes once set.
- `majority(s, R)`: |s ∩ R| > |R| / 2 for a set-like lattice `s` and a role or closed unary relation `R`. The
  cardinality of a closed relation is not a point of order. The verifier maps it to a quorum sort (VER-008).
- `when_final(e)` and the literals `final r(…)`, `final not r(…)` (§14.5): finality is monotone.
- `cluster_version() >= V` (§19.4).
- A user method declared `threshold fn` (§11.8).
- **Stable reads.** A method declared `stable fn m(self) -> T after t` promises that once the threshold method `t`
  holds, `m` never changes as `self` grows (Bloom^L's "monotone, then immutable", C's `stable`). A call of `m` is
  classified monotone when the same body guards it with `x.t()`; otherwise it is NM and needs a bang (BLS0703).
  ANA-142/143 use this.

### 11.7 Lattice folds in expressions (LANG-123)

`lset{ e | B }`, `lmax{ e | B }`, `lmin{ e | B }`, `lbool{ c | B }`, `lbag{ e | B }`, `lpset{ e | B }` and
`lmap{ k => v | B }` fold the singletons of `e` over the valuations of body `B` into a lattice value. They are
monotone (a morphism from the input set) and need no bang, unlike the relational aggregates of §10.1:
`max!` is a plain value that can go down when an input tuple goes away; `lmax{ … }` is an `LMax` read through
thresholds. Variables of the enclosing body used inside `B` are correlated; each must also be bound by a positive
literal of `B` (BLS0500). An empty fold is ⊥.

```blossom
view quorum_ok(t) = term(t) where lset{ v | vote(t, v) }.size() >= QUORUM;
```

```ir
fold$4be1(T; {V}) :- vote(T, V).                         // a tick-scoped lattice relation keyed by the correlation
quorum_ok(T) :- term(T), S = fold$4be1[T], $size(S) >= QUORUM.     // lookup: ⊥ (size 0) when there are no votes
```

### 11.8 User-defined lattices (LANG-135, ODD-09 (c))

```blossom
lattice Ballot2 = Lex<LMax<u64>, LMax<Node>>;           // (1) composition of built-ins: nothing to check
lattice Cart {                                          // (2) a product: fieldwise merge, correct by construction
    ops: LMap<u64, LPoint<CartOp>>,
    expect: LPoint<u64>,
}
extern lattice Hll = "blossom_sketch::Hll" {            // (3) a Rust type implementing the Merge trait
    monotone fn estimate(self) -> LMax<u64>;
    morphism fn contains(self, x: Bytes) -> LBool;
}
impl Cart {
    threshold fn complete(self) -> bool { … }
    stable fn summary(self) -> Map<String, i64> after complete { … }
    fn missing(self) -> Set<u64> { … }                  // no class: NM, callers write c.missing!()
}
```

Method classes are declared with the `fn` prefix: `morphism` (join-preserving, CR-23), `bimorphism`, `monotone`,
`antitone`, `threshold`, `stable … after t`; a method with no class is NM. Every class is a law obligation: the law
harness (TEST-083) tests it and the SMT backend proves it where the fragment allows (VER-014); a refuted claim is
BLS0704, and the result is reported "proven" or "tested". Composition and product lattices are "proven" for merge,
⊥ and order; `extern` lattices are "tested". Method bodies are pure and may `reveal!` freely. A group or ring type can
never be declared a lattice (LANG-142).

### 11.9 Lattices in messages; monotone reset (LANG-137, LANG-284, CR-52)

A channel column may be a lattice. The channel's key is its non-lattice columns, so the sender merges all of a tick's
values per (destination, key) into one message, and the receiver merges same-key arrivals within one delivered
batch; accumulation across ticks needs a persistent sink (`on sync(vc) { next clock(vc); }`). SEM-109: a channel
may carry deltas instead of full values only when every consumer is a join-morphism into persistent state.

Monotone reset: `table round_votes(key: String, v: Lex<LMax<u64>, LSet<Node>>);` and
`next round_votes(k, Lex::of(e + 1, set[]))` raise the epoch, which discards the old set.

### 11.10 Weighted collections, groups and exactly-once channels (LANG-138, LANG-142, LANG-158, CR-35)

```blossom
zset table stock(sku: String);                           // ℤ weights; `bag table` for ℕ weights (insert-only)
on received(s, _id) { emit stock(s); }                   // weight 1
on shipped(s, _id) { emit stock(s) weight -1; }
view in_stock(s) = distinct!(stock(s));                  // weight > 0
view qty(s, n) = clamped!(stock(s), n);                  // n = max(w, 0), rows with n > 0
view raw(s, w) = weights!(stock(s), w);                  // the raw non-zero weight
#[fault(lossy)]
channel stock_delta(delta: ZSet<String>) exactly_once(dots);
```

- Weights are checked `i64`; overflow is a hard runtime error (BLSR004). A `bag` must be insert-only, which ANA-030
  must prove; a possible negative contribution is BLS0313.
- Z-set relations live in the IR's Z-set stratum (ENG-062). Rules whose head is a Z-set relation may read Z-set
  atoms directly (linear and bilinear operators). Any other rule must read a Z-set through one of the three banged
  views, because the edge from a Z-set stratum into a set or lattice stratum is negative (ODD-06 amendment,
  BLS0700 otherwise). A Z-set is never final without a seal (SEM-017).
- Group and ring types (LANG-142): `Z` (checked `i64`), `Zn<N>`, `ZSet<T>`, tuples and maps of these, and user
  types with `impl Group for T { fn zero() -> T { … } fn add(a: T, b: T) -> T { … } fn neg(a: T) -> T { … } }`
  (`impl Ring` adds `one` and `mul`). Each operator over a group-typed collection is classified linear, bilinear or
  non-linear (ENG-062).
- A group-typed payload crosses nodes **only** through a channel declared `exactly_once(dots | cumulative | tree)`
  (default `dots`, ODD-26); a group-typed column on a plain channel is BLS0307 (ANA-015). The compiler inserts the
  wrapper (DIST-015/016/017, a lattice stratum); updates in one tick are summed into one payload and a zero sum sends
  nothing; the receiver's `unwrap` (ENG-070) turns wrapper Δ into Z-set Δ exactly once per dot (SEM-036).

```ir
decl zset stock(sku: String)
stock(S) += 1  :- M::h1$when(S, Id).
stock(S) += -1 :- M::h2$when(S, Id).
in_stock(S) :- $zweight(stock, (S), W), W > 0.              // negative edge out of the Z-set stratum
stock_delta(@R; D)@async :- …, D := $zdelta(stock).         // wrapped (W2)
stock(S) += W :- $unwrap(stock_delta, D), $member($entries(D), (S, W)).
```

---

## 12. The operator × collection legality matrix (LANG-066)

Checked statically (ANA-005). A forbidden combination is BLS0400 naming the verb, the target's kind and the legal
alternatives.

| Target \ verb | `emit` | `next` | `send` | `delete` | `upsert` | `seal` |
|---|---|---|---|---|---|---|
| `table`, `durable table`, `soft table` | insert now (and persist) | insert at t+1 | ✗ | ✓ | ✓ | ✓ if `sealed by` |
| `sealed table` | bootstrap only | bootstrap only | ✗ | ✗ | ✗ | ✗ (automatic) |
| `table … range(c)` | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ if `sealed by` |
| `zset table`, `bag table` | `+w` now | `+w` at t+1 | ✗ | ✗ | ✗ | ✗ |
| lattice-valued table, `cell`, `durable cell` | merge now | merge at t+1 | ✗ | ✗ (LANG-284) | ✗ | ✓ (table) if `sealed by` |
| `scratch`, `scratch cell`, scratch lattice table | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| own `output` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| an instance's `input`; an interposition's writable side | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| own `input`, an instance's `output`, a relation parameter, a `view` | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| `channel` | ✗ | ✗ | ✓ | ✗ | ✗ | ✓ if `sealed by` |
| `loopback` | ✗ | ✗ | ✓ (no `to`) | ✗ | ✗ | ✗ |
| `static` | ✗ (only `fact`) | ✗ | ✗ | ✗ | ✗ | ✗ |
| timers, `boot`, `recovered`, `stdin`, sessions, service results, `#[readonly]` | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| `stdout`, a `service` call | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ |
| `halt` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| `localtick` | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ |

Further rules: a `send` of `c: A -> B` must be placed at `A` and its atoms read at `B` (BLS0404); `fact` targets only
`static` relations (and, in specs, inputs with `at tick`) (BLS0405); `bootstrap` (not `fresh`) may not write a
durable relation (BLS0402); in a `migrate` block only `while` handlers with `emit` are allowed (§19.3).

---

## 13. Stratification, time, monotonicity and determinism as the user sees them

### 13.1 The dependency graph

The compiler builds the dependency graph of the lowered program. Each IR rule `h :- b` contributes an edge from every
relation read in `b` to `h`, labeled with its **kind** (same-tick for deductive rules; next for inductive rules;
async for async rules) and its **polarity**. In surface terms: `emit` and view alternatives give same-tick edges;
`next`, `delete` and `upsert` give next edges (a `$del`/`$ups` scratch is read only by the frame rule, so deletion
never creates a same-tick cycle); `send` gives async edges. Every header and block contributes the edges of its
`$when` rule.

### 13.2 Negative edges: the points of order (SEM-021, ANA-022)

| SEM-021 negative edge | Surface spelling |
|---|---|
| negation | `not r(…)`, `not { … }` |
| a non-lattice aggregate | `count!`, `sum!`, `min!`, `max!`, `avg!`, `collect!`, `collect_set!`, `collect_map!`, `bool_and!`, `bool_or!`, `percentile!`, `ola_*!`, user aggregates `name!` |
| a deletion | the `delete` and `upsert` verbs; `resolve` policies |
| an outer join | `outer` |
| an order-sensitive operator | `index!`, `seq!`, `top!`, `limit!`, `argmin!`, `argmax!`, `fold!`, `reduce!` |
| a choice site | `choose!`, `choose_least!`, `choose_most!`, `choose_rand!` (with `sticky`) |
| an Anti or NM lattice operation | `.name!(…)` methods; `not` applied to a lattice threshold |
| `reveal` | `reveal!(…)` |
| delta relations | `inserted`, `deleted` |
| a Z-set read into a set or lattice stratum | `distinct!`, `clamped!`, `weights!` |
| ∀ over an open domain | `forall` whose domain is not closed (§9.8) |

Everything else is positive: joins, projection, union, `let`, generators, lattice merges, morphisms, bimorphisms,
monotone functions, thresholds, `sealed` tests, finality tests, and `forall` over a closed domain. So every point of
order is a bang call or one of the keywords `not outer inserted deleted delete upsert resolve forall`. The
points-of-order report (ANA-022) lists each with its source span and its label/statement, and is the authority;
the language server highlights them (TEST-092).

### 13.3 Acceptance: temporal stratification (SEM-020, SEM-022, CR-10)

A program is accepted iff its **same-tick** subgraph has no strongly connected component containing a negative edge.
The error (BLS0502) prints the cycle as a path of surface constructs with spans, for example:

```
error[BLS0502]: `seen` depends negatively on itself within one tick
  --> e02_reliable_broadcast.bls:54:12  `if not seen(origin, id)` in handler `receive`
  --> e02_reliable_broadcast.bls:56:13  `emit seen(origin, id)` in handler `receive`
  = help: write `next seen(origin, id)`: a cycle through `next` is accepted
```

Cycles through `next` or `send` are accepted: inductive and async rules read the completed fixpoint and may negate or
aggregate any stratum (SEM-003). Within a tick, a relation's stratum is the longest path to it counting negative
edges; the result does not depend on which valid stratification is chosen (SEM-023). Lattice occurrences get their
polarity from SEM-102 (§11.4), so recursion through morphisms and monotone functions stays in one stratum
(SEM-031), and an in-tick fixpoint that does not converge is a hard runtime error (CR-53, BLSR007), never silent
truncation.

### 13.4 Choice and order stratification (SEM-086)

No choice site and no order-sensitive site may lie on a same-tick recursive cycle, even a positive one (BLS0503,
with the cycle as witness). Each stratum is evaluated as a choice fixpoint over complete candidates. Cycles through
`next` are fine.

### 13.5 `monotone` assertions (ANA-020, ANA-021, ANA-141)

`monotone view`, `monotone on`/`monotone while`, `monotone module` and `monotone choreography` state "this is
coordination-free". The compiler checks that no negative edge of §13.2 occurs in the region (lattice polarity
included); a failure is BLS0702 and names each point of order with its span. A `monotone` module or choreography
whose channels are all guarded (ANA-141: every async head persistent, or ephemeral with join-morphism consumers)
gets the Dedalus+^L confluence certificate printed in the build output. E5's `QuorumWrite` is such a module.

### 13.6 Determinism (SEM-080–088, ANA-011, ANA-038, ANA-039, ODD-10 (c))

Every relation and output gets a class (SEM-087): **deterministic**; **seed-dependent** (stateless and multi-FD
`choose!`, `choose_least!`/`choose_most!` with possible ties); or **schedule-dependent** (`sticky`, `choose_rand!`,
`rand`, `random`, `seq!`, `tick()`, `now()`, and deltas over asynchronous inputs). Classes propagate to outputs
(ANA-039).

- `#[nondet("reason")]` on a handler, view, statement, relation or output records accepted nondeterminism with a
  mandatory reason (LANG-204). It propagates through interfaces and appears in certificates.
- `#[deterministic]` on an output asserts that ANA-039 classifies it deterministic or seed-dependent; a
  schedule-dependent output is BLS0603. This is the determinism marker the semantics judge asked for, enforced by
  analysis rather than by a bang, because determinism is a separate axis from monotonicity.
- The ANA-011 lints (an `index!`/`top!` over persistent input, a carried `fold!` over persistent input, an
  uncaptured `rand` over persistent input, `choose` into a keyed relation with X̄ ⊄ key, `seq!` numbers that escape
  without `durable`) are warnings, and errors under `--strict` (BLS0601, BLS0602, BLS0605). The standard library
  builds under `--strict` (ODD-10 (c)).

### 13.7 What the editor shows that the syntax does not

Four facts depend on types or on the whole program, so the language server shows them as semantic tokens
(TEST-092) rather than the syntax: points of order that are type-directed (a lattice comparison, a `forall` over an
open domain); heads that **merge** into a lattice rather than insert; level-triggered `send`s (resend sites, with
their DIST-007 status); and `send`s whose release waits for a durable commit in the same tick (the durability
barrier, SEM-072).

---

## 14. Distribution: channels, roles, partitioning, seals, finality

### 14.1 Channels and locations (LANG-150–153, CR-14, CR-15)

Local state lives implicitly at `self` (CR-14). The only way to put a fact on another node is `send` into a channel;
every body atom of a program rule is local (LANG-151, ANA-004), except under `#[localize]` (§14.6). The channel
forms, destinations, sender columns, keys and lattice merging are in §7.7; roles, placement, projection and
inferred ACLs in §6.10; sessions in §18.4.

- **Identity.** A channel's wire identity is its instance path and name plus its schema id (DIST-003, CR-41), so
  two instances of one module never exchange messages.
- **Blazes annotations** (ANA-041). `sealed by (k̄)` is the `Seal_k̄` stream annotation. `#[replicated]` on a
  channel or input is the `Rep` annotation: its producers are replicas that emit the same stream, which Blazes label
  propagation (ANA-042) uses. The default annotation is `Async`. Grey-box component annotations (ANA-044, P2) are
  reserved: `#[blazes(…)]` is BLS0907 in this edition.

### 14.2 Fault models (LANG-155, CR-12, SEM-040, SEM-043)

`#[fault(model)]` on a channel declares the delivery the environment provides:

| Model | Meaning |
|---|---|
| `lossy` (default) | a message may be lost, per (sender, receiver, send tick) (SEM-073) |
| `lossy_delayed` | loss is modeled as delay forever |
| `reliable` | no loss except what a crash implies; unordered |
| `reliable_ordered` | reliable, and FIFO per sender |

The model sets the receiving relation's stream properties (ANA-030) and drives the simulator, LDFI and the model
checker. It adds no IR rules. The normative semantics for reasoning and confluence is always fair causal delivery
(SEM-040): loss and crashes are fault models, not semantics (CR-12). Every message is a set element per tick, and a
message to an unknown node is dropped (SEM-040, DIST-004).

### 14.3 Partitioning and routing (LANG-154)

```blossom
channel occ(word: String, split: u32, line: u64, pos: u64): Mapper -> Reducer partition by word;
send occ(w, s, l, p);                              // no `to`: sent to Reducer.route(w)
durable table kv(key: String, val: Bytes) key(key) partition by key;    // placement of state on a cluster role
```

- On a channel, `partition by e` makes a `send` without `to` go to the destination role's owner of `e`:
  `Dst.route(e)`. For a `Node -> Node` channel, `over REL` names a closed unary relation of candidate nodes
  (BLS0804 if missing). Writing `to` on a partitioned channel is BLS0403.
- `R.route(k)` is **rendezvous hashing** over the members of `R`: the member `m` maximizing
  `(PRF_σc("route", fp(k), fp(m)), m)`. Every node computes the same owner, and a membership change moves only the
  keys of the members that changed (unlike `hash % n`). `c.owner(k)` is the owner under a channel's or table's
  partitioning.
- On a `table` placed at a cluster role, `partition by e` declares where each tuple lives: a write at a node other
  than `R.route(e)` is a runtime violation (BLSR008), and a static error where the compiler proves the ownership test
  false. ANA-082 (co-hashing) and Blazes (ANA-043) use the declaration.

```ir
occ(@D, W, S, L, P)@async :- M::map$when(L, S, Text, P, W), D := $route(Reducer, W).
```

### 14.4 Seals and punctuations (LANG-207, CR-27, ANA-046, ANA-063, ANA-065, LIB-066)

A seal says "no more tuples of this relation with this key value will ever appear, and there were `n` of them". The
compiler maintains the count digests, so a seal that overtakes its data is harmless and loss is detected.

**Declaration.** `sealed by (k̄) [producers REL]` on a channel, an input or a table names the seal key. The
**producer set** of a channel `Src -> Dst` is the members of `Src`; for a `Node -> Node` channel it is `REL`, a
closed unary relation, which a unanimous read requires (BLS0803).

**Producer.** `seal c(k: v) to d;` promises that this node will send no more `c` tuples with key `v` to `d`. Without
`to` it seals toward every member of the destination role. The statement must name exactly the seal key columns
(BLS0407). Sealing is idempotent and is resent until the runtime proves the receiver has it (DIST-007, below). A
`send` of a sealed key after its seal is the runtime violation "send after seal".

**Consumer.** `sealed c(k: v) from m` holds once producer `m`'s seal for key `v` has arrived and the number of
distinct `c` tuples with key `v` received from `m` equals the seal's digest. `sealed c(k: v)` (without `from`) holds
once **every** producer has sealed `v` (Blazes' unanimous vote; ANA-046 skips the vote when the analysis proves one
producer per key). Both are positive, CLOSED thresholds: once true, true forever (MAR Lemma 5), and the analysis marks
the sealed partition CLOSED (ANA-065, ANA-121), so exact reads guarded by a seal are certified (ANA-142) and the
receive log is reclaimable (ANA-063). They are still reported as coordination points (ANA-046).

**Lowering** of `seal occ(split: s);` (at `Mapper`, toward every `Reducer`) and of the reducer's reads:

```ir
// producer: the send log of every `send occ(…)` statement, and the seal
occ$out(D, W, S, L, P) :- M::map$when(L, S, Text, P, W), D := $route(Reducer, W).
occ$out(D, W, S, L, P)@next :- occ$out(D, W, S, L, P).
occ$cnt(D, S, count<(W, L, P)>) :- occ$out(D, W, S, L, P).
occ$mine(D, S) :- M::punctuate$when(S), Reducer$members(D).
occ$mine(D, S)@next :- occ$mine(D, S).
occ$seal(@D, S, C)@async :- occ$mine(D, S), occ$cnt(D, S, C).          // level-triggered: resent every tick
occ$seal(@D, S, 0)@async :- occ$mine(D, S), notin occ$cntk(D, S).
occ$frozen(D, S, C)@next :- occ$mine(D, S), occ$cnt(D, S, C).
occ$frozen(D, S, 0)@next :- occ$mine(D, S), notin occ$cntk(D, S).
violation("send after seal", "occ", (D, S)) :- occ$frozen(D, S, C), occ$cnt(D, S, C2), C2 != C.
// consumer
occ$in(W, S, L, P, M) :- occ(W, S, L, P | M, _).
occ$in(W, S, L, P, M)@next :- occ$in(W, S, L, P, M).
occ$sl(S, C, M) :- occ$seal(S, C | M, _).
occ$sl(S, C, M)@next :- occ$sl(S, C, M).
occ$rc(S, M, count<(W, L, P)>) :- occ$in(W, S, L, P, M).
occ$sealed_from(S, M) :- occ$sl(S, C, M), occ$rc(S, M, C).             // `sealed occ(split: s) from m`
occ$sealed_from(S, M) :- occ$sl(S, 0, M), notin occ$rck(S, M).
occ$sealed_from(S, M)@next :- occ$sealed_from(S, M).
occ$open(S) :- occ$sl(S, _, _), Mapper$members(M), notin occ$sealed_from(S, M).
occ$sealed(S) :- occ$sl(S, _, _), notin occ$open(S).                   // `sealed occ(split: s)`: unanimous
violation("seal digest conflict", "occ", (S, M)) :- occ$sl(S, C1, M), occ$sl(S, C2, M), C1 != C2.
violation("seal overflow", "occ", (S, M)) :- occ$sl(S, C, M), occ$rc(S, M, K), K > C.
```

The generated channel `occ$seal` has `occ`'s direction and fault model, and its receiver persists every seal, so
ARM's conditions hold by construction (ANA-061): the runtime stops resending a seal once the receiver has
acknowledged it (DIST-007). The digest counts distinct tuples, which is exact under set semantics with honest nodes
(DIST-011); the two violations turn every non-monotone situation into a loud error, which is why the analysis may
treat `sealed` as monotone even though its lowering uses negation.

**Input seals.** `input line(…) sealed by (split);` lets the host seal an input key through the host API; the
runtime delivers the persistent `line$sealed(S)`, a later host insert with a sealed key is rejected at the API
(BLSR009), and `sealed line(split: s)` reads it. Punctuation thus flows from the source through the pipeline (E6).

**Local seals.** On a table declared `sealed by (k̄)`, `seal orders(day: d);` promises no more inserts with that key;
`sealed orders(day: d)` reads the promise, and a later insertion of that key is a violation:

```ir
orders$sealed(D) :- M::close$when(D).
orders$sealed(D)@next :- orders$sealed(D).
orders$was_sealed(D)@next :- orders$sealed(D).
violation("insert after seal", "orders", (D)) :- orders$was_sealed(D), orders(D, X), notin orders$prev(D, X).
```

A `sealed table` is sealed as a whole after bootstrap (§7.10). Seals under dynamic membership are epoch-scoped: the
key includes the epoch and the producer relation is the epoch's sealed member relation (DIST-042).

### 14.5 Finality (LANG-212, SEM-016, SEM-017, ANA-120–122, CR-36)

- `final output r(…)` is accepted only if ANA-120 classifies `r` as POS-, NEG-, TOP-, THRESH-, FINITE- or
  SEALED-final; MIXED needs the runtime gate, and NEVER-FINAL (the inverse curse: Z-set inputs, deletable host
  inputs, `delete`/`upsert` driven by unsealed input, PN-style values) is BLS0705. At runtime each emitted row
  carries `provisional`, `final_present` or `final_absent`, gated by ANA-121/122.
- `final r(…)` and `final not r(…)` are body literals that hold when the tuple is final-present or final-absent
  (LANG-212's `is_final`); `when_final(e)` is a threshold on a lattice expression. Finality is monotone, so all three
  are positive.
- A running node never infers completion from silence or quiescence (CR-36); finality comes only from these
  analyses, including through seals.
- `#[finite(cap = N)]` on a module declares it a finite-state component for ANA-122's exact analysis (2PC and Paxos
  decision states, phase and timer automata); exceeding the cap is a hard error.

### 14.6 Bodies that span locations (LANG-095, CR-15, ODD-11)

```blossom
#[localize(chain)]
on query(d), link(x) @ s, path(d) @ x { send answer(d) to s; }
```

The IR stays single-location. A handler may name atoms at other nodes (`@ loc`) only under `#[localize(chain)]` or
`#[localize(link)]` (NDlog's Algorithm 2 for link-restricted rules); the location must be bound before the remote
atom (well-connectedness, BLS0805). The compiler rewrites each location change into a generated channel hop that
carries the bound variables, prints the rewrite, and always emits BLS1005 ("localized cross-node body: two hops per
derivation; no semantics under failure"). The rewrite mirrors remote relations, so it is exact only for insert-only
relations: a mirrored relation with a `delete` or `upsert` is BLS0805.

---

## 15. Time, timers and randomness

### 15.1 Clock, tick and randomness (LANG-170–175, CR-18)

| Built-in | Meaning | IR |
|---|---|---|
| `now()` | the wall clock (`Instant`), sampled once per tick and recorded for replay | `$now` |
| `tick()` | the node's tick counter (`u64`), durable across incarnations; marks the rule time-dependent | `$tick` |
| `random()` | `rand(())` | |
| `rand(k…)` | a `u64`: `PRF_σnode("rand", incarnation, tick, fp(k̄))`; the same key gives the same value within a node and tick | `$rand(k̄)` |
| `rand_float(k…)` | an `f64` in [0, 1) | `$rand_float(k̄)` |
| `rand_range(lo, hi, k…)` | an unbiased value in [lo, hi) for integers and durations (retries use a counter extension) | `$rand_range(…)` |

Rule bodies make no ambient impure calls (CR-18). Nothing is recorded per draw: replay records the seeds and
incarnations (SEM-084, DIST-033). A value that must stay fixed across ticks is captured into state with `next` or
`upsert` (E3's election deadline); an uncaptured `rand` over persistent input is ANA-011's lint (BLS0601).
Cryptographic randomness comes from a `service` (§16.4), never from `rand`.

### 15.2 Timers (LANG-172, LANG-173, CR-19, ODD-16)

```blossom
timer beat every 1s;                  // physical, periodic
timer lease every 500ms times 20;     // stops after 20 firings
timer probe every 5 ticks;            // logical: counts local ticks
timer kick once after 300ms;          // one-shot
timer start once;                     // fires in the first tick of each incarnation
timer wake every 10ms while waiting;  // physical, only while `waiting` holds
```

A timer is an event relation `name(count: u64, at: Instant)`: the firing number and the firing time. Physical
timers are fed by the timer wheel (DIST-030), which also triggers ticks; under simulation they run on the virtual
clock, and under LDFI they are mapped to rounds by the spec's `round` (ODD-16 (c)). `once` without `after` is
`start(0, $now) :- boot().` A logical timer is its own Dedalus expansion:

```ir
probe$left(5) :- boot().
probe$left(K2)@next :- probe$left(K), K > 1, K2 := K - 1.
probe$left(5)@next :- probe$left(1).
probe(N, T) :- probe$left(1), N := $tick / 5, T := $now.
```

A logical timer keeps the node ticking (its counter is a staged change, SEM-009), so it is meant for simulation and
LDFI; a deployed program that uses one gets BLS1006.

**Guarded timers** (`every d while G`, HD item 4). `G` is a view or table placed where the timer is, and depends on
carried state only: tables, statics and views of them, with no event, input, message, stream, `now()`, `tick()` or
`rand` anywhere beneath it (BLS0412; an unknown name is BLS0200). (A guard that held at the end of a tick and would
be false at the next without the state changing could not be noticed by a node that is asleep.) The timer fires only while `G` held at the end of the node's latest tick, i.e. `G`'s
contents in that tick (so `upsert` makes it hold from the next tick). While `G` is empty the timer is dormant: it is
never due, and it wakes the node for nothing, so a node whose guards are all empty sleeps until a message, input or
other timer arrives. When a tick ends with `G` holding, the timer fires again from its first firing after that tick:
the firings it missed are skipped, not delivered late, and `count` still says where on the boot timeline a firing
is. Before the first tick every guarded timer is dormant (the boot tick decides). In the synchronous world a guarded
timer delivers a round's firings iff `G` held at the end of the node's previous round. A polling timer that only
matters while something waits (a request's deadline) is the use: `timer fetch_wake every 10ms while fetch_waiting;`.
LDFI refuses a program with a guarded timer (LANG-172): its firings depend on state that faults can change, which
the hazard encoding does not model yet. IR: `TimerDecl.guard`; the node observes `G` after each tick, as it does
`halt`.

### 15.3 When ticks happen (SEM-009, ODD-04 (c))

A node ticks only on a message, a timer event, host input, or a staged state change. An idle stretch is
observationally equivalent to a run of empty ticks. Retry idioms use explicit timers; a per-node heartbeat timer is
optional deployment configuration.

---

## 16. Functions and host interop

### 16.1 Pure functions (LANG-180–182)

```blossom
fn words(text: String) -> Vec<String> {
    text.split_whitespace().map(|w| w.to_lowercase())
}
#[injective]
fn slot_key(term: u64, idx: u64) -> (u64, u64) { (term, idx) }
```

A `fn` body is a block of `let`s and a final expression, in a total, pure sublanguage: no recursion, no relation
access, no `now()`, `tick()` or `rand`, immutable values, closures only as arguments to the built-in collection
combinators (`map`, `filter`, `filter_map`, `fold`, `all`, `any`, …). `error("message")` aborts the tick with a
located hard error (BLSR010): it is how a function refuses an impossible input, never a silent default. Totality is
enforced by a step budget: an evaluation, from a call made outside any function to its return, may apply closures
and build `range` elements at most `FN_STEP_BUDGET` (10⁷) times in all; past it the tick aborts with BLSR012, the
same in both evaluators. A format's generated functions (§16.7) are not metered: every loop in them is bounded by the
bytes left (or by the value encoded), so they terminate without a budget, and a value of any size in a frame decodes.
The program's own expressions in a format (element arguments, conditions, defaults) take no closure and no `range`
(BLS0301), so each is evaluated in time bounded by its size and the values it reads; a function they call is
metered, and every metered call of one evaluation spends from its one budget, however deep and however many times
(a condition's call per array item adds up). Evaluation depth is bounded too: since functions do not recurse, the deepest evaluation a
program can make (an expression's nesting, plus the bodies of the functions it calls and the closures a combinator
applies, plus one level per literal of the rule it is in) is computed at compile time, and a program deeper than
`MAX_EVAL_DEPTH` (1024 levels) is BLS0217, so no evaluation can overflow the stack a tick runs on. An expression
higher than 1024 (a long chain of operators or calls) is refused by the parser already (BLS0100). Algebraic
properties are attributes (`#[injective]`, `#[commutative]`, `#[associative]`, `#[idempotent]`) checked by TEST-087
and used by ANA-043, ANA-080 and fold legality. A function with a lattice-typed parameter declares its monotonicity
class with a prefix (`monotone fn`, `morphism fn`, `antitone fn`, `threshold fn`); without one it is NM and is called
with a bang. Lowering: an IR pure function; calls stay calls.

**Failure as absence: `?`.** In a function whose result is an `Option`, `e?` is `e`'s value when it is `Some(v)`;
when it is `None`, the function returns `None` at once — the function-body counterpart of a rule body, where a
failed `let Some(x) = e` derives nothing (EXTENSIONS 2.1):

```blossom
fn read_item(c: Cur) -> Option<(Item, Cur)> {
    let (slot, c) = read_u64(c)?;
    let (part, c) = read_i32(c)?;
    Some((Item { slot: slot, part: part }, c))
}
```

A `?` must be evaluated whenever its `let` (or the result) is: under a branch (`if`, a `match` arm, the right of
`&&`/`||`) or in a closure it is BLS0218, as in a function whose result is not an `Option` and in a rule body.
Lowering: the frontend rewrites each `?` into a `match` on its operand, in evaluation order, before name
resolution; nothing reaches the IR.

**Generic functions and function parameters.** A function may take type parameters, and parameters of a function
type `fn(A, B) -> R` (EXTENSIONS 2.2):

```blossom
fn read_list<T>(c: Cur, item: fn(Cur) -> Option<(T, Cur)>) -> Option<(Vec<T>, Cur)> { … }
view items(x) = frame(b), let Some(x) = read_list(cur(b), read_item);
```

The argument for a function parameter is a function *named* at the call: a declared function with fixed types, or
the caller's own function parameter passed on. A function parameter is only called or passed on, never stored,
returned or captured as a value; a function type appears only as a parameter's type; a closure or a generic
function cannot be passed (all BLS0219). A type parameter may occur inside tuples, `Option`, `Vec`, `Set` and `Map`
in the signature, and has no bounds (every type is `Eq`, `Hash` and ordered); the body's own annotations cannot name
one. Each call is checked with its type parameters inferred there, from the arguments, the function arguments'
signatures and the context, like any rule variable; one the call does not determine is BLS0300, and so is a body
that does not type-check at the types a call gives it. A generic function that no call reaches is name-resolved but
not type-checked. Recursion through generic calls is BLS0213, as for any function; a function parameter the built-in
or relation of its name would shadow is BLS0201; more than 10,000 instances (nested generic calls multiply them) is
BLS0220. Lowering: monomorphization — each
call is an instance, a copy of the body with the named functions substituted, and instances with the same type
arguments and function arguments are one IR function, named `read_list<Item, read_item>`; the IR has no generics
and no function values.

### 16.2 Host functions and table functions (LANG-181, LANG-183)

```blossom
#[injective]
extern fn sha256(b: Bytes) -> Bytes = "blossom_std::hash::sha256";
extern table fn lines(path: String) -> (lineno: u64, text: String) = "blossom_std::io::lines";
```

An `extern fn` is a Rust function declared pure; it is memoized per input per tick, and its declared properties are
claims with TEST-087's proved/tested/refuted status. An `extern table fn` is a generator relation with a binding
pattern (inputs bound, outputs free), used as `(n, line) in lines(path)` (LANG-092); file sources are table
functions (LANG-051).

### 16.3 Opaque host types (LANG-027)

`extern type Regex = "regex::Regex";` declares an opaque type whose Rust type must implement `Eq`, `Hash`, `Ord` and
serialization; its values may be stored, compared and sent, and its methods are reachable only through `extern fn`s.

### 16.4 Async services (LANG-184)

```blossom
service fetch(id: u64, url: String) -> (status: u16, body: Bytes);
on want(i, u) { send fetch(i, u); }                     // the call; no `to`: the host service endpoint
on fetch.result(i, u, s, b) { emit got(i, s, b); }      // the result, at a later tick
```

A service is an external call whose result arrives as input at a later tick (the Dedalus rendezvous). It declares a
send-only channel to the host and the event relation `name.result(inputs…, outputs…)`. The host binds the
implementation at deployment.

```ir
fetch(@$host, I, U)@async :- M::h1$when(I, U).
got(I, S, B) :- M::h2$when(I, U, S, B).              // M::h2$when(…) :- fetch.result(I, U, S, B).
```

### 16.5 The host API (LANG-185, LANG-067)

The host API is not surface syntax (ODD-22 (c)): the host inserts into `input`s (always for a future tick), subscribes
to an `output` by full contents or by deltas, runs host code between ticks (`sync_do`, `async_do`) and steps single
ticks. Callbacks run after the tick commits and cannot affect it (step 6 of §4.2).

### 16.6 Output handlers and blobs (LANG-186, LANG-028)

`#[handler("rust::path")] output write_chunk(id: u64, data: Blob);` calls the host handler for each emitted row after
the tick commits; this is the BOOM-FS data path, where bytes move outside the engine and tuples hold `Blob` handles.

### 16.7 Formats: binary layouts (EXTENSIONS 2.5)

A `format` declares a byte layout once; the compiler derives its decoder and encoder, as Prolog's definite clause
grammars run one description both ways.

```blossom
format compact_string = prefixed(uvarint, 1, utf8);              // an alias: an element with a name
format compact_array(F) = array(uvarint, 1, F);                  // with element parameters
format MetadataTopic(version: i16) {
    topic_id: bytes(16),
    name:     nullable(compact_string),
    tags,
}
format MetadataRequest(version: i16) {
    topics:      nullable(compact_array(MetadataTopic(version))),
    allow_auto:  bool,
    include_ops: bool if version >= 8,
    tags,
}
```

A record format is a `struct` of its named fields plus two functions, called as `Name::decode` and `Name::encode`:

```text
fn MetadataRequest::decode(b: Bytes, p: u64, version: i16) -> Option<(MetadataRequest, u64)>
fn MetadataRequest::encode(x: MetadataRequest, version: i16) -> Bytes
```

Decoding reads the value at `p` and returns it with the position after it, or `None` when the bytes run out or
break the layout — never a runtime error on the bytes: a length or count read from the bytes is checked against the
bytes left before anything uses it, and the generated functions are bounded by their input instead of the step
budget (§16.1), so a well-formed value of any size decodes in time linear in its bytes (the program's expressions in
the format take no closure and no `range`, BLS0301, and the functions they call spend from the evaluation's one step
budget). Encoding fails the tick when a value does not fit the layout:
a length beyond its prefix's type (BLSR004), a `bytes(n)` value of another size (BLSR010). `decode(encode(x))` is
`Some((x, end))` for every value whose absent conditional fields hold their defaults; the rules that make it so are
checked: `rest` and `utf8` (and a tuple or record ending in one) come last, an array's items take at least one byte
each, and a bias fits its length's type (BLS0301).

Elements (parameters in parentheses; every element is a name or a call):

| Element | Value | Bytes |
|---|---|---|
| `u8` `i8` `u16` `i16` `u32` `i32` `u64` `i64` | the integer | big-endian |
| `bool` | `bool` | one byte; any nonzero byte reads as `true`, `true` writes 1 |
| `uvarint`, `varint` | `u64`, `i64` | LEB128; `varint` zigzag |
| `bytes(n)` | `Bytes` | exactly `n` bytes (encoding a value of another size is BLSR010) |
| `rest`, `utf8` | `Bytes`, `String` | every byte left |
| `prefixed(L, bias, E)` | `E`'s | a length `n + bias` written as `L` (an integer or varint), then `E` in exactly `n` bytes |
| `array(L, bias, E)` | `Vec` of `E`'s | a count `n + bias` written as `L`, then `n` elements |
| `nullable(P)` | `Option` of `P`'s | a prefixed value or array whose length may be the bias less one: `None` |
| `constant(E, v)` | none | `E` with value `v`: written on encode, checked on decode |
| `ignored(E, v)` | none | `E`, read past and dropped on decode; `v` written on encode |
| `select(c, A, B)` | `A`'s (and `B`'s: one type, or both none) | `A` where `c` (an expression over the parameters) holds, `B` otherwise — an encoding that changes with a protocol version |
| `(E1, E2, …)` | the tuple of the valued ones (the value itself, if only one) | the elements in order |
| `tags` | none | a tagged-field section: a `uvarint` count of (tag, size, bytes); written empty, read and skipped |
| `Name(args)` | the record | another record format, with its arguments |
| `name(args)` | the alias's | an alias, its parameters replaced |

A field is `name: element`, optionally `if cond` (an expression over the format's parameters and earlier fields; an
absent field writes nothing and decodes to its type's zero, or to `= default`, which a nested record requires); an
element with no value (`constant`, `ignored`, `tags`) has no name. Element arguments read the parameters. Aliases and records
live in a file or module (BLS0110 in an `at` section) and may be declared after their use; misuse is BLS0301 (an
unknown element, a wrong arity, `nullable` over something else or over an unsigned length with bias 0, a field name
on a valueless element, an alias that expands into itself, a `select` whose elements differ in type) or BLS0201 (two formats of one name). Lowering: none — a
format expands, once includes are in place, into the struct, the two functions, generated functions for compound
elements (`Name$d1`, `Name$e1`, …) and shared helpers (`format$…`), all ordinary Blossom.

---

## 17. Invariants, specs and verification

### 17.1 Runtime invariants (LANG-200)

```blossom
invariant vote_needs_term "a vote was recorded for a term above the current term":
    never voted_for(t, _), current_term(c) where t > c;
```

An invariant in a module is checked on every node at every tick; every valuation of its body is a violation:

```ir
violation("M::vote_needs_term", (T, C)) :- voted_for(T, _), current_term(C), T > C.
```

`#[on_violation(abort | alert | log | ship(node))]` chooses the action; the default is `abort` (fail loudly).
`log` records the violation with its provenance; `ship` sends it to a remote checker. `violation` feeds no other
relation.

### 17.2 Spec items (LANG-201, TEST-020, TEST-022, VER-001)

A `spec` is a separate program evaluated over the trace of a run of its target, never on a node; nothing in a spec
feeds a protocol relation (ANA-010, BLS0901).

```blossom
spec SimpleLogFaults for SimpleLog {
    include Clique;                                   // another spec's members
    include DelivAssert;
    faults { eot: 4, eff: 2, crashes: 0, model: sync }
    check ldfi expect fails;
}
```

| Member | Meaning |
|---|---|
| `for Path<T…>(K = v, …)` | the target: a module, choreography or program root; without `for`, the spec is a reusable fragment for `include` |
| `nodes A, B, C;` | the scenario's node constants (SCREAMING_CASE, §2.6) |
| `assign Role = [A, B];` | role membership for a multi-role target |
| `faults { eot, eff, crashes, model, delay, round }` | the failure spec ⟨EOT, EFF, maxCrashes⟩ (TEST-020); an omission is allowed iff 1 ≤ send tick < EFF (CR-21); `model: sync` (delivery at t+1, the LDFI mode, TEST-006) or `async`; `delay` bounds async delivery; `round` maps physical time to rounds (ODD-16) |
| `fact r(…) [@ n] [at tick k];` | scenario facts: into a `static` relation at node `n` (all `nodes` without `@`), or into an `input` at tick `k` (LANG-069); `fact c(…) @ n from s at tick k` is a message of client session `s` (a number) on a channel `c` from an external role (§18.4). An instance's relations are named by path (`tpc.begin`), in facts and in located atoms |
| `view …` | spec rules: views over global state; they may join across locations (LANG-201) |
| `view pre(…)`, `view post(…)` | the LDFI outcome oracle: same schema, evaluated at EOT (TEST-022) |
| `invariant name: never B;` | safety over every visited global state |
| `liveness name: eventually B within N ticks after eff;` | bounded liveness (VER-001): `B` holds at some point in [EFF, EFF + N] (or after the given tick expression) |
| `prove G by induction using L1, L2;` | `G ∧ L1 ∧ L2` is an inductive invariant (VER-006–010) |
| `expect confluent(out);`, `expect deterministic(out);` | the certificate ANA-029 / ANA-039 must give for output `out` |
| `check tool [{ opts }] [expect holds \| fails];` | run a verifier (§17.5) |

### 17.3 Atoms in specs (LANG-070, TEST-080, CR-20)

- Every atom of a target relation names its location with `@`: `log(p) @ x` (binding or testing `x`). Spec views
  and oracles are unlocated.
- **Time.** An atom is read at the evaluation point: EOT for `pre` and `post` (quiescence in simulation); every
  global state the checker visits for `invariant`; each tick of the window for `liveness`. `r(…) @ n at tick k` reads
  tick `k` of node `n` (LANG-070); `ever r(…) @ n` holds if the tuple held at some tick up to the evaluation point.
- **The network.** `sent c(…) @ d from s` holds if `s` sent the message to `d` at or before the evaluation point: the
  network as a grow-only set (VER-006).
- **Oracles** (spec-only, CR-20, ANA-010): `crashed(n)` (n crashed at or before the evaluation point), `crash(n, t)`
  (n crashed at tick t), `hb(n1, t1, n2, t2)` (happens-before). A crashed node's frozen state stays visible to specs.
- **Quorums.** `quorum v in R { B }` holds if some majority of role `R` satisfies `B`; the first-order encoding uses
  a quorum sort with the intersection axiom instead of a cardinality count, which keeps it in EPR (VER-008).

### 17.4 Lowering to trace queries (TEST-080, TEST-081)

Every protocol relation `r` has the trace relation `r$log(Node, X̄, Tick)`. Spec rules are plain stratified Datalog
over the trace relations, evaluated once per evaluation point `P`:

```ir
missing_log(A, Pl, P) :- point(P), log$log(X, Pl, P), neighbor$log(X, A, P), notin log$log(A, Pl, P).
crashed$at(X, P) :- point(P), crash(X, T), T <= P.
pre(X, Pl) :- eot(P), log$log(X, Pl, P), notin bcast$log(X, Pl, 1), notin crashed$at(X, P).
violation("election_safety", (A, B, T, P)) :- point(P), won$ever(A, T, P), won$ever(B, T, P), A != B.
won$ever(N, T, P) :- won$log(N, T, P1), point(P), P1 <= P.
```

### 17.5 Checks (TEST-001, TEST-020–040, VER-002–010, VER-005)

| `check` | Tool | Uses |
|---|---|---|
| `ldfi` | LDFI, Molly-2 (TEST-020–040) | `faults`, `pre`, `post`; a missing `pre` or `post` is BLS0900 (CR-30) |
| `bmc { ticks, delay, in_flight }` | bounded explicit-state model checking (VER-002) | invariants, liveness, `faults` |
| `smt` | first-order transition system and Z3 (VER-006–010) | `prove` goals; `quorum` and `sent` literals |
| `sim { runs, seed }` | deterministic simulation (TEST-001) | invariants, `pre`/`post`, liveness |
| `asp { ticks }` | bounded ASP encoding with clingo (VER-003) | invariants |

`expect holds` or `expect fails` makes the check a CI gate (TEST-039); without it the result is only reported. Every
result states its bounds certificate: the model, the number of nodes, the ticks, the delay window and the failure
budget (VER-005).

### 17.6 Verification annotations in programs

`#[trusted("reason")]` on a module or choreography stops the CALM analysis from re-flagging its internals, and VER-020
then requires its interface spec to pass (LANG-205). `monotone` (§13.5), `#[nondet]` and `#[deterministic]`
(§13.6), `final output` (§14.5) and `#[finite]` (§14.5) state CALM and determinism facts that the compiler checks or
tracks.

---

## 18. Security: principals, senders, ACLs and sessions

### 18.1 Principals and the node directory (LANG-240)

`Principal` is an authenticated identity (a SPIFFE id, ODD-31); it is distinct from `Node`, a routable location. The
static relation `node_dir(node, addr, principal, role)` is the directory (static for static membership, epoch-sealed
for dynamic membership), and `principal_of(n)` and `role_of(n)` are built-in functions over it.

### 18.2 Sender and principal columns (LANG-241, SEM-091)

```blossom
on put(id, key, val) from s principal p, writers(p) { … }
```

A received channel tuple carries its transport-authenticated sender and principal as implicit columns. `from s`
binds the sender (`Node<Src>`, or `Session`); `principal p` binds `principal_of(s)` for a peer or the session's
principal for a client. They are never payload, so they cannot be forged. In the IR they are the trailing columns
(`put(Id, Key, Val | S, P)`), projected away when no rule reads them, so identical facts from different senders merge
exactly when nobody asks who sent them (SEM-091). ANA-106 (BLS0802) warns when a `Node`-typed payload column is used
as an identity (a reply address, a vote's voter, a quorum member) without being equated with `from`.

### 18.3 ACLs (LANG-242, CR-40, ODD-33)

- **Inferred, default-deny** (P0). A channel accepts a frame only from the roles that have a `send` into it,
  computed by projection (§6.10). Nobody has to write an ACL for a client to be unable to send `append_entries`.
- **Explicit** (P1). `#[accept(…)]` on a channel narrows the inferred ACL: its arguments are role names and
  `external` (a union of sources) and optionally `principal in REL`, where `REL` is a unary `static` or `table`
  relation of the receiving node read at the last committed tick. `#[accept(external, principal in admins)]` admits
  only client sessions whose principal is in `admins`. An explicit ACL that excludes a role the program itself sends
  from is BLS0800 (ANA-105): the program would drop its own messages.
- **Importers** narrow an instance's ACL (never widen it) with `acl a.c accept(…);`.
- Enforcement is per message at ingress, before the tick (DIST-062). A rejected message is dropped, counted and
  audited: it is an omission (SEM-090), so every safety argument, CALM certificate and LDFI verdict carries over
  (CR-40). ACLs are ingress configuration, not rules.

### 18.4 External clients and sessions (LANG-243)

A role declared `external` is a set of client sessions, not nodes. A channel from it carries a `Session` sender; a
channel to it is egress-only, and its `to` expression must be a `Session`. Roles that receive from an external role
get the event relations `session_open(s, p, at)` and `session_closed(s, reason)`. A reply to a closed session is
dropped and counted. A follower that forwards a client request carries an `on_behalf_of: Principal` payload column,
which the leader accepts only on channels whose ACL admits cluster roles alone; client ids are checked against
`principal` (ANA-106).

### 18.5 Authorization (LANG-244)

Data-dependent authorization is ordinary rules: views derive `authorized(p, op, obj)` (RBAC, delegation, k-of-n), and a
missing authorization emits `authz_denied(session, op, obj, reason)`, which becomes an error reply and is counted per
rule. It is program logic, not an omission (CR-40).

### 18.6 Signed values (LANG-245, P2)

`Signed<T>`, `sign(x)` and `verify(s)` are reserved for a later edition; using them is BLS0907.

---

## 19. Versions, schema evolution and migrations

### 19.1 Program version and the schema lock (LANG-260)

`program kv version 3;` names the program and its version. The checked-in `schema.lock` records, for every released
version and for every channel, durable relation, interface and wire type: field numbers, names, types, defaults, key
columns, lattice type identities, reserved numbers, canonical-form hashes, the edition and the minimum supported
version. `blossom release` appends a version. CI fails when a schema changes without a version bump (TEST-108,
BLS0903) or on any ANA-100, ANA-102 or ANA-105 error.

### 19.2 Field numbers and evolution metadata (LANG-261, LANG-265)

```blossom
durable table kv(key: String #1, val: Bytes #2, #[since(3)] ver: u64 #3 = 0) key(key);
#[reserved(4)]
enum Op { Put #1, Del #2, #[since(3)] Expire #3, #[unknown] Unknown #5 }
channel request_vote(term: u64 #1, last_idx: u64 #2, last_term: u64 #3, #[since(7)] prevote: bool #4 = false): Server -> Server;
```

- Every column of a channel, durable relation or interface, and every field and variant of a type that reaches one,
  has a stable field number `#n`; the compiler assigns missing ones and records them. Numbers are never reused;
  `#[reserved(n, …)]` retires them. The in-memory layout stays positional; the codec and the WAL use the numbers.
- A field added later has `#[since(N)]` and a default (`= v` or an `Option` type) (BLS0904). Named atoms may omit it
  without `..` (§9.2), so existing rules keep compiling.
- Stored or sent enums need an `#[unknown]` variant (BLS0308); variants are encoded by number.
- `#[deprecated(since = N)]` warns at every use. `#[semantics_changed(since = N)]` on a field forces a new field
  number (BLS0904).

### 19.3 Migrations (LANG-262)

```blossom
migrate from 2 {
    while old.kv(key, val) {
        emit kv(key, val, 0);
    }
}
```

A `migrate from N { … }` block is a separate program that runs at recovery (DIST-082), and at finalization for
non-monotone migrations (ANA-103), as one stratified tick over the decoded version-`N` checkpoint. Its views and
`while` handlers read `old.r` (typed by version `N`'s lock entry) and `emit` into current durable relations. `on`,
`next`, `send`, `delete`, `upsert`, `seal`, `now()`, `tick()` and `rand` are forbidden (BLS0906); pure functions are
allowed. Several versions are migrated one step at a time. A key collision the migration causes is a hard error
naming both source tuples (BLSR001). Migrations that only add defaulted fields, project, widen, or rename (declared with
`#[renamed_from("old")]` on the new field or relation) are synthesized and need no block. A migration of a lattice
column must be a declared `morphism` (ANA-104). `migrate from N down` is P2 and is BLS0907 in this edition.

### 19.4 Channel translation and version gates (LANG-263, LANG-264, SEM-092)

```blossom
translate request_vote to 6 {
    while request_vote(t, li, lt, false) { emit old.request_vote(t, li, lt); }
}
translate request_vote from 6 {
    while old.request_vote(t, li, lt) { emit request_vote(t, li, lt, false); }
}
become_candidate: on prevote_round(t, li, lt), p in Server where cluster_version() >= 7 { … }
```

- `translate c to N` rewrites tuples for a receiver on version `N`; `translate c from N` rewrites frames from a
  sender on `N`. Each handler is tuple-local: one channel atom, pure functions, no state (ANA-103, BLS0906). They run
  in the codec layer (DIST-087), not in the tick. A tuple that no `to` handler matches is disallowed: it is dropped and
  counted as an omission (SEM-090). Mixed-version behavior that needs state uses explicit dual channels gated by
  `cluster_version` (LIB-123).
- `cluster_version()` is a built-in `LMax<u32>` input sampled once per tick and recorded; it is read only through the
  threshold `cluster_version() >= V`, so gates add no points of order (SEM-092). Writing a relation, column or variant
  marked `#[since(V)]`, or sending a channel or non-default field marked `#[since(V)]`, must be dominated by such a
  gate (ANA-102, BLS0905) unless the handler or statement carries `#[unsafe_ungated("reason")]`.
- A hot install (SEM-094, P2) is an admin-plane operation (DIST-066) and has no surface syntax.

---

## 20. Diagnostics (TEST-091)

Every diagnostic has a stable code, a primary span, secondary spans for every piece of evidence, and, where one
exists, a fix-it. `blossom explain BLSnnnn` prints the long form with an example. Severities: **E** error, **W**
warning (an error under `--strict`, ODD-10 (c)), **R** runtime hard error (the tick aborts with a located report; it is
never truncated or defaulted).

**Lexical (BLS00xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0001 | E | unexpected character |
| BLS0002 | E | unterminated string, byte string or block comment |
| BLS0003 | E | unknown numeric suffix (`10kb`) |
| BLS0004 | E | a hard keyword followed by `!(` (`not!(…)`): bangs mark operators, not keywords |
| BLS0005 | E | unknown escape in a string |

**Syntax (BLS01xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0100 | E | unexpected token, with the expected-token set |
| BLS0101 | E | missing `;` (inserted during recovery) |
| BLS0102 | E | `let` as a statement: bind it in the header or an `if`/`for` condition |
| BLS0103 | E | chained comparison or range (`a < b < c`) |
| BLS0104 | E | struct literal in a no-struct context: parenthesize it |
| BLS0105 | E | a label not followed by `on`, `while` or `monotone` |
| BLS0106 | E | a declaration clause given twice, or on a kind that does not take it |
| BLS0107 | E | an aggregate clause (`default`, `per`, `by`, …) outside the call's parentheses |
| BLS0108 | E | `after` on a non-`stable` fn, or a `stable fn` without `after` |
| BLS0109 | E | an `if` expression without `else` |
| BLS0110 | E | an item where it may not appear (§6.2), or `pub` on a relation |

**Names and modules (BLS02xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0200 | E | unknown name |
| BLS0201 | E | duplicate declaration, alias or label; a reserved name (`lset`, `set`, `map`, …) declared |
| BLS0202 | E | a relation used as a function or a function used as a relation |
| BLS0203 | E | not an interface of the instance (`data.outbox`) |
| BLS0204 | E | unknown module, protocol or file path |
| BLS0205 | E | import arguments: unknown parameter, unbound relation parameter, column types that do not match |
| BLS0206 | E | protocol conformance (a redeclared interface differs, an extra interface) or role binding (unbound role, kinds differ) |
| BLS0207 | E | `override` with nothing to override, or a same-name block without `override` |
| BLS0208 | E | interposition on a non-interface, or on one interface twice |
| BLS0209 | E | a hard keyword used as a binding: write `r#kw` |
| BLS0210 | E | unknown attribute, or an attribute on an item that does not take it |
| BLS0211 | E | a `const`, `param`, module value parameter or spec node name that is not SCREAMING_CASE |
| BLS0212 | E | `from`/`principal` on an atom that is not a channel or loopback |
| BLS0213 | E | a recursive function (functions are total, §16.1) |
| BLS0214 | E | a `let` block outside a function body, or a closure that is not a combinator's argument in one (§16.1) |
| BLS0215 | E | a function body that reads a relation, `now()`, `tick()`, `self`, randomness or a role's members (§16.1) |
| BLS0216 | E | an `extern fn` that names no host function of the standard library, or declares a different signature (§16.2) |
| BLS0217 | E | an evaluation deeper than the bound the evaluators' stacks are sized for (§16.1) |
| BLS0218 | E | `?` where it cannot return early: outside a function returning `Option`, under a branch, the right of `&&`/`||`, a nested block or a closure (§16.1) |
| BLS0219 | E | a function type outside a function's parameter list, a function parameter neither called nor passed on, or a function argument that is not a named function (§16.1) |
| BLS0220 | E | generic functions instantiated more than the bound allows (each call is an instance; nested generic calls multiply them) (§16.1) |

**Types (BLS03xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0300 | E | type mismatch, listing every piece of evidence (LANG-021) |
| BLS0301 | E | arity mismatch |
| BLS0302 | E | unknown field in a named atom, or omitted non-`since` columns without `..` |
| BLS0303 | E | `..` in a head, or a head missing a column that has no default |
| BLS0304 | E | a lattice column as a key, join key or group key |
| BLS0305 | E | `==`/`!=` on lattice values |
| BLS0306 | E | a lattice comparison in the non-threshold direction: use `reveal!` or `not (x > c)` |
| BLS0307 | E | a group-typed payload on a channel without `exactly_once` (ANA-015) |
| BLS0308 | E | an enum that reaches a channel, durable relation or interface without an `#[unknown]` variant |
| BLS0309 | E | a constant expression overflows |
| BLS0310 | E | a view column's type cannot be inferred |
| BLS0311 | E | a lattice lift with no expected lattice type |
| BLS0312 | E | `f64` as the element of `LMax`/`LMin` |
| BLS0313 | E | a possibly negative contribution into a `bag` |
| BLS0314 | E | a `match` that does not cover every value of its scrutinee |
| BLS0315 | E | a `Conn` in a channel or a durable relation |

**Legality (BLS04xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0400 | E | a verb that cannot target this kind (§12) |
| BLS0401 | E | a write into a `sealed table` outside bootstrap |
| BLS0402 | E | a durable relation written in a plain `bootstrap` (use `bootstrap fresh`) |
| BLS0403 | E | `send` destination: missing `to`, `to` on a loopback, on a column-form channel or on a partitioned channel |
| BLS0404 | E | a send, receive, read or write placed at a role the relation does not live at |
| BLS0405 | E | `fact` into a non-`static` relation (or, in a spec, into an input without `at tick`) |
| BLS0406 | E | a write into an own `input`, an instance `output`, a relation parameter or a `view` |
| BLS0407 | E | `seal` of a relation without `sealed by`, or naming other than exactly its seal key |
| BLS0408 | E | a rule outside every `at` section in a multi-role module, or placed at an external role |
| BLS0409 | E | `else` after a condition that is not a single scalar guard |
| BLS0410 | E | `delete`/`upsert` on a lattice-valued relation (LANG-284) |
| BLS0411 | E | `resolve prefer(…)` naming no handler, one twice, or one that does not write the table with `next` or `upsert` (§10.7) |
| BLS0412 | E | a timer's `while` guard that is not a view or table placed where the timer is (§15.2) |
| BLS0430 | E | a tree element inside a `for` block with neither an id nor a key (its rows would repeat one id) |
| BLS0431 | E | a key on a tree element that has an id |
| BLS0432 | E | a fragment's tree elements where no tree encloses the call |
| BLS0433 | E | a fragment that calls itself, directly or through others |
| BLS0434 | E | a `tree` declaration whose relations do not have the shapes of their roles |
| BLS0435 | E | an interpolation hole's format spec other than `.N` |
| BLS0436 | E | a fragment parameter that is not a variable name (variables start with a lowercase letter or `_`) |

**Rules, time and stratification (BLS05xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0500 | E | range restriction: a variable of a head, negation, guard, `to` or `weight` is not bound (ANA-001) |
| BLS0501 | E | `let` re-binds a variable: write `x == e`; a match arm in a rule re-binds a rule variable |
| BLS0502 | E | a negative edge on a same-tick cycle, with the cycle as a path of surface constructs (ANA-002) |
| BLS0503 | E | a choice or order-sensitive site on a same-tick recursive cycle (SEM-086) |
| BLS0504 | E | `on` without a positive event literal, with the chain that makes the header standing |
| BLS0505 | W | `while` with an event literal: write `on` |
| BLS0506 | E | a handler emits a relation it tests negatively (fix-it: `next`; override: `#[allow(self_negation)]`) |
| BLS0507 | E | a relation atom after `where` |
| BLS0508 | E | a body atom at another location without `#[localize]` (ANA-004) |
| BLS0509 | E | a spec-only oracle or trace relation in a program rule (ANA-010) |
| BLS0511 | E | a `default` with grouping columns but no `per` driver, a driver that does not determine the group, or, with a driver, an aggregate with no identity (`min!`, `max!`, `index!`) and no `default` |

**Choice and determinism (BLS06xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0600 | E | a seeded site (`choose*!`, `seq!`) in an unlabelled handler or a multi-alternative view |
| BLS0601 | W | ANA-011: `index!`/`top!`/carried `fold!` over persistent input, or an uncaptured `rand` over persistent input |
| BLS0602 | W | `seq!` numbers that reach a `send` or an output without `durable` |
| BLS0603 | E | a `#[deterministic]` output is schedule-dependent |
| BLS0604 | E | a lattice-typed variable among a choice's chosen columns |
| BLS0605 | W | a `choose` into a keyed relation whose determinant is not within the key |

**Monotonicity and lattices (BLS07xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0700 | E | a non-monotone operation without its bang (fix-it inserts it) |
| BLS0701 | W | a superfluous bang on a monotone operation |
| BLS0702 | E | a `monotone` region contains points of order (each is listed) |
| BLS0703 | E | a `stable` method read without its threshold guard in the same body, and without a bang |
| BLS0704 | E | a law or class claim refuted by the harness or the SMT backend |
| BLS0705 | E | a `final output` that ANA-120 cannot classify, or that is NEVER-FINAL |
| BLS0706 | E | `DomPair` outside `unsafe` |
| BLS0707 | E | `threshold(…)` values that are not pairwise incompatible |

**Distribution and security (BLS08xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0800 | E | an explicit ACL excludes a role the program sends from (ANA-105) |
| BLS0802 | W | a payload column used as an identity without being equated with `from`/`principal` (ANA-106) |
| BLS0803 | E | an unanimous `sealed c(…)` read with no producer set |
| BLS0804 | E | `partition by` on a `Node -> Node` channel without `over` |
| BLS0805 | E | a localized body that is not well-connected, or mirrors a relation with deletions |

**Specs and versions (BLS09xx)**

| Code | Sev | Meaning |
|---|---|---|
| BLS0900 | E | `check ldfi` without `pre`, `post` or `faults` (CR-30) |
| BLS0901 | E | a spec rule that would feed a protocol relation |
| BLS0902 | E | `prove … using …` names an unknown invariant |
| BLS0903 | E | a schema change without a version bump or lock update (TEST-108) |
| BLS0904 | E | ANA-100 compatibility: a reused field number, a changed type, a field added without a default, a `semantics_changed` field without a new number |
| BLS0905 | E | a `since` feature written or sent without a `cluster_version()` gate (ANA-102) |
| BLS0906 | E | a forbidden construct in a `migrate` or `translate` block, or a translation that is not tuple-local |
| BLS0907 | E | a P2 feature reserved for a later edition (entanglement, `Signed<T>`, `migrate … down`, `#[blazes]`) |

**Lints (BLS1xxx)**, warnings by default: BLS1001 naming convention; BLS1002 unused variable, relation or
interface (ANA-008); BLS1003 possible same-tick key conflict, with a two-message example (ANA-007); BLS1004 wildcard
under an aggregate; BLS1005 localized cross-node body; BLS1006 logical timer in a deployed build; BLS1007 soft-state
TTL shorter than a body's (ANA-006); BLS1008 `if` that binds variables or `for` that binds none.

**Runtime (BLSRxxx)**: BLSR001 key violation (SEM-050), naming both derivations; BLSR002 conflicting upserts
(SEM-051), naming both statements; BLSR003 an invariant with `abort`; BLSR004 arithmetic overflow, division by zero,
out-of-range cast, weight overflow; BLSR005 `collect_map!` duplicate key; BLSR006 `LPoint` conflict; BLSR007 an
in-tick fixpoint that does not converge (CR-53); BLSR008 a write at a non-owner of a partitioned table; BLSR009 a host
insert into a sealed input key; BLSR010 `error("…")` in a function; BLSR012 a pure function's evaluation exceeds its
step budget.

---

## 21. Compatibility frontends

### 21.1 Molly/Dedalus `.ded` (LANG-220, P1, ODD-12)

`.ded` files are compiled by a separate frontend onto the same IR, so the Molly corpus runs verbatim and reproduces
Molly's LDFI verdicts (FEATURES.md §11.5, the LDFI corpus). The frontend accepts Molly's dialect exactly (R06 §3.2):

```
clause     ::= 'include' STRING ';' | fact | rule
fact       ::= predicate ';'                              -- carries @<int>
rule       ::= predicate ':-' bodyTerm (',' bodyTerm)* ';'
predicate  ::= ['notin'] IDENT '(' [atom (',' atom)*] ')' ['@next' | '@async' | '@' INT]
atom       ::= IDENT '<' IDENT '>' | expr | constant      -- aggregates count/max/min/sum in heads
expr       ::= constant OP (expr | constant)              -- right-nested, no parentheses, no precedence
comments   ::= '//' … | '/* … */' | '#' …
```

Mapping to the IR:
- The first column of every relation is its location (`Node`); a rule's location is its first body predicate's
  first column, and every body predicate must share it (body locality).
- Capitalized identifiers are variables and `_` is a wildcard; other identifiers and strings are constants; node
  names are the lowercase constants in location position.
- `p(…)@next :- …` is inductive, `p(…)@async :- …` is async to the head's first column, and there are no implicit
  tables: persistence is the explicit `p(X)@next :- p(X);` rule, which the engine recognizes as storage (LANG-065).
- A fact `p(…)@k;` is an input event at tick `k` (CR-13: Molly's round `k` is our tick `k`); a relation with only
  `@k` facts is an input. `p(…)@k` in a body is an absolute-time atom and is legal only in `pre`/`post` rules.
- Types are inferred (Molly's INT, STRING and LOCATION become `i64`, `String` and `Node`).
- Expressions keep Molly's right-nested, precedence-free parse, so arithmetic means what it meant in Molly.
- Aggregates use Molly's split rewrite (§10.1).
- `crash(From, Node, Time)` is the spec oracle `crash(node, t)`; it is legal only in `pre`/`post` and their helpers
  (CR-20, ANA-010). `pre` and `post` rules become the outcome oracle of an implicit spec, so
  `blossom ldfi simplog.ded deliv_assert.ded --eot 4 --eff 2 --nodes a,b,c --crashes 0` is Molly's `SyncFTChecker`.
- A `.ded` file may be `include`d into a `.bls` module (§6.6); its relations get inferred schemas.

### 21.2 Other frontends (P2)

The Overlog/NDlog (LANG-221), Hydroflow `datalog!` (LANG-222) and Bloom collection-expression (LANG-223) frontends
map onto the same IR with the differences of CR-01–CR-08 documented. They are P2 and are not part of this edition.

---

## 22. Rejected alternatives and refinements of FEATURES.md

### 22.1 Why this base

The three judges disagreed: ergonomics chose B (8.0; A 6.0, C 5.5, D 5.0), semantics chose A (8.5; C 8.0, D 7.0,
B 6.5), tooling chose D (7.5; A 7.0, C 5.5, B 5.0). The decision weighs them as follows.

- **The skeleton is what every user reads and writes**, and Blossom's users write protocols: every flagship system
  in FEATURES §10 is a message-driven protocol or a dataflow driven by messages and seals. B's skeleton maps one RPC
  or message to one handler, states the timing of every consequence, shows re-derivation (`on`/`while`) and writes
  choreographies in message order. A's and C's flat rules cannot group consequences without duplicating bodies,
  cannot show resends, and pay a declaration per intermediate relation; D's comprehensions are the most verbose in
  protocol code and put a bang on nearly every line. These are structural and cannot be fixed by grafts.
- **Every defect the semantics and tooling judges found in B is fixable inside the skeleton**, and §1.1 lists the
  fix for each: materialized headers make every statement one IR rule with a stable, label-based id; no `let`
  statements and the BLS0506 error remove the sequential misreading and the `emit`/`next` trap; braces keep one
  meaning per context; the keyword set is small and escapable.
- **A's foundations are adopted wholesale** where they were strongest: the bang discipline, which is what makes
  monotonicity syntactically checkable; the declared-producer seal soundness; the read-time soft-state TTL; the
  banged Z-set boundary; complete LANG coverage; Rust items and precedence; `#[attr]`.
- **D's and C's best ideas fill the remaining gaps**: inferred-schema views, `forall` over closed domains,
  `partition by`, keywords as field names, `where`, `bootstrap fresh`, input seals, relation parameters, `max!`
  versus `lmax{…}`, `stable`, and the verification vocabulary.

### 22.2 Rejected alternatives

| Alternative | From | Why rejected |
|---|---|---|
| Flat `kind head <- body;` rules as the rule form | A | One event's consequences are spread over many rules with repeated bodies (E1, E4 in A); no edge/level distinction; head-first order hurts completion. Handlers lower to the same IR rules. |
| Both a flat rule form and handlers | — | Two ways to write every rule; the formatter could not normalize; tests and teaching double. Views cover the Datalog-style definitions flat rules are good at. |
| Attributes carrying core semantics (`#[durable]`, `#[key]`, `#[seal]`) | A | Readers treat attributes as optional metadata. Storage, keys, partitioning, seals and resolution are words (principle 6). |
| `!` for negation and boolean not; bang keywords `not!`, `all!`, `delete!`, `group!` | A, D | `!` had three or four meanings and saturated protocol code (D counted about twenty bangs in its Raft). Here `!` means only "non-monotone operator"; relational negation and the deferred mutations are keywords. |
| `delete! next r` / `upsert! next r` | D | The verbs are a closed list with one timing each (§8.2); a mandatory redundant token is noise. |
| Aggregates recognized only by head position, without a bang | B | Lattice methods need a marker anyway (B's own `a.le(b)` was unmarked and contradicted its rule); one rule, "a bang call is non-monotone", covers aggregates, choice, order, `reveal` and lattice methods. |
| Comprehension clauses (`from … join … select`) and record binders as the rule form | D | Verbose for protocols; sink-before-binder (`send x to g.cand from g in grant`); a SQL look over set semantics; lowering depended on clause order. Named atoms with the `since` exemption give the schema-evolution benefit. |
| Chosen columns inferred from liveness | D | A P0 fidelity defect (LANG-108): editing `select` changed seeds. Every choice spells `Ȳ per X̄`. |
| Prolog punctuation (`:-`, the end-dot), unterminated declarations with open clause lists | C | The end-dot is whitespace-sensitive and breaks dot completion; open clause lists let a later keyword change old parses (`accept(…)` as a rule head). Here everything ends in `;` or `}`. |
| Identifier classes by case in the lexer | C | Acronyms lex wrong (`ID`); case conventions are lints, with one hard rule for constants (§2.6). |
| Type-directed lattice comparisons (`a <= b` as ⊑), `\/`, `\|>` | C | Polarity invisible in text; one-off sigils. Comparisons are allowed only in the threshold direction and joins are `.join()`. |
| `on role { }` for location | C | `on` means "on this event"; locations are `at Role { }`. |
| `(key) -> (vals)` schemas | B | `->` also meant role direction. Keys are `key(…)`. |
| Brace set and map literals, lattice-lifted `{v}` | B | Braces had six or seven meanings. `set[…]`, `map[k => v]`, and lifts by expected type. |
| `let` statements inside blocks | B | They made handlers read as sequential code. |
| Position-derived rule ids | B, A | Reordering changed seeds and replay. Labels and content hashes only (§4.3). |
| `?:` ternary | B | Reused `:`; `if … { } else { }` is an expression. |
| `always forall … implies` formulas | C | Denial invariants with `quorum`, `sent`, `ever` and `forall` express VER-001's properties in one form that every checker shares; a second, first-order syntax would add a parser and a semantics for no extra expressiveness in the EPR fragment. |
| Correlated relational aggregates in expressions (`count{…} default 0`) | C, D | A second relational aggregation form with different empty-group semantics. Relational aggregates are head-only (with `per` for defaults); only monotone lattice folds are expressions. |
| `x in (a, b]` ring intervals | A–D | Mismatched brackets defeat bracket matching and recovery. `a<..=b` and friends (§9.4). |
| `#` meaning only field numbers | C | Deviates from LANG-208 (P0). Kept `#` comments with a one-character lexical rule (§2.2). |
| `'label:` / `rule name:` label syntax | A, D | Unnecessary: no item starts with an identifier, so `name:` is unambiguous. |
| Implicit `$sender` fields | D | A new lexical class for two columns; `from s principal p` reads better and is the same IR. |
| Struct-bodied relation declarations `table r { … }` | A | Declarations in parentheses mirror atoms; per-column metadata goes in column attributes. |
| Named atoms open by default | B | A forgotten join field silently widened the match. Strict, with the `since` exemption for evolution. |
| Seals sealed by any single producer by default | C | Unsound with several producers per key (CR-27). Unanimity by default, per-producer reads explicit. |
| `resolve` over same-tick derivations | C | Not in LANG-117; changes when SEM-050 fires. Relation-level resolution governs t+1 candidates only. |
| TTL checked only when carrying to t+1 | B, D | Stale after idle stretches (a silent peer looks alive). Read-time filter (§7.9). |

### 22.3 Refinements of FEATURES.md

None of these changes a CR-xx; each is a spelling or a clarification. They are recorded in `docs/DECISIONS.md`.

1. **LANG-190.** `next` means t+1 everywhere, including inside `bootstrap`; "a `<+` in bootstrap takes effect at
   tick 0" is written `emit`. "Imported modules bootstrap before the module that imports them" means that all
   bootstrap statements run in one boot-tick fixpoint ordered by stratification, so an importer may read what an
   instance's bootstrap derived; no other order is observable. `bootstrap` runs in every incarnation;
   `bootstrap fresh` only when no durable state was recovered.
2. **CR-14.** A direction-form channel declares its location with `: Src -> Dst` instead of a named `@` column; the
   column form keeps one `@` column. Both normalize to the IR's first column, and atoms have exactly the declared
   columns.
3. **CR-16.** `fact` targets only `static` relations (a fact in a mutable relation would be undeletable); initial
   mutable state goes in `bootstrap`.
4. **LANG-208.** `#` comments are kept, except before `[`, `![` or a digit.
5. **Spellings.** `notin`/`!p(…)` → `not p(…)`; `X := e` → `let x = e`; `count<X>` → `count!(x)`; `;` alternatives →
   `any { … }` or view alternatives; `temp` → `view`; `periodic` → `timer`; `seal r on key = v` → `seal c(k: v)` with a
   compiler-maintained digest; `emit c to N`/`accept c from N` → `translate c to N`/`translate c from N` (so `emit`
   stays a verb and `accept` an ACL word); `majority<N in Members>` → `majority(s, R)` and, in specs,
   `quorum v in R { … }`; `x in (a, b]` → `x in a<..=b`; `nondet "reason"` → `#[nondet("reason")]`.
6. **Seals** default to unanimity over the declared producer set, with per-producer reads (`sealed c(k: v) from m`).
7. **ANA-041's `Rep`**, which no proposal had, is `#[replicated]`; every compiler-generated IR relation is
   provenance-transparent (§4.1); `#[deterministic]` asserts an output's determinism class (§13.6).

---

## Appendix A. LANG coverage

Every LANG item of FEATURES §2 and where it is specified. P2 items are either given their reserved form or listed as
not in this edition.

| LANG | Construct | § |
|---|---|---|
| 001 (P0) | unordered items, statements and literals | 6.1 |
| 002 (P0) | own lexer, parser and checker; no closures in rule bodies | 2, 3, 9.12 |
| 003 (P0) | `input`/`output` interfaces; `pub` only on non-relations | 6.3, 7.6 |
| 004 (P0) | `import M<T>(K = v) as a`; `a.b.r` | 6.5 |
| 005 (P1) | `include M;`, `include "file.bls";`, `include "file.ded";` | 6.6 |
| 006 (P1) | `protocol`, `module M: P`, protocol-bounded type parameters | 6.4, 6.7 |
| 007 (P1) | labelled handlers, views and `block`s; `override` | 6.8 |
| 008 (P1) | `interpose a.i as (outside, inside)` | 6.9 |
| 009 (P1) | roles, `at` sections, `choreography`, `import … with (…)`, projection | 6.10 |
| 010 (P1) | `const`, `param`, module value parameters | 6.4 |
| 011 (P2) | several program roots in one runtime | not in this edition |
| 020 (P0) | typed relations, `key(…)`, `key()`, `like` | 7.1 |
| 021 (P0) | inference; errors list all evidence | 5.6, 20 |
| 022 (P0) | scalar types | 5.1 |
| 023 (P0) | tuples, `Vec`, `Set`, `Map`, structs, enums | 5.2 |
| 024 (P0) | canonical total order | 5.5 |
| 025 (P1) | `Option<T>`, no null | 5.2 |
| 026 (P1) | `Mod<N>`, `0x…I`, `a<..=b` ring intervals | 5.1, 9.4 |
| 027 (P1) | `extern type` | 16.3 |
| 028 (P1) | `Blob` | 5.1, 16.6 |
| 040–045 (P0) | `table`, `scratch`, `channel`, `input`/`output`, `durable`, `static` | 7.2–7.7 |
| 046 (P1) | `loopback`, `next localtick()` | 7.8, 7.15 |
| 047 (P1) | `view` (inferred schema; subsumes `temp`) | 8.3 |
| 048 (P1) | `soft table … ttl … max …` | 7.9 |
| 049 (P1) | `sealed table` | 7.10 |
| 050 (P1) | `range(col)` | 7.11 |
| 051 (P1) | `stdin`, `stdout`, table-function file sources, `#[readonly]` | 7.15, 16.2 |
| 052 (P1) | `halt` | 7.15 |
| 053 (P1) | `#[materialize]`, `#[recompute]` | 7.16 |
| 054 (P2) | host-backed collections | not in this edition |
| 060–064 (P0) | `emit`, `next`, `send`, `delete`, `upsert` | 8.2 |
| 065 (P0) | explicit persistence recognized as storage | 7.2 |
| 066 (P0) | the legality matrix | 12 |
| 067 (P0) | host inputs only for future ticks | 7.6, 16.5 |
| 068 (P0) | labels; stable rule ids | 4.3 |
| 069 (P1) | `fact … at tick k` | 8.4, 17.2 |
| 070 (P1) | `r(…) @ n at tick k` in specs | 17.3 |
| 071 (P1) | `inserted`, `deleted` | 9.10 |
| 072 (P2) | entanglement | reserved: BLS0907 |
| 080–081 (P0) | positional and named atoms, constants | 9.2 |
| 082–083 (P0) | `not`, `not { … }`, the three anti-joins | 9.3 |
| 084 (P0) | Rust precedence plus `not`, `**`, `++`, ranges | 3.3 |
| 085 (P0) | `let` | 9.4 |
| 086 (P0) | joins | 9.2 |
| 087 (P1) | `outer` | 9.6 |
| 088 (P1) | generators, destructuring | 9.4 |
| 089 (P1) | `any`, view alternatives, `if` values | 9.7, 8.3 |
| 090 (P1) | `in` membership | 9.4 |
| 091 (P1) | `r[k]`, `r[lo..hi]` | 9.9 |
| 092 (P1) | ranges and table functions with binding patterns | 9.4, 16.2 |
| 093 (P1) | order-sensitive operators with canonical ties | 10.3, 10.5 |
| 094 (P1) | `r.keys`, `r.values`, `c.payloads`, `schema_of` | 9.13 |
| 095 (P1) | `#[localize(chain \| link)]` | 14.6 |
| 096 (P2) | NDlog link literals | not in this edition |
| 097 (P1) | `index!(by … per …)` | 10.5 |
| 098 (P1) | `seq!(…)` | 10.5 |
| 099 (P2) | in-tick recursive greedy choice | not in this edition (BLS0503) |
| 100–101 (P0) | head aggregates, GROUP BY, non-monotone | 10.1 |
| 102–104 (P1) | collection, exemplary, statistical aggregates | 10.1, 10.3 |
| 105 (P1) | `aggregate` items | 10.8 |
| 106 (P1) | `per` drivers and `default` | 10.2 |
| 107 (P2) | an aggregate choosing the destination | not in this edition |
| 108 (P0) | `choose!(Ȳ per X̄)` | 10.4 |
| 109 (P1) | `reduce!` | 10.6 |
| 110 (P0) | `fold!`, carried form with `per` | 10.6 |
| 111 (P1) | `majority(s, R)`; spec `quorum` | 11.6, 17.3 |
| 112 (P1) | derived combiners | 10.8 |
| 113 (P1) | `ola_*!`, `scale_by_progress!` | 10.9 |
| 114–116 (P1) | `least`/`most`, `sticky`, several choices | 10.4 |
| 117 (P1) | `resolve` | 10.7 |
| 118 (P0) | canonical order everywhere | 5.5, 10 |
| 120–124 (P0) | lattice types, columns, merges, conversions, built-ins | 11.1–11.5 |
| 125 (P0) | operation classes and the bang rule | 11.4 |
| 126 (P0) | thresholds, `threshold(…)` | 11.6 |
| 127 (P0) | `reveal!` | 11.4 |
| 128 (P0) | persistent and scratch lattices | 11.1 |
| 129 (P0) | typed ⊥ | 11.3 |
| 130–131 (P0) | `VClock`, `Lex`, `Ballot`, `Lww` | 11.5 |
| 132–134 (P1) | `LDom`, tombstone and causal lattices | 11.5 |
| 135 (P1) | user lattices and method classes | 11.8 |
| 136 (P1) | `unsafe DomPair` | 11.5 |
| 137 (P1) | lattices in messages | 11.9 |
| 138 (P1) | `zset`/`bag`, banged views | 11.10 |
| 139 (P1) | `snapshot` | 10.10 |
| 142 (P1) | groups and rings | 11.10 |
| 150–152 (P0) | locations, body locality, `self`, roles as member sets, `node_dir` | 14.1, 6.10, 18.1 |
| 153 (P1) | cluster roles | 6.10 |
| 154 (P1) | `partition by`, `R.route(k)`, `owner` | 14.3 |
| 155 (P1) | `#[fault(…)]` | 14.2 |
| 158 (P1) | `exactly_once(…)` | 11.10 |
| 170–175 (P0) | `tick()`, `now()`, timers, `random()`, `rand(…)` | 15 |
| 180–181 (P0) | built-in library, pure `fn`, `extern fn` | 16.1, 16.2, B |
| 182 (P1) | declared properties and classes | 16.1 |
| 183 (P1) | `extern table fn` | 16.2 |
| 184 (P1) | `service` | 16.4 |
| 185 (P0) | host API | 16.5 |
| 186 (P1) | `#[handler]` outputs | 16.6 |
| 190 (P0) | `fact`, `bootstrap`, `bootstrap fresh` | 8.4, 22.3 |
| 200 (P0) | `invariant … : never …` | 17.1 |
| 201 (P0) | `spec` | 17 |
| 202 (P1) | `std::catalog` | 7.15 |
| 203 (P2) | metaprogramming and hot install | admin plane only (19.4) |
| 204 (P1) | `#[nondet("…")]` | 13.6 |
| 205 (P1) | `#[trusted("…")]` | 17.6 |
| 206 (P1) | `#[atomic]` outputs | 7.6 |
| 207 (P1) | `sealed by`, `seal`, `sealed … [from m]` | 14.4 |
| 208 (P0) | `//`, `/* */`, `#` comments | 2.2 |
| 212 (P1) | `final output`, `final`/`final not` literals, `when_final` | 14.5 |
| 220 (P1) | the `.ded` frontend | 21.1 |
| 221–223 (P2) | other frontends | 21.2 |
| 240 (P0) | `Principal`, `node_dir`, `principal_of`, `role_of` | 18.1 |
| 241 (P0) | `from`, `principal` | 18.2 |
| 242 (P0/P1) | inferred ACLs; `#[accept(…)]`; `acl` | 18.3 |
| 243 (P1) | `external` roles, sessions | 18.4 |
| 244 (P1) | authorization idiom | 18.5 |
| 245 (P2) | `Signed<T>` | reserved: BLS0907 |
| 260 (P0) | `program … version`, `schema.lock` | 19.1 |
| 261 (P0) | `#n`, `#[since]`, defaults, `#[reserved]`, `#[unknown]` | 19.2 |
| 262 (P0/P1/P2) | `migrate from N`; `down` reserved | 19.3 |
| 263 (P1) | `translate c to/from N` | 19.4 |
| 264 (P1) | `cluster_version() >= V`, `#[unsafe_ungated]` | 19.4 |
| 265 (P1) | `#[deprecated]`, `#[semantics_changed]` | 19.2 |
| 280–284 (P0/P1) | generators and lookups, adjoined ⊥, `sum_values`, `join` defaults, `Lex` reset | 11.3, 11.5, 11.4, 11.9 |

---

## Appendix B. The built-in library (LANG-180)

All functions are pure. Methods on values use `.`; there are no closures outside function bodies.

| Area | Functions and methods |
|---|---|
| Arithmetic | `+ - * / % **` (checked); `abs`, `min(a, b)`, `max(a, b)`, `clamp`, `wrapping_add`, `wrapping_sub`, `wrapping_mul`, `saturating_add`, `pow`, `sqrt` (`f64`) |
| Bits | `& \| ^ ~ << >>`, `count_ones`, `leading_zeros` |
| Strings | `len`, `++`, `split_whitespace() -> Vec<String>`, `split(sep)`, `to_lowercase`, `to_uppercase`, `trim`, `starts_with`, `ends_with`, `contains`, `replace`, `parse_u64() -> Option<u64>`, `parse_i64`, `to_string` (every type; this build: integers, `f64`, `bool`, `String`), `x.to_fixed(n)` (`f64`: `n` digits after the point, at most 64) |
| Bytes | `len`, `slice(lo, hi) -> Option<Bytes>`, `concat`, `to_hex`, `from_utf8() -> Option<String>`; big-endian reads `u8_at(p)`, `i8_at(p)`, `u16_be_at(p)` … `i64_be_at(p) -> Option<T>`; patches `put_u8(p, x)` … `put_i64_be(p, x) -> Option<Bytes>`; varints `uvarint_at(p) -> Option<(u64, u64)>`, `varint_at(p) -> Option<(i64, u64)>` (value and next position; `None` when truncated, longer than 10 bytes or past `u64`); `Bytes::from_u8(x)` … `Bytes::from_i64_be(x)`, `Bytes::uvarint(x)`, `Bytes::varint(x)`, `Bytes::empty()`, `Bytes::join(v)`; `s.to_utf8()` on strings |
| Vec | `len`, `get(i) -> Option<T>`, `first`, `last`, `push`, `concat`, `contains`, `enumerate() -> Vec<(u64, T)>`, `sort`, `reverse`, `flatten()` (on a `Vec<Vec<T>>`), `dedup`, `map`, `filter`, `filter_map`, `fold`, `scan(init, |acc, x| e) -> Vec<A>` (the accumulator after each element), `scan_while(init, |acc, x| e) -> Vec<A>` (`e: Option<A>`; as `scan`, stopping at the first step that is `None`: over a `range`, a loop that ends early), `all`, `any` (closures: function bodies only); `to_set() -> Set<T>`, `to_map() -> Map<K, V>` (on a `Vec<(K, V)>`: a repeated key keeps its last value) |
| Ranges | `range(lo: u64, hi: u64) -> Vec<u64>`: `lo` up to, not including, `hi`; a combinator over `range(…)` walks it without building it |
| Set | `len`, `contains`, `insert`, `remove`, `union`, `intersection`, `difference`, `items() -> Vec<T>` |
| Map | `len`, `get(k) -> Option<V>`, `contains(k)`, `contains_key`, `insert`, `remove`, `keys`, `values`, `entries() -> Vec<(K, V)>` |
| Option | `is_some`, `is_none`, `unwrap_or(d)`, `map`, `and_then`; `Some`, `None` |
| Time | `now()`, `tick()`; `Duration::from_millis`, `.as_millis()` (a `Duration`'s, or an `Instant`'s since the deployment epoch, which is the Unix epoch in a deployment), `Instant - Instant`, `Instant ± Duration` |
| Randomness | `random()`, `rand(k…)`, `rand_float(k…)`, `rand_range(lo, hi, k…)` (§15.1) |
| Hashing and ids | `hash64(x)` (canonical fingerprint, stable across versions), `fingerprint(x)`; `std::hash::sha256` |
| Locations | `self`, `R.size()`, `R.route(k)`, `c.owner(k)`, `principal_of(n)`, `role_of(n)` |
| Lattices | the constructors and methods of §11.5; `reveal!`, `threshold(…)`, `majority(s, R)`, `when_final(e)` |
| Versions | `cluster_version()` |
| Errors | `error("message")`: a located hard error (BLSR010) |

---

## Appendix C. Reserved words and attributes

**Hard keywords** (§2.3): `as bootstrap channel choreography const delete else emit enum extern false fn for if impl
import in include input interpose invariant lattice let loopback match migrate module next not on output override
param program protocol pub scratch seal self send spec static struct table translate true type upsert use view where
while`.

**Contextual keywords**, by position:

| Position | Words |
|---|---|
| program header | `version`, `edition` |
| item start | `role`, `at`, `cell`, `timer`, `fact`, `service`, `aggregate`, `snapshot`, `block`, `acl`; modifiers `durable`, `soft`, `sealed`, `zset`, `bag`, `final`, `monotone`; function classes `morphism`, `bimorphism`, `monotone`, `antitone`, `threshold`, `stable` |
| role kinds | `process`, `cluster`, `external` |
| import | `with` |
| module parameters | `rel` |
| function signatures | `after` |
| types | `unsafe` |
| aggregate items | `State` |
| declaration clauses | `like`, `key`, `ttl`, `max`, `range`, `resolve`, `partition`, `by`, `over`, `sealed`, `producers`, `exactly_once`; policies `choose`, `choose_rand`, `choose_least`, `choose_most`, `merge`, `sticky` |
| timers | `every`, `ticks`, `times`, `once`, `after` |
| statements | `to`, `weight`, `resolve`, `fresh` (after `bootstrap`) |
| body literals | `outer`, `inserted`, `deleted`, `sealed`, `final`, `per`, `any`, `forall`, `ever`, `sent`, `quorum` |
| atom suffixes | `from`, `principal`, `weight`, `at` + `tick` |
| bang-call clauses | `per`, `by`, `default`, `least`, `most`, `sticky`, `durable`, `release`, `asc`, `desc` |
| expressions | `set`, `map` (before `[`), `lset`, `lmax`, `lmin`, `lbool`, `lmap`, `lbag`, `lpset` (before `{`) |
| invariants | `never` |
| migrations and translations | `from`, `to`, `down` |
| snapshots | `of`, `at`, `progress`, `every`, `upto`, `mode`, `estimate` |
| specs | `nodes`, `assign`, `faults`, `liveness`, `eventually`, `within`, `ticks`, `after`, `eff`, `prove`, `by`, `induction`, `using`, `expect`, `confluent`, `deterministic`, `check`, `ldfi`, `bmc`, `smt`, `sim`, `asp`, `holds`, `fails` |
| ACL items | `accept` |

**Built-in attributes**:

| Attribute | On | § |
|---|---|---|
| `#[fault(lossy \| lossy_delayed \| reliable \| reliable_ordered)]` | channels | 14.2 |
| `#[accept(roles…, external, principal in REL)]` | channels | 18.3 |
| `#[replicated]` | channels, inputs | 14.1 |
| `#[atomic]`, `#[handler("path")]` | outputs | 7.6, 16.6 |
| `#[nondet("reason")]` | handlers, views, statements, relations, outputs | 13.6 |
| `#[deterministic]` | outputs | 13.6 |
| `#[trusted("reason")]`, `#[finite(cap = N)]` | modules, choreographies | 17.6, 14.5 |
| `#[readonly]` | tables | 7.15 |
| `#[materialize]`, `#[recompute]` | views, scratches | 7.16 |
| `#[localize(chain \| link)]` | handlers | 14.6 |
| `#[on_violation(abort \| alert \| log \| ship(n))]` | invariants | 17.1 |
| `#[allow(lint)]`, `#[warn(lint)]`, `#[deny(lint)]` | any item or statement | 20 |
| `#[unsafe_ungated("reason")]` | handlers, statements | 19.4 |
| `#[since(N)]`, `#[deprecated(since = N)]`, `#[semantics_changed(since = N)]`, `#[renamed_from("old")]` | columns, fields, variants, relations | 19.2, 19.3 |
| `#[reserved(n, …)]` | relations, structs, enums | 19.2 |
| `#[unknown]` | enum variants | 5.2, 19.2 |
| `#[injective]`, `#[commutative]`, `#[associative]`, `#[idempotent]` | functions, aggregates | 16.1, 10.8 |
