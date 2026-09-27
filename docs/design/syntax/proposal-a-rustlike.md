# Blossom surface syntax, proposal A: "Rust-flavored relational"

Status: proposal (design phase). Author: Claude, 2026-09-27. Normative inputs: `docs/DECISIONS.md`,
`docs/research/FEATURES.md` (§1 CR-xx, §2 LANG-xxx), research reports R01–R15.

This proposal gives Blossom declarations that look like Rust items and rules that look like Datalog with Rust
patterns. Every construct lowers to the Dedalus core IR (ENG-001). Section 3 gives the lowering for each
construct; section 4 is the example corpus; section 5 is the self-critique. Appendix A maps every P0/P1 LANG
feature to the construct that expresses it.

---

## 1. Design philosophy and overview

### 1.1 Philosophy

**A Rust programmer should be able to read it cold.** Blossom's declarations are Rust items: `struct`, `enum`,
`type`, `const`, `fn`, `impl`, `mod`-like `module`s with `pub` interfaces, `use` paths, generics, and
`#[attributes]`. A relation declaration is a struct body with a storage-class keyword in front
(`#[durable] table store { #[key] key: Key, val: Val }`). Lattices are generic types (`LMax<u64>`,
`LMap<Node, LMax<u64>>`). A protocol is written like a trait, and a module that implements it is written like a
type with a supertrait bound (`module ReliableBroadcast<P>: Broadcast<P>`). Rust programmers already know all of
these forms.

**Rules are Datalog, and time is a keyword.** A rule is `head <- body;`. The arrow always points from the
evidence to the conclusion. When the conclusion is not "the same node, the same tick", a keyword in front of
the head says what it is instead:

| Rule | Meaning | Dedalus |
|---|---|---|
| `store(k, v) <- put(k, v);` | holds now, on this node | deductive |
| `next store(k, v) <- put(k, v);` | holds at the next tick | inductive `@next` |
| `send ack(k) @ c <- put(k, _) from c;` | arrives at `c` later | async `@async` |
| `delete store(k, v) <- del(k), store(k, v);` | gone from the next tick | frame-rule deletion |
| `upsert store(k, v) <- put(k, v);` | replaces the key's row at the next tick | delete-by-key + `@next` |

The five keywords (none, `next`, `send`, `delete`, `upsert`) are the complete set of state transitions. No
rule changes state within the tick, so `grep -E '^\s*(next|send|delete|upsert)'` lists every place where time
moves.

**Variables are lowercase and bound by occurrence.** Body atoms use Rust pattern syntax: positional
`store(k, v)`, named `store { key: k, .. }`, field punning `store { key, val }`, `_`, `..`, tuples, and enum
patterns. A lowercase name that occurs twice is a join, as in Datalog; this is the one place where Blossom
patterns differ from Rust patterns, where the second occurrence would be a new binding (§5 discusses this).
Constants are `SCREAMING_CASE` or paths (`Role::Leader`), so a lowercase identifier in a pattern is always a
variable. This removes Rust's const-vs-binding pattern ambiguity.

**Non-monotonicity is spelled with a bang.** Every operator that puts a negative edge into the dependency
graph (SEM-021) is written with `!`:

- `!store(k, _)` is negation (anti-join);
- `count!(x)`, `sum!`, `min!`, `max!`, `collect!`, `argmin!` are non-lattice aggregates;
- `choose!`, `choose_least!`, `index!`, `seq!`, `topk!`, `fold_ordered!` are choice and order-sensitive
  operators;
- `opt!(price(i, p))` is a left outer join;
- `reveal!(l)` is an exact lattice read, and `l.lt_eq!(m)` / `l.concurrent!(m)` are antitone or non-monotone
  lattice methods;
- `inserted!(r(..))` and `deleted!(r(..))` are delta pseudo-relations.

Monotone things have no bang: joins, projections, lattice merges (`a | b`), morphisms (`m.at(k)`), monotone
functions (`s.size()`), and thresholds (`s.size() >= 3`, `s.contains(x)`). The type checker enforces the
spelling. Calling an antitone or non-monotone method without `!` is an error ("`lt_eq` is antitone in `self`;
write `lt_eq!` to acknowledge a point of order"), and so is a bang on a monotone operator. Every negative edge
in a program therefore carries a `!` (the converse does not hold: `!b` on a plain `bool` is ordinary boolean
not). The points-of-order report (ANA-022) and a reviewer's eye find the same places.

**The surface hides nothing from the analyzer.** Rule bodies contain no closures and no host code (LANG-002).
Pure functions (`fn`) are written in a small total expression language, or declared `extern` with declared
properties. Every surface construct in §3 has an exact Dedalus expansion, and the IR is the only thing the
analyses, the simulator and the backends see.

**The grammar is LL(1) nearly everywhere.** Items begin with a keyword, and rules begin with a label, a rule-kind
keyword or an identifier. Expressions are parsed with a Pratt parser using Rust precedence. The only LL(3) point
is `lattice Name` (cell, type, or alias). Recovery synchronizes on `;`, `}` and item keywords (§2.6).

### 1.2 Name-level overview

**Program structure.**

| Item | Purpose | LANG |
|---|---|---|
| `program name version N;` | root header, schema lock anchor | 260 |
| `module M<P, const K: T = v>: Proto<P> { … }` | instantiable component with `pub` interfaces | 003, 006 |
| `protocol Proto<P> { input …; output …; }` | interface-only contract (a "trait" for modules) | 006 |
| `import M<P = T> as a;` | new, independent instance of a module | 004 |
| `use path::{A, B};` | bring names into scope (no instance) | — |
| `include M;` / `include "file.bls";` | flat mixin / textual include | 005 |
| `rules name { … }`, `override rules name { … }` | named rule blocks, override by name | 007 |
| `role R { … }`, `cluster C { … }`, `external X { … }` | choreographic location blocks | 009, 153, 243 |
| `const N: T = v;`, `param N: T = v;` | constant; deploy-time parameter | 010 |
| `type`, `struct`, `enum`, `fn`, `extern fn`, `extern type` | values and pure functions | 022–027, 180–183 |
| `aggregate name(…) -> T { init; step; finish; combine }` | user-defined aggregate | 105, 112 |
| `lattice Name { fields }`, `lattice Name = Type;`, `impl Name { … }` | user lattice | 135 |
| `service name(…) -> (…);` | async host service | 184 |
| `spec name for Program { … }` | LDFI / verification spec | 200–201, VER-001 |
| `migrate from N { … }`, `emit c to N { … }`, `accept c from N { … }` | schema evolution | 262–263 |
| `snapshot s of r at progress …;` | progressive snapshot | 139 |
| `acl path accept(…);` | narrow an imported channel's ACL | 242 |

**Collections** (storage class keyword, plus attributes):

| Declaration | Meaning | LANG |
|---|---|---|
| `table r { … }` | persistent (frame rule) | 040 |
| `#[durable] table r { … }` | persistent, WAL-logged, committed before sends | 044 |
| `#[soft(ttl = 3s, max = 1000)] table r { … }` | TTL state | 048 |
| `#[sealed] table r { … }` | writable only in `bootstrap` | 049 |
| `#[range(col)] table r { … }` | range-compressed set | 050 |
| `#[zset] table r { … }`, `#[bag] table r { … }` | weighted collections | 138 |
| `scratch r { … }` | tick-local | 041 |
| `channel c { @dst: Node, … }` | asynchronous, one `@` column | 042 |
| `loopback l { … }` | channel to self through the network path | 046 |
| `input r { … }` / `output r { … }` | interface (module or host boundary) | 043 |
| `static r { … }` | holds at every tick; filled by facts or config | 045 |
| `lattice x: L;` / `scratch lattice x: L;` | 0-ary lattice cell (Bloom^L identifier) | 120, 128 |
| `timer t every 100ms;`, `timer t every 5 ticks;`, `timer t once;` | physical / logical timers | 172–173 |

**Rule kinds** (keyword in front of the head): none, `next`, `send`, `delete`, `upsert`, `seal`, `deny`,
`temp`. **Body literals:** atoms, `!atom`, `!(conj)`, `per atom`, `let pat = e`, `for pat in e`,
`either { … } or { … }`, `opt!(atom)`, guards, and bang goals. **Atom suffixes:** `@ loc`, `at tick k`,
`from s`, `principal p`, `as row`.

**The bang table** (every negative-edge construct and its spelling):

| SEM-021 negative edge | Blossom spelling |
|---|---|
| negation `notin` | `!r(..)`, `!(conj)` |
| non-lattice aggregate | `count!`, `sum!`, `min!`, `max!`, `avg!`, `collect!`, `collect_set!`, `bool_and!`, `bool_or!`, `percentile!`, `argmin!`, `argmax!`, user aggregates `my_agg!` |
| deletion | `delete` rule kind (and `upsert`) |
| outer join | `opt!(r(..))` |
| order-sensitive operator | `index!`, `seq!`, `sort!`, `topk!`, `limit!`, `fold_ordered!`, `reduce!` |
| choice site | `choose!`, `choose_least!`, `choose_most!`, `choose_rand!` (and `sticky` forms) |
| antitone / NM lattice op | `x.method!(..)` for methods declared `#[antitone]` or unannotated |
| `reveal` | `reveal!(x)` |
| delta pseudo-relations | `inserted!(r(..))`, `deleted!(r(..))` |
| Z-set read into a set/lattice stratum | `distinct!(z(..))`, `clamped!(z(..), n)`, `weights!(z(..), w)` |

---

## 2. Lexical structure and grammar

### 2.1 Source files

- Source files are UTF-8 and use the extension `.bls`. A file is a module body. A file that starts with
  `program name version N;` is a program root. `.ded` files are parsed by the Molly-compatible frontend
  (LANG-220) and may be `include`d.
- `use a::b::C;` resolves `a::b` against the file tree the way Rust resolves `mod` paths: `a/b.bls` or
  `a/b/mod.bls`, relative to the crate root (the directory of the program root). `std::…` is the standard
  library.

### 2.2 Tokens

**Whitespace and comments** (LANG-208).
- `//` to end of line, and nestable `/* … */`.
- `///` and `//!` are doc comments; they attach to the next item or field, or to the enclosing item.
- `#` starts a line comment **unless** the next character is `[` (outer attribute) or the next two are `![`
  (inner attribute). So `# note`, `#1 note` and `#` alone are comments, and `#[key]` is an attribute. The rule
  keeps Molly/Overlog-style `#` comments working in pasted code while keeping Rust attributes.

**Identifiers.** `[A-Za-z_][A-Za-z0-9_]*`, excluding keywords. A raw identifier `r#next` is one token (the `#`
comment rule applies only where a token starts), so a column may be named after a keyword. By convention (enforced as a warning, and as an
error only where it removes an ambiguity):
- relations, variables, functions, modules' instance aliases: `snake_case`;
- types, lattices, modules, protocols, roles, enum variants: `CamelCase`;
- constants and params: `SCREAMING_CASE` (**error** if a `const` is lowercase, because a lowercase identifier in
  a pattern is always a binding).

**Bang identifiers.** An identifier immediately followed by `!` and then `(` (no whitespace) is a single
`BANG_IDENT` token: `count!(`, `choose!(`, `reveal!(`. After a `.` the same shape is a bang method:
`.lt_eq!(`. `a != b` and `!r(x)` are unaffected, because `!=` and a prefix `!` are not `ident!(`.

**Labels.** `'` followed by an identifier is a `LABEL` token (`'grant`). Blossom has no character literals, so
`'` has no other use. Labels name rules (LANG-068), the way Rust labels name loops.

**Keywords (reserved).**
```
program version module protocol role cluster external import use include as pub const param
type struct enum fn extern aggregate lattice impl service spec migrate emit accept snapshot acl
table scratch channel input output static loopback timer rules override bootstrap
next send delete upsert seal deny temp
let for in per either or if else match true false self super crate
```
**Contextual keywords** (keywords only in the listed position, ordinary identifiers elsewhere): `every`,
`once`, `ticks`, `times` (timers); `digest`, `weight`, `resolve` (head suffixes); `at`, `tick`, `from`,
`principal` (atom suffixes); `sticky`, `by`, `order`, `asc`, `desc`, `default`, `over` (aggregate arguments);
`like` (relation shape); `down`, `to` (versions); `of`, `progress`, `upto`, `mode`, `estimate` (snapshots);
`nodes`, `failures`, `bounds`, `invariant`, `eventually`, `within`, `after`, `eff`, `quorum`, `net` (specs).

**Punctuation and operators.**
```
<-  @  ::  .  ..  ..=  ,  ;  :  (  )  {  }  [  ]  #[  #![  '
+  -  *  /  %  **  ++  &  |  ^  <<  >>  !  &&  ||
==  !=  <  <=  >  >=  =  =>  ->  |  _
```
`<-` is always the rule arrow: the lexer takes the longest match, so `a<-b` is `a <- b`. Write `a < -b` for
"less than negative b". Rust reserved `<-` for the same reason.

**Literals.**
- Integers: `42`, `1_000`, `0xff`, `0b1010`, with an optional type suffix (`7u8`, `3i32`, `0u64`).
- Modular IDs (LANG-026): a hex literal with suffix `I`, e.g. `0x1f3aI`; its width comes from the expected
  `Mod<N>` type.
- Floats: `1.5`, `2e-3`, `1.0f64`. (`f64` is not an `Ord` lattice element; LANG-022.)
- Durations: an integer or float immediately followed by a unit: `250us`, `100ms`, `2s`, `5m`, `1h`. Type
  `Duration`.
- Strings `"…"` with Rust escapes; byte strings `b"…"` (type `Bytes`).
- `true`, `false`, `()`.
- A string literal is accepted where a `Node` or `Principal` is expected **only** in facts, `static` config and
  specs (`neighbor("b") @ "a";`), where it names a node of the deployment. Rules never build node names from
  strings.

### 2.3 Types

```
Type      = Path [ "<" TypeArg { "," TypeArg } ">" ]      (* u64, Node<Participant>, LMap<Node, LMax<u64>> *)
          | "(" [ Type { "," Type } ] ")"                 (* tuples, unit *)
          | "unsafe" Type                                 (* LANG-136: DomPair only *)
          | "impl" Path ;                                 (* reserved *)
TypeArg   = Type | Expr (* const generic *) | Ident "=" Type ;
```
Built-in scalar types (LANG-022): `bool`, `i8`–`i128`, `u8`–`u128`, `f64`, `String`, `Bytes`, `()`, `Node`,
`Node<Role>`, `Principal`, `Session`, `Duration`, `Instant`, `Mod<N>` (N-bit modular id), `Blob` (handle to an
out-of-line byte stream, LANG-028). Compound values (LANG-023): tuples, `Vec<T>`, `OrdSet<T>`, `OrdMap<K, V>`,
`Option<T>`, `struct`s and `enum`s, all compared structurally and ordered canonically (LANG-024). Lattices
(§3.6) are the `L…` family plus user lattices. Group-typed values (LANG-142): `Z`, `Zn<N>`, `ZSet<T>`.

### 2.4 Full EBNF

Notation: `{ x }` is zero or more, `[ x ]` is optional, `|` is choice, terminals are quoted, `(* … *)` are
comments. `Ident` excludes keywords. `Sep<X, s>` abbreviates `[ X { s X } [ s ] ]` (a possibly empty,
trailing-separator-tolerant list).

```ebnf
(* ======================= files and items ======================= *)
File            = { InnerAttr } [ ProgramHeader ] { Item } EOF ;
ProgramHeader   = "program" Ident "version" IntLit ";" ;
InnerAttr       = "#![" Attr "]" ;
OuterAttr       = "#[" Sep<Attr, ","> "]" ;
Attr            = Path [ "(" Sep<AttrArg, ","> ")" | "=" Expr ] ;
AttrArg         = Ident "=" Expr | Attr | Expr ;

Item            = { OuterAttr } [ "pub" ] ItemBody ;
ItemBody        = UseItem | ImportItem | IncludeItem | ConstItem | ParamItem
                | TypeAlias | StructItem | EnumItem
                | RelItem | LatticeItem | TimerItem | ServiceItem
                | FnItem | ExternItem | AggregateItem | ImplItem
                | ProtocolItem | ModuleItem | RoleItem | AclItem
                | RulesBlock | BootstrapBlock | Rule
                | SnapshotItem | SpecItem | MigrateItem | EmitItem | AcceptItem ;

UseItem         = "use" UseTree ";" ;
UseTree         = Path [ "::" ( "*" | "{" Sep<UseTree, ","> "}" ) ] [ "as" Ident ] ;
ImportItem      = "import" Path [ GenericArgs ] "as" Ident ";" ;
IncludeItem     = "include" ( Path | StringLit ) ";" ;
ConstItem       = "const" Ident ":" Type "=" Expr ";" ;
ParamItem       = "param" Ident ":" Type [ "=" Expr ] ";" ;
TypeAlias       = "type" Ident [ Generics ] "=" Type ";" ;
StructItem      = "struct" Ident [ Generics ] ( "{" Sep<FieldDecl, ","> "}" | "(" Sep<Type, ","> ")" ";" ) ;
EnumItem        = "enum" Ident [ Generics ] "{" Sep<Variant, ","> "}" ;
Variant         = { OuterAttr } Ident [ "{" Sep<FieldDecl, ","> "}" | "(" Sep<Type, ","> ")" ] ;
Generics        = "<" Sep<GenericParam, ","> ">" ;
GenericParam    = Ident [ ":" Bounds ] [ "=" Type ]
                | "const" Ident ":" Type [ "=" Expr ] ;
Bounds          = Path [ GenericArgs ] { "+" Path [ GenericArgs ] } ;
GenericArgs     = "<" Sep<TypeArg, ","> ">" ;

(* ======================= relations ======================= *)
RelItem         = RelKind Ident [ Generics ] RelShape ;
RelKind         = "table" | "scratch" | "channel" | "input" | "output" | "static" | "loopback" ;
RelShape        = "{" Sep<FieldDecl, ","> "}"                 (* named columns *)
                | "(" Sep<Type, ","> ")" ";"                  (* positional columns 0, 1, … *)
                | ":" Type ";"                                (* rows of a struct type *)
                | "like" Path ";" ;                           (* reuse another relation's schema *)
FieldDecl       = { OuterAttr } [ "@" ] Ident ":" Type [ "=" Expr ] ;
LatticeItem     = [ "scratch" ] "lattice" Ident ":" Type ";"            (* 0-ary cell *)
                | "lattice" Ident [ Generics ] "=" Type ";"             (* lattice alias / composition *)
                | "lattice" Ident [ Generics ] "{" Sep<FieldDecl, ","> "}" ;  (* product-lattice DSL *)
TimerItem       = "timer" Ident ( "every" Expr [ "ticks" ] [ "times" Expr ] | "once" ) ";" ;
ServiceItem     = "service" Ident "(" Sep<Param, ","> ")" "->" "(" Sep<Param, ","> ")" ";" ;
Param           = Ident ":" Type ;

(* ======================= functions ======================= *)
FnItem          = "fn" Ident [ Generics ] "(" Sep<FnParam, ","> ")" "->" Type Block ;
FnParam         = "self" | Pattern ":" Type ;
Block           = "{" { "let" Pattern [ ":" Type ] "=" Expr ";" } Expr "}" ;
ExternItem      = "extern" ( "fn" Ident [ Generics ] "(" Sep<Param, ","> ")" "->" Type
                           | "table" "fn" Ident "(" Sep<Param, ","> ")" "->" "(" Sep<Param, ","> ")"
                           | "type" Ident [ ":" Bounds ]
                           | "lattice" Ident [ Generics ] ) ";" ;
AggregateItem   = "aggregate" Ident [ Generics ] "(" Sep<Param, ","> ")" "->" Type
                  "{" { AggField } "}" ;
AggField        = ( "type" "State" "=" Type | Ident "=" Expr ) ";" ;   (* init, step, finish, combine *)
ImplItem        = "impl" [ Path [ GenericArgs ] "for" ] Type "{" { { OuterAttr } FnItem } "}" ;

(* ======================= modules and roles ======================= *)
ProtocolItem    = "protocol" Ident [ Generics ] "{" { { OuterAttr } ( RelItem | ConstItem | TypeAlias ) } "}" ;
ModuleItem      = "module" Ident [ Generics ] [ ":" Bounds ] "{" { Item } "}" ;
RoleItem        = ( "role" | "cluster" | "external" ) Ident "{" { Item } "}" ;
AclItem         = "acl" RelRef Attr ";" ;
RulesBlock      = [ "override" ] "rules" Ident "{" { { OuterAttr } Rule } "}" ;
BootstrapBlock  = "bootstrap" "{" { { OuterAttr } Rule } "}" ;

(* ======================= rules ======================= *)
Rule            = [ LABEL ":" ] Head { "," Head } [ "<-" Body ] ";" ;
Head            = [ HeadKind ] RelRef [ HeadArgs ] { HeadSuffix } ;
                  (* A head without HeadArgs is legal only as a whole-relation copy, `a <- b;`, whose body is a
                     single bare relation name; a 0-ary relation is always written `name()`. *)
HeadKind        = "next" | "send" | "delete" | "upsert" | "seal" | "deny" | "temp" ;
HeadArgs        = "(" Sep<Expr, ","> ")"
                | "{" Sep<HeadField, ","> "}" ;
HeadField       = Ident [ ":" Expr ] | ".." Expr ;
HeadSuffix      = "@" Expr | "at" "tick" Expr | "digest" Expr | "weight" Expr | "resolve" Policy ;
Policy          = "choose" [ "sticky" ] | "choose_rand" [ "sticky" ]
                | ( "choose_least" | "choose_most" ) "(" Expr ")" | "merge" ;
RelRef          = Ident { "." Ident } ;             (* alias.alias.rel ; resolved, not parsed, as relation *)

Body            = Conj ;
Conj            = Lit { "," Lit } ;
Lit             = "!" ( "(" Conj ")" | AtomLit )                        (* negation *)
                | "per" ( AtomLit | "(" Conj ")" )                      (* driver for defaults *)
                | "let" Pattern "=" Expr
                | "for" Pattern "in" Expr
                | "either" "{" Conj "}" { "or" "{" Conj "}" }
                | "quorum" Ident "in" RelRef "{" Conj "}"               (* spec only *)
                | "net" AtomLit                                        (* spec only *)
                | AtomLit ;
AtomLit         = Expr { AtomSuffix } ;             (* atoms, guards and bang goals share Expr syntax *)
AtomSuffix      = "@" Expr | "at" "tick" Expr | "from" Pattern | "principal" Pattern | "as" Ident
                | "weight" Pattern ;

(* ======================= expressions (Pratt; see 2.5) ======================= *)
Expr            = Prefix { InfixOp Prefix | PostfixOp } ;
Prefix          = { "-" | "!" } Primary ;
Primary         = Literal | "self" | "_" | ".." [ Expr ]
                | Path [ Turbofish ]
                | Path "(" Sep<Arg, ","> ")"                     (* call: fn, relation atom, constructor *)
                | Path "{" Sep<FieldPat, ","> "}"                (* named atom / struct value; not in NoStruct *)
                | BANG_IDENT Sep<BangArg, ","> { BangClause } ")"
                | "(" Sep<Expr, ","> ")"                         (* grouping, tuples *)
                | "[" Sep<Expr, ","> "]"                         (* Vec literal *)
                | "if" ExprNoStruct Block "else" ( Block | IfExpr )
                | "match" ExprNoStruct "{" Sep<MatchArm, ","> "}"
                | Closure ;                                      (* only inside fn bodies and aggregate items *)
Arg             = Expr | Ident ":" Expr ;
FieldPat        = Ident [ ":" Expr ] | ".." [ Expr ] ;
BangArg         = [ "sticky" ] Expr | "*" | Ident "=" Expr ;
BangClause      = "per" GroupKeys | "by" OrderKeys | "order" "by" OrderKeys | "default" Expr | "over" Expr ;
GroupKeys       = Expr | "(" Sep<Expr, ","> ")" ;
OrderKeys       = OrderKey | "(" Sep<OrderKey, ","> ")" ;
OrderKey        = Expr [ "asc" | "desc" ] ;
PostfixOp       = "." Ident [ Turbofish ] "(" Sep<Arg, ","> ")"
                | "." BANG_IDENT Sep<Arg, ","> ")"               (* antitone / NM method *)
                | "." ( Ident | IntLit )                        (* field or tuple index *)
                | "[" [ Expr ] "]"                               (* lookup: r[k], lattice[k], cell[] *)
                | "as" Type ;
InfixOp         = "**" | "*" | "/" | "%" | "+" | "-" | "++" | "<<" | ">>" | "&" | "^" | "|"
                | "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" IntervalOrExpr
                | "&&" | "||" | ".." | "..=" ;
IntervalOrExpr  = ( "(" | "[" ) Expr "," Expr ( ")" | "]" ) | Expr ;   (* ring interval after `in` *)
MatchArm        = Pattern [ "if" Expr ] "=>" Expr ;
Closure         = "|" Sep<Pattern, ","> "|" Expr ;
Pattern         = PatAtom { "|" PatAtom } ;
PatAtom         = "_" | ".." | Literal | Ident | Path [ "(" Sep<Pattern, ","> ")" | "{" Sep<FieldPat, ","> "}" ]
                | "(" Sep<Pattern, ","> ")" | Literal ".." "=" Literal ;
Turbofish       = "::" GenericArgs ;

(* ======================= versions, snapshots, specs ======================= *)
MigrateItem     = "migrate" "from" IntLit [ "down" ] "{" { Rule } "}" ;
EmitItem        = "emit" RelRef "to" IntLit "{" { Rule } "}" ;
AcceptItem      = "accept" RelRef "from" IntLit "{" { Rule } "}" ;
SnapshotItem    = "snapshot" Ident "of" RelRef "at" "progress"
                  ( "every" Expr "upto" Expr | "(" Sep<Expr, ","> ")" )
                  [ "mode" Ident ] [ "estimate" Expr ] ";" ;
SpecItem        = "spec" Ident [ "for" Path ] "{" { SpecMember } "}" ;
SpecMember      = "nodes" "[" Sep<Expr, ","> "]" ";"
                | "failures" "{" Sep<Ident ":" Expr, ","> "}"
                | "bounds" "{" Sep<Ident ":" Expr, ","> "}"
                | { OuterAttr } "invariant" Ident [ "(" Sep<Param, ","> ")" ] "<-" Body ";"
                | "eventually" RelRef "within" Expr "after" "eff" ";"
                | Item ;          (* spec rules, facts with `@ loc` / `at tick k`, helper relations *)
```

### 2.5 Operator precedence

Highest first; this is Rust's table plus `**`, `++` and `in`.

| Level | Operators | Assoc. |
|---|---|---|
| 1 | method call `.m(..)`, bang method `.m!(..)`, field `.f`, lookup `[..]` | left |
| 2 | unary `-`, `!` | prefix |
| 3 | `as` | left |
| 4 | `**` | right |
| 5 | `*` `/` `%` | left |
| 6 | `+` `-` `++` | left |
| 7 | `<<` `>>` | left |
| 8 | `&` | left |
| 9 | `^` | left |
| 10 | `\|` (bitwise or; lattice join on lattice operands) | left |
| 11 | `==` `!=` `<` `<=` `>` `>=` `in` | none (must parenthesize chains) |
| 12 | `&&` | left |
| 13 | `\|\|` | left |
| 14 | `..` `..=` | none |

`if … { } else { }` and `match` are primaries (LANG-089's conditional value; there is no `?:`). As in Rust, a
struct-shaped primary `P { … }` is not allowed directly in an `if`/`match` scrutinee.

### 2.6 Parsing strategy and error recovery

- **Dispatch.** After attributes and `pub`, the first token selects the item (keywords), or a rule (`LABEL`,
  a rule-kind keyword, or an identifier). `lattice` needs three tokens (`lattice x :` cell, `lattice X =`
  alias, `lattice X {`/`<` DSL). `scratch lattice` needs two. Every other decision is LL(1).
- **Atoms are parsed as expressions.** `store(k, v)`, `is_valid(x)` and `Some(x)` are all `Call` nodes, and
  `store { key, .. }` is a `StructPat` node. Name resolution (relations, functions, constructors live in one
  namespace per scope) classifies them afterwards. `_` and `..` are primaries that are legal only in pattern
  positions (atom arguments, `let`, `for`, `match`); the checker reports them elsewhere. This keeps the parser
  context-free and lets an IDE parse incomplete code.
- **Suffixes stop the Pratt loop.** `@`, `from`, `principal`, `at`, `as`, `weight`, `digest` and `resolve` are
  not infix operators, so an expression ends before them.
- **Recovery.** On an error inside a rule, the parser skips to the next `,` at body depth 0 (keeping the other
  literals), else to `;`. On an error inside an item, it skips to the matching `}` or the next item keyword that
  starts a line. A missing `;` is detected when a token that can only start an item or a rule (`table`,
  `next`, a `LABEL`, …) follows a complete rule on a new line; the parser inserts the `;` and reports it.
  Unbalanced brackets are recovered with an indentation-aware matcher. Every error carries the span and the
  expected-token set.

---

## 3. Constructs and their lowering to Dedalus

### 3.0 The lowering target

Every lowering below is written in the textual form of the Dedalus core IR. The conventions follow R02 and
R11/G1 §3:

- IR variables are `Uppercase`. Surface variable `k` becomes IR `K`.
- Local atoms carry the location `self` and the tick implicitly (Dedalus sugar). An async head names its
  destination as the first column, `m(@D, X̄)@async`, which is the IR normal form of CR-14.
- A delivered channel tuple carries a trailing, runtime-filled sender column (SEM-091). The IR writes it as the
  last argument: `m(X̄, S)`. When no rule reads it, it is projected away and set semantics is unchanged.
- Rule kinds: `h :- b.` (deductive), `h@next :- b.` (inductive), `h(@D, …)@async :- b.` (async).
- `notin p(X̄)` is negation. `X := e` binds a fresh variable. `count<X>`, `min<X>`, … are head aggregates
  (GROUP BY the other head terms, set semantics).
- Lattice-valued relations (Dedalus^L, SEM-100): `p(K̄; E)` is a head or generator whose lattice value is `E`;
  `X = p[K̄]` is a lookup that returns ⊥ for an absent cell. A 0-ary cell is `p(; E)` / `X = p[]`.
- Built-in inputs sampled once per tick: `$now`, `$tick`, `$boot` (true only in the first tick of an
  incarnation), `$seed`. Built-in relations: `R$members(N)` (members of role `R`), `$dir(N, Addr, P, Role)` (the
  node directory, LANG-240).
- Compiler-generated relations use `$` in their name (`store$del`, `s3$cand`), which no surface name can
  contain, so a lowering never captures a user name. A site id `sN` is stable (module, rule label, ordinal:
  SEM-084).
- A relation declaration in the IR is `decl <class> name(key columns | value columns)`, plus storage flags.

### 3.1 Programs, facts and bootstrap (LANG-001, 190, 069, 260)

A program root starts with its header. The order of items and rules is irrelevant (LANG-001).

```
program kvs version 3;
```
Lowering: `decl program kvs version 3` in the catalog; the schema lock (LANG-260) is keyed by it.

**Facts** are rules with no body. A fact without a time holds at every tick (CR-16). A fact with `at tick k` is
an event at tick k (LANG-069).
```
edge(1, 2, 5);
bcast("hello") at tick 1;
```
```
edge(1, 2, 5).            // holds at every tick of every node that runs this program
bcast("hello")@1.
```
In a `spec` or a scenario, a fact also names its node: `neighbor("b") @ "a";` → `neighbor(@a, b).`

**Bootstrap** rules run in the first tick of every incarnation (LANG-190; SEM-012; after a restart the
durable relations are already loaded, SEM-071). A `next` inside `bootstrap` takes effect in that same first tick,
as Bloom's bootstrap `<+` does.
```
bootstrap {
    config(k, v) <- default_config(k, v);
    next epoch(0);
}
```
```
config(K, V) :- $boot, default_config(K, V).
epoch(0)     :- $boot.                           // `next` in bootstrap = now, in the boot tick
```

### 3.2 Relation declarations and collection kinds (LANG-020, 040–053, 121)

**Schemas.** A relation body is a struct body. Every column is a key unless some field carries `#[key]`, in which
case exactly the `#[key]` fields are the key (LANG-020). `#[key()]` on the relation declares an empty key: the
relation holds at most one row (a register). `#[key(a, b)]` on the relation is the same as `#[key]` on `a` and
`b`. A relation with lattice-typed columns has as its key exactly its non-lattice columns (LANG-121); putting
`#[key]` on a lattice column, or declaring a key that is not all the non-lattice columns, is an error.

```
table link { src: Node, dst: Node, cost: u64 }                       // key = (src, dst, cost)
table store { #[key] key: String, val: Bytes }                       // key = (key)
#[key()] table leader_hint { node: Node, term: u64 }                 // register
table votes_by_term { term: u64, voters: LSet<Node> }                // key = (term), value lattice LSet
scratch edge(u32, u32);                                               // positional: columns `0`, `1`
#[key(dst, src, ident)] struct Delivery<P> { dst: Node, src: Node, ident: u64, payload: P }
table buf: Delivery<P>;                                               // rows of a struct; key from the struct
table buf2 like buf;                                                  // same schema (Bud `pipe_in.schema`)
```
```
decl table link(src: Node, dst: Node, cost: u64 |)
decl table store(key: String | val: Bytes)
decl table leader_hint(| node: Node, term: u64)
decl table votes_by_term(term: u64 ; LSet<Node>)
decl scratch edge(0: u32, 1: u32 |)
decl table buf(dst: Node, src: Node, ident: u64 | payload: P)
decl table buf2(dst: Node, src: Node, ident: u64 | payload: P)
```
The engine enforces the key constraint at every tick (SEM-050): two distinct rows with the same key are a runtime
error unless all the differing columns are lattices, in which case they merge.

**`table`** (LANG-040) — persistent. Every table gets the frame rule, and deletions go to a generated `$del`
relation:
```
table link { src: Node, dst: Node, cost: u64 }
```
```
link(S, D, C)@next :- link(S, D, C), notin link$del(S, D, C).
```

**`scratch`** (LANG-041) — no frame rule; empty at the start of each tick. `next s(..)` into a scratch appears
only in the next tick.

**`#[durable] table`** (LANG-044) — same rules as `table`; the IR declaration carries `durable`, so the tick's
staged deltas are appended to the WAL and fsynced before the outbox is released (SEM-072). Columns get stable
field numbers (§3.13).
```
#[durable] table store { #[key] key: String, val: Bytes }
```
```
decl table store(key: String | val: Bytes) durable
store(K, V)@next :- store(K, V), notin store$del(K, V).
```

**`static`** (LANG-045) — holds at every tick. Its rows come from bodyless facts or from the deployment config
under the same name (`static member { n: Node }` reads the config key `member`). Writing a `static` relation from
a rule is an error (LANG-066).
```
decl static member(n: Node |)            // an EDB present at every tick; no rule may derive it
```

**`input` / `output`** (LANG-043) — tick-local interface relations. The catalog records the direction (LANG-003).
Host insertions into an `input` land in the host's next tick (LANG-067); a rule in the importing module may
write an instance's `pub input` in the current tick.
```
decl input  pipe_in(dst: Node, src: Node, ident: u64 | payload: P) interface(in)
decl output pipe_out(dst: Node, src: Node, ident: u64 | payload: P) interface(out)
```

**`channel`** (LANG-042, 150) — exactly one field is marked `@`; its type is `Node`, `Node<Role>` or `Session`.
Only `send` rules may write a channel. In atoms the `@` column is **omitted**: the head supplies it with
`@ expr`, and on the receiving side it is always `self`.
```
channel ack { @dst: Node, id: u64 }
send ack(i) @ s <- msg(i, _) from s;
got(i) <- ack(i);
```
```
decl channel ack(@dst: Node, id: u64 |)
ack(@S, I)@async :- msg(I, _, S).
got(I) :- ack(I, _).                               // trailing column: sender, unused → projected away
```
A channel's key is checked at the sender (Bud semantics, R03 §2.6).

**`loopback`** (LANG-046) — a channel whose destination is always `self`, delivered through the network path, so
the tuple arrives in a later tick. No `@` column is declared or written.
```
loopback retry_later { id: u64 }
send retry_later(i) <- failed(i);
```
```
retry_later(@self, I)@async :- failed(I).
```
`send localtick();` is the built-in 0-ary loopback: it requests another tick.

**`#[soft(ttl = T, max = M)] table`** (LANG-048, SEM-060/061) — TTL state with deterministic expiry at tick
boundaries against the tick's sampled `now`. A hidden birth column is stored; a re-derivation refreshes it.
```
#[soft(ttl = 3s, max = 1024)] table alive { peer: Node }
alive(s) <- heartbeat() from s;
```
```
decl table alive$s(peer: Node | birth: Instant)           // storage
decl scratch alive$d(peer: Node |)                         // derived this tick (all surface rules into `alive`)
alive$d(S) :- heartbeat(S).                                // each surface rule now targets alive$d
alive$live(X, B) :- alive$s(X, B), $now - B < 3s.          // present unless expired
alive$live(X, $now) :- alive$d(X).                         // derived now = born/refreshed now
alive$top(X, max<B>) :- alive$live(X, B).                // latest birth wins (refresh)
alive$rk(X, I) :- alive$top(X, B), I = index<by (B, X) desc>.   // §3.5 index expansion
alive(X) :- alive$rk(X, I), I < 1024.                      // `max`: keep the newest M, evict by (birth, canonical)
alive$s(X, B)@next :- alive$top(X, B), alive(X), notin alive$del(X).
```
A `next alive(x)` rule lowers to `alive$n(X)@next :- …` plus `alive$d(X) :- alive$n(X).`, so the tuple's birth is
the tick at which it appears. A soft head derived every tick from a soft body is re-derived, and so refreshed,
every tick (SEM-061). ANA-006 checks TTL(head) ≥ TTL(body).

**`#[sealed] table`** (LANG-049) — only `bootstrap` rules may write it; any other writer is a compile error. After
the boot tick the compiler emits a whole-relation seal.
```
decl table config(…) sealed
config$sealed() :- notin $boot.                  // CLOSED for ANA-121 from the second tick on
```

**`#[range(col)] table`** (LANG-050) — every column is a key; `col` (an integer column) is stored as disjoint
`[lo, hi]` buckets per value of the other columns. Semantics are those of `table`; only the storage differs:
`decl table acked(src: Node, seq: u64 |) range(seq)`.

**`#[readonly] table`**, **`#[file_reader("path")] input lines { #[key] lineno: u64, text: String }`**, **`stdin`,
`stdout`, `halt`** (LANG-051, 052) — built-in or host-fed sources. `stdin(line)` is an input holding the lines read
since the last tick. `send stdout(text)` writes at the end of the tick in canonical order (LANG-118). `halt(kill)`
is a built-in output: `halt(false) <- done();` stops the node at the end of the tick, `halt(true)` also the
process. Every rule kind that writes a read-only source is rejected (LANG-066).

**`#[materialize]` / `#[recompute]`** (LANG-053) on a `scratch` or derived `output` selects incremental
maintenance or recomputation. The IR carries the flag; the meaning is unchanged.

**Timers** (LANG-172, 173) — a timer is an `input` relation `t(id: u64, at: Instant)` that the runtime fills.
```
timer beat every 100ms;              // physical; virtual time under simulation (ODD-16)
timer probe every 5 ticks;           // logical: counts local ticks
timer settle every 1s times 10;      // at most 10 events
timer init once;                     // one event in the first tick of each incarnation
```
```
decl input beat(id: u64 | at: Instant) timer(physical, 100ms)
decl input probe(id: u64 | at: Instant) timer(logical, 5)
decl input settle(id: u64 | at: Instant) timer(physical, 1s, times 10)
init(0, $now) :- $boot.
```

**`temp`** (LANG-047) — a scratch declared by its single defining rule; the schema is inferred. A `temp` whose
name shadows a declared relation is an error.
```
temp acked(i, s) <- buf(_, s, i, _), ack(i) from s;
```
```
decl scratch acked(0: u64, 1: Node |)            // inferred
acked(I, S) :- buf(_, S, I, _), ack(I, S).
```

**Whole-relation copy.** When two relations have the same row type, `a <- b;` (optionally with a kind:
`next a <- b;`, `send a <- b;` for a channel whose `@` column is part of the row) copies every row. This is the
Bloom `buf <= pipe_in` idiom and is how interposition glue is usually written.
```
buf <- pipe_in;
```
```
buf(D, S, I, P) :- pipe_in(D, S, I, P).
```

### 3.3 Rules and temporal kinds (LANG-060–068, 117, 200)

**Deductive** (LANG-060) — same node, same tick; may be recursive.
```
reach(a, b) <- edge(a, b);
reach(a, c) <- reach(a, b), edge(b, c);
```
```
reach(A, B) :- edge(A, B).
reach(A, C) :- reach(A, B), edge(B, C).
```

**`next`** (LANG-061) — the head holds at t+1. Inductive rules read the completed fixpoint of tick t and never
recurse within a tick (SEM-003).
```
next seen(m) <- msg(m);
```
```
seen(M)@next :- msg(M).
```

**`send … @ dst`** (LANG-062) — the head is delivered to `dst` at a later tick. The `@` expression fills the
channel's `@` column. A head on another node must be a `send` (LANG-151), and a `send` may target only a channel,
a loopback, `stdout` or a service.
```
send vote_reply(t, true) @ c <- grant(c, t);
```
```
vote_reply(@C, T, true)@async :- grant(C, T).
```

**`delete`** (LANG-063, CR-06) — removes the exact tuple from a `table` at t+1. It still holds at t (SEM-005). If
the tuple is also inserted for t+1, the insert wins (CR-05).
```
delete pending(id, n) <- ack(id) from n, pending(id, n);
```
```
pending$del(I, N) :- ack(I, N), pending(I, N).
// consumed only by the frame rule:  pending(I, N)@next :- pending(I, N), notin pending$del(I, N).
```

**`upsert`** (LANG-064) — at t+1, atomically deletes every tuple with the head's key and inserts the head.
```
upsert store(k, v) <- put_win(k, v);
```
```
store$ups(K, V) :- put_win(K, V).              // decl scratch store$ups(key | val): SEM-051 key check here
store$del(K, V0) :- store$ups(K, _), store(K, V0).
store(K, V)@next :- store$ups(K, V).           // insert wins if V0 == V (CR-05)
```
Two different upserts to one key in one tick violate the key of `store$ups`, which is SEM-051's error.

**Resolution policies** (LANG-117, ODD-02(c)) replace that error with a declared choice. On the relation, the
policy covers every candidate row for t+1 (persisted rows, `next` inserts and upserts). On an `upsert` head, it
covers only that tick's conflicting upserts.
```
#[resolve(choose_least(ts))] table reg { #[key] k: String, ts: (u64, Node), v: Bytes }   // LWW register
upsert kv(k, v) resolve choose_least(order) <- put(k, v, order);
```
```
// relation-level: candidates for t+1, then a per-key choose_least (§3.5 expansion) picks the survivor.
// These rules replace reg's frame rule; `upsert reg(..)` contributes through reg$n after deleting its key.
reg$cand(K, TS, V) :- reg(K, TS, V), notin reg$del(K, TS, V).
reg$cand(K, TS, V) :- reg$n(K, TS, V).                  // every `next reg(..)` rule targets reg$n
reg$cmin(K, min<TS>) :- reg$cand(K, TS, V).
reg(K, TS, V)@next :- reg$cand(K, TS, V), reg$cmin(K, TS).
// rule-level: the upserts of this tick compete; the loser is dropped before store$ups
kv$uc(K, V, O) :- put(K, V, O).
kv$umin(K, min<O>) :- kv$uc(K, V, O).
kv$ups(K, V) :- kv$uc(K, V, O), kv$umin(K, O).
```
(`choose_least` over a column that may tie adds the seeded priority as a tie-break, §3.5.) A relation with a
`resolve` policy may not sit on a same-tick cycle (SEM-086).

**Multiple heads.** `h1, h2 <- body;` is one rule per head over the same body; each head keeps its own kind.
```
delete candidate_in(t), next leader_in(t) <- won(t);
```
```
candidate_in$del(T) :- won(T).
leader_in(T)@next   :- won(T).
```

**`deny`** (LANG-200) — a runtime invariant. Every derived tuple is a violation; the attribute picks the action.
```
#[on_violation(abort)]
deny truncate_committed(i) <- truncate(i), commit_index(c), i <= c;
```
```
violation("truncate_committed", (I)) :- truncate(I), commit_index(C), I <= C.     // action: abort the node
```
Other actions: `alert`, `log` (with provenance), `ship(@checker)` (send the violation to a remote checker).

**Labels** (LANG-068). `'name:` labels a rule. Labels are unique per module; provenance, tracing, coverage, plan
hints and choice-site ids use them. Unlabeled rules get `module::rN`, with N the rule's ordinal in its block.

**Host insertion** (LANG-067). The host API writes only `input` relations and only for a future tick (the
runtime's staging buffer is exactly a `next`). There is no surface form for host writes into the current tick.

**Legality matrix** (LANG-066), checked by the type checker:

| target \ kind | none | `next` | `send` | `delete` | `upsert` | `seal` |
|---|---|---|---|---|---|---|
| `table` (any flavor) | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ |
| `#[sealed] table` | boot only | boot only | ✗ | ✗ | ✗ | ✗ |
| `scratch`, `output`, `temp` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| own `input` | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| instance's `pub input` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| persistent lattice relation / cell | ✓ (merge) | ✓ (merge) | ✗ | ✗ | ✗ | ✗ |
| `scratch` lattice | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| `channel`, `loopback`, `stdout`, service | ✗ | ✗ | ✓ | ✗ | ✗ | `channel` with `#[seal]` |
| `static`, timer, `stdin`, file, `#[readonly]` | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |

### 3.4 Rule bodies (LANG-080–095)

**Positional and named atoms** (LANG-080, 081). Positional arguments are patterns. Named atoms use Rust struct
patterns with punning and `..`. A literal or constant in an atom is a selection. A repeated variable is an
equality join.
```
ok(k) <- store(k, _), limit(k, k);                       // repeated k: equality
big(k) <- store { key: k, val }, val.len() > MAX_VAL;    // `val` punned: binds val
v1(k) <- store(k, b"x");                                 // constant selection (LANG-081)
rv(c) <- request_vote(t, ..) from c, t > 5;              // `..` = the rest
```
```
ok(K) :- store(K, _), limit(K, K).
big(K) :- store(K, VAL), $len(VAL) > MAX_VAL.
v1(K) :- store(K, b"x").
rv(C) :- request_vote(T, _, _, C), T > 5.
```

**Whole-row binding** (`as`). `r(..) as m` binds `m` to the row value of `r`'s row type; `h { ..m }` spreads a row
into a head. This is Bud's `pipe_sent <= msg_acked`.
```
pipe_sent { ..m } <- buf(..) as m, acked(m.ident);
```
```
pipe_sent(D, S, I, P) :- buf(D, S, I, P), acked(I).
```

**Negation and anti-joins** (LANG-082, 083). `!atom` is `notin`. Every variable in a negated atom must be bound by
a positive literal (ANA-001), except `_`. The three anti-join forms of Bud are: whole tuple, key pair, and key pair
plus a predicate. The third is a negated conjunction `!( … )`, whose inner variables are existential.
```
fresh(m) <- msg(m), !seen(m);                          // whole tuple
orphan(k) <- store(k, _), !owner(k, _);                // key pair
not_dominated(k, v) <- cand(k, v), !(best(k, w), w >= v);   // key pair + predicate
```
```
fresh(M) :- msg(M), notin seen(M).
orphan(K) :- store(K, _), notin owner(K, _).
not_dominated$b(K, V) :- cand(K, V).                              // bindings of the outer body
not_dominated$n(K, V) :- not_dominated$b(K, V), best(K, W), W >= V.
not_dominated(K, V) :- cand(K, V), notin not_dominated$n(K, V).
```

**Guards and expressions** (LANG-084). Any boolean expression is a guard. Arithmetic, bit operations, `**`,
comparisons, `&&`, `||`, `if`/`match` values, and `++` concatenation follow §2.5. Integer overflow is a runtime
error, never a wrap (checked arithmetic); `wrapping_add` and friends are explicit functions.

**`let`** (LANG-085). `let pat = expr` binds fresh variables; the planner orders literals by binding. A `let`
that would re-bind an already-bound variable is an error ("`x` is already bound; write `x == e`"): `let` never
shadows and never silently joins. A refutable pattern filters (Rust's `let … else`, without the `else`: a
non-match means the rule does not fire).
```
r(k, n) <- s(k, v), let n = v.len() * 2;
r2(x) <- s(_, v), let Some(x) = parse_u64(v);
```
```
r(K, N) :- s(K, V), N := $len(V) * 2.
r2(X) :- s(_, V), Some(X) := parse_u64(V).
```

**Joins** (LANG-086). Comma is conjunction; shared variables are equi-joins; `a.x == b.y` guards are
equivalence classes; a body with no shared variables is a Cartesian product. Semi-joins are atoms whose variables
do not reach the head. There is no separate natural-join syntax: named atoms with punning are natural joins on
the punned names (`a { k, .. }, b { k, .. }`).

**Left outer join** (LANG-087) — `opt!(atom)`. Columns of the optional atom that are first bound inside it become
`Option`-typed. It is non-monotone (bang).
```
line_total(o, q, p) <- order_line(o, item, q), opt!(price(item, p));      // p: Option<u64>
```
```
line_total(O, Q, Some(P)) :- order_line(O, I, Q), price(I, P).
line_total(O, Q, None)    :- order_line(O, I, Q), notin price(I, _).
```

**Unnest and destructuring** (LANG-088). `for pat in expr` ranges over a `Vec`, `OrdSet`, `OrdMap` (as `(k, v)`
pairs), a range, or an extern table function. Tuple and struct patterns destructure.
```
entry(i, t, c) <- append(prev, entries), for (j, (t, c)) in entries.enumerate(), let i = prev + 1 + j;
```
```
entry(I, T, C) :- append(PREV, ES), $member($enumerate(ES), (J, (T, C))), I := PREV + 1 + J.
```
`$member(Coll, X)` is the IR's built-in generator; its first argument must be bound (binding pattern, LANG-092).

**Generator relations with binding patterns** (LANG-092). Ranges and extern table functions are infinite relations
that may be used only with bound inputs; the checker rejects an unbound input with the binding pattern in the
message.
```
slot(s) <- window(lo, hi), for s in lo..hi;
part(n, line) <- doc(d), for (n, line) in split_lines(d);     // extern table fn split_lines(s: String) -> (n: u64, line: String);
```
```
slot(S) :- window(LO, HI), $range(LO, HI, S).
part(N, L) :- doc(D), split_lines$tf(D, N, L).                 // table function, input D bound
```

**Disjunction and conditionals** (LANG-089). `either { A } or { B }` lowers to one rule per branch. An `if`/`match`
in an expression is a pure value; it needs no rule split.
```
ae_ok(l) <- append_entries(_, pi, pt, ..) from l, either { pi == 0 } or { log(pi, pt, _) };
```
```
ae_ok(L) :- append_entries(_, PI, PT, _, _, L), PI == 0.
ae_ok(L) :- append_entries(_, PI, PT, _, _, L), log(PI, PT, _).
```

**Membership** (LANG-090). `x in rel` for a unary relation is the atom `rel(x)`; `x in role` is `role$members(x)`;
`x in coll` for a value collection is `$member(coll, x)`; `x in (a, b]` (and `[a, b)`, `(a, b)`, `[a, b]`) is a
ring-interval test on `Mod<N>` values that wraps around, so `(n, n]` is the whole ring (LANG-026). Emptiness is
`!r(..)`; there is no hidden `exists?` inside a closure.
```
fwd(k, n) <- lookup(k), finger(i, n), k in (me_id, n];
```
```
fwd(K, N) :- lookup(K), finger(I, N), $ring_oc(K, ME_ID, N).
```

**Indexed lookup and range scans** (LANG-091). `r[k]` on a relation keyed by `k` binds the value columns (a record,
or the single value column); if there is no row, the literal fails. `r[lo..hi]` is a range scan and is used with
`for`.
```
prev_ok(pi) <- ae(pi, pt), let (t, _) = log[pi], t == pt;
resend(f, i, t, c) <- next_idx(f, n), last(m), for (i, t, c) in log[n..=m];
```
```
prev_ok(PI) :- ae(PI, PT), log(PI, T, _), T == PT.
resend(F, I, T, C) :- next_idx(F, N), last(M), log(I, T, C), I >= N, I <= M.   // compiled to an index range query
```

**Utility projections** (LANG-094). `r.keys(k̄)`, `r.values(v̄)` and `c.payloads(..)` (a channel atom without the
sender) are atoms over projections; `r.schema()`, `r.key_cols()` are compile-time constants usable in `const`s.
Renaming is a `temp` or a whole-row spread.

**Bodies that span locations** (LANG-095, CR-15, ODD-11). Atoms with explicit `@ loc` in a protocol rule are
accepted only with `#[localize(chain)]` or `#[localize(link)]`; the compiler rewrites each hop into a `send`
(NDlog Algorithm 2 for `link`) and warns.
```
#[localize(chain)]
send path(d) @ s <- link(x) @ s, path(d) @ x;
```
```
path$hop(@X)@async :- link(X).                             // at s: ask x (chain rewrite); sender column = s
path(@S, D)@async :- path$hop(S), path(D).                 // at x: answer s   (lint: "localized cross-node body")
```

### 3.5 Aggregation, choice, numbering and folds (LANG-100–118)

All constructs in this section are negative edges (SEM-021), so all are spelled with `!`. Monotone aggregation is
written with lattices (§3.6).

**Head aggregates** (LANG-100, 101). A head term that is a bang aggregate makes the other head terms the GROUP BY.
Input is deduplicated first (set semantics). An empty group produces no row (CR-08). The lowering is Molly's
two-level rewrite, which keeps body variables that do not reach the head from changing the grouping.
```
votes_for(t, count!(v)) <- vote(t, v, _);
stats(s, min!(l), max!(l), avg!(l), sum!(l), count!(*)) <- sample(s, id, l);
```
```
votes_for$v(T, V) :- vote(T, V, _).
votes_for(T, count<V>) :- votes_for$v(T, V).
stats$v(S, ID, L) :- sample(S, ID, L).                         // count!(*) counts distinct body valuations
stats(S, min<L>, max<L>, avg<L>, sum<L>, count<(ID, L)>) :- stats$v(S, ID, L).
```
The available aggregates: `count!(x)`, `count!(*)`, `sum!`, `min!`, `max!`, `avg!` (LANG-100);
`collect!(x)` → `Vec` in canonical order, `collect_set!(x)` → `OrdSet`, `collect_map!(k, v)` → `OrdMap` (a
duplicate key with different values is a runtime error) (LANG-102; `accum_pair` is `collect_set!((a, b))`);
`bool_and!`, `bool_or!` (LANG-103); `percentile!(p, x)` (nearest rank in canonical order), `sort!(x by k)`
(LANG-104, 118); and user aggregates `name!(..)`.

**Defaults driven by an outer relation** (LANG-106, CR-08). A `per` literal marks the driver: the rule produces one
row per driver tuple even when the rest of the body is empty. `count!`, `sum!`, `collect!`, `collect_set!`,
`fold_ordered!` use their identity (0, 0, empty, empty, init); every other aggregate must say `default v`.
```
acks_so_far(r, count!(a)) <- per pending(r), ack(r, a);
best_bid(i, max!(p default 0)) <- per item(i), bid(i, _, p);
```
```
acks_so_far$v(R, A) :- pending(R), ack(R, A).
acks_so_far$a(R, count<A>) :- acks_so_far$v(R, A).
acks_so_far(R, C) :- acks_so_far$a(R, C).
acks_so_far(R, 0) :- pending(R), notin acks_so_far$a(R, _).
best_bid$v(I, B, P) :- item(I), bid(I, B, P).
best_bid$a(I, max<P>) :- best_bid$v(I, B, P).
best_bid(I, M) :- best_bid$a(I, M).
best_bid(I, 0) :- item(I), notin best_bid$a(I, _).
```
`per ( conj )` drives from a conjunction: `per (r in Reducer)`.

**Exemplary aggregates as body goals** (LANG-103). `argmin!(c per g)` keeps the body rows whose `c` is minimal in
group `g`, every tied row included. `argmax!` is symmetric. `per` may be omitted for one global group.
```
next_hop(s, d, via, c) <- route(s, d, via, c), argmin!(c per (s, d));
```
```
next_hop$m(S, D, min<C>) :- route(S, D, VIA, C).
next_hop(S, D, VIA, C) :- route(S, D, VIA, C), next_hop$m(S, D, C).
```

**Top-k and limit** (LANG-104, 093, 118). `topk!(k, by key [desc] per g)` keeps the rows whose rank (the `index!`
below) is `< k`; ties are broken by the canonical order of the whole tuple, so the result is deterministic
(ANA-038 lints "ties broken by canonical order" when the key does not determine the tuple).
```
top3(s, id, l) <- sample(s, id, l), topk!(3, by l desc per s);
```
```
top3$h(S, ID, L) :- sample(S, ID, L).
top3$r(S, ID, L, I) :- top3$h(S, ID, L), I = index<per (S) by (L desc, (S, ID, L))>.
top3(S, ID, L) :- top3$r(S, ID, L, I), I < 3.
```

**User-defined aggregates** (LANG-105, 112, 182). An `aggregate` item gives the state type, `init`, `step`, `finish`
and, when it is declared associative and commutative, `combine`. Closures are allowed here because the item is a
pure function definition, not a rule body.
```
#[commutative, associative]
aggregate mean(x: f64) -> f64 {
    type State = (f64, u64);
    init = (0.0, 0);
    step = |(s, n), x| (s + x, n + 1);
    combine = |(s1, n1), (s2, n2)| (s1 + s2, n1 + n2);
    finish = |(s, n)| if n == 0 { 0.0 } else { s / (n as f64) };
}
avg_latency(svc, mean!(ms)) <- sample(svc, _, ms);
```
```
avg_latency$v(SVC, MS) :- sample(SVC, _, MS).
avg_latency(SVC, mean<MS>) :- avg_latency$v(SVC, MS).     // mean: UDA{C,A}; engine may merge partials
```
Without `#[commutative, associative]` the engine evaluates the aggregate as `fold_ordered!` in canonical order
(LANG-110). TEST-015 shuffles to check the declaration. From `combine` the compiler derives the sender-side
partial for partitioned channels (LANG-112); a non-idempotent partial may cross a channel only through an
`#[exactly_once]` channel (§3.8).

**Estimators** (LANG-113). `ola_sum!(x, conf = 0.95, interval = clt | hoeffding(lo, hi))`, `ola_count!`,
`ola_avg!` return `(estimate, lo, hi)`; `scale_by progress` and `scale_by coverage(h)` appear in `snapshot` items
(§3.12). All carry the documented assumptions in the catalog and are typed `nondet "progressive"`.

**Choice** (LANG-108, 114–116, SEM-085). `choose!(y per x)` is the FD x → y: one y per x per tick, the candidate
with the least seeded priority. Both sides may be tuples. `choose!(y)` without `per` is one global choice per
tick. In a head, `choose!(y)` is the aggregate form: the group is the other head terms.
```
'lock: grant_lock(r, x) <- waiting(x, r), !held(r, _), choose!(x per r);
```
```
s1$cand(R, X) :- waiting(X, R), notin held(R, _).
s1$pmin(R, min<P>) :- s1$cand(R, X), P := $prio(s1, (R), (X)).        // (PRF_σc(site, R, X), X)
s1$chosen(R, X) :- s1$cand(R, X), s1$pmin(R, P), P == $prio(s1, (R), (X)).
grant_lock(R, X) :- waiting(X, R), notin held(R, _), s1$chosen(R, X).
```
- `choose_least!(c per x)` and `choose_most!(c per x)` order by `c` first, then the seeded priority, then
  canonical order; unlike `argmin!` they return exactly one exemplar (LANG-114). The expansion replaces `$prio` by
  `(C, $prio(..))`.
- `choose_rand!(y per x)` uses the node seed, incarnation and tick (`$rprio`), so it redraws every tick.
- `choose!(sticky y per x)` and `choose_rand!(sticky y per x)` keep the previous tick's choice while it is still a
  candidate (LANG-115). The expansion carries `held` with `@next`:
  ```
  s1$keep(X, Y) :- s1$held(X, Y), s1$cand(X, Y), notin s1$forced(X).
  s1$kept(X) :- s1$keep(X, _).
  s1$pminf(X, min<P>) :- s1$cand(X, Y), notin s1$kept(X), P := $prio(s1, X, Y).
  s1$fresh(X, Y) :- s1$cand(X, Y), notin s1$kept(X), notin s1$forced(X), s1$pminf(X, P), P == $prio(s1, X, Y).
  s1$chosen(X, Y) :- s1$keep(X, Y).
  s1$chosen(X, Y) :- s1$fresh(X, Y).
  s1$chosen(X, Y) :- s1$ovr(X, Y).
  s1$held(X, Y)@next :- s1$chosen(X, Y).          // `durable` if the rule carries #[durable]
  ```
  `s1$ovr` / `s1$forced` come from the simulator's `__choice` override input (TEST-012) and are empty in
  production.
- Several choose goals in one rule are the greedy multi-FD scan of R12 §5.2 (LANG-116):
  `pair(a, b) <- likes(a, b), choose!(b per a), choose!(a per b);` lowers to
  `s2$acc := fold_ordered(∅, s2$step, s2$cand(A, B) order by ($prio(s2, (A, B)), (A, B)))` and
  `pair(A, B) :- likes(A, B), $member(s2$acc, (A, B)).` (the fold is §3.5 `fold_ordered!` below).
- A choice may not sit on a same-tick recursive cycle (SEM-086); lattice-typed variables may not appear on the
  chosen side.

**`index!`** (LANG-097). A head term `index!(by k per g)` is a dense, 0-based rank within the tick, after the head
has been deduplicated; the order is `k`, then the canonical order of the whole head tuple.
```
slotted(p, index!()) <- client_req(p), is_leader();
```
```
slotted$h(P) :- client_req(P), is_leader().
slotted(P, I) :- slotted$h(P), I = count<P2> default 0 { slotted$h(P2), P2 <c P }.
// read as: slotted$lt(P, P2) :- slotted$h(P), slotted$h(P2), P2 <c P.
//          slotted$a(P, count<P2>) :- slotted$lt(P, P2).
//          slotted(P, I) :- slotted$a(P, I).   slotted(P, 0) :- slotted$h(P), notin slotted$a(P, _).
```
(`<c` is the canonical order, extended with the `by` keys first. The engine sorts the batch instead: ENG-072.)

**`seq!`** (LANG-098). A head term `seq!()` gives each distinct head tuple the next number the first time it
appears, and never reuses a number. `seq!(durable)` makes the expansion's state durable, which is required when the
number reaches a `send` or an `output` (ANA-011). `seq!(release)` frees a tuple's slot when the tuple leaves (the
number itself is still never reused). `per g` numbers per group.
```
assigned(req, seq!(durable)) <- id_request(req);
```
```
s4$h(R) :- id_request(R).
s4$assigned(R, I)@next :- s4$assigned(R, I).                      // durable
s4$has(R) :- s4$assigned(R, _).
s4$new(R) :- s4$h(R), notin s4$has(R).
s4$nrank(R, J) :- s4$new(R), J = count<R2> default 0 { s4$new(R2), R2 <c R }.
s4$ncount(count<R>) :- s4$new(R).
s4$hwm(0) :- $boot, notin s4$hwm$p(_).
s4$hwm(H) :- s4$hwm$p(H).
s4$hwm$p(H + C)@next :- s4$hwm(H), s4$ncount(C).                 // durable
s4$hwm$p(H)@next :- s4$hwm(H), notin s4$ncount(_).
assigned(R, I) :- s4$h(R), s4$assigned(R, I).
assigned(R, H + J) :- s4$nrank(R, J), s4$hwm(H).
s4$assigned(R, H + J)@next :- s4$nrank(R, J), s4$hwm(H).
```

**`fold_ordered!`** (LANG-110). `fold_ordered!(init, step, row order by key)` left-folds the pure `fn step(S, Row)
-> S` over the tick's distinct rows in (key, canonical) order.
- Aggregate form: a head term; an empty group gives no row.
- Carried-state form: with a `per state(s)` driver the fold starts from the current state and an empty body gives
  back `s`. ANA-011 lints a carried fold whose rows come from a persistent relation (it re-applies every row every
  tick, as Dedalus says).
```
fn apply(s: KvState, cmd: Cmd) -> KvState { s.apply(cmd) }
next sm(fold_ordered!(s, apply, cmd order by i)) <- per sm(s), to_apply(i, cmd);
```
```
s5$rk(I, CMD, K) :- to_apply(I, CMD), K = index<by (I)>.            // index expansion as above
s5$acc(0, S) :- sm(S).
s5$acc(K + 1, S2) :- s5$acc(K, S), s5$rk(I, CMD, K), S2 := apply(S, CMD).
s5$n(count<(I, CMD)>) :- to_apply(I, CMD).
sm(S2)@next :- s5$acc(N, S2), s5$n(N).
sm(S)@next :- sm(S), notin s5$n(_).                                  // `per`: empty body keeps the state
```
The recursion through `s5$acc` is positive and bounded by the row count, so SEM-020 accepts it.
`reduce!(init, f)` is `fold_ordered!` in canonical order unless `f` is declared commutative and associative
(LANG-109).

### 3.6 Lattices (LANG-120–137, 280–284)

**Lattice-valued relations and cells** (LANG-121, 128, 280). A relation with a lattice-typed column is
lattice-valued: its key is its non-lattice columns and two derivations with the same key merge. A lattice relation
is persistent by default (CR-24); `scratch` makes it tick-scoped. A 0-ary cell is declared with `lattice`.
```
table votes_by_term { term: u64, voters: LSet<Node> }
votes_by_term(t, LSet::of(v)) <- vote(t) from v;                 // each derivation is a singleton; they merge
won(t) <- votes_by_term(t, vs), vs.size() >= QUORUM;             // generator: non-⊥ cells only

lattice seen: LSet<Node>;                                         // a Bloom^L identifier, starts at ⊥
seen(LSet::of(v)) <- vote(_) from v;
send result() @ COORD <- seen.size() >= QUORUM;                  // a cell is read by lookup: `seen`
scratch lattice eff: LMax<u64>;                                   // reset to ⊥ every tick
```
```
decl table votes_by_term(term: u64 ; LSet<Node>)
votes_by_term(T; X)@next :- votes_by_term(T; X).                 // implicit identity rule (SEM-104)
votes_by_term(T; {V}) :- vote(T, V).
won(T) :- votes_by_term(T; VS), size(VS) >= QUORUM.              // Mon then threshold: a + edge
decl lattice seen(; LSet<Node>)
seen(; X)@next :- seen(; X).
seen(; {V}) :- vote(_, V).
result(@COORD)@async :- X = seen[], size(X) >= QUORUM.           // lookup of a 0-ary cell (D5)
decl scratch lattice eff(; LMax<u64>)                             // no identity rule
```

**Heads merge; `next` merges later** (LANG-122). A deductive head into a lattice relation merges now (`<=`), a
`next` head merges at t+1 (`<+`). `delete`, `upsert` and `send` into a lattice relation are compile errors;
lattice state is reset only with the `Lex<epoch, L>` idiom (LANG-284).

**Reads** (LANG-280, 129). A positional or named atom `r(k, x)` is a **generator**: it ranges over cells whose
value is not ⊥ and binds `x` to the cell's value. `r[k]` is a **lookup**: `k` must be bound elsewhere and the
result is ⊥ of the value type when the cell is absent (typed ⊥, LANG-129). A 0-ary cell's name used as an
expression is a lookup. `m.at(k)` on an `LMap` also returns typed ⊥.
```
lead(t) <- candidate_in(t), let vs = votes_by_term[t], vs.is_quorum_of(member);
```
```
lead(T) :- candidate_in(T), VS = votes_by_term[T], quorum(VS, member).
```

**Converting** (LANG-123). Collection → lattice: a head whose lattice column is `LSet::of(x)`, `LMap::of(k, l)`,
`LMax(x)`, … is the implicit fold (every body row contributes a singleton, and they merge). Lattice → collection:
a threshold guard (`b` for an `LBool`, `s.contains(x)`, `n >= c`), or iteration over a set-like lattice, which is a
morphism (Bloom^L `to_collection`):
```
member_row(x) <- let s = seen, for x in s.items();              // LSet → rows: morphism
entry(k, v) <- let m = kv_cell, for (k, v) in m.entries();      // LMap → rows (non-⊥ entries): morphism
```
```
member_row(X) :- S = seen[], $member($items(S), X).             // $items is classed M: a + edge
entry(K, V) :- M = kv_cell[], $member($entries(M), (K, V)).
```

**Operation classes** (LANG-125, R04 §2.4 is normative). Each method declares a class per argument. The bang rule
applies: Anti and NM methods must be called as `x.m!(..)`.

| Lattice | Monotone (no bang) | Needs `!` |
|---|---|---|
| `LBool` | guard `b` (threshold, `when_true`), `b.and(c)` (BM), `b.or(c)` | `b.not!()` (Anti) |
| `LMax<T>` | `l >= c`, `l > c` (thresholds), `l + c` (M), `l.min_of(c)` (M), `a + b` (BM) | `reveal!(l) <= c` |
| `LMin<T>` | `l <= c`, `l < c` (thresholds), `l + c` (M), `a + b` (BM, tropical) | `reveal!(l) >= c` |
| `LSet<T>`, `LPSet<T>` | `contains(x)`, `intersect(o)`, `map(f)`, `filter(f)`, `product(o)`, `items()`, `min_elem()`, `max_elem()` (M); `size()`, `sum()` (Mon); `is_quorum_of(r)` (Mon threshold) | `is_empty!()`, `difference!(o)` (Anti in `o`) |
| `LBag<T>` | `multiplicity(x)`, `contains(x)`, `intersect(o)`, `a + b` (M/BM); `size()` (Mon) | — |
| `LMap<K, L>` | `at(k)`, `has_key(k)`, `key_set()`, `map_values(f)`, `entries()`, `intersect(o)` (M); `size()`, `sum_values()` (Mon, LANG-282) | `lt_eq!(o)` |
| `Lex<K, L>` (LANG-131) | `key()` (M) | `val!()` |
| `LDom<V, L>` (LANG-132) | `version()` (M) | `value!()` (NM) |
| `VClock` = `LMap<Node, LMax<u64>>` (LANG-130) | `at(n)`, `>=` a constant clock | `leq!(o)`, `lt!(o)`, `concurrent!(o)` |
| `LPoint<T>` | `get()` (threshold → `Option<T>`) | — |
| every lattice | `a \| b` (join), `a \| C` with a constant (LANG-283's default) | `reveal!(a)`, `a == b` is rejected |

`reveal!(x)` is deep: nested lattices are revealed to plain values (an `LMax<T>` to `T`, an `LSet<T>` to
`OrdSet<T>`, an `LMap<K, L>` to `OrdMap<K, revealed L>` without ⊥ entries, an `LPoint<T>` to `T`).

A comparison between a lattice value and a plain value is accepted only in the direction that is a threshold in
the lattice's order (`>=`/`>` for `LMax`, `<=`/`<` for `LMin`, `>=` a constant for `VClock`); the other direction is
an error that suggests `reveal!`. Comparisons between two lattice values are methods (`a.leq!(b)`), because they
are antitone in one argument.

**Built-in lattices** (LANG-124, 130–134, 136). `LBool`, `LMax<T>`, `LMin<T>` (with an adjoined ⊥ of −∞/+∞,
LANG-281), `LSet<T>`, `LMap<K, L>` (⊥ values count as absent), `LBag<T>`, `LPSet<T>`, `LPair<A, B>`,
`LWithBot<L>`, `LWithTop<L>`, `LConflict<T>`, `LPoint<T>` (merging two different values is a hard `Conflict`
error), `LUnit`, `LVec<L>`, `LUnionFind<T>`, `Lex<K, L>` (a proper lexicographic pair with a chain key: when keys
are incomparable the merge is `(k ⊔ k′, ⊥)`), `LDom<V, L>` (antichain / MV-register), `LTombSet<T>`,
`LTombMap<K, L>` (with pluggable tombstone sets), `Causal<DotSet>`, `Causal<DotFun<T>>`, `Causal<DotMap<K, L>>`.
Standard aliases: `type VClock = LMap<Node, LMax<u64>>;`,
`lattice Ballot = Lex<LMax<u64>, LMax<Node>>;`, `lattice Lww<T> = Lex<LMax<(u64, Node)>, LPoint<T>>;`.
`DomPair` exists only as `unsafe DomPair<K, V>` in a type, which the compiler reports (LANG-136, CR-25).

**User-defined lattices** (LANG-135, ODD-09 (c)). Three forms.
1. *Composition* of verified constructors: `lattice Ballot = Lex<LMax<u64>, LMax<Node>>;`
2. *Product DSL*: a struct of lattices. The merge is fieldwise and ⊥ is the all-⊥ value; both are correct by
   construction, so only the methods need checking.
3. *Extern*: a Rust type implementing the `Merge` trait, declared with its laws status.
Methods live in an `impl` and declare their class with `#[morphism]`, `#[monotone]`, `#[antitone]` or
`#[threshold]` (a monotone map into `Option<T>` whose `Some` values are pairwise incompatible, i.e. LANG-126's
generic `threshold(t1..tn)`); a method with no class attribute is non-monotone and is called with `!`.
```
lattice Window { hwm: LMax<u64>, seen: LSet<u64> }

impl Window {
    #[monotone]
    fn count(self) -> LMax<u64> { self.seen.size() }

    #[threshold]
    fn complete_upto(self, n: u64) -> Option<()> {
        let s = reveal!(self.seen);
        if (0..n).all(|i| s.contains(i)) { Some(()) } else { None }
    }

    fn gaps(self, n: u64) -> Vec<u64> {                              // NM: callers write w.gaps!(n)
        let s = reveal!(self.seen);
        (0..n).filter(|i| !s.contains(i)).collect()
    }
}

#[rust("analytics::Hll"), laws(tested)]
extern lattice Hll;
```
```
decl lattice-type Window = product(hwm: LMax<u64>, seen: LSet<u64>)          // merge/⊥ fieldwise
decl fn Window::count : Window -> LMax<u64> [Mon]                            // law obligation TEST-083
decl fn Window::complete_upto : Window × u64 -> Option<()> [Threshold(self)]
decl fn Window::gaps : Window × u64 -> Vec<u64> [NM]
decl lattice-type Hll = extern("analytics::Hll") laws(tested)                // reported "tested, not proven"
```
Inside an `impl`, method bodies may `reveal!` freely: the polarity analysis uses the method's declared class, not
its body. Every class claim is a law obligation: the harness (TEST-083) tests it and the SMT backend (VER-014)
proves it where it can; a refuted claim is a compile error.

**Lattices inside messages** (LANG-137, CR-52). A channel column may be a lattice. The channel's key is its
non-lattice columns, so the sender merges all of a tick's values per (destination, key) and the receiver merges
same-key arrivals within one tick's batch. Accumulation across ticks needs a persistent sink.
```
channel gossip { @dst: Node, vc: VClock }
send gossip(clock) @ p <- sync(..), peer(p), let clock = my_vc;
my_vc(vc) <- gossip(vc);
```
```
gossip(@P; VC)@async :- sync(_, _), peer(P), VC = my_vc[].      // one message per (P) per tick, carrying the join
my_vc(; VC) :- gossip(; VC).
```

**Keyed monotone sum** (LANG-282): `m.sum_values()` on `LMap<K, LMax<u64>>` is monotone and supports thresholds.
Company control (Ross–Sagiv), with `held(a, b, m)` mapping each intermediary to the share of `b` that `a` controls
through it: `controls(a, b) <- held(a, b, m), m.sum_values() > 50;`.

**Monotone reset** (LANG-284): lattice state is "reset" by bumping an epoch in a `Lex`:
```
table counter { name: String, c: Lex<LMax<u64>, LMax<u64>> }
next counter(n, Lex::new(e, LMax(0))) <- reset(n), let e = counter[n].key() + 1;       // the epoch only grows
```
```
counter(N; lex(E, 0))@next :- reset(N), C = counter[N], E := key(C) + 1.   // new epoch discards the old component
```

### 3.7 Weighted collections and group types (LANG-138, 142, 158)

**Group and ring types** (LANG-142). Built-ins: `Z` (checked i64), `Zn<N>`, `ZSet<T>`, and tuples and maps of
these. A user group is an `impl Group for T` with `zero`, `add`, `neg` (a ring adds `one`, `mul`). A group type can
never be a lattice; `impl Group` on a `lattice` is an error.
```
struct Money { cents: i64 }
impl Group for Money {
    fn zero() -> Money { Money { cents: 0 } }
    fn add(a: Money, b: Money) -> Money { Money { cents: a.cents + b.cents } }
    fn neg(a: Money) -> Money { Money { cents: -a.cents } }
}
```
**Weighted collections** (LANG-138). `#[zset] table` has ℤ weights and `#[bag] table` has ℕ weights (insert-only,
proved by ANA-030). A head adds weight 1 by default; `weight w` gives another weight. Reads into a set or lattice
rule use one of the three declared views, each a bang form because the edge out of a Z-set stratum is negative.
```
#[zset] table stock { sku: String }
stock(s) <- received(s, _);
stock(s) weight -1 <- shipped(s, _);
in_stock(s) <- distinct!(stock(s));                 // weight > 0
qty(s, n) <- clamped!(stock(s), n);                 // max(w, 0)
raw(s, w) <- weights!(stock(s), w);                 // raw weight
```
```
decl zset stock(sku: String)                             // DBSP stratum; linear operators only
stock(S) += 1  :- received(S, _).
stock(S) += -1 :- shipped(S, _).
in_stock(S) :- $zweight(stock, (S), W), W > 0.          // negative edge from the Z-set stratum
qty(S, N) :- $zweight(stock, (S), W), N := max(W, 0), N > 0.
raw(S, W) :- $zweight(stock, (S), W).
```
Weights are checked i64; overflow is a hard error.

**Wrapped exactly-once channels** (LANG-158, CR-35). A group-typed payload may cross nodes only through a channel
declared `#[exactly_once(dots | cumulative | tree)]` (default `dots`, ODD-26). A plain channel with a group payload
is a compile error (ANA-015).
```
#[exactly_once(dots)]
channel stock_delta { @dst: Node, delta: ZSet<String> }
send stock_delta(d) @ r <- replica(r), let d = stock.delta();     // this tick's Z-set Δ, summed
stock(s) weight w <- stock_delta(d), for (s, w) in d.entries();
```
```
decl channel stock_delta(@dst: Node | delta: ZSet<String>) wrapped(W2)   // DIST-015 inserted by the compiler
stock_delta(@R; D)@async :- replica(R), D := $zdelta(stock).             // zero sum sends nothing
stock(S) += W :- $unwrap(stock_delta, D), $member($entries(D), (S, W)).  // ENG-070: wrapper Δ → Z-set Δ
```

### 3.8 Locations, channels, principals (LANG-150–158, 240–244)

**Location** (LANG-150–152). Local state lives at `self`. `self` is a `Node` (a `Node<R>` inside `role R`). The
built-in relation `member` of the root program lists all nodes (LANG-152's `members`); each role `R` is usable as
a unary relation of its members (`p in Participant`). Every body atom of a protocol rule is local (LANG-151): a
rule whose atoms name another location is rejected unless it carries `#[localize]` (§3.4).

**Sender and principal** (LANG-241, SEM-091). A channel atom may bind its authenticated sender with `from s` and
its principal with `principal p`. They are runtime columns, never payload, so they cannot be forged.
```
append(l, t) <- append_entries(t, ..) from l;
put_ok(s, k, v) <- client_put(k, v) from s principal p, owns_prefix(p, k);
authz_denied(s, k) <- client_put(k, _) from s principal p, !owns_prefix(p, k);
```
```
append(L, T) :- append_entries(T, _, _, _, _, L).
put_ok(S, K, V) :- client_put(K, V, S), P := $principal(client_put, S), owns_prefix(P, K).
authz_denied(S, K) :- client_put(K, _, S), P := $principal(client_put, S), notin owns_prefix(P, K).
```
For a peer, `principal` is `principal_of(sender)`; for a session it is the session's principal. The directory
(LANG-240) is the built-in `node_dir(node, addr, principal, role)`, with `principal_of(n)` and `role_of(n)`.
`Principal` values print as SPIFFE ids. ANA-106 warns when a `Node`-typed payload column is used as an identity
without being equated with the sender.

**ACLs** (LANG-242, ODD-33). By default a channel accepts a frame only from the roles that have a `send` rule
targeting it (inferred from the choreography). `#[accept(..)]` narrows that set, or opens an external channel.
An importer narrows an instance's ACL with `acl`.
```
#[accept(from = [Leader, Follower])]
channel append_entries { @dst: Node, term: u64, prev_idx: u64, prev_term: u64, entries: Vec<Entry>, commit: u64 }
#[accept(external, principal in kv_writers)]            // kv_writers: a unary static or table relation
channel client_put { @dst: Node, key: String, val: Bytes }
acl raft.client_req accept(external, principal in operators);
```
Lowering: the IR carries `acl(append_entries) = {Leader, Follower}`, `acl(client_put) = external ∧ principal ∈
kv_writers@last-committed-tick`. Enforcement is at ingress (DIST-062); a rejected frame is an omission (SEM-090),
so there are no rules to lower. ANA-105 errors if an explicit ACL excludes an inferred sender.

**External clients and sessions** (LANG-243). An `external` role is not a `Node`. Its channels carry a `Session`
as the sender; replies go to `@ s` on a channel whose `@` column is a `Session` (egress only). `session_open(s,
p, at)` and `session_closed(s, reason)` are built-in inputs.
```
external Client {}
#[accept(external)] channel get_req { @dst: Node, id: u64, key: String }
channel get_resp { @dst: Session, id: u64, val: Option<Bytes> }
send get_resp(i, v) @ s <- get_req(i, k) from s, opt!(store(k, v));
```
```
get_resp(@S, I, Some(V)) :- get_req(I, K, S), store(K, V).
get_resp(@S, I, None)    :- get_req(I, K, S), notin store(K, _).
```

**Authorization idiom** (LANG-244) is ordinary rules: `authorized(p, op, obj)` policies, `authz_denied(..)`
outputs, and a library rule that turns denials into error replies. The runtime counts `authz_denied` rows.

**Channel fault models** (LANG-155): `#[fault(reliable_ordered)]` (a reliable ordered prefix), `#[fault(lossy_delayed)]`
(loss modeled as infinite delay; the default), `#[fault(lossy)]`, `#[fault(reliable)]` (reliable, unordered).
The attribute sets the receiving relation's stream properties (ANA-030) and drives the simulator; it adds no rules.

**Partitioning** (LANG-154). `#[partition(by = key, hash)]` or `#[partition(by = key, range)]` on a relation
declares its placement; `R::route(k)` gives the member of role `R` that owns `k` (rendezvous hashing over the
sorted member list, so every node computes the same owner), and `rel.owner(k)` the owner under the relation's
declared partitioning.
```
send put(k, v) @ Shard::route(k) <- client_put(k, v) from _;
```
```
put(@N, K, V)@async :- client_put(K, V, _), N := $route(Shard, K).
```

**Cluster roles** (LANG-153). Inside `cluster C { … }`, `self: Node<C>`; `C` is the member relation;
`C::route(k)` sends to a member; broadcast is a `send` joined with `m in C`; received tuples are keyed by sender
through `from`; `C::membership(n, epoch)` is the membership stream (dynamic membership, DIST-042).

### 3.9 Time, timers and randomness (LANG-170–175)

| Surface | Meaning | IR |
|---|---|---|
| `tick()` | local tick counter; marks the rule time-dependent | `$tick` |
| `now()` | wall clock, one value per tick, recorded for replay | `$now` |
| `random()` | `rand(())` | `$rand(())` |
| `rand(k…)` | PRF of (node seed, incarnation, tick, fingerprint of k) | `$rand(k̄)` |
| `rand_float(k…)`, `rand_range(lo, hi, k…)` | helpers; `rand_range` is unbiased | `$rand_float`, `$rand_range` |
| `timer t every d [times n];`, `every n ticks`, `once` | timer inputs (§3.2) | runtime-fed `input` |
| `boot()` | true in the first tick of each incarnation | `$boot` |

A value that must stay fixed across ticks is captured into state with `next`/`upsert` (the R12 idiom), for example
an election deadline:
```
upsert deadline(now() + d) <- reset_timer(), eff_term(t), let d = rand_range(150ms, 300ms, ("election", t));
```
```
deadline$ups(N) :- reset_timer(), eff_term(T), D := $rand_range(150ms, 300ms, ("election", T)), N := $now + D.
deadline$del(X) :- deadline$ups(_), deadline(X).
deadline(N)@next :- deadline$ups(N).
```
ANA-011 lints `rand`/`random`/`choose_rand!` over persistent inputs whose results are not captured.

### 3.10 Functions, host values and services (LANG-180–186, 026–028)

**Pure functions** (LANG-181, 182). `fn` bodies use a total, pure expression language: `let`, `if`, `match`,
arithmetic, calls, closures passed to the built-in collection combinators (`map`, `filter`, `filter_map`, `fold`,
`all`, `any`, `enumerate`, `collect`, …), and the built-in library of LANG-180 (strings, math, hashing, list, set
and map operations, `to_string`, ids). No recursion (every `fn` terminates), no I/O, no `now()`/`rand()`. Values
are immutable: `m.insert(k, v)` returns a new map. `error("msg")` aborts the tick with a hard, located runtime
error (it is how a function refuses an impossible input; it is never a silent default). Properties are attributes
checked by TEST-087 and used by the analyses:
```
#[injective] fn slot_key(term: u64, idx: u64) -> (u64, u64) { (term, idx) }
#[monotone] fn grow(x: u64) -> u64 { x * 2 }
```
Lowering: a `fn` becomes an IR pure function with its declared properties; calls stay calls (`$f(args)`).

**Extern functions and types** (LANG-027, 181, 183). Rust code enters only through declared, pure signatures:
```
#[rust("blossom_std::hash::sha256"), pure] extern fn sha256(b: Bytes) -> Bytes;
#[rust("regex::Regex"), ord, hash, serialize] extern type Regex;
#[rust("fsutil::split_lines")] extern table fn split_lines(s: String) -> (n: u64, line: String);
```
An `extern type` is opaque: its values may be stored and compared, and its methods are reachable only through
declared `extern fn`s. A table function is a generator relation whose inputs must be bound (§3.4).

**Wide modular ids and ring intervals** (LANG-026): `Mod<160>`, literals `0x…I`, modular `+`, `-`, `<<`, and
`x in (a, b]` (§3.4). **Blobs** (LANG-028): a `Blob` column holds a handle; bytes move through handlers.

**Async services** (LANG-184). A service is an external call whose result arrives in a later tick.
```
service fetch(id: u64, url: String) -> (status: u16, body: Bytes);
send fetch(i, u) <- want(i, u);
got(i, s, b) <- fetch.done(i, u, s, b);
```
```
decl channel fetch(@$host, id: u64, url: String)          // a channel to the host service endpoint
decl input fetch$done(id: u64, url: String | status: u16, body: Bytes)
fetch(@$host, I, U)@async :- want(I, U).
got(I, S, B) :- fetch$done(I, U, S, B).
```

**Host API and output handlers** (LANG-185, 186). The host injects into `input`s (deferred) and subscribes to an
`output` either by full contents or by deltas. An output with `#[handler(rust = "path")]` calls a host handler for
each emitted row (the BOOM-FS data path); the handler runs after the tick commits, so it cannot affect the tick.

### 3.11 Modules, protocols and composition (LANG-003–010, 205, 206)

**Modules and visibility** (LANG-003). A `module` is an instantiable component. Inside it, only `pub input` and
`pub output` relations (and `pub` types, constants and functions) are visible to importers; every other relation is
private. `pub` on a `table`, `scratch` or `channel` is an error: interfaces are the only connection points.
```
module Counter {
    pub input incr { key: String, id: u64 }
    pub output total { #[key] key: String, n: u64 }
    table counts { key: String, ids: LSet<u64> }
    counts(k, LSet::of(i)) <- incr(k, i);                   // counting ids makes a retried incr harmless
    total(k, n) <- counts(k, ids), let n = reveal!(ids.size());
}
```
Lowering happens per instance (below); the module itself has no IR.

**Import creates an instance** (LANG-004). `import M<Args> as a;` instantiates `M` with its generic and const
arguments substituted (monomorphization) and every relation renamed into `a::`. The importer reaches `a.total`
and `a.incr`; nested instances are `a.b.rel`. Two instances are independent. Reusing an alias is an error.
```
import Counter as page_views;
import Counter as clicks;
page_views.incr(u, v) <- visit(u, v);
report(u, n) <- page_views.total(u, n);
```
```
page_views::counts(K; X)@next :- page_views::counts(K; X).
page_views::counts(K; {I}) :- page_views::incr(K, I).
page_views::total(K, N) :- page_views::counts(K; IDS), N := $reveal(size(IDS)).
clicks::counts(K; X)@next :- clicks::counts(K; X).           // a second, independent copy
clicks::counts(K; {I}) :- clicks::incr(K, I).
clicks::total(K, N) :- clicks::counts(K; IDS), N := $reveal(size(IDS)).
page_views::incr(U, V) :- visit(U, V).
report(U, N) :- page_views::total(U, N).
```

**`use`** brings names into scope without creating anything: `use std::delivery::{Broadcast, ReliableBroadcast};`.

**Include** (LANG-005). `include M;` copies `M`'s items and rules into the current module flat, in one namespace
(Bloom's mixin): `M`'s `pub input`s become this module's own interfaces. `include "legacy.ded";` includes a file
textually, resolved relative to the including file (a `.ded` file goes through the Molly frontend).

**Protocols** (LANG-006). A protocol lists interfaces only. A module implements it with the supertrait-style bound
`module M: Proto`; the protocol's interfaces become the module's `pub` interfaces (the module does not redeclare
them). A module generic over a protocol picks the implementation at composition time.
```
protocol Delivery<P> {
    input pipe_in { dst: Node, src: Node, ident: u64, payload: P }
    output pipe_sent { dst: Node, src: Node, ident: u64, payload: P }
    output pipe_out { dst: Node, src: Node, ident: u64, payload: P }
}
module BestEffort<P>: Delivery<P> {
    channel chan { @dst: Node, src: Node, ident: u64, payload: P }
    send chan(s, i, p) @ d <- pipe_in(d, s, i, p);
    pipe_out(self, s, i, p) <- chan(s, i, p);
    pipe_sent <- pipe_in;
}
module Multicast<D: Delivery<String>> {
    import D as d;
    pub input mcast_send { ident: u64, payload: String }
    pub input members { n: Node }
    pub output mcast_done { ident: u64 }
    table outstanding { ident: u64 }
    table unacked { ident: u64, dst: Node }
    d.pipe_in(n, self, i, p) <- mcast_send(i, p), members(n), n != self;
    outstanding(i) <- mcast_send(i, _);
    unacked(i, n) <- mcast_send(i, _), members(n), n != self;
    delete unacked(i, n) <- d.pipe_sent(n, _, i, _), unacked(i, n);
    mcast_done(i) <- outstanding(i), !unacked(i, _);
    delete outstanding(i) <- mcast_done(i);
}
import Multicast<D = BestEffort<String>> as mc;
```
Lowering: `Multicast<D = BestEffort<String>>` is instantiated with `d` bound to a `BestEffort<String>` instance;
the IR is the renamed union of both. A protocol with no implementation bound is a compile error at the import.

**Named rule blocks and override** (LANG-007). `rules name { … }` groups rules under a name. In a module that
`include`s another, `override rules name { … }` replaces the included block of that name. Two blocks with one name
in a module, or an `override` with nothing to override, are errors.
```
module Voter { pub input ballot { id: u64 } pub output vote { id: u64, yes: bool }
    rules decide { vote(i, true) <- ballot(i); } }
module Grumpy { include Voter;
    override rules decide { vote(i, false) <- ballot(i); } }
```
```
// Grumpy's IR: the included `decide` block is dropped and replaced
vote(I, false) :- ballot(I).
```

**Interposition** (LANG-008). A module imports a component and routes the component's interfaces through its own
rules; nothing special is needed beyond instance-qualified names. Example E9 interposes on a broadcast interface.
```
module Guarded<D: Delivery<Bytes>, const MAX: u64 = 65536>: Delivery<Bytes> {
    import D as inner;
    pub output rejected { dst: Node, ident: u64 }
    inner.pipe_in(d, s, i, p) <- pipe_in(d, s, i, p), p.len() <= MAX;     // interposed input
    rejected(d, i) <- pipe_in(d, _, i, p), p.len() > MAX;
    pipe_out <- inner.pipe_out;                                          // pass-through outputs
    pipe_sent <- inner.pipe_sent;
}
```

**Constants and parameters** (LANG-010). `const` is fixed at compile time; `param` is a deploy-time value from the
config or the command line (`blossom run --param REPLICAS=5`), with an optional default. Module const generics
(`module ReliableBroadcast<P, const RETRY: Duration = 1s>`) are per-instance constants.
```
const QUORUM: u64 = 3;
param REPLICAS: u32 = 3;
```
Lowering: constants are substituted; params become IR constants bound at deployment and recorded in the trace
header (TEST-010).

**Choreographic modules** (LANG-009, 153, 243). A module may contain several location blocks: `role R { … }` (one
process), `cluster C { … }` (SPMD members) and `external X { … }` (clients, not `Node`s). Channels are declared at
module level and their `@` column is typed by the receiving role (`@dst: Node<Participant>`). Relations declared
inside a block live on that role; rules inside a block run there. The compiler projects one program per role.
```
module Ping {
    channel ping { @dst: Node<Ponger> }
    channel pong { @dst: Node<Pinger> }
    role Pinger {
        timer t every 1s;
        pub output answered {}
        send ping() @ p <- t(..), p in Ponger;
        answered() <- pong() from _;
    }
    role Ponger {
        send pong() @ s <- ping() from s;
    }
}
```
A rule runs on the role of the block it is written in. A channel atom may be read only in the block of the role its
`@` column is typed with, which is how the compiler checks body locality per role.
```
// projection for Pinger
decl input t(id: u64 | at: Instant) timer(physical, 1s)
decl output answered()
ping(@P)@async :- t(_, _), Ponger$members(P).
answered() :- pong(_).
// projection for Ponger
pong(@S)@async :- ping(S).
// ACL inferred: ping accepts only role Pinger; pong accepts only role Ponger (LANG-242)
```
In the single Dedalus program that the verifiers see, each rule also gets a `$role(R)` guard (the heterogeneous-role
encoding of R02 §4.2), and `Node<R>` becomes a sort.

**Trusted modules, atomic regions, accepted nondeterminism** (LANG-205, 206, 204).
- `#[trusted("hand-verified 2PC coordinator; see proofs/2pc.md")] module …` — CALM analysis does not re-flag the
  module's internals; VER-020 checks its interface spec instead.
- `#[atomic] rules name { … }` — outputs derived by these rules are released only after the tick's state updates
  are visible to snapshot reads (read-after-write within the node). Lowering: the IR marks the block's heads as
  "release after commit"; no rules change.
- `#[nondet("reason")]` on a rule, relation or interface records accepted nondeterminism with its mandatory reason;
  it propagates through interfaces and appears in the determinism certificate (ANA-029).

### 3.12 Deltas, seals, finality, snapshots, catalog (LANG-071, 139, 202, 207, 212)

**Delta pseudo-relations** (LANG-071). `inserted!(r(..))` holds for rows present now and absent in the previous
tick; `deleted!(r(..))` for rows present in the previous tick and absent now. They work on any relation (the
compiler keeps a one-tick shadow). They compare against the past, so they are banged.
```
came_up(p) <- inserted!(alive(p));
went_down(p) <- deleted!(alive(p));
```
```
alive$prev(P)@next :- alive(P).
came_up(P) :- alive(P), notin alive$prev(P).
went_down(P) :- alive$prev(P), notin alive(P).
```

**Seals and punctuations** (LANG-207, CR-27). A channel that can be sealed declares its producers and, optionally,
its seal key. A `seal` rule sends a punctuation: "I will send you no more tuples of this channel (with this key
value), and I sent you `n` of them." The receiver reads `c.sealed(..)`, which holds once **every** producer has
sealed that key and, for each producer, the number of distinct tuples received equals its digest. Sealed-ness never
reverts, so `c.sealed(..)` is a threshold and carries no bang; reads guarded by it are certified SEALED-final
(ANA-120) and let Edelweiss reclaim the receive log (ANA-063).
```
#[seal(key = (part), producers = Mapper)]
channel shuffle { @dst: Node<Reducer>, part: u32, word: String, pos: u64 }
seal shuffle(part: p) @ r digest n <- finished(p), r in Reducer, sent_count(p, r, n);     // on a Mapper
final_count(w, count!(pos)) <- shuffle.sealed(part: p), got(p, w, pos);                   // on a Reducer
```
```
// producer side
shuffle$seal(@R, P, N)@async :- finished(P), Reducer$members(R), sent_count(P, R, N).
// receiver side (generated once per sealable channel)
shuffle$log(M, P, W, POS)@next :- shuffle$log(M, P, W, POS).
shuffle$log(M, P, W, POS) :- shuffle(P, W, POS, M).                 // M = sender (a Mapper)
shuffle$sl(M, P, N)@next :- shuffle$sl(M, P, N).
shuffle$sl(M, P, N) :- shuffle$seal(P, N, M).
shuffle$n(M, P, count<(W, POS)>) :- shuffle$log(M, P, W, POS).
shuffle$pd(M, P) :- shuffle$sl(M, P, N), shuffle$n(M, P, N).
shuffle$pd(M, P) :- shuffle$sl(M, P, 0), notin shuffle$n(M, P, _).
shuffle$open(P) :- shuffle$sl(_, P, _), Mapper$members(M), notin shuffle$pd(M, P).
shuffle$sealed(P) :- shuffle$sl(_, P, _), notin shuffle$open(P).
violation("seal_digest_conflict", (M, P)) :- shuffle$sl(M, P, N1), shuffle$sl(M, P, N2), N1 != N2.
violation("seal_overflow", (M, P)) :- shuffle$sl(M, P, N), shuffle$n(M, P, K), K > N.
// user rule
final_count(W, count<POS>) :- shuffle$sealed(P), got(P, W, POS).
```
`#[seal(producers = R)]` without a key seals the whole stream per producer (`seal c() @ d digest n`,
`c.sealed()`). A `seal` rule on a local `table` declared `#[seal(key = (k))]` promises no more local inserts for that
key: `seal orders(day: d) <- day_closed(d);` lowers to a persistent `orders$sealed(D)` plus
`violation("insert_after_seal", …)` for any later insert with that key. The two violations make the seal's
guarantee checked, not assumed.

**Final outputs** (LANG-212). `#[final] pub output r { … }` is accepted only if ANA-120 classifies `r` as POS-,
NEG-, TOP-, THRESH-, FINITE- or SEALED-final; at runtime every emitted row carries `final_present` (or the output
reports `final_absent`). `is_final(r(x))` and `when_final(e)` are monotone built-ins (thresholds).
```
#[final] pub output hot { word: String }
hot(w) <- counts(w, ids), ids.size() >= 1000;              // POS-final: emitted early, never retracted
```
Lowering: no extra rules; the IR marks `hot` final with the proof class, and the runtime gates emission with the
ANA-121/122 tests.

**Progressive snapshots** (LANG-139, 113). A `snapshot` item publishes a lattice relation's value each time
producer progress crosses a point. It is implicitly `#[nondet("progressive snapshot")]`.
```
snapshot wc_snap of word_count at progress every 0.1 upto 0.9 mode committed_only estimate scale_by coverage(hour);
```
```
wc_snap$prog(mean_over<N_MAPS>(P)) :- m_progress(M, P).                     // unheard producers count as 0
wc_snap$point(PT) :- $points(0.1, 0.9, PT), wc_snap$prog(P), P >= PT.         // threshold
wc_snap(PT, P, CLASS, K, EST) :- wc_snap$point(PT), notin wc_snap$taken(PT), wc_snap$prog(P),
                                 word_count(K; C), EST := $scale_coverage($reveal(C), K),
                                 CLASS := $class(word_count).                 // nondet "progressive"
wc_snap$taken(PT)@next :- wc_snap$point(PT).
wc_snap$taken(PT)@next :- wc_snap$taken(PT).
```

**Catalog relations** (LANG-202). `use std::catalog;` gives read-only relations over the compiled program:
`catalog.rule(id, label, kind, head, span)`, `catalog.depends(rule, head, body, negative, in_body)`,
`catalog.stratum(rel, n)`, `catalog.schema(rel, col, ty, is_key)`, `catalog.interface(rel, dir)`. They are
`static` relations of the running node; the analyses themselves are written against the same relations.

### 3.13 Program versions and schema evolution (LANG-260–265)

**Field numbers, defaults, variants** (LANG-261, 265). Field numbers are auto-assigned and recorded in
`schema.lock`; writing them is optional. `#[tag(n)]` pins a number, `#[since(v)]` marks a field or variant added
in version `v`, `= v` gives the default a reader uses when the field is absent, `#[reserved(n…)]` on a relation or
type retires numbers, `#[unknown]` marks the fallback variant every stored or sent enum must have,
`#[deprecated(since = v)]` warns at every use, and `#[semantics_changed(since = v)]` forces a new field number.
```
program raft_kv version 7;
#[durable] table kv { #[key] #[tag(1)] key: String, #[tag(2)] val: Bytes, #[tag(3), since(7)] ver: u64 = 0 }
enum Entry { #[tag(1)] Put(PutCmd), #[tag(2)] Del(DelCmd), #[tag(3)] Noop, #[tag(4), since(7)] Cas(CasCmd), #[unknown] Unknown }
channel request_vote { @dst: Node, term: u64, last_idx: u64, last_term: u64, #[since(7)] prevote: bool = false }
```
Lowering: the IR relation keeps positional columns; the codec and WAL use the tags; the lock records every
version's schema (LANG-260). No rules are generated.

**Migrations** (LANG-262). `migrate from N { … }` rules read `old::r` (typed by version N's lock entry) and write
current durable relations. Temporal kinds, `send`, `now()` and `rand()` are rejected inside. Auto-migrations
(defaults, projections, widening, renames) need no block.
```
migrate from 6 {
    kv(k, v, 0) <- old::kv(k, v);
    tomb(k, t) <- old::deleted(k, t), !old::kv(k, _);
}
```
```
// evaluated once, at recovery, as a single stratified tick over the decoded v6 checkpoint (DIST-082)
kv(K, V, 0) :- old::kv(K, V).                          // tuple-local (ANA-103)
tomb(K, T) :- old::deleted(K, T), notin old::kv(K, _).  // non-monotone: runs at finalization
```

**Channel translation** (LANG-263). `emit c to N { … }` rewrites tuples for a receiver on version N;
`accept c from N { … }` for frames from a sender on N. Each rule has exactly one channel atom (tuple-local). A tuple
that matches no `emit` rule is disallowed and dropped as an omission.
```
emit request_vote to 6 {
    old::request_vote(t, li, lt) <- request_vote(t, li, lt, false);
}
accept request_vote from 6 {
    request_vote(t, li, lt, false) <- old::request_vote(t, li, lt);
}
```
Lowering: codec-layer functions, one per rule (`$emit_request_vote_6`), never program rules.

**Version gates** (LANG-264). `cluster_version()` is an `LMax<u32>` input sampled once per tick; programs read it
only by threshold: `cluster_version() >= 7`. ANA-102 requires every write of a `#[since(v)]` relation, field or
variant, and every send of a `#[since(v)]` channel or non-default field, to be dominated by such a gate, unless the
rule carries `#[unsafe_ungated("reason")]`.
```
send request_vote(t, li, lt, true) @ p <- prevote_round(t, li, lt), peer(p), cluster_version() >= 7;
```
```
request_vote(@P, T, LI, LT, true)@async :- prevote_round(T, LI, LT), peer(P), CV = $cluster_version[], CV >= 7.
```

### 3.14 Specs, invariants and verification (LANG-070, 200, 201, TEST-020–030, VER-001)

A `spec` item is a separate program over a run of the target program. Its rules may join across locations, read
oracles (`crash`, `hb`, trace relations, the network) and use absolute time. Spec relations never feed protocol
relations (ANA-010), and protocol rules may not read oracles (CR-20).

```
spec name for target_program {
    nodes ["a", "b", "c"];                                   // the node set of the scenario
    failures { eot: 6, eff: 4, crashes: 1 }                  // LDFI failure spec (TEST-020; CR-21)
    bounds { ticks: 12, delay: 3, in_flight: 20 }            // bounded model checking (VER-002/003/005)
    neighbor("b") @ "a";                                     // scenario facts: located, optionally timed
    bcast("hello") @ "a" at tick 1;
    helper(x) <- …;                                          // spec rules
    pre(…) <- …;  post(…) <- …;                              // outcome oracle (TEST-022)
    invariant name(params) <- body;                          // safety: any row is a violation
    #[inductive] invariant name(params) <- body;             // also attempted as an SMT inductive invariant
    eventually post within 3 after eff;                      // bounded liveness
}
```
(`…` in this template stands for user code; §4 has complete specs.)

**Reuse.** A `spec` without `for` holds reusable members (assertions, scenario facts); `include name;` copies
them into another spec. A scenario fact without `@` holds at every node listed in `nodes`.

**Atoms in a spec.** A spec atom names its location: `log(p) @ x`. Its time is:
- in `pre`/`post` and their helpers, the end of the run (EOT, or quiescence in simulation);
- in an `invariant`, every global state the checker visits;
- with `at tick k` (LANG-070), tick `k` of that node; `at tick _` or `at tick t` ranges over the whole trace
  (TEST-080's `R_log`).

**Oracles.** `crash(n, t)` (node `n` crashed at tick `t`; CR-20), `hb(n1, t1, n2, t2)` (happens-before),
`net m(args) @ d from s` (message `m` from `s` to `d` was sent; the network as a grow-only set, VER-006), and
`quorum v in r { conj }` (some majority of `r` satisfies `conj`; the verifier's quorum sort, VER-008).

**Lowering.** Every located atom becomes a read of the global trace or state:
```
missing_log(a, p) <- log(p) @ x, neighbor(a) @ x, !log(p) @ a;
pre(x, p) <- log(p) @ x, !bcast(p) @ x at tick 1, !crash(x, _);
invariant two_leaders(t, a, b) <- leader_in(t) @ a at tick _, leader_in(t) @ b at tick _, a < b;
```
```
missing_log(A, P) :- log$eot(X, P), neighbor$eot(X, A), notin log$eot(A, P).
pre(X, P) :- log$eot(X, P), notin bcast$log(X, P, 1), notin crash(X, _).
violation("two_leaders", (T, A, B)) :- leader_in$log(A, T, _), leader_in$log(B, T, _), A < B.
// r$log(Node, X̄, Tick) is TEST-080's trace relation; r$eot(Node, X̄) is r at the node's last tick
```
The same spec drives every tool: the simulator checks invariants at each step and `pre`/`post` at the end; LDFI
(TEST-020–030) computes lineage of the `post` rows of the failure-free run and searches for falsifiers within the
`failures` budget; bounded model checking (VER-002/003) explores within `bounds`; `#[inductive]` invariants go to
the first-order translation (VER-006–010). A `spec` without `pre` or `post` is valid for invariants only; LDFI on
it is a hard error (CR-30).

**Runtime invariants** inside the program itself use `deny` (§3.3); they are checked on every node every tick.

---

## Appendix A (inline): LANG coverage

Every P0/P1 feature of FEATURES.md §2, and where it is expressed. P2 items are listed when the syntax already has
a natural place for them.

| LANG | Construct | § |
|---|---|---|
| 001 | unordered items and rules | 3.1 |
| 002 | own lexer/parser/checker; no closures in rule bodies | 1.1, 2 |
| 003 | `module` with `pub input`/`pub output` | 3.11 |
| 004 | `import M as a`; `a.b.rel` | 3.11 |
| 005 | `include M;`, `include "file";` | 3.11 |
| 006 | `protocol P { … }`, `module M: P`, `module X<D: P>` | 3.11 |
| 007 | `rules name { }`, `override rules name { }` | 3.11 |
| 008 | interposition through instance-qualified rules | 3.11, E9 |
| 009 | `role`/`cluster`/`external` blocks, `Node<R>`, projection | 3.11, E4 |
| 010 | `const`, `param`, const generics | 3.11 |
| 011 (P2) | several `program` roots in one runtime | — |
| 020 | struct-bodied relations, `#[key]`, `#[key()]`, `like`, `: Struct` | 3.2 |
| 021 | inference inside rules; errors list all evidence spans | 2.6, 5 |
| 022 | scalar types | 2.3 |
| 023 | tuples, `Vec`, `OrdSet`, `OrdMap`, `struct`, `enum` | 2.3 |
| 024 | canonical total order on every type (`<` everywhere) | 2.3 |
| 025 | `Option<T>`; no nil padding | 2.3, 3.4 |
| 026 | `Mod<N>`, `0x…I`, `x in (a, b]` | 2.2, 3.4 |
| 027 | `extern type` | 3.10 |
| 028 | `Blob` | 3.10 |
| 040–045 | `table`, `scratch`, `channel`, `input`/`output`, `#[durable] table`, `static` | 3.2 |
| 046 | `loopback`, `send localtick()` | 3.2 |
| 047 | `temp` head | 3.2 |
| 048 | `#[soft(ttl, max)] table` | 3.2 |
| 049 | `#[sealed] table` | 3.2 |
| 050 | `#[range(col)] table` | 3.2 |
| 051 | `stdin`, `stdout`, `#[file_reader]`, `#[readonly]` | 3.2 |
| 052 | `halt(kill)` | 3.2 |
| 053 | `#[materialize]`, `#[recompute]` | 3.2 |
| 054 (P2) | `#[provider(..)] table` | — |
| 060–064 | none / `next` / `send` / `delete` / `upsert` | 3.3 |
| 065 | explicit persistence: `next p(x) <- p(x), !gone(x);` is accepted as written | 3.3 |
| 066 | legality matrix | 3.3 |
| 067 | host writes only future ticks | 3.3 |
| 068 | `'label:` | 3.3 |
| 069 | `fact at tick k;` | 3.1 |
| 070 | `r(..) @ n at tick k` in specs | 3.14 |
| 071 | `inserted!(r(..))`, `deleted!(r(..))` | 3.12 |
| 072 (P2) | `at tick t` binding in protocol rules, gated by `#[entangled]` | 5 |
| 080–081 | positional/named atoms, punning, `..`, constants | 3.4 |
| 082–083 | `!atom`, `!(conj)` | 3.4 |
| 084 | Rust precedence + `**`, `++` | 2.5 |
| 085 | `let pat = e` | 3.4 |
| 086 | conjunction, shared variables, guards | 3.4 |
| 087 | `opt!(atom)` | 3.4 |
| 088 | `for pat in e`, destructuring | 3.4 |
| 089 | `either { } or { }`, `if`/`match` values | 3.4 |
| 090 | `x in rel`, `x in role`, `x in coll` | 3.4 |
| 091 | `r[k]`, `r[lo..hi]` | 3.4 |
| 092 | `for x in lo..hi`, extern table fns, binding patterns | 3.4 |
| 093 | `sort!`, `index!`, `topk!`, `limit!`, `percentile!` with canonical tie-breaks | 3.5 |
| 094 | `.keys`, `.values`, `.payloads`, `.schema()` | 3.4 |
| 095 | `#[localize(chain \| link)]` | 3.4 |
| 097 | `index!(by k per g)` | 3.5 |
| 098 | `seq!()`, `seq!(durable)`, `seq!(release)` | 3.5 |
| 099 (P2) | reserved: `choose_least!` inside a recursive stratum | — |
| 100–101 | `count!`, `sum!`, `min!`, `max!`, `avg!` | 3.5 |
| 102 | `collect!`, `collect_set!`, `collect_map!` | 3.5 |
| 103 | `argmin!`, `argmax!`, `bool_and!`, `bool_or!` | 3.5 |
| 104 | `percentile!`, `topk!`, `limit!` | 3.5 |
| 105 | `aggregate` items | 3.5 |
| 106 | `per` driver + identities / `default` | 3.5 |
| 107 (P2) | `send m(..) @ min!(n)` parses already | — |
| 108 | `choose!(y per x)`, `choose!` head aggregate, `choose_rand!` | 3.5 |
| 109 | `reduce!` | 3.5 |
| 110 | `fold_ordered!`, carried form with `per` | 3.5 |
| 111 | `s.is_quorum_of(r)`; `quorum v in r { }` in specs | 3.6, 3.14 |
| 112 | `combine` in `aggregate`; derived partials | 3.5 |
| 113 | `ola_sum!`, `ola_count!`, `ola_avg!`, `scale_by` | 3.5, 3.12 |
| 114–116 | `choose_least!`, `choose_most!`, `sticky`, several goals | 3.5 |
| 117 | `#[resolve(..)]`, `upsert … resolve …` | 3.3 |
| 118 | canonical order everywhere | 3.5 |
| 120–124 | lattice types, lattice columns, merges, conversions, built-ins | 3.6 |
| 125 | operation classes; the bang rule | 3.6 |
| 126 | thresholds, `#[threshold]` methods | 3.6 |
| 127 | `reveal!` | 3.6 |
| 128 | persistent vs `scratch` lattices | 3.6 |
| 129 | typed ⊥ from `r[k]`, `m.at(k)` | 3.6 |
| 130–134 | `VClock`, `Lex`/`Ballot`/`Lww`, `LDom`, tombstone and causal lattices | 3.6 |
| 135 | `lattice` DSL, alias, `extern lattice`, `impl` with classes | 3.6 |
| 136 | `unsafe DomPair<..>` | 3.6 |
| 137 | lattice columns in channels, merge at sender | 3.6 |
| 138 | `#[zset]`, `#[bag]`, `weight`, `distinct!`/`clamped!`/`weights!` | 3.7 |
| 139 | `snapshot` item | 3.12 |
| 142 | `impl Group`/`Ring`, `Z`, `Zn`, `ZSet` | 3.7 |
| 150–152 | `@` column, body locality, `self`, `member`, role relations | 3.8 |
| 153 | `cluster C`, `C::route`, `C::membership` | 3.8 |
| 154 | `#[partition(..)]`, `R::route(k)`, `rel.owner(k)` | 3.8 |
| 155 | `#[fault(..)]` | 3.8 |
| 158 | `#[exactly_once(..)]` | 3.7 |
| 170–175 | `tick()`, `now()`, timers, `random()`, `rand(k)` | 3.9 |
| 180–186 | built-ins, `fn`, properties, extern/table fns, `service`, host API, `#[handler]` | 3.10 |
| 190 | facts, `bootstrap { }` | 3.1 |
| 200 | `deny` with `#[on_violation]` | 3.3 |
| 201 | `spec` rules | 3.14 |
| 202 | `std::catalog` | 3.12 |
| 204 | `#[nondet("reason")]` | 3.11 |
| 205 | `#[trusted("reason")]` | 3.11 |
| 206 | `#[atomic] rules` | 3.11 |
| 207 | `#[seal(..)]`, `seal` rules, `c.sealed(..)` | 3.12 |
| 208 | `//`, `/* */`, `#` | 2.2 |
| 212 | `#[final]`, `is_final`, `when_final` | 3.12 |
| 220 | `.ded` files through the Molly frontend | 2.1 |
| 240–244 | `Principal`, `from`, `principal`, `#[accept]`, `acl`, `external`, sessions | 3.8 |
| 245 (P2) | `Signed<T>`, `sign`, `verify` | — |
| 260–265 | `program … version`, `#[tag]`, `#[since]`, defaults, `#[unknown]`, `migrate`, `emit`/`accept`, `cluster_version() >= v`, `#[deprecated]`, `#[semantics_changed]` | 3.13 |
| 280–284 | generators vs lookups, adjoined ⊥, `sum_values`, `\| C` defaults, `Lex` reset | 3.6 |

---

## 4. Example corpus

Every example is complete: it type-checks against the rules of §2–§3 as written, with no elided parts.

### E1. Key-value store node (put/get/delete, acks, durable table, upsert)

```
// kvs.bls
program kvs version 1;

type Key = String;
type Val = Bytes;
type ReqId = u64;

/// Clients are external: they connect through the client listener and are identified by their Session.
external Client {}

// ---- requests: external ingress; the sender is the client's Session, bound with `from` ----
#[accept(external)] channel put_req { @dst: Node, id: ReqId, key: Key, val: Val }
#[accept(external)] channel get_req { @dst: Node, id: ReqId, key: Key }
#[accept(external)] channel del_req { @dst: Node, id: ReqId, key: Key }

// ---- replies: egress only, addressed to the session ----
channel put_ack  { @dst: Session, id: ReqId }
channel get_resp { @dst: Session, id: ReqId, key: Key, val: Option<Val> }
channel del_ack  { @dst: Session, id: ReqId, existed: bool }

/// The store. Durable: the changes staged in a tick are fsynced before that tick's acks leave (SEM-072).
#[durable] table store { #[key] key: Key, val: Val }

/// This tick's puts, with the session that sent each.
scratch put_in { s: Session, id: ReqId, key: Key, val: Val }
/// The one put per key that survives this tick. The key makes "one per key" a checked fact.
scratch put_win { #[key] key: Key, val: Val }

'recv_put: put_in(s, i, k, v) <- put_req(i, k, v) from s;

/// Several clients may put the same key in one tick. Serialize them deterministically by (session, id): the
/// greatest one is ordered last and is the value that survives. Every put is acknowledged; ordering the others
/// first within the tick keeps the history linearizable.
'pick: put_win(k, v) <- put_in(s, i, k, v), choose_most!((s, i) per k);

/// Upsert: from the next tick on, the key maps to the winning value only.
'apply_put: upsert store(k, v) <- put_win(k, v);
'ack_put:   send put_ack(i) @ s <- put_in(s, i, _, _);

/// Reads see the state at the start of the tick (CR-04): a get in the same tick as a put returns the old value.
'get_hit:  send get_resp(i, k, Some(v)) @ s <- get_req(i, k) from s, store(k, v);
'get_miss: send get_resp(i, k, None) @ s <- get_req(i, k) from s, !store(k, _);

/// Delete removes the stored row at the next tick. If a put of the same key lands in the same tick, the upsert
/// re-inserts and insert wins (CR-05): the delete is ordered before the put.
'apply_del:    delete store(k, v) <- del_req(_, k), store(k, v);
'ack_del_hit:  send del_ack(i, true) @ s <- del_req(i, k) from s, store(k, _);
'ack_del_miss: send del_ack(i, false) @ s <- del_req(i, k) from s, !store(k, _);
```

Points of order the compiler reports: `choose_most!` in `'pick` (seed-free here, because the cost `(s, i)` is
unique per request: ANA-038 D5), the negations in `'get_miss` and `'ack_del_miss`, and the `upsert`/`delete`
rules. All are local to one node, which is what a single-node store is.

### E2. Reliable broadcast module (retry timer + acks, ReliableDelivery style)

```
// std/bcast/reliable_broadcast.bls

pub type MsgId = u64;

/// The broadcast contract. The importer supplies `members` every tick, so group membership stays its decision
/// (and can itself be interposed on).
pub protocol Broadcast<P> {
    /// Broadcast `payload` under an id that is unique for this origin.
    input bcast_in { id: MsgId, payload: P }
    /// The current group. Self may be listed; it is ignored.
    input members { n: Node }
    /// A message delivered on this node, once per (origin, id) per incarnation.
    output deliver { #[key] origin: Node, #[key] id: MsgId, payload: P }
    /// Every other member has acknowledged `id`.
    output bcast_done { id: MsgId }
}

/// At-least-once transmission with periodic retransmission until acknowledged, and receiver-side
/// deduplication. This is bud-sandbox's ReliableDelivery (buffer, periodic resend, ack, garbage collect)
/// specialized to a group.
pub module ReliableBroadcast<P, const RETRY: Duration = 1s>: Broadcast<P> {
    channel msg { @dst: Node, id: MsgId, payload: P }
    channel ack { @dst: Node, id: MsgId }
    timer retry every RETRY;

    /// My broadcasts that are not finished yet.
    table outbox { #[key] id: MsgId, payload: P }
    /// (id, member) pairs still waiting for an ack.
    table pending { id: MsgId, to: Node }
    /// Messages already delivered here, by (origin, id).
    table delivered { origin: Node, id: MsgId }

    scratch peer { n: Node }
    'peers: peer(n) <- members(n), n != self;

    rules send_side {
        'remember: outbox(i, p) <- bcast_in(i, p);
        'fan_out:  pending(i, n) <- bcast_in(i, _), peer(n);
        'first:    send msg(i, p) @ n <- bcast_in(i, p), peer(n);
        'resend:   send msg(i, p) @ n <- retry(..), outbox(i, p), pending(i, n);
        'acked:    delete pending(i, n) <- ack(i) from n, pending(i, n);
        'done:     bcast_done(i) <- outbox(i, _), !pending(i, _);
        'gc:       delete outbox(i, p) <- outbox(i, p), !pending(i, _);
    }

    rules receive_side {
        'self_deliver: deliver(self, i, p) <- bcast_in(i, p);
        'deliver:      deliver(o, i, p) <- msg(i, p) from o, !delivered(o, i);
        'remember_rx:  next delivered(o, i) <- msg(i, _) from o;
        'ack:          send ack(i) @ o <- msg(i, _) from o;
    }
}
```

Behavior worth checking against the rules:
- `bcast_done(i)` fires exactly once. The last ack arrives at tick t and deletes the last `pending` row for t+1; at
  t+1 `'done` holds and `'gc` stages the removal of `outbox(i)`, so at t+2 nothing is left. A broadcast with no
  peers finishes in the tick it starts.
- `'deliver` reads `delivered` from the start of the tick, and `'remember_rx` writes it for the next tick, so a
  duplicate in a later tick is dropped. Duplicates within one tick collapse by set semantics (CR-03). The key on
  `deliver` turns an origin that reuses an id for a different payload into a loud key error.
- The ack is addressed to the authenticated sender (`from o`), not to a payload field (ANA-106).
- The bangs mark the three non-monotone places: `!pending` (completion is a point of order, as it must be:
  "everyone acked" is a universal), `!delivered` (dedup), and the `delete` rules.

### E3. Raft leader election, as rules

Every node runs the same program. The design follows R07 §11.1: all Raft state of a node is read at the same
tick, the term is settled first ("term first"), and a node grants at most one vote per term per tick, choosing
deterministically among same-tick requests.

```
// raft_election.bls
program raft_election version 1;

const ELECTION_MIN: Duration = 150ms;
const ELECTION_MAX: Duration = 300ms;
const HEARTBEAT: Duration = 50ms;

/// All servers of the cluster, this one included. Read from the deployment config.
static member { n: Node }

// ---------------- messages ----------------
channel request_vote { @dst: Node, term: u64, last_idx: u64, last_term: u64 }
channel vote_reply   { @dst: Node, term: u64, granted: bool }
channel heartbeat    { @dst: Node, term: u64 }

// ---------------- durable state (Raft §3.8: persisted before any reply that depends on it) ----------------
/// The latest term this server has seen. Absent on first boot, which means 0.
#[durable, key()] table current_term { term: u64 }
/// At most one vote per term, ever.
#[durable] table voted_for { #[key] term: u64, candidate: Node }
/// The log. Election reads only its last entry; the replication half of Raft (FLAG-004) writes it.
#[durable] table log { #[key] idx: u64, term: u64, cmd: Bytes }

// ---------------- volatile state ----------------
table candidate_in { term: u64 }
table leader_in { term: u64 }
#[key()] table deadline { at: Instant }
/// Votes received per term. A lattice: counting toward a majority is a monotone threshold.
table votes_got { term: u64, voters: LSet<Node> }

timer clock every 10ms;
timer beat every HEARTBEAT;

pub output leader_now { term: u64 }
pub output stepped_down { term: u64 }

// ---------------- per-tick views ----------------
scratch peer { n: Node }
scratch msg_term { term: u64 }
scratch term_seen { term: u64 }
#[key()] scratch eff_term { term: u64 }
#[key()] scratch last_log { idx: u64, term: u64 }
scratch i_lead { term: u64 }
scratch valid_heartbeat { leader: Node }
scratch rv_ok { cand: Node, term: u64 }
#[key(term)] scratch grant { cand: Node, term: u64 }
scratch start_election {}
scratch reset_timer {}
scratch won { term: u64 }

'peers: peer(n) <- member(n), n != self;

// ---------------- term first ----------------
'mt_rv: msg_term(t) <- request_vote(t, _, _);
'mt_vr: msg_term(t) <- vote_reply(t, _);
'mt_hb: msg_term(t) <- heartbeat(t);
term_seen(0);
'ts_cur: term_seen(t) <- current_term(t);
'ts_msg: term_seen(t) <- msg_term(t);
/// The term this tick runs in: the highest term known before the tick or carried by any message in it.
'eff: eff_term(max!(t)) <- term_seen(t);

/// Persist the effective term, or the next one when this tick starts an election.
'adopt: upsert current_term(e) <- eff_term(e), !current_term(e), !start_election();
'bump:  upsert current_term(e + 1) <- eff_term(e), start_election();

// ---------------- the last log entry (for the election restriction, Raft §5.4.1) ----------------
'last:      last_log(i, t) <- log(i, t, _), argmax!(i);
'empty_log: last_log(0, 0) <- !log(_, _, _);

// ---------------- leadership and heartbeats ----------------
/// A leader of an older term is not a leader any more: stepping down on a higher term is this rule.
'lead:   i_lead(t) <- leader_in(t), eff_term(t);
'report: leader_now(t) <- i_lead(t);
'hb:     send heartbeat(t) @ p <- beat(..), i_lead(t), peer(p);
'hb_ok:  valid_heartbeat(l) <- heartbeat(t) from l, eff_term(t);

'down_leader:    delete leader_in(t) <- leader_in(t), eff_term(e), e > t;
'down_report:    stepped_down(t) <- leader_in(t), eff_term(e), e > t;
'down_candidate: delete candidate_in(t) <- candidate_in(t), eff_term(e), e > t;
'yield:          delete candidate_in(t) <- candidate_in(t), eff_term(t), valid_heartbeat(_);

// ---------------- randomized election timeout ----------------
'arm_boot:  reset_timer() <- boot();
'arm_hb:    reset_timer() <- valid_heartbeat(_);
'arm_vote:  reset_timer() <- grant(_, _);
'arm_elect: reset_timer() <- start_election();
/// The draw is captured into state, so it stays fixed until the next reset (R12 §5.8).
'rearm: upsert deadline(now() + d) <- reset_timer(), eff_term(e),
                                     let d = rand_range(ELECTION_MIN, ELECTION_MAX, ("election", e));
'timeout: start_election() <- clock(..), deadline(d), now() >= d,
                              !i_lead(_), !valid_heartbeat(_), !grant(_, _);

// ---------------- candidate: vote for self, solicit votes ----------------
'candidate: next candidate_in(e + 1) <- start_election(), eff_term(e);
'self_vote: next voted_for(e + 1, self) <- start_election(), eff_term(e);
'self_tally: next votes_got(e + 1, LSet::of(self)) <- start_election(), eff_term(e);
'solicit:   send request_vote(e + 1, li, lt) @ p <- start_election(), eff_term(e), last_log(li, lt), peer(p);

// ---------------- voter: RequestVote ----------------
/// The candidate's log is at least as up to date as mine, and its term is the term of this tick.
'up_to_date: rv_ok(c, t) <- request_vote(t, li, lt) from c, eff_term(t), last_log(mi, mt),
                            lt > mt || (lt == mt && li >= mi);
/// At most one new vote per term, chosen deterministically (least node id) among same-tick requests.
'pick:    grant(c, t) <- rv_ok(c, t), !voted_for(t, _), choose_least!(c per t);
/// A retransmitted request from the candidate I already voted for gets the same answer.
'regrant: grant(c, t) <- rv_ok(c, t), voted_for(t, c);
'record:  next voted_for(t, c) <- grant(c, t), !voted_for(t, _);
'yes:     send vote_reply(t, true) @ c <- grant(c, t);
'no:      send vote_reply(e, false) @ c <- request_vote(t, _, _) from c, !grant(c, t), eff_term(e);

// ---------------- candidate: count votes ----------------
'tally: votes_got(t, LSet::of(v)) <- vote_reply(t, true) from v, candidate_in(t), eff_term(t);
'win:   won(t) <- candidate_in(t), eff_term(t), let vs = votes_got[t], vs.is_quorum_of(member);
'become_leader: delete candidate_in(t), next leader_in(t) <- won(t);
'announce: send heartbeat(t) @ p <- won(t), peer(p);
```

Why the rules are safe (the obligations E10 checks):
- **One vote per term.** `grant` is keyed by term, `'pick` fires only when no vote is recorded, `'regrant` only
  for the recorded candidate, and `'record` makes the vote durable before `'yes` is released (the durable commit
  precedes the outbox, SEM-072). The self vote is recorded in the same tick that solicits votes.
- **Term first.** `eff_term` joins every term seen before anything else is decided, so a message from an older term
  can neither grant a vote (`'up_to_date` requires `eff_term(t)`) nor count toward a win (`'tally` requires
  `eff_term(t)`). A leader that sees a higher term stops being `i_lead` in that same tick.
- **Majority.** `vs.is_quorum_of(member)` is |vs ∩ member| > |member|/2, a monotone threshold over a grow-only set,
  which the verifier maps to its quorum sort (LANG-111, VER-008).
- **Points of order.** `max!`, `argmax!`, `choose_least!`, the negations, and the `delete`/`upsert` rules. All are
  node-local: Raft's server must not be decoupled or partitioned (ANA-083), which the analysis reports.

The per-term `votes_got` cells are never deleted (lattices cannot be); one small cell per term is the cost. A
production module would key the tally by a `Lex<LMax<u64>, LSet<Node>>` cell to reuse one cell across terms
(LANG-284).

### E4. Two-phase commit: one choreographic module, two roles, timeout abort

```
// std/commit/two_phase_commit.bls

pub type Xid = u64;

/// Two-phase commit with presumed abort and a coordinator timeout (LIB-041). One module, two roles. The compiler
/// projects a Coordinator program and a Participant program, and infers every channel's ACL from the `send`
/// rules: only Participants may send `vote`, only the Coordinator may send `prepare` and `decision`.
pub module TwoPhaseCommit<const TIMEOUT: Duration = 2s, const RESEND: Duration = 200ms> {
    channel prepare      { @dst: Node<Participant>, xid: Xid }
    channel vote         { @dst: Node<Coordinator>, xid: Xid, yes: bool }
    channel decision     { @dst: Node<Participant>, xid: Xid, commit: bool }
    channel decision_ack { @dst: Node<Coordinator>, xid: Xid }

    role Coordinator {
        /// Start a transaction.
        pub input begin { xid: Xid }
        /// The outcome, emitted in the tick the decision is made.
        pub output outcome { #[key] xid: Xid, commit: bool }

        #[durable] table running { #[key] xid: Xid, started: Instant }
        #[durable] table votes { xid: Xid, from: Node<Participant>, yes: bool }
        #[durable] table decided { #[key] xid: Xid, commit: bool }
        table acked { xid: Xid, from: Node<Participant> }
        timer check every RESEND;

        /// One decision per transaction per tick; two different ones are a key error, never a silent pick.
        #[key(xid)] scratch decide { xid: Xid, commit: bool }
        scratch missing_vote { xid: Xid }
        scratch unacked { xid: Xid }

        // phase 1: log the transaction (durable before `prepare` leaves), then ask everyone
        'log_begin: next running(x, now()) <- begin(x), !running(x, _);
        'prepare:   send prepare(x) @ p <- begin(x), p in Participant;
        'reprepare: send prepare(x) @ p <- check(..), running(x, _), !decided(x, _), p in Participant,
                                          !votes(x, p, _);
        'collect:   votes(x, p, y) <- vote(x, y) from p, running(x, _);

        // decide: any "no" aborts; all "yes" commits; silence past the timeout aborts
        'missing:    missing_vote(x) <- running(x, _), p in Participant, !votes(x, p, true);
        'abort_no:   decide(x, false) <- running(x, _), votes(x, _, false), !decided(x, _);
        'commit_all: decide(x, true) <- running(x, _), !missing_vote(x), !decided(x, _);
        'timeout:    decide(x, false) <- check(..), running(x, t0), now() - t0 >= TIMEOUT,
                                         missing_vote(x), !decided(x, _);

        // phase 2: log the decision (durable before it is announced), announce, re-announce until acked
        'record:   next decided(x, c) <- decide(x, c);
        'report:   outcome(x, c) <- decide(x, c);
        'announce: send decision(x, c) @ p <- decide(x, c), p in Participant;
        'redecide: send decision(x, c) @ p <- check(..), decided(x, c), p in Participant, !acked(x, p);
        'ack:      acked(x, p) <- decision_ack(x) from p;

        // garbage collection once every participant has acknowledged; `decided` is kept for late messages
        'unacked:  unacked(x) <- decided(x, _), p in Participant, !acked(x, p);
        'gc_run:   delete running(x, t0) <- running(x, t0), decided(x, _), !unacked(x);
        'gc_votes: delete votes(x, p, y) <- votes(x, p, y), decided(x, _), !unacked(x);
        'gc_acks:  delete acked(x, p) <- acked(x, p), decided(x, _), !unacked(x);
    }

    cluster Participant {
        /// The local resource manager's verdict for a transaction; it may arrive before or after `prepare`.
        pub input can_commit { xid: Xid, yes: bool }
        /// The decision, emitted once per transaction per incarnation of the log.
        pub output apply { #[key] xid: Xid, commit: bool }

        table willing { #[key] xid: Xid, yes: bool }
        table asked { xid: Xid, coord: Node<Coordinator> }
        #[durable] table prepared { #[key] xid: Xid, coord: Node<Coordinator>, yes: bool }
        #[durable] table outcome_log { #[key] xid: Xid, commit: bool }

        'willing: willing(x, y) <- can_commit(x, y);
        'asked:   asked(x, c) <- prepare(x) from c;

        // vote, with the vote logged durably in the same tick (released only after the fsync)
        'log_vote_now:   prepared(x, c, y) <- prepare(x) from c, willing(x, y);
        'log_vote_later: prepared(x, c, y) <- can_commit(x, y), asked(x, c);
        'vote_now:       send vote(x, y) @ c <- prepare(x) from c, willing(x, y);
        'vote_later:     send vote(x, y) @ c <- can_commit(x, y), asked(x, c);

        // learn the decision once; acknowledge every copy
        'learn: next outcome_log(x, c) <- decision(x, c), !outcome_log(x, _);
        'apply: apply(x, c) <- decision(x, c), !outcome_log(x, _);
        'ack:   send decision_ack(x) @ co <- decision(x, _) from co;
    }
}
```

The projection gives the Coordinator the rules of its block plus the receive side of `vote` and `decision_ack`,
and gives each Participant its block plus `prepare` and `decision`. The inferred ACLs are
`acl(prepare) = acl(decision) = {Coordinator}` and `acl(vote) = acl(decision_ack) = {Participant}`. A participant
that answers both yes and no makes `'abort_no` and `'commit_all` derive two different rows for one key of
`decide`, which is a loud runtime error rather than a silent choice.

### E5. Lattices: vector clocks, a monotone shopping cart, a quorum threshold, a user lattice

```
// lattice_demo.bls
program lattice_demo version 1;

pub type VClock = LMap<Node, LMax<u64>>;
pub type SessionId = u64;
pub type OpId = u64;
pub type Item = String;

static member { n: Node }

// =============== (a) vector clocks ===============
pub module ClockGossip<const SYNC: Duration = 500ms> {
    pub input local_event { id: u64 }
    pub input peers { n: Node }
    /// A received event whose clock my history already covered when it arrived.
    pub output stale { from: Node, id: u64 }
    /// A received event concurrent with my history when it arrived.
    pub output concurrent { from: Node, id: u64 }

    lattice my_vc: VClock;
    channel stamped { @dst: Node, id: u64, vc: VClock }
    channel sync { @dst: Node, vc: VClock }
    timer sync_timer every SYNC;
    #[key()] scratch n_events { n: u64 }
    scratch ranked { id: u64, rank: u64 }

    'count: n_events(count!(e)) <- local_event(e);
    'rank:  ranked(e, index!()) <- local_event(e);
    /// Each local event gets its own clock: my entry advanced by the event's rank + 1 within the tick.
    'stamp: send stamped(e, my_vc | LMap::of(self, my_vc.at(self) + (r + 1))) @ p
                <- ranked(e, r), peers(p), p != self;
    'bump:  next my_vc(LMap::of(self, my_vc.at(self) + n)) <- n_events(n);
    /// Received clocks are merged for the next tick, so this tick's comparisons see my clock before arrival.
    'merge_event: next my_vc(vc) <- stamped(_, vc);
    'gossip:      send sync(my_vc) @ p <- sync_timer(..), peers(p), p != self;
    'merge_sync:  next my_vc(vc) <- sync(vc);
    'stale:       stale(s, e) <- stamped(e, vc) from s, vc.leq!(my_vc);
    'concurrent:  concurrent(s, e) <- stamped(e, vc) from s, vc.concurrent!(my_vc);
}

// =============== (b) a monotone shopping cart with a user-defined lattice ===============
pub enum CartOp {
    Add { item: Item, qty: i64 },                                // qty < 0 removes
    Checkout { first: OpId, reply_to: Session, via: Node },      // ends the session's op range at its own id
    #[unknown] Unknown,
}

/// One session's cart as a replica knows it: every operation heard of, by id. A product-DSL lattice: the merge
/// is the fieldwise merge (map union), and two different operations under one id are a hard Conflict error
/// (`LPoint`), as in Bloom's lcart.
pub lattice Cart { ops: LMap<OpId, LPoint<CartOp>> }

impl Cart {
    /// The session's final contents and where to reply, once the cart is complete: exactly one checkout exists and
    /// every id from its `first` up to its own id is present. Before that, None. Monotone-then-immutable: once
    /// Some(v), every larger Cart gives Some(v) or a hard error, which is the `#[threshold]` law.
    #[threshold]
    fn summary(self) -> Option<(Session, Node, OrdMap<Item, i64>)> {
        let ops = reveal!(self.ops);
        let checkouts = ops.iter().filter_map(|(id, op)| match op {
            CartOp::Checkout { first, reply_to, via } => Some((id, first, reply_to, via)),
            _ => None,
        }).collect();
        match checkouts.len() {
            0 => None,
            1 => {
                let (last, first, reply_to, via) = checkouts[0];
                if (first..=last).all(|i| ops.contains_key(i)) {
                    let totals = ops.range(first..last).fold(OrdMap::new(), |acc, (_, op)| match op {
                        CartOp::Add { item, qty } => acc.insert(item, acc.get(item).unwrap_or(0) + qty),
                        CartOp::Checkout { .. } => error("cart: checkout inside another checkout's range"),
                        CartOp::Unknown => error("cart: operation from a newer version inside the range"),
                    });
                    Some((reply_to, via, totals.filter(|_, n| n > 0)))
                } else {
                    None
                }
            }
            _ => error("cart: two checkouts for one session"),
        }
    }
}

pub module MonotoneCart<const SYNC: Duration = 1s> {
    pub input replicas { n: Node }

    #[accept(external)] channel action   { @dst: Node, session: SessionId, op: OpId, item: Item, qty: i64 }
    #[accept(external)] channel checkout { @dst: Node, session: SessionId, op: OpId, first: OpId }
    channel reply     { @dst: Session, session: SessionId, items: OrdMap<Item, i64> }
    channel cart_sync { @dst: Node, session: SessionId, cart: Cart }

    table carts { session: SessionId, cart: Cart }
    table replied { session: SessionId }
    timer sync_timer every SYNC;

    'add: carts(s, Cart { ops: LMap::of(o, LPoint::of(CartOp::Add { item: i, qty: q })) })
              <- action(s, o, i, q);
    'checkout: carts(s, Cart { ops: LMap::of(o, LPoint::of(CartOp::Checkout { first: f, reply_to: c, via: self })) })
              <- checkout(s, o, f) from c;
    /// Anti-entropy: ship whole carts; the receiver's merge makes duplicates and reordering harmless.
    'anti_entropy: send cart_sync(s, cart) @ r <- sync_timer(..), carts(s, cart), replicas(r), r != self;
    'merge:        carts(s, cart) <- cart_sync(s, cart);
    /// No coordination: the reply's content is final the moment `summary` is Some. Only the replica that holds
    /// the client's session replies; `replied` only suppresses re-sending the same final answer.
    'respond: send reply(s, items) @ c <- carts(s, cart), let Some((c, via, items)) = cart.summary(),
                                          via == self, !replied(s);
    'once:    next replied(s) <- carts(s, cart), let Some(_) = cart.summary();
}

// =============== (c) quorum as a monotone threshold ===============
pub module QuorumVote<const QUORUM: u64 = 3> {
    pub input cast { coordinator: Node, ballot: u64 }
    #[final] pub output quorum_reached { ballot: u64 }

    channel vote { @dst: Node, ballot: u64 }
    table votes { ballot: u64, voters: LSet<Node> }

    'cast:    send vote(b) @ c <- cast(c, b);
    'tally:   votes(b, LSet::of(v)) <- vote(b) from v;
    /// size() is monotone and `>= QUORUM` is a threshold: no bang, no point of order, POS-final.
    'reached: quorum_reached(b) <- votes(b, vs), vs.size() >= QUORUM;
}

/// The same protocol with a set and a count, for contrast: the bang on `count!` marks the point of order, and
/// `#[final]` on this output would be rejected by ANA-120 (the count is not a threshold of a lattice).
pub module QuorumVoteCounted<const QUORUM: u64 = 3> {
    pub input cast { coordinator: Node, ballot: u64 }
    pub output quorum_reached { ballot: u64 }

    channel vote { @dst: Node, ballot: u64 }
    table voted { ballot: u64, voter: Node }
    scratch tally { #[key] ballot: u64, n: u64 }

    'cast:    send vote(b) @ c <- cast(c, b);
    'store:   voted(b, v) <- vote(b) from v;
    'count:   tally(b, count!(v)) <- voted(b, v);
    'reached: quorum_reached(b) <- tally(b, n), n >= QUORUM;
}

// =============== composition ===============
import ClockGossip as clocks;
import MonotoneCart as cart;
import QuorumVote<QUORUM = 3> as quorum;

'clock_peers: clocks.peers(n) <- member(n);
'cart_peers:  cart.replicas(n) <- member(n);
```

What the analyzer reports: in `ClockGossip`, `count!`, `index!`, `leq!` and `concurrent!` are points of order (the
last two correctly so: "is this concurrent?" is a non-monotone question). In `MonotoneCart` the data path is
monotone; `!replied` is a point of order that only suppresses re-sends of a final value. `QuorumVote` is monotone
end to end, and `QuorumVoteCounted` is not.

### E6. Word count: hash-partitioned shuffle, reducer aggregation, seals for end of input

```
// wordcount.bls
program wordcount version 1;

const HOT: u64 = 1000;

/// The job plan: which mapper reads which input split.
static assignment { #[key] split: u32, mapper: Node<Mapper> }

/// One occurrence of a word, identified by its position in the input. The identity makes every shuffle tuple
/// idempotent, so at-least-once delivery cannot inflate a count (CR-35). Each mapper seals the stream to each
/// reducer when it has sent everything, with the number of tuples it sent as the digest.
#[seal(producers = Mapper)]
#[fault(reliable)]
channel shuffle { @dst: Node<Reducer>, split: u32, line: u64, pos: u32, word: String }

fn words(text: String) -> Vec<String> {
    text.split_whitespace().map(|w| w.to_lowercase()).filter(|w| !w.is_empty()).collect()
}

cluster Mapper {
    /// Lines of a split, fed by the host's reader.
    pub input line { split: u32, lineno: u64, text: String }
    /// The host has read the whole split and it had `lines` lines.
    pub input split_end { split: u32, lines: u64 }

    table seen_line { split: u32, lineno: u64 }
    table split_total { #[key] split: u32, lines: u64 }
    /// What this mapper has sent, per reducer, to compute the seal digests.
    table sent { reducer: Node<Reducer>, split: u32, lineno: u64, pos: u32 }
    table sealed_already {}

    scratch line_count { #[key] split: u32, n: u64 }
    scratch split_done { split: u32 }
    scratch unfinished {}
    scratch all_done {}
    scratch sent_count { #[key] reducer: Node<Reducer>, n: u64 }

    // map and shuffle: route each occurrence to the reducer that owns the word
    'map:      send shuffle(s, l, p as u32, w) @ Reducer::route(w)
                   <- line(s, l, text), for (p, w) in words(text).enumerate();
    'log_sent: sent(Reducer::route(w), s, l, p as u32)
                   <- line(s, l, text), for (p, w) in words(text).enumerate();

    // end of input: every assigned split has all its lines
    'lines:       seen_line(s, l) <- line(s, l, _);
    'total:       split_total(s, n) <- split_end(s, n);
    'count_lines: line_count(s, count!(l)) <- per split_total(s, _), seen_line(s, l);
    'split_done:  split_done(s) <- split_total(s, n), line_count(s, n);
    'unfinished:  unfinished() <- assignment(s, self), !split_done(s);
    'all_done:    all_done() <- !unfinished();

    // punctuation: to every reducer, "no more shuffle tuples from me; I sent you n"
    'digest: sent_count(r, count!((s, l, p))) <- per (r in Reducer), sent(r, s, l, p);
    'seal:   seal shuffle() @ r digest n <- all_done(), !sealed_already(), sent_count(r, n);
    'once:   next sealed_already() <- all_done();
}

cluster Reducer {
    /// Final counts: emitted once every mapper's seal has arrived and every sealed tuple is here.
    #[final] pub output word_count { #[key] word: String, n: u64 }
    /// Early, final answers that need no seal: a threshold over a growing set (POS-final).
    #[final] pub output hot_word { word: String }

    table occ { split: u32, line: u64, pos: u32, word: String }
    table occ_ids { word: String, ids: LSet<(u32, u64, u32)> }

    'store: occ(s, l, p, w) <- shuffle(s, l, p, w);
    'grow:  occ_ids(w, LSet::of((s, l, p))) <- shuffle(s, l, p, w);
    'hot:   hot_word(w) <- occ_ids(w, ids), ids.size() >= HOT;
    'final: word_count(w, count!((s, l, p))) <- shuffle.sealed(), occ(s, l, p, w);
}
```

How the pieces meet:
- `Reducer::route(w)` is rendezvous hashing over the sorted `Reducer` members, so the `'map` and `'log_sent` rules
  agree on the owner and the digest counts exactly the tuples each reducer should receive.
- The seal is sent once (`sealed_already`), which is enough on a `#[fault(reliable)]` channel. Under a lossy fault
  model the right change is to resend the seal on a timer; seals are idempotent (same digest), and a conflicting
  digest is a violation (§3.12).
- `shuffle.sealed()` holds at a reducer once every mapper has sealed and the number of distinct tuples received
  from each equals its digest. The `count!` in `'final` is a point of order, but it is guarded by the seal, so
  ANA-120 classifies `word_count` SEALED-final and accepts `#[final]`. `hot_word` is POS-final with no seal at all:
  the job's two CALM output classes (early threshold, sealed final) are both visible in the source.
- A mapper with no assigned splits is done in its first tick and sends each reducer a seal with digest 0.

### E7. Single-node analytics: transitive closure, shortest paths, stratified negation

```
// graph.bls
program graph_analytics version 1;

static node { id: u32 }
static edge { src: u32, dst: u32, w: u64 }

node(1); node(2); node(3); node(4); node(5);
edge(1, 2, 4); edge(2, 3, 1); edge(1, 3, 7); edge(3, 1, 2); edge(4, 5, 3);

scratch reach { src: u32, dst: u32 }
/// Shortest distance, as a min-lattice so it can sit inside the recursion: `d + w` on an LMin is a morphism,
/// and non-negative weights make the fixpoint finite (CR-53).
scratch dist { src: u32, dst: u32, d: LMin<u64> }
scratch via_cost { src: u32, dst: u32, via: u32, cost: u64 }

pub output reachable { src: u32, dst: u32 }
pub output shortest { #[key] src: u32, #[key] dst: u32, d: u64 }
pub output first_hop { src: u32, dst: u32, via: u32 }
pub output unreachable { src: u32, dst: u32 }

// transitive closure
'tc_base: reach(a, b) <- edge(a, b, _);
'tc_step: reach(a, c) <- reach(a, b), edge(b, c, _);
'tc_out:  reachable(a, b) <- reach(a, b);

// shortest paths: min aggregation as a lattice (monotone, recursive, one stratum with reach)
'sp_base: dist(a, b, LMin(w)) <- edge(a, b, w);
'sp_step: dist(a, c, d + w) <- dist(a, b, d), edge(b, c, w);
'sp_out:  shortest(a, b, n) <- dist(a, b, d), let n = reveal!(d);

// the first hop of a shortest path: an exemplary (non-lattice) min, in a later stratum
'via_direct: via_cost(a, c, c, w) <- edge(a, c, w);
'via_other:  via_cost(a, c, b, w + n) <- edge(a, b, w), dist(b, c, d), let n = reveal!(d);
'first_hop:  first_hop(a, c, b) <- via_cost(a, c, b, x), argmin!(x per (a, c));

// stratified negation: pairs of distinct nodes with no path
'unreach: unreachable(a, b) <- node(a), node(b), a != b, !reach(a, b);
```

Strata: {`reach`, `dist`} (positive; the lattice recursion is monotone), then {`via_cost`, `shortest`} (`reveal!`),
then {`first_hop`} (`argmin!`) and {`unreachable`} (`!reach`). Expected outputs for the facts above:

| relation | rows |
|---|---|
| `reachable` | (1,1) (1,2) (1,3) (2,1) (2,2) (2,3) (3,1) (3,2) (3,3) (4,5) |
| `shortest` | (1,1,7) (1,2,4) (1,3,5) (2,1,3) (2,2,7) (2,3,1) (3,1,2) (3,2,6) (3,3,7) (4,5,3) |
| `first_hop` | (1,1,2) (1,2,2) (1,3,2) (2,1,3) (2,2,3) (2,3,3) (3,1,1) (3,2,1) (3,3,1) (4,5,5) |
| `unreachable` | (1,4) (1,5) (2,4) (2,5) (3,4) (3,5) (4,1) (4,2) (4,3) (5,1) (5,2) (5,3) (5,4) |

### E8. Soft-state heartbeat failure detector

```
// failure_detector.bls
program failure_detector version 1;

const BEAT: Duration = 1s;
const TTL: Duration = 3s;

static member { n: Node }

channel heartbeat { @dst: Node }
timer beat every BEAT;

/// A peer is alive while a heartbeat from it arrived within the last TTL. Every heartbeat re-derives the row and
/// refreshes its birth; expiry is decided at tick boundaries against the tick's `now()` (SEM-060), and the
/// `beat` timer guarantees a tick at least every BEAT, so a silent peer is suspected within TTL + BEAT.
#[soft(ttl = TTL, max = 4096)] table alive { peer: Node }

pub output suspect { peer: Node }
pub output fd_event { peer: Node, up: bool }

scratch peer { n: Node }

'peers:   peer(n) <- member(n), n != self;
'beat:    send heartbeat() @ p <- beat(..), peer(p);
'heard:   alive(s) <- heartbeat() from s, peer(s);
'suspect: suspect(p) <- peer(p), !alive(p);
'up:      fd_event(p, true) <- inserted!(alive(p));
'down:    fd_event(p, false) <- deleted!(alive(p));
```

The soft table lowers as in §3.2: `alive(s)` becomes a write to `alive$d`, storage keeps the birth, and `alive` at
tick t is the rows whose birth is within 3 s of `now()` at t. `deleted!(alive(p))` is therefore exactly the
expiry event, and `inserted!` the first heartbeat after a silence.

### E9. Importing E2 twice and interposing on one interface

```
// chat.bls
program chat version 1;

use std::bcast::reliable_broadcast::{Broadcast, ReliableBroadcast, MsgId};

const MAX_BLOB: u64 = 1_048_576;

static member { n: Node }

/// Two independent reliable-broadcast instances from E2: a fast one for small text messages and a slow one for
/// bulk data, with different payload types and retry periods. The bulk instance's `deliver` interface is
/// interposed on: every delivery is metered, and oversized blobs are dropped before the application sees them.
pub module DualBroadcast {
    import ReliableBroadcast<P = String, RETRY = 200ms> as control;
    import ReliableBroadcast<P = Bytes, RETRY = 2s> as bulk;

    pub input members { n: Node }
    pub input post_text { id: MsgId, text: String }
    pub input post_blob { id: MsgId, blob: Bytes }
    pub output text { origin: Node, id: MsgId, text: String }
    pub output text_done { id: MsgId }
    pub output blob { origin: Node, id: MsgId, blob: Bytes }
    pub output dropped_blob { origin: Node, id: MsgId, size: u64 }
    pub output bulk_bytes { #[key] origin: Node, total: u64 }

    /// Bytes delivered per origin, keyed by message id so that duplicates cannot inflate the meter.
    table metered { origin: Node, sizes: LMap<MsgId, LMax<u64>> }

    // membership fans out to both instances
    'members_control: control.members(n) <- members(n);
    'members_bulk:    bulk.members(n) <- members(n);

    // the control instance, used as is
    'post_text: control.bcast_in(i, t) <- post_text(i, t);
    'text:      text(o, i, t) <- control.deliver(o, i, t);
    'text_done: text_done(i) <- control.bcast_done(i);

    // the bulk instance, input side: refuse oversized blobs before anything is sent
    'post_blob: bulk.bcast_in(i, b) <- post_blob(i, b), b.len() <= MAX_BLOB;
    'refuse:    dropped_blob(self, i, b.len()) <- post_blob(i, b), b.len() > MAX_BLOB;

    // the bulk instance, delivery side: interposition on bulk.deliver
    'meter:  metered(o, LMap::of(i, LMax(b.len()))) <- bulk.deliver(o, i, b);
    'pass:   blob(o, i, b) <- bulk.deliver(o, i, b), b.len() <= MAX_BLOB;
    'block:  dropped_blob(o, i, b.len()) <- bulk.deliver(o, i, b), b.len() > MAX_BLOB;
    'report: bulk_bytes(o, n) <- metered(o, sizes), let n = reveal!(sizes.sum_values());
}

import DualBroadcast as chat;

'chat_members: chat.members(n) <- member(n);
```

Instantiation produces the relations `chat::control::msg`, `chat::control::outbox`, … and
`chat::bulk::msg`, `chat::bulk::outbox`, …: two disjoint copies of E2 with `P` and `RETRY` substituted. Their
channels are distinct (`chat::control::msg` and `chat::bulk::msg` have different wire names and schema ids), so a
bulk message can never be delivered by the control instance. The application sees only `chat.text`,
`chat.blob`, `chat.dropped_blob` and `chat.bulk_bytes`; `chat.bulk.deliver` is reachable only inside
`DualBroadcast`, which is what makes the interposition airtight. The `'block` rule guards against senders whose
own check differs (for example, an older program version).

### E10. Verification specs: LDFI for simple broadcast, election safety for E3

The broadcast programs are Molly's `simplog` and `ack_rb` (R02 §12.5), written in Blossom.
```
// simplog.bls
program simplog version 1;

static neighbor { n: Node }
pub input bcast { payload: String }
table log { payload: String }
channel log_msg { @dst: Node, payload: String }

'origin: log(p) <- bcast(p);
'fan:    send log_msg(p) @ n <- bcast(p), neighbor(n);
'recv:   log(p) <- log_msg(p);
```
```
// ack_rb.bls: every node that logs a message relays it to each neighbor until that neighbor acknowledges
program ack_rb version 1;

static neighbor { n: Node }
pub input bcast { payload: String }
table log { payload: String }
table acked { by: Node, payload: String }
channel rbcast { @dst: Node, payload: String }
channel ack { @dst: Node, payload: String }

'origin: log(p) <- bcast(p);
'relay:  send rbcast(p) @ n <- log(p), neighbor(n), !acked(n, p);
'recv:   log(p) <- rbcast(p);
'ack:    send ack(p) @ s <- rbcast(p) from s;
'acked:  acked(s, p) <- ack(p) from s;
```
The specs. A spec without `for` holds reusable assertions; `include` copies them into a run spec. A scenario fact
without `@` holds at every node listed in `nodes`.
```
// delivery_specs.bls

/// Molly's deliv_assert.ded.
spec deliv_assert {
    /// Someone has the message in its log, but one of its neighbors does not.
    missing_log(a, p) <- log(p) @ x, neighbor(a) @ x, !log(p) @ a;
    /// Precondition: a node other than the broadcaster logged it and did not crash.
    pre(x, p)  <- log(p) @ x, !bcast(p) @ x at tick 1, !crash(x, _);
    /// Outcome: every node that logged it has no neighbor missing it.
    post(x, p) <- log(p) @ x, !missing_log(_, p);
}

/// Three nodes in a clique; `a` broadcasts at tick 1.
spec clique_scenario {
    nodes ["a", "b", "c"];
    neighbor("b") @ "a"; neighbor("c") @ "a";
    neighbor("a") @ "b"; neighbor("c") @ "b";
    neighbor("a") @ "c"; neighbor("b") @ "c";
    bcast("hello") @ "a" at tick 1;
}

/// Expected: a counterexample (Molly: simplog + deliv_assert, EOT 4, EFF 2, 0 crashes). The minimal falsifier
/// is one omission, a → b at tick 1: c logs the message (so `pre` holds) while b never does (so `post` is empty).
spec simplog_ldfi for simplog {
    include clique_scenario;
    include deliv_assert;
    failures { eot: 4, eff: 2, crashes: 0 }
}

/// Expected: no counterexample (Molly: ack_rb + deliv_assert, EOT 8, EFF 6, 1 crash). Relaying until acked
/// survives any omissions before EFF plus one crash.
spec ack_rb_ldfi for ack_rb {
    include clique_scenario;
    include deliv_assert;
    failures { eot: 8, eff: 6, crashes: 1 }
    /// Bounded liveness: after the last possible omission, everyone who can have it has it within 2 ticks.
    eventually post within 2 after eff;
}
```
Election safety for E3, as a trace invariant for the simulator, LDFI and bounded model checking, plus an inductive
strengthening for the SMT backend:
```
// raft_election_specs.bls
spec raft_election_safety for raft_election {
    nodes ["n1", "n2", "n3"];
    member("n1"); member("n2"); member("n3");                  // at every node
    failures { eot: 30, eff: 20, crashes: 1 }
    bounds { ticks: 30, delay: 4, in_flight: 24 }

    /// Election Safety (Raft §5.2): at most one leader per term, over the whole run.
    invariant two_leaders(t: u64, a: Node, b: Node) <-
        leader_in(t) @ a at tick _, leader_in(t) @ b at tick _, a < b;

    // An inductive invariant, checked on every global state by the first-order backend (VER-006–010).
    // Each conjunct is a denial: any satisfying row is a counterexample to induction.

    /// A node votes at most once per term.
    #[inductive] invariant vote_once(t: u64, n: Node, a: Node, b: Node) <-
        voted_for(t, a) @ n, voted_for(t, b) @ n, a < b;
    /// A granted vote in the network is backed by the voter's durable record.
    #[inductive] invariant reply_backed(t: u64, c: Node, v: Node) <-
        net vote_reply(t, true) @ c from v, !voted_for(t, c) @ v;
    /// Every vote a candidate has tallied is backed by the voter's durable record.
    #[inductive] invariant tally_backed(t: u64, c: Node, v: Node) <-
        votes_got(t, vs) @ c, for v in vs.items(), !voted_for(t, c) @ v;
    /// A leader of term t holds the votes of a quorum for t.
    #[inductive] invariant leader_has_quorum(t: u64, l: Node) <-
        leader_in(t) @ l, !(quorum v in member { voted_for(t, l) @ v });
    /// The safety property itself, stated on a single state; it follows from the previous four by quorum
    /// intersection (VER-008's axiom) and `vote_once`.
    #[inductive] invariant one_leader(t: u64, a: Node, b: Node) <-
        leader_in(t) @ a, leader_in(t) @ b, a < b;
}
```
Expected: no counterexample within the failure budget, and the inductive invariant discharged by the SMT backend.
The quorum literal is what keeps the first-order encoding in EPR: `quorum v in member { … }` becomes an
existential over the quorum sort with the intersection axiom (R10 A3.6), instead of a cardinality count.

---

## 5. Self-critique

### 5.1 Where the Rust flavor misleads

1. **Repeated variables join; they do not shadow.** In Rust, `(x, x)` is an error and a second `let x` shadows.
   Here a second occurrence of `x` in the body is an equality join, which is the whole point of Datalog. A Rust
   programmer will read it right most of the time, because relational code is visibly relational, but the
   mismatch is real. Mitigations: `let x = e` when `x` is already bound is a compile error ("`x` is already bound;
   write `x == e`"), and the hover type of every variable shows every atom that binds it.
2. **The bang is overloaded.** `!` is boolean not, negation of an atom, and (as `name!(` / `.name!(`) the
   non-monotone marker. The rule "every negative edge carries a `!`" holds, but its converse does not: `!b` on a
   plain `bool` is not a point of order. `!(x > 3)` parses as a negated conjunction whose only literal is a guard,
   which means the same as boolean not, so the two readings never disagree, but a reader has to know that. A
   distinct sigil (for example `~`) was considered and rejected: `!` is what Rust programmers already read as
   "look here", and the type checker enforces the spelling in both directions.
3. **`name!(…)` is not a macro.** Rust readers expect `count!(…)` to expand to code. Here it is an operator with
   its own argument grammar (`per`, `by`, `default`, `sticky`). The postfix form `.leq!(…)` is not Rust at all.
   Both are chosen for visibility, not for fidelity; the documentation must say so on page one.
4. **Heads that merge look like heads that insert.** `votes(t, LSet::of(v))` merges because the column is a
   lattice, which only the declaration says. The `L…` naming convention and IDE hovers help; a head-level marker
   (`votes(t) |= LSet::of(v)`) was considered and rejected because it breaks the "a head is an atom" uniformity
   that the multi-head and whole-relation forms rely on. This is the weakest readability point of the proposal.
5. **Channel atoms have one column fewer than their declaration.** The `@` column is omitted in atoms (it is
   `self` on receipt and `@ expr` on send). Named atoms make this invisible, but positional arity errors on
   channels need a dedicated message ("the `@dst` column is written as `@ …` after the head").
6. **`fn` bodies look like Rust and are not.** No references, no mutation, no loops, no recursion, closures only
   inside `fn` and `aggregate` items, and `error("…")` instead of `panic!`. The subset is deliberate (totality
   and purity are what make UDF properties checkable), but porting real Rust helpers will hit the boundary. The
   escape hatch is `extern fn`.
7. **Struct-typed relations and struct values share syntax.** `buf { dst, .. }` in a body is an atom; in an
   expression position `Delivery { dst: d, .. }` is a value. Name resolution tells them apart (relation vs type
   namespace), so there is no parse ambiguity, but a reader can confuse them.

### 5.2 Semantic points the syntax does not make visible

1. **`delete` and `upsert` carry no bang.** They are negative edges (SEM-021), marked by their leading keyword
   instead. `grep` finds them, but the "every negative edge has a `!`" slogan needs this footnote.
2. **Schedule-dependent built-ins are not marked.** `now()`, `tick()`, `rand()`, `choose_rand!` and sticky
   choices make outputs schedule-dependent (SEM-087) but only `choose_rand!` has a bang. ANA-011 and the
   determinism certificate report them; under `--strict` an uncaptured use over persistent input is an error. A
   `nondet!` wrapper was considered and rejected as noise for the common, captured case (a deadline written to
   state).
3. **`c.sealed()` is presented as a threshold**, and its lowering uses negation. It is monotone in time only
   because the digest checks turn every non-monotone situation (a conflicting or exceeded digest) into a violation.
   The analysis must treat `$sealed` specially (ANA-065's CLOSED rule), not derive its polarity from the lowering.
4. **Seals assume a known producer set.** `#[seal(producers = R)]` requires `R`'s membership to be fixed for the
   stream. Dynamic membership needs epoch-scoped seals (`#[seal(key = (epoch), producers = R)]` with `R`'s
   membership sealed per epoch, DIST-042); the syntax allows it, but no example exercises it.
5. **Per-tick batching is invisible at the rule site.** A reader of E3's `'pick` must know that "one vote per
   term" is per *tick*, that the batch is a set, and that reads see the start of the tick. These are the semantics
   (CR-01–04), and no syntax can make them obvious; the documentation and the simulator's space-time diagrams
   must.
6. **`per` is subtle.** It changes an aggregate from GROUP BY semantics to "one row per driver tuple", and in the
   carried `fold_ordered!` form it also supplies the initial state. It is one keyword doing two related jobs.

### 5.3 Grammar and tooling costs

1. **Ring intervals** (`x in (a, b]`) make the token after `in` decide between an interval and an expression. It is
   one token of lookahead, but mismatched brackets are unusual enough that editors will fight them.
2. **`<-` steals `a<-b`**, and `#` comments steal `#`-prefixed text that is not an attribute. Both follow
   precedent (Rust reserves `<-`; Molly uses `#`), and both are documented, but both will produce confusing
   errors in pasted code.
3. **The keyword list is long.** Many of the domain words are contextual (`every`, `digest`, `sticky`, `nodes`,
   …), which keeps them usable as column names, but `send`, `next`, `delete`, `table`, `static`, `input` and
   `output` are reserved and are also natural column names. Raw identifiers (`r#next`) are the Rust answer and are
   supported.
4. **Error recovery inside long bodies** relies on `,` as a synchronization point; a missing `)` in an atom can
   swallow the rest of the body before the indentation-aware matcher catches it.

### 5.4 Weaknesses in the example corpus

- **E1** orders same-tick puts by `(session, id)`. That is deterministic and linearizable, but not fair: a client
  whose session id sorts high always wins ties. A real store would order by arrival or by a client timestamp,
  which needs `choose_least!` over a tie-free `(ts, session)`.
- **E2** keeps `delivered` volatile, so a receiver redelivers after a restart. Making it `#[durable]` is one
  attribute, at the cost of a WAL write per message.
- **E3** is the election half of Raft. The per-term `votes_got` cells are never reclaimed, and `log` is written only
  by the replication half, so a standalone build gets ANA-008's "never written" warning.
- **E5**'s cart replies only from the replica that holds the client's session (`via == self`), because a
  `Session` is bound to one node. Replies from other replicas would be dropped and counted.
- **E6** sends each seal once and relies on `#[fault(reliable)]`. Under a lossy fault model the seal must be
  resent on a timer, which the text describes but the example does not show.

### 5.5 Alternatives considered

- **`:-` or Bloom's `<=`/`<+`/`<~` as the arrow.** `:-` reads as Prolog to a Rust programmer; Bloom's operators put
  the temporal kind in the middle of the line. A leading keyword makes the kind the first token (LL(1), and
  scannable), and keeps a single arrow.
- **`@next` / `@async` suffixes (Dedalus).** Rejected for the same reason: the kind is the most important fact
  about a rule and belongs at the start.
- **Relations declared with parentheses** (`table link(src: Node, dst: Node)`). Rejected in favour of struct
  bodies, which carry per-field attributes (`#[key]`, `#[tag(n)]`, `#[since(v)]`) naturally and match `struct`
  items, so a relation's row type is a struct. The tuple-struct form (`scratch edge(u32, u32);`) remains for
  small positional helpers.
- **Method-chain rule bodies** (`store.join(put).filter(..)`, Hydro/Bloom style). Rejected: chains hide join
  structure and binding, and closures would reintroduce opaque code into rule bodies (LANG-002).
