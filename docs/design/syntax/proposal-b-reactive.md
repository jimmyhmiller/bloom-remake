# Blossom surface syntax, proposal B: reactive and choreographic blocks

Status: design proposal (angle B of the syntax bake-off). Normative inputs: `docs/DECISIONS.md` and
`docs/research/FEATURES.md` (CR-xx in §1 are binding). Every construct below lowers to the Dedalus core IR
(ENG-001): deductive, inductive (`@next`) and async rules over located relations, persistence as storage,
deletion as `notin del_r`, and lattice-valued relations (Dedalus^L, CR-50).

Contents

1. Design philosophy and name-level overview
2. Lexical structure and full EBNF grammar
3. Every construct, with an example and its exact Dedalus lowering
4. Required example corpus E1–E10
5. Self-critique

---

## 1. Design philosophy

A distributed-systems engineer thinks about a protocol as *who reacts to what, and what happens next*:
"when a `put` arrives from a client, store the value and acknowledge it". P, Erlang and Hydro all organize
programs this way. Dedalus thinks about the same protocol as a set of rules whose bodies hold at a tick. The
two views are the same thing seen from different sides: a handler is a rule body, and each consequence in the
handler is one rule head. Blossom's angle-B syntax makes that correspondence the unit of the language.

- **Handlers are rule bodies with several heads.** `on put{key, val} from c { upsert kv(key, val); send
  put_ok{key} to c; }` is two Dedalus rules that share the body `put(key, val) from c`. There is no control
  flow, no mutable variable and no ordering among the statements: the statements are an unordered set of
  consequences, and `let` only names an expression.
- **The verb says the temporal edge.** Five verbs exist, one per Dedalus rule kind or Bloom operator:
  `emit` (deductive, this tick, Bloom `<=`), `next` (inductive, `@next`, `<+`), `send` (async, `<~`),
  `delete` (`<-`) and `upsert` (`<+-`). A reader never has to work out which kind a rule is.
- **Edge-triggered and level-triggered handlers look different.** `on` requires at least one *event* atom in
  its header (a message, an input, a timer, a delta). `while` has none and fires at every tick in which its
  body holds. Dedalus re-derives rules over persistent state at every tick (CR-26, ODD-05), so a `send` under
  `while` is a periodic resend. The keyword makes that visible, instead of leaving it as a surprise.
- **Always-true derived state is a `view`.** `view reachable(a, b) { edge(a, b); reachable(a, c), edge(c, b); }`
  is a closed, tick-local definition: all of its rules are in one place and nothing else writes it.
- **Choreography is first-class.** A `choreography` declares roles (`process`, `cluster`, `external`),
  typed channels between them (`channel vote(term: u64): Server -> Server`), and `at Role { ... }` sections
  that can be reopened, so a protocol reads in message order: coordinator, then participant, then
  coordinator. The compiler projects one program per role (LANG-009) and infers the default-deny channel ACLs
  from the sends (LANG-242, ODD-33).
- **Points of order stand out.** Every non-monotone operator is a reserved word from a short, closed list
  (`not`, `outer`, `inserted`, `deleted`, `reveal`, aggregates, `choose…`, `index`, `seq`, `fold_ordered`,
  `delete`, `upsert`, `else`). No operator symbol or function call is ever non-monotone. Lattices are read
  through thresholds by default; an exact read needs `reveal`. A `monotone` modifier on a view, handler or
  module turns "this must be CALM" into a compile-time check.
- **Parser friendliness.** The grammar is LL(1) except for four documented two-token decisions (§2.4), and
  expressions use a Pratt parser. Items start with a keyword, statements end with `;`, and blocks close with
  `}`, so error recovery can resynchronize on `;`, `}` and item keywords.

### 1.1 Name-level overview

| Form | Meaning | Lowers to |
|---|---|---|
| `program kv version 2;` | program identity and schema version (LANG-260) | lock-file entry |
| `module M(P: T = d) implements Proto { … }` | module with parameters (LANG-003, LANG-010) | namespaced rules |
| `protocol Proto { input …; output …; }` | abstract interface (LANG-006) | interface catalog entry |
| `choreography C { role …; channel …; at R { … } }` | multi-role module (LANG-009) | one projected module per role |
| `import M(P = v) as a;` | new instance (LANG-004) | relations renamed to `a.r` |
| `include M;` / `include "f.bls";` | flat mixin / textual include (LANG-005) | union of items |
| `interpose a.i as (outside, inside) { … }` | route an instance interface through local rules (LANG-008) | renaming |
| `table`, `scratch`, `static`, `range`, `cell`, `input`, `output`, `loopback` | collections (LANG-040..050) | persistence rules or none |
| `durable`, `soft … ttl … max …`, `sealed` | storage modifiers (LANG-044, 048, 049) | WAL flag, TTL guard, write check |
| `channel c(f: T): A -> B [options];` | async relation with one location column (LANG-042) | `@async` head relation |
| `timer t every 2s;` / `every 5 ticks` | periodic timers (LANG-172/173) | runtime input or counter rules |
| `view v(x, y = agg(z)) { alt; alt; }` | derived, tick-local relation (LANG-047/053) | deductive rules |
| `on BODY { STMTS }` / `while BODY { STMTS }` | handler blocks | one rule per statement |
| `emit`, `next`, `send … to`, `delete`, `upsert` | consequences | deductive, `@next`, `@async`, `del_r`, upsert triple |
| `seal c{k: v} to d;` / `sealed c{k: v} from m` | punctuations (LANG-207) | count-digest seal protocol |
| `bootstrap { … }`, `fact r(…);` | tick-0 rules and static facts (LANG-190) | `boot()`-guarded rules / EDB |
| `invariant i: never BODY;` | runtime violation (LANG-200) | `violation(…) :- BODY` |
| `spec S for M { … }` | LDFI and verification spec (LANG-201, TEST-020..) | spec program over traces |
| `migrate from 1 { … }`, `translate c to 1 { … }` | schema evolution (LANG-262/263) | migration program, codec rules |
| `lattice L { f: A, g: B }` / `lattice L = …;` | user lattices (LANG-135) | product constructor / alias |
| `fn`, `extern fn`, `aggregate`, `service` | pure functions, UDFs, UDAs, async services | builtins in rule bodies |

---

## 2. Lexical structure and grammar

### 2.1 Lexical structure

- **Encoding.** UTF-8 source, file extension `.bls`. Identifiers are ASCII.
- **Whitespace** separates tokens and is otherwise insignificant. There is no layout rule.
- **Comments** (LANG-208): `// …` to end of line; `/* … */`, which nests; `/// …` doc comments attached to the
  next item; and `# …` to end of line. The `#` form is a comment only when the character after `#` is not an
  ASCII digit, because `#12` is a field-number token (LANG-261). This is a one-character-lookahead lexical
  rule, and it lets Dedalus-style `#` comments and protobuf-style field numbers coexist.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`. Names that contain `__` are reserved for compiler-generated
  relations. By convention (enforced by a lint, as in Rust) relations, variables and fields are `snake_case`,
  types, roles, lattices and protocols are `UpperCamel`, and constants, parameters and spec node names are
  `SCREAMING_SNAKE`. The convention matters for one resolution rule: an identifier in a pattern position
  that resolves to a constant, parameter, enum variant, spec node name or `self` matches that value; any other
  identifier binds a variable (Rust's rule for patterns).
- **Hard keywords** (never identifiers):
  `program module protocol choreography implements extends import include as interpose override role at
  spec on while emit next send delete upsert seal sealed if else let for in not outer inserted deleted any
  exists ever view table scratch static range cell input output loopback channel timer durable soft final
  monotone fn extern type lattice aggregate const param fact bootstrap invariant never migrate translate
  resolve reveal ring choose nondet trusted match self true false`. (`choose` is hard because it also starts
  the FD clause `choose (x) -> (y)`; in a head it is still written like an aggregate call, `choose(y)`.)
- **Contextual keywords** (keywords only where the grammar expects them, identifiers elsewhere):
  `version to from principal every times once after ticks ttl max ranged like reliable ordered lossy fair accept
  external cluster process carries via exactly_once default over per by materialized recomputed readonly
  atomic since reserved deprecated semantics_changed old nodes faults eot eff crashes pre post
  liveness eventually within check expect holds fails unsafe_ungated morphism antitone service of
  progress upto mode props assign sticky rand least most partition localize tick scale_by release zset bag weight group unsafe`.
  Aggregate names (`count`, `sum`, `min`, `max`, `avg`, `collect`, `collect_set`, `argmin`, `argmax`,
  `bool_and`, `bool_or`, `percentile`, `top`, `bottom`, `choose_least`, `choose_most`,
  `choose_rand`, `index`, `seq`, `fold_ordered`, `ola_sum`, `ola_count`, `ola_avg` and user aggregates) are
  resolved names, not keywords: a call to one is legal only in a head-aggregate position (§3.7). Scalar
  minimum and maximum are `least(a, b)` and `greatest(a, b)`, so `min` never means two things.
- **Literals.**
  - Integers: `42`, `1_000`, `0x2a`, `0b1010`, with optional type suffix `42u32`, `7i64`. Modular IDs for
    Chord (LANG-026): `0x…I` with a suffix giving the width, e.g. `0x3fI160`.
  - Floats: `1.5`, `2e-3`, `1.5f64`.
  - Strings `"…"` with Rust escapes; byte strings `b"…"`.
  - Durations: an integer or decimal immediately followed by a unit, `150ms`, `2s`, `1.5s`, `5m`, `1h`,
    `10us`, `3ns`. Logical tick counts are written `5 ticks` (the contextual keyword follows an expression).
  - `true`, `false`, `None`, `Some(e)`, unit `()`.
  - Collection literals: list `[a, b]`, set `{a, b}`, map `{k: v, k2: v2}`, tuple `(a, b)`. When the
    expected type is a lattice, a literal is lifted into it: `{v}` is an `lset` singleton, `{n: 3}` an `lmap`,
    `3` an `lmax`/`lmin` value (LANG-123).
- **Punctuation and operators:** `( ) [ ] { } , ; : :: . .. ..= -> => = == != < <= > >= + - * / % ** & | ^ ~
  << >> && || ! ? @ ++`. `->` separates key columns from value columns and source role from destination role.
  `::` separates enum type and variant. `..`/`..=` build ranges.
- **Token `#n`** (digits after `#`): an explicit stable field number.

### 2.2 Operator precedence (Pratt table, loosest first)

| Level | Operators | Associativity |
|---|---|---|
| 1 | `? :` (ternary) | right |
| 2 | `\|\|` | left |
| 3 | `&&` | left |
| 4 | `== != < <= > >= in` | non-associative |
| 4b | `.. ..=` (ranges, so `x in lo..hi` needs no parentheses) | non-associative |
| 5 | `\|` | left |
| 6 | `^` | left |
| 7 | `&` | left |
| 8 | `<< >>` | left |
| 9 | `+ - ++` (`++` is list/string concatenation) | left |
| 10 | `* / %` | left |
| 11 | `**` | right |
| 12 | prefix `- ! ~` | — |
| 13 | postfix call `f(…)`, method `.m(…)`, field `.f`, index `[e]`, slice `[a..b]` | left |

This is the standard C/Rust ordering (LANG-084). Molly's right-nested arithmetic is not reproduced; the Molly
frontend (LANG-220) keeps its own parser.

### 2.3 Full EBNF

Notation: `{ x }` is zero or more, `[ x ]` is optional, `( a | b )` groups alternatives, `"kw"` is a terminal.
`Ident`, `IntLit`, `StringLit`, `Literal` and `FieldNo` are tokens from §2.1.

```ebnf
(* ---------------- files and top-level items ---------------- *)
File          = [ ProgramHdr ] { Item } EOF ;
ProgramHdr    = "program" Ident "version" IntLit ";" ;
Item          = ModuleDecl | ProtocolDecl | ChoreoDecl | SpecDecl | ModItem ;
              (* ModItems at file level belong to the implicit root module named by `program` *)

ModuleDecl    = { ModuleMod } "module" Ident [ ParamList ] [ "implements" PathList ]
                "{" { ModItem } "}" ;
ModuleMod     = "monotone" | "trusted" StringLit ;
ProtocolDecl  = "protocol" Ident [ ParamList ] [ "extends" PathList ] "{" { ProtoItem } "}" ;
ProtoItem     = RelDecl | ChannelDecl | TypeDecl | ConstDecl | ParamDecl ;
                (* only input/output RelDecls and channels are legal in a protocol *)
ChoreoDecl    = { ModuleMod } "choreography" Ident [ ParamList ] [ "implements" PathList ]
                "{" { ChoreoItem } "}" ;
ChoreoItem    = RoleDecl | AtSection | ModItem ;
RoleDecl      = "role" Ident [ ":" ( "process" | "cluster" | "external" ) ] ";" ;
AtSection     = "at" Ident ( "{" { ModItem } "}" | ModItem ) ;

ParamList     = "(" [ Param { "," Param } ] ")" ;
Param         = Ident ":" Type [ "=" Expr ] ;
PathList      = Path { "," Path } ;
Path          = Ident { "." Ident } ;

ModItem       = Import | Include | Interpose | TypeDecl | LatticeDecl | FnDecl | AggDecl
              | ServiceDecl | ConstDecl | ParamDecl | RelDecl | CellDecl | ChannelDecl
              | TimerDecl | SnapshotDecl | FactDecl | ViewDecl | Handler | Bootstrap
              | Invariant | Migrate | Translate | Labeled | GroupDecl
              | "unsafe" StringLit ModItem ;                   (* e.g. an item using DomPair *)
GroupDecl     = "group" Ident "=" "{" { Ident [ "(" Ident { "," Ident } ")" ] "=" Expr ";" } "}" ";" ;
Labeled       = [ "override" ] Ident ":" ( Handler | Bootstrap ) ;          (* LL(2): Ident ":" *)

(* ---------------- modules ---------------- *)
Import        = "import" Path [ ArgList ] "as" Ident [ ":" Path ] ";" ;
ArgList       = "(" [ Ident "=" Expr { "," Ident "=" Expr } ] ")" ;
Include       = "include" ( Path | StringLit ) ";" ;
Interpose     = "interpose" Path "as" "(" Ident "," Ident ")" "{" { ModItem } "}" ;

(* ---------------- types, lattices, functions ---------------- *)
TypeDecl      = "type" Ident [ Generics ] "=" ( Type | EnumBody | RecordBody ) ";"
              | "extern" "type" Ident "=" StringLit ";" ;
EnumBody      = "enum" "{" Variant { "," Variant } [ "," ] "}" ;
Variant       = [ FieldNo ] Ident [ "(" Type { "," Type } ")" | RecordBody ] ;
RecordBody    = "{" Field { "," Field } [ "," ] "}" ;
Field         = [ FieldNo ] Ident ":" Type [ "=" Expr ] { ColAttr } ;
Generics      = "<" Ident { "," Ident } ">" ;
Type          = Path [ "<" Type { "," Type } ">" ] | "(" [ Type { "," Type } ] ")" ;
LatticeDecl   = "lattice" Ident [ Generics ]
                ( "=" Type ";"                                   (* alias of a lattice type *)
                | RecordBody                                     (* product: verified constructor *)
                | "=" "extern" StringLit ";" ) ;                 (* Rust Merge impl, law-tested *)
FnDecl        = [ FnClass ] "fn" Ident "(" [ Param { "," Param } ] ")" "->" Type "=" Expr ";"
              | "extern" "fn" Ident "(" [ Param { "," Param } ] ")" "->" Type { FnProp }
                "=" StringLit ";"
              | "extern" "table" "fn" Ident "(" [ Param { "," Param } ] ")" "->" "(" Col { "," Col } ")"
                "=" StringLit ";" ;
FnClass       = "monotone" | "morphism" | "antitone" ;
FnProp        = "pure" | "monotone" | "morphism" | "antitone" | "injective" | "commutative"
              | "associative" | "idempotent" ;
AggDecl       = "aggregate" Ident "(" Param { "," Param } ")" "->" Type
                "{" { Ident [ "(" Ident { "," Ident } ")" ] "=" Expr ";" }
                [ "props" FnProp { "," FnProp } ";" ] "}" ;      (* init, step, merge, finish *)
ServiceDecl   = "service" Ident "(" [ Param { "," Param } ] ")" "->" "(" Col { "," Col } ")"
                "=" StringLit ";" ;
ConstDecl     = "const" Ident ":" Type "=" Expr ";" ;
ParamDecl     = "param" Ident ":" Type [ "=" Expr ] ";" ;

(* ---------------- collections ---------------- *)
RelDecl       = { RelMod } RelKind [ "final" ] Ident Schema { RelOpt } ";" ;
RelMod        = "durable" | "soft" | "sealed" | "materialized" | "recomputed" | "readonly"
              | "atomic" | "zset" | "bag" | "nondet" StringLit ;
RelKind       = "table" | "scratch" | "static" | "range" | "input" | "output" | "loopback" ;
Schema        = "(" [ Col { "," Col } ] ")" [ "->" "(" Col { "," Col } ")" ]
              | "like" Path ;                                      (* LANG-020 schema reuse *)
Col           = [ FieldNo ] [ "@" ] [ "ranged" ] Ident ":" Type [ "=" Expr ] { ColAttr } ;
ColAttr       = "since" IntLit | "deprecated" "since" IntLit | "semantics_changed" "since" IntLit ;
RelOpt        = "ttl" Expr | "max" Expr | "resolve" Policy
              | "partition" "by" Expr "over" Ident | "reserved" FieldNo { "," FieldNo } ;
Policy        = "choose" [ "sticky" ] | "choose_rand" [ "sticky" ]
              | ( "choose_least" | "choose_most" ) "(" Expr ")" ;
CellDecl      = { RelMod } "cell" Ident ":" Type [ "=" Expr ] ";" ;
ChannelDecl   = "channel" Ident Schema [ ":" Ident "->" Ident ] { ChanOpt } ";" ;
ChanOpt       = "reliable" | "ordered" | "lossy" | "fair"
              | "sealed" "by" "(" Ident { "," Ident } ")"
              | "carries" Type "via" "exactly_once" "(" Ident ")"
              | "accept" "from" Acl { "|" Acl } | "since" IntLit ;
Acl           = Ident | "external" | "principal" "in" Path ;
TimerDecl     = "timer" Ident ( "every" Expr [ "ticks" ] [ "times" Expr ]
                              | "once" "after" Expr [ "ticks" ] ) ";" ;
SnapshotDecl  = "snapshot" Ident "of" Path "at" "progress"
                ( "every" Expr "upto" Expr | "(" Expr { "," Expr } ")" ) [ "mode" Ident ] ";" ;
FactDecl      = "fact" Head [ "at" Expr ] ";" ;

(* ---------------- rules ---------------- *)
ViewDecl      = { ViewMod } "view" Ident "(" [ HeadCol { "," HeadCol } ] ")"
                ( "=" Body ";" | "{" { Body ";" } "}" ) ;
ViewMod       = "monotone" | "materialized" | "recomputed" | "nondet" StringLit ;
HeadCol       = Ident [ ":" Type ] [ "=" Expr [ AggTail ] ] ;
Handler       = { ( "monotone" | "nondet" StringLit ) } [ "localize" ] ( "on" | "while" ) Body Block ;
Bootstrap     = "bootstrap" Block ;
Invariant     = "invariant" Ident ":" "never" Body ";" ;
Block         = "{" { Stmt } "}" ;
Stmt          = "emit"   Head [ "weight" Expr ] ";"
              | "next"   Head [ "weight" Expr ] ";"
              | "send"   Head [ "to" Expr ] ";"
              | "delete" Head ";"
              | "upsert" Head [ "resolve" Policy ] ";"
              | "seal"   Path "{" FieldPat { "," FieldPat } "}" "to" Expr ";"
              | "let"    Expr "=" Expr ";"                          (* lhs is a pattern *)
              | "if"     Body Block [ "else" Block ]
              | "for"    Body Block ;
Head          = Path [ "(" [ HeadArg { "," HeadArg } ] ")"
                     | "{" [ HeadField { "," HeadField } ] "}" ] ;
HeadArg       = [ "@" ] [ Ident "=" ] Expr [ AggTail ] ;           (* LL(2): Ident "=" *)
HeadField     = Ident [ ":" Expr [ AggTail ] ] ;                    (* `f` alone puns variable f *)
AggTail       = [ "per" "(" Ident { "," Ident } ")" ] [ "by" "(" Expr { "," Expr } ")" ]
                [ "default" Expr [ "over" Atom ] ] [ "scale_by" Ident [ "(" Expr ")" ] ] [ "release" ] ;

Body          = Clause { "," Clause } ;
Clause        = "not" Clause
              | ( "inserted" | "deleted" | "outer" | "exists" | "ever" ) Atom
              | "sealed" Atom
              | "final" [ "not" ] Atom                        (* LANG-212 finality test *)
              | "at" Ident Ident ":" Atom                      (* only under `localize` (LANG-095) *)
              | "let" Expr "=" Expr
              | "any" "{" Body ";" { Body ";" } "}"
              | "choose" "(" [ Ident { "," Ident } ] ")" "->" "(" Ident { "," Ident } ")"
                [ "sticky" | "rand" | "least" Expr | "most" Expr ]
              | Expr { AtomSuffix } ;            (* classified after parsing: atom, generator, filter *)
Atom          = Path [ "(" [ ArgPat { "," ArgPat } ] ")" | "{" [ FieldPat { "," FieldPat } ] "}" ]
                { AtomSuffix } ;
ArgPat        = [ "@" ] Expr ;                   (* Expr includes `_` in pattern positions *)
FieldPat      = Ident [ ":" Expr ] ;
AtomSuffix    = "from" Expr | "principal" Expr | "at" Expr | "weight" Expr ;

(* ---------------- expressions (Pratt, table in 2.2) ---------------- *)
Expr          = Prefix { Infix } ;
Prefix        = Literal | "_" | "self" | Path [ "::" Ident ]
              | "(" [ Expr { "," Expr } [ "," ] ] ")"
              | "[" [ Expr { "," Expr } ] "]"
              | "{" [ Elem { "," Elem } ] "}"                      (* set or map literal *)
              | ( "-" | "!" | "~" ) Expr
              | "match" Expr "{" Expr "=>" Expr { "," Expr "=>" Expr } [ "," ] "}"
              | "reveal" "(" Expr ")"
              | "choose" [ "sticky" ] "(" Expr ")"                 (* head aggregate only *)
              | "final" "(" Expr ")"                                (* when_final, LANG-212 *)
              | "ring" ( "(" | "[" ) Expr "," Expr ( ")" | "]" )   (* LANG-026 ring interval *)
              | "nondet" StringLit "(" Expr ")" ;
Elem          = Expr [ ":" Expr ] ;
Infix         = BinOp Expr | "?" Expr ":" Expr
              | "(" [ CallArg { "," CallArg } ] ")"                (* call *)
              | "." Ident                                          (* field or method *)
              | "[" Expr [ ( ".." | "..=" ) Expr ] "]"             (* index, lookup, range scan *)
              | "{" [ FieldPat { "," FieldPat } ] "}" ;            (* record literal / named atom; LL(2) *)
CallArg       = "*" | Expr [ "by" Expr ] ;                          (* count(*), argmin(c by d) *)
BinOp         = "||" | "&&" | "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "|" | "^" | "&"
              | "<<" | ">>" | "+" | "-" | "++" | "*" | "/" | "%" | "**" | ".." | "..=" ;

(* ---------------- specs ---------------- *)
SpecDecl      = "spec" Ident "for" Path [ ArgList ] "{" { SpecItem } "}" ;
SpecItem      = "nodes" Ident { "," Ident } ";"
              | "assign" Ident "=" "{" Ident { "," Ident } "}" ";"
              | FactDecl | "input" Head "at" Expr ";"
              | "faults" "{" { ( "eot" | "eff" | "crashes" ) Expr ";" } "}"
              | ( "pre" | "post" ) "(" [ HeadCol { "," HeadCol } ] ")" "=" Body ";"
              | ViewDecl | ConstDecl | Invariant
              | "liveness" Ident ":" "eventually" Body "within" Expr "ticks" "after" ( "eff" | Expr ) ";"
              | "check" Ident [ "{" { Ident Expr ";" } "}" ] [ "expect" ( "holds" | "fails" ) ] ";" ;

(* ---------------- evolution ---------------- *)
Migrate       = "migrate" "from" IntLit "{" { ViewDecl | Handler } "}" ;
Translate     = "translate" Ident ( "to" | "from" ) IntLit "{" { Handler } "}" ;
```

### 2.4 Where the grammar is not LL(1), and how recovery works

1. **`Ident ":"` at item level** starts a labelled handler (`retransmit: on …`). Every other item starts with
   a keyword, so two tokens decide it.
2. **`Path "{"`** is either a named-field atom or record literal (`put{key, val}`, `Cart{ops: …}`) or the end
   of a handler header followed by its block (`on retry { … }`). The parser looks at the token after `{`:
   a statement keyword (`emit next send delete upsert seal let if for`) or `}` means a block; an identifier
   followed by `:`, `,` or `}` means fields. Statement keywords are hard keywords and cannot be field names,
   so this is decided with two tokens. `retry {}` is read as an empty block; the zero-field atom is spelled
   `retry`. Rust faces the same problem with struct literals in `if` headers and forbids them; we can accept
   them because our statement vocabulary is a closed keyword set. The same test applies wherever an expression
   can be followed by `{`: the postfix `{` is consumed only when a field list follows. In
   `match old { Some(v) => … }`, `Some (` is not a field list, so the brace belongs to `match`. `X{}` is never
   a record literal; the all-⊥ lattice value is written `bot()`.
3. **`Ident "="` inside a head argument list** names an aggregate or computed column (`emit t(k, n = count(x))`).
   Expressions never contain `=`, so two tokens decide it.
4. **Clause classification is semantic, not syntactic.** A clause is parsed as an expression plus suffixes.
   The resolver then classifies it: a call or named-field expression whose head resolves to a relation is an
   atom; `x in e` where `x` is not yet bound is a generator; anything else must have type `bool` and is a
   filter. The resolver also splits a dotted `Path` into an instance path and field accesses
   (`data.deliver(…)` versus `op.item`). Keeping this out of the grammar makes the parser context-free, and
   the resolver's error ("`kv` is a relation, but it is used as a function") is more useful than a parse error.
5. **`final`** followed by `(` is the `when_final` expression; followed by an identifier or `not` it is the
   finality clause. Two tokens decide it.

Recovery: on an error inside a block the parser skips to the next `;` or `}` at the same nesting depth; inside
an item it skips to the next hard keyword that can start an item (`on`, `while`, `view`, `table`, …) at
nesting depth 0. Because statements and alternatives are `;`-terminated and all blocks are brace-delimited,
one bad statement never swallows the rest of a module.

---

## 3. Constructs and their lowering to Dedalus

### 3.0 IR notation used in this section

All lowerings target the Dedalus IR of ENG-001, printed in Dedalus/Molly style:

```
h(N, x̄) :- b1(N, ȳ1), …, notin c(N, z̄), X := e, e1 < e2.   % deductive: same node N, same tick
h(N, x̄)@next :- ….                                          % inductive: node N, tick t+1
h(D, x̄)@async :- …, D := e.                                 % async: delivered to D as h(D, x̄, N)
h(N, ḡ, count<Y>) :- ….                                     % aggregate head, GROUP BY ḡ
h(N, k̄, V) :- ….   (V lattice-typed)                        % cells merge by ⊔ under the key FD (CR-50)
```

- `N` is the local node. Every local atom has `N` as its first column. That is how body locality (LANG-151)
  is enforced: a rule has exactly one location variable, shared by all its body atoms.
- An async head's first column is the destination. At delivery the runtime appends the sender column
  (SEM-091). The receiver's rules see `h(N, x̄, S)`; if no rule reads `S`, it is projected away.
- `[lbl]` before a rule is its stable label (LANG-068). Seeded choices use it as their site id (SEM-084).
- Names containing `__` are compiler-generated. Instance paths prefix relation names (`rb.outbox`); the
  examples below omit the prefix for the root module.
- `now(N, T)`, `boot(N)` and `tick(N, K)` are runtime input relations. `boot` holds only at tick 0 of an
  incarnation. `now` and `tick` are singletons sampled once per tick (CR-18).
- Every table `r` has the **frame rule** `r(N, x̄)@next :- r(N, x̄), notin r__del(N, x̄).` Every lattice
  table has the identity frame rule `r(N, k̄, V)@next :- r(N, k̄, V).` (SEM-104). Persistence is recognized
  and compiled to storage (ENG-003); the rules state the meaning.

### 3.1 Programs, modules, constants (LANG-001..011)

**Unordered items (LANG-001).** Items in a module and statements in a block form sets. Reordering them never
changes the IR, apart from the ordinals in auto-generated labels (so blocks that contain seeded choice should
be labelled; see §3.4).

**Program header (LANG-260).**
```blossom
program kvstore version 2;
```
This records the name and version in `schema.lock`. It produces no rules.

**Constants and parameters (LANG-010).**
```blossom
const MAJORITY: u32 = 2;                 // compile-time constant, folded into every rule
param RETRY: Duration = 2s;              // deploy-time: `blossom run --param RETRY=500ms`
module ReliableBroadcast(RETRY: Duration = 2s) { … }   // module parameter, bound at import
```
Lowering: `const` is folded at compile time. `param` and module parameters stay symbolic in the IR as
`param(RETRY)` terms until link time. The deployment manifest binds them and records the binding for replay.
A parameter can appear anywhere an expression can; a parameter whose type is a protocol (`D: Delivery`) can
only be imported (§3.9).

### 3.2 Types, values and schemas (LANG-020..028)

```blossom
type Entry    = { term: u64, cmd: bytes };                  // record, structural equality
type Decision = enum { Commit, Abort, unknown };            // `unknown` fallback: required once sent/stored
type Hint     = Option<Node>;
extern type Sketch = "tide::HyperLogLog";                   // opaque host value (LANG-027)

table log(idx: u64) -> (term: u64, cmd: bytes);             // key (idx), values (term, cmd)
table member(n: Node);                                       // no `->`: every column is a key
table leader() -> (n: Node);                                 // empty key: a register (singleton)
table buf like pipe_in;                                      // schema reuse (LANG-020)
durable table kv(#1 key: string) -> (#2 val: bytes, #3 ver: u64 = 0 since 2);   // LANG-261
table chunk(id: u64) -> (data: blob);                        // blob column: handle to out-of-line bytes
```

Lowering. Each declaration becomes an IR relation whose first column is the location:
`log(N: Node, idx: u64 | term: u64, cmd: bytes)`, with key `{N, idx}`. The key is what SEM-050 checks. Field
numbers and `since` go into `schema.lock` and the wire codec; they do not affect rules. Types are inferred
inside rules by unification (LANG-021). Every type has the canonical total order of LANG-024, so `<` works on
records, tuples and enums. `Option<T>` replaces null (LANG-025). `u160` together with `x in ring(a, b]`
gives Chord's modular IDs and wrap-around intervals (LANG-026); `ring(a, b]` lowers to the builtin
`ring_in(X, A, B, open, closed)`.

### 3.3 Collections (LANG-040..054)

| Declaration | Example | IR |
|---|---|---|
| `table` (LANG-040) | `table seen(o: Node, id: u64);` | frame rule `seen(N,O,I)@next :- seen(N,O,I), notin seen__del(N,O,I).` |
| `scratch` (LANG-041) | `scratch due(id: u64);` | no frame rule: empty at every tick start |
| `channel` (LANG-042) | `channel ack(id: u64): Node -> Node;` | async-only relation `ack(D, I)`; receiver sees `ack(N, I, S)` |
| `input` / `output` (LANG-043) | `input put(k: string) -> (v: bytes);` | tick-local interface relation, catalog direction `in`/`out` |
| `durable table` (LANG-044) | `durable table vote(t: u64) -> (c: Node);` | table frame rule, catalog flag `durable`: WAL + barrier (SEM-072) |
| `static` (LANG-045) | `static peer(n: Node);` | EDB relation holding at every tick; filled by `fact` or deploy config |
| `loopback` (LANG-046) | `loopback wake(k: u32);` | channel whose destination is `N`: `wake(N, K)@async :- …` |
| `soft table` (LANG-048) | `soft table heard(n: Node) ttl 3s max 1024;` | TTL expansion below |
| `sealed table` (LANG-049) | `sealed table conf(k: string) -> (v: string);` | table + write check below |
| `range` (LANG-050) | `range acked(src: Node, ranged seq: u64);` | table (all-key); `seq` stored as disjoint intervals, never reclaimed |
| `cell` (Bloom^L identifier, LANG-120) | `cell clock: lmap<Node, lmax<u64>>;` | 0-key lattice relation `clock(N, V)`, identity frame |
| `materialized view` / `recomputed view` (LANG-053) | `recomputed view hot(k) = …;` | same rules; physical hint only |

**Built-in collections (LANG-051/052).** `stdio.line(text)` is an input holding the lines read from stdin since
the last tick; `emit stdio.out(text);` writes at end of tick, in canonical order (LANG-118). File sources are
table functions: `extern table fn file_lines(path: string) -> (lineno: u64, text: string)`. `readonly` marks a
host-maintained relation that rules may not write. `emit halt();` stops the node at the end of the tick.
`emit localtick();` requests another tick (LANG-046); it lowers to `localtick(N)@async :- …` into the node's
own loopback, so the extra tick is observable exactly as a self-message is (SEM-041).

**Soft state (LANG-048, CR-17, SEM-060).** `soft table heard(n: Node) ttl 3s max 1024;` with writer
`on hb from n { emit heard(n); }` (the channel is `hb(sent_at: Timestamp)`; a bare relation name is an
atom with every column a wildcard) lowers to:

```
heard(N, X)        :- heard__s(N, X, _).                        % visible contents
heard(N, X)        :- hb(N, _, S), X := S.                         % [writer] emit: visible now …
heard__ref(N, X)   :- hb(N, _, S), X := S.                         % … and a refresh request
heard__s(N, X, T)@next :- heard__ref(N, X), now(N, T).            % (re)birth at this tick's now
heard__s(N, X, B)@next :- heard__s(N, X, B), notin heard__ref(N, X), notin heard__del(N, X),
                          now(N, T), T - B < 3s, heard__keep(N, X).
heard__rank(N, X, index<> by (-B, X)) :- heard__s(N, X, B).       % newest first, canonical tie-break
heard__keep(N, X)  :- heard__rank(N, X, R), R < 1024.             % max size: evict the oldest
```

Re-deriving a tuple resets its birth and is not an insertion (SEM-060). A soft head derived from a soft body
inherits the refresh through `heard__ref` (SEM-061). ANA-006 checks that a head's TTL is at least that of every
soft body atom.

**Sealed collections (LANG-049).** `sealed table conf(…)` accepts writes only from `bootstrap` and `fact`:

```
conf__w(N, K, V) :- <any non-bootstrap write body>.               % one per writing rule
violation("sealed write: conf", K) :- conf__w(N, K, _).           % LANG-200, a hard error
conf__sealed(N)@next :- boot(N).  conf__sealed(N)@next :- conf__sealed(N).   % whole-relation seal after tick 0
```
The analyzer treats `conf` as CLOSED from tick 1 on (ANA-121).

**Durability (LANG-044).** `durable` adds no rules. It sets the catalog flag that makes the runtime log the
relation's deltas and commit them at the end of the tick before the tick's outbox is released (SEM-002 step 4,
SEM-072). `next` and `upsert` into a durable table at tick t are therefore on disk before any message derived
at t leaves. Two consequences follow. Raft's "persist before replying" needs no extra delay tick (E3), and
2PC's "log, then send" is one handler (E4).

### 3.4 Handlers and consequences (LANG-060..068)

A handler is `on BODY { STATEMENTS }` or `while BODY { STATEMENTS }`, optionally labelled. Each rule-producing
statement becomes one Dedalus rule whose body is the handler's body plus every enclosing `if`/`for` body and
every `let` in scope.

```blossom
store: on put{key, val} from c {
  upsert kv(key, val);
  send put_ok{key} to c;
}
```
```
[store.1] kv__up(N, K, V) :- put(N, K, V, C).
          kv__del(N, K, W) :- kv__up(N, K, _), kv(N, K, W).        % generated once per upserted relation
          kv(N, K, V)@next :- kv__up(N, K, V).
[store.2] put_ok(D, K)@async :- put(N, K, V, C), D := C.
```

The two rules share the body `put(N, K, V, C)`. When a handler produces two or more rules and its body is
more than one atom, the compiler may factor the body into a provenance-transparent scratch
`store__body(N, K, V, C) :- put(N, K, V, C).` and read it from each head. That is common-subexpression
elimination. The semantics is always the inlined form, and provenance (TEST-023) collapses the factored node.

**The five verbs.**

| Statement | Bloom | IR head | Legal targets (LANG-066, checked statically) |
|---|---|---|---|
| `emit r(…)` | `<=` | `r(N, …) :- B.` | scratch, table, durable, soft, cell and lattice tables (merge now), `output`, an imported instance's `input` |
| `next r(…)` | `<+` | `r(N, …)@next :- B.` | the same targets as `emit`; lattice targets merge at t+1 |
| `send c(…) to d` | `<~` | `c(D, …)@async :- B, D := d.` | `channel` and `loopback` only |
| `delete r(…)` | `<-` | `r__del(N, …) :- B.` | table, durable, soft. Not scratch, lattice, cell or range |
| `upsert r(…)` | `<+-` | `r__up` + `r__del` + `@next` (above) | table, durable, soft with no lattice column |

Everything else is a compile error that names the verb, the target kind and the legal alternatives. That
includes `emit`/`next` into a channel, `send` into a table, any write into `static`, a timer, `readonly`, the
module's own `input`, or a view, and `delete`/`upsert` on a lattice (LANG-284). A `view` is closed: only its own
alternatives define it, so a reader finds all of its rules in one place.

**Deletion and upsert semantics** follow CR-05/06/07. `delete` removes the exact tuple at t+1 and insert wins.
The `r__up` relation carries r's key, so two different upserts to one key in one tick violate the key
constraint on `r__up`. That is exactly SEM-051, and the error names both source rules. `upsert … resolve
POLICY` and the relation-level `resolve` option replace the error with LANG-117 resolution (§3.7).

**Nesting, `let`, `if`, `for`.**

```blossom
receive: on msg{origin, id, payload} from s {
  send ack{origin, id} to s;
  let key = (origin, id);
  if not seen(origin, id) {
    emit deliver(origin, id, payload);
    next seen(origin, id);
  }
  for member(n), n != self, n != s {
    send relay{origin, id, payload} to n;
  }
}
```
```
[receive.1] ack(D, O, I)@async :- msg(N, O, I, P, S), D := S.
[receive.2] deliver(N, O, I, P) :- msg(N, O, I, P, S), Key := (O, I), notin seen(N, O, I).
[receive.3] seen(N, O, I)@next  :- msg(N, O, I, P, S), Key := (O, I), notin seen(N, O, I).
[receive.4] relay(D, O, I, P)@async :- msg(N, O, I, P, S), Key := (O, I), member(N, M), M != N, M != S, D := M.
```
(`Key` is unused here and the compiler drops it; it is kept to show that a `let` reaches every later
statement.) `if BODY` and `for BODY` both conjoin `BODY`. They differ only in intent: `for` introduces new
variables, `if` usually only filters. `if C { A } else { E }` gives `A` the conjunct `C` and `E` the conjunct
`!C`. `else` is legal only when `C` is a single boolean filter over bound variables, so its negation is a
selection. An `else` after an atom condition would be a hidden anti-join; the compiler rejects it and asks for
an explicit `if not …`.

**`let` is naming, not sequencing.** A `let` is in scope for the statements after it in the same block and in
nested blocks. It adds `X := e` to the body of each of those rules. Moving a statement above a `let` it uses
is a scope error, not a change of meaning.

**`on` versus `while` (edge- versus level-triggered).** Every relation is classified *event* or *standing*:

- events: channels, inputs, timers, loopbacks, `boot`, delta atoms (`inserted`/`deleted`), and views and
  scratches every one of whose rules has a positive event atom;
- standing: everything else (tables, statics, lattice tables, cells, and views derived from them).

The classification is the least fixpoint of "standing if some defining rule has no positive event atom".
`on BODY` requires a positive event atom in `BODY`; otherwise the error says "this handler fires at every tick
in which its body holds; write `while` if that is intended". `while BODY` with an event atom gets the lint
"use `on`". The two lower identically. The keyword exists to make Dedalus's re-derivation visible: under
`while buf(d, m) { send msg{m} to d; }` the message is re-sent at every tick while `buf` holds (ODD-05 literal
semantics), and the reader can see that it will be.

**Labels and named rules (LANG-068).** `retransmit: on retry, outbox(d, id, p) { … }` labels the handler; its
rules are `retransmit.1`, `retransmit.2`, … in depth-first statement order. Unlabelled handlers get
`<module>.h<k>` labels by position. Such labels are stable only while the handler list is unchanged, so the
compiler warns when an unlabelled handler contains a seeded choice, `rand` or `seq` (their site ids come from
the label, SEM-084). A view's rules are labelled `<view>.<alternative ordinal>`. Duplicate labels in a module are
an error. Override is described in §3.9.

**Explicit Dedalus persistence (LANG-065).** A table is sugar. The unsugared form is legal and is recognized as
persistence (ENG-003):
```blossom
scratch p(x: u64);  scratch del_p(x: u64);
while p(x), not del_p(x) { next p(x); }
```
lowers to `p(N, X)@next :- p(N, X), notin del_p(N, X).`, which the engine stores exactly as it stores a
`table`.

**Host insertion (LANG-067).** Hosts write only to `input` relations and service results, which the runtime
delivers at the next tick. No syntax can insert into the current tick from outside.

### 3.5 Time-indexed atoms and delta pseudo-relations (LANG-069..072)

```blossom
fact bcast("data") at 1;                   // an input event at tick 1 (CR-16); without `at`: static fact
```
lowers to the EDB fact `bcast(N, "data")@1` (Molly's notation, CR-13).

In `spec` bodies an atom may carry `at k` (absolute time) or `ever` (some tick). Program rules may not
(ANA-010).
```blossom
pre(x, pl) = log(@x, pl), not bcast(@x, pl) at 1, not crashed(x);
```
lowers to the trace query `pre(X, P) :- log__log(X, P, EOT), notin bcast__log(X, P, 1), notin crash(X, _).`
over the automatic trace relations `r__log(loc, …, tick)` (TEST-080).

**Deltas (LANG-071).** `inserted r(x̄)` holds for tuples present at t and absent at t−1. `deleted r(x̄)` holds
for tuples present at t−1 and absent at t.
```blossom
on inserted decision(id, d), txn(id, c, _) { send outcome{txn: id, d} to c; }
```
```
decision__prev(N, I, D)@next :- decision(N, I, D).                 % generated once per delta-read relation
[h.1] outcome(Dst, I, D)@async :- decision(N, I, D), notin decision__prev(N, I, D), txn(N, I, C, _), Dst := C.
% deleted r(x̄)  ≡  r__prev(N, x̄), notin r(N, x̄)
```
`r__prev` inherits `r`'s durability. For a durable relation, deltas after a restart are therefore relative to
the last committed tick, and recovery does not produce a burst of spurious `inserted` facts. Both delta forms
are negations and are points of order.

**Entanglement (LANG-072, P2)** would bind a tick number (`p(x) at t` in a program rule). It is outside this
proposal's P0/P1 scope. The reserved form is `at tick t` behind `--allow-entanglement`, and ANA-003 rejects it
otherwise.

### 3.6 Rule bodies (LANG-080..099)

**Atoms (LANG-080/081).** Positional atoms list every declared column: `log(i, t, _)`. Repeated variables mean
equality, `_` is a wildcard, and constants really match (`log(1, t, _)`). Named atoms list any subset of
fields and treat the rest as wildcards: `put{key}` binds `key`; `put{key: k, val: b"x"}` binds `k` and matches
a constant. A bare name `retry` is the all-wildcard atom. Arguments may be expressions over bound variables:
`log(i + 1, t, _)` is `log(N, J, T, _), J = I + 1` with `I` bound. Named field access in filters
(`a.x == b.y`) works on record-typed variables.

**Negation and anti-joins (LANG-082/083).** `not r(x, _)` is range-restricted: its variables other than `_`
must be bound by positive clauses (ANA-001). The three anti-join forms are `not r(x, y)` (whole tuple),
`not r(x, _)` (key pair) and `not any { r(x, z), z > y; }` (key pair plus predicate). `not any {…}` lowers to a
fresh `aux(N, X, Y) :- r(N, X, Z), Z > Y.` plus `notin aux(N, X, Y)`.

**Expressions (LANG-084)** follow §2.2. Every expression is pure: `now()`, `tick()`, `random()` and
`rand(k)` read the tick's sampled inputs (§3.10), and host functions must be declared `pure` (§3.11).

**Let (LANG-085).** `let d = d1 + w` lowers to `D := D1 + W`. A `let` whose pattern is refutable filters:
`let PointVal::Val(n) = e` keeps only bindings where `e` matches and binds `n`
(`Match(E, Val(Nv))`). The planner orders clauses by binding
availability. Every head variable must be bound (ANA-001).

**Joins (LANG-086).** Shared variables join. Comma is conjunction. Cartesian products need no syntax.
`exists r(x, _)` is a semi-join that binds nothing new: `r__proj(N, X) :- r(N, X, _).` and then
`r__proj(N, X)`.

**Left outer join (LANG-087).**
```blossom
on get{req, key} from c, outer kv(key, val) { send get_ok{req, val} to c; }     // val: Option<bytes>
```
```
[h.1a] get_ok(D, R, Some(V))@async :- get(N, R, K, C), kv(N, K, V), D := C.
[h.1b] get_ok(D, R, None)@async    :- get(N, R, K, C), notin kv__k(N, K), D := C.
       kv__k(N, K) :- kv(N, K, _).
```
Variables that only the `outer` atom binds get type `Option<T>`. `outer` is a point of order.

**Unnest and destructuring (LANG-088).** `x in e`, where `x` is unbound and `e` is a list, set or map
(entries are `(k, v)`), is a generator. The pattern may destructure: `(pos, w) in enumerate(words(text))`. It
lowers to the builtin generator `elem(E, X)`, whose binding pattern requires `E` bound (LANG-092).
`r(x, (a, b))` destructures a tuple column in place.

**Disjunction (LANG-089).** `any { A; B; }` inside a body duplicates the rule once per alternative; a view with
several alternatives does the same. Scalar conditional values use `c ? a : b` or `match`, which lower to
`X := ite(C, A, B)`.

**Membership (LANG-090).** `x in s` with `x` bound is membership. On a lattice `s` it is the monotone
`contains` threshold; on a list or set value it is a filter. `exists r(…)` and `not exists r(…)` (read "is
empty") compile to visible semi- and anti-joins, never to closures.

**Indexed lookup and range scans (LANG-091, LANG-280).** On a lattice relation, `votes[t]` is a lookup that
returns the cell value, or ⊥ if the cell is absent: `votes__look(N, T, V)` with ⊥ default. On a keyed
non-lattice relation, `(i, t, c) in log[lo..hi]` is a range scan: `log(N, I, T, C), Lo <= I, I < Hi`, planned as
an index range query.

**Generator relations (LANG-092).** `x in lo..hi` and `x in lo..=hi` lower to `range_gen(Lo, Hi, X)` with
`Lo` and `Hi` required bound. Library generators such as `less_than(n)` are table functions (§3.11) with
declared binding patterns.

**Order-sensitive operators and projections (LANG-093/094)** are aggregates (§3.7): `top(k, x by key)`,
`collect(x by key)`, `percentile(p, x)`. `keys(r)`, `values(r)` and `payloads(c)` are views generated on
demand. For example, `payloads(c)` is `view c__payloads(x̄) = c(x̄);` with the location and sender columns
dropped.

**Multi-location bodies (LANG-095, ODD-11).** Program rules are single-location. In a choreography, a body may
name a relation at another role only in a `localize` handler, whose `at Role r: atom` clauses name the remote
atoms:
```blossom
localize on request{k} from c, let r = Replica.by_hash(k), at Replica r: store(k, v) {
  send reply{k, v} to c;
}
```
The remote location must be bound before the remote atom (well-connectedness). The compiler applies the chain
rewrite: one `@async` hop per location change, each hop a generated channel carrying the bound variables.
It prints the rewrite and emits the ODD-11 lint. The IR stays single-location (CR-15).

### 3.7 Aggregation, choice and folds (LANG-100..118)

Aggregates appear only in heads: as a view column `name = agg(…)`, or as an argument of `emit`/`next`/`send`.
The group is every non-aggregate head term (SQL GROUP BY), plus the node and the tick, which every relation
carries implicitly. Input is the set of distinct body bindings (LANG-100). An aggregate over non-lattice data
is a negative edge (CR-09). The aggregate's name is the visible marker.

**Plain aggregates (LANG-100/101).**
```blossom
view load(w: Node, n = count(t)) = assigned(t, w);
```
```
[load.1] load(N, W, count<T>) :- assigned(N, T, W).
```
`count(*)` counts distinct bindings of all body variables. `sum`, `min`, `max` and `avg` are analogous. An
empty group produces no row (CR-08). An aggregate view with several alternatives first collects them into a
union relation `v__u` (one rule per alternative, carrying the aggregated and grouping variables) and then
aggregates `v__u` once. The input is the set of distinct bindings of the body's *named* variables; `_` is
projected away first, so two contributions that differ only under `_` count once. Bind a variable (for
example an op id) to keep them apart; the compiler warns when a `sum` or `count` body has a wildcard in a
column that is not functionally determined by the named variables.

**Defaults (LANG-106).** `default e over atom` emits one row per driving tuple. Without `over`, it applies to
the empty-key group.
```blossom
view load(w: Node, n = count(t) default 0 over worker(w)) = assigned(t, w);
view last_idx(i = max(i0) default 0) = log(i0, _, _);
```
```
load__a(N, W, count<T>) :- assigned(N, T, W).
load(N, W, C) :- load__a(N, W, C).
load(N, W, 0) :- worker(N, W), notin load__a(N, W, _).
last_idx__a(N, max<I0>) :- log(N, I0, _, _).
last_idx(N, I) :- last_idx__a(N, I).
last_idx(N, 0) :- notin last_idx__a(N, _).
```

**Collection, exemplary and statistical aggregates (LANG-102..104, 118).** `collect(x by k)` gives a list
sorted by `(k, canonical)`. `collect_set(x)` and `collect_map(k, v)` give sets and maps. `argmin(x by k)` and
`argmax(x by k)` give *every* tied exemplar, one row each:
```
view nearest(a, b = argmin(b0 by d)) = dist_to(a, b0, d);
nearest__m(N, A, min<D>) :- dist_to(N, A, B0, D).
nearest(N, A, B0) :- dist_to(N, A, B0, D), nearest__m(N, A, D).
```
`top(k, x by key)` and `bottom(k, x by key)` emit up to k rows per group. They lower to `index` (below) plus
`R < k`. `percentile(p, x)` is nearest-rank over the canonical order. `bool_and` and `bool_or` are ordinary
aggregates. All order-sensitive aggregates break ties by canonical order (LANG-093/118). ANA-038's D2 lint
reports where a tie-break actually happens.

**User-defined aggregates (LANG-105) and combiners (LANG-112).**
```blossom
aggregate sum_sq(x: i64) -> i64 {
  init = 0;
  step(acc, x) = acc + x * x;
  merge(a, b) = a + b;
  finish(acc) = acc;
  props commutative, associative;
}
```
In a head, `sum_sq(x)` lowers to the builtin aggregate `uda<sum_sq><X>`. If the commutativity and associativity
claims are *proved* (TEST-087), the engine may fold in any order and derives a partial-aggregate combiner for
partitioned channels (LANG-112). If they are only tested, or not claimed, the aggregate is evaluated as
`fold_ordered` in canonical order (LANG-110). No syntax asks for a combiner: it follows from declared and
proved properties.

**Seeded choice (LANG-108, CR-45, SEM-085).**
```blossom
view fresh_grant(t: u64, c = choose(c0)) = rv_ok(c0, t), not voted_for(t, _);
```
```
[fresh_grant.1] fresh_grant__cand(N, T, C0) :- rv_ok(N, C0, T), notin voted_for__k(N, T).
fresh_grant__pick(N, T, argmin<(P, C0)>) :- fresh_grant__cand(N, T, C0), P := prf(σc, "fresh_grant.1", T, C0).
fresh_grant(N, T, C) :- fresh_grant__pick(N, T, (_, C)).
```
The FD is (node, tick, T) → C. The seed σc is shared by all nodes (ODD-38), so nodes that see the same
candidates agree. Variants:
- `choose_least(y, cost)` and `choose_most(y, cost)` (LANG-114) order by `(cost, prf, y)`;
- `choose_rand(y)` keys the PRF with the node seed, incarnation and tick (schedule-dependent);
- `choose sticky(y)` (LANG-115) adds the carried state
  ```
  x__held(N, T, C)@next :- x(N, T, C).
  x__kept(N, T) :- x__held(N, T, C), x__cand(N, T, C).
  x(N, T, C) :- x__held(N, T, C), x__cand(N, T, C).
  x(N, T, C) :- x__pick(N, T, (_, C)), notin x__kept(N, T).
  ```
  (`x__held` is durable only when the view is declared `durable`);
- the FD clause `choose (x̄) -> (ȳ)` puts a choice goal in a body, and several may appear in one rule (LANG-116):
  `edge(a, b), choose (a) -> (b), choose (b) -> (a)` is a matching. Candidates are scanned in seeded-priority
  order and each is accepted iff it is FD-consistent with those already accepted; the IR builtin is
  `choice<[(A)->(B), (B)->(A)], site>`.

A choice site may not sit on a same-tick recursive cycle (SEM-086). The compiler labels the output's
nondeterminism class (SEM-087).

**Relation-level resolution (LANG-117, ODD-02 (c)).**
```blossom
durable table kv(key: string) -> (val: bytes, ts: u64) resolve choose_most(ts);
on put{key, val, ts} { upsert kv(key, val, ts); }
```
```
kv__cand(N, K, V, T) :- kv(N, K, V, T), notin kv__del(N, K, V, T).     % persisting tuples
kv__cand(N, K, V, T) :- kv__up(N, K, V, T).                          % every next/upsert body feeds this
kv__pick(N, K, choose_most<(V, T), T>) :- kv__cand(N, K, V, T).
kv(N, K, V, T)@next :- kv__pick(N, K, V, T).       % replaces the frame rule and the insertion rules
```
The statement form `upsert kv(…) resolve POLICY;` does the same for upsert conflicts alone (SEM-051): the
candidates are that tick's upserts to the key. If both forms are present, the statement form applies to upserts
and the relation form to everything else. A resolved relation may not be on a same-tick cycle.

**Ranking (LANG-097) and stable numbering (LANG-098).**
```blossom
view slot(cmd: bytes, r = index() per (client) by (seqno)) = request(client, seqno, cmd);
view entry_id(x: Entry, n = seq() by (x.ts)) = accepted(x);
```
`index()` is the dense 0-based rank in `(by-key, canonical)` order after the head is deduplicated. Its
reference expansion:
```
slot__lt(N, G, X, Y) :- slot__h(N, G, X), slot__h(N, G, Y), (key(Y), Y) < (key(X), X).
slot__c(N, G, X, count<Y>) :- slot__lt(N, G, X, Y).
slot(N, G, X, C) :- slot__c(N, G, X, C).
slot(N, G, X, 0) :- slot__h(N, G, X), notin slot__c(N, G, X, _).
```
(`slot__h` is the deduplicated head projection. The engine sorts instead of evaluating the quadratic
reference, and ENG-067 checks that the two agree.) `seq()` has the high-water-mark expansion:
```
entry_id__new(N, X) :- accepted(N, X), notin entry_id__num__k(N, X).
entry_id__r(N, X, R) :- <index of entry_id__new by (X.ts)>.
entry_id__cnt(N, count<X>) :- entry_id__new(N, X).
entry_id(N, X, B + R) :- entry_id__new(N, X), entry_id__r(N, X, R), entry_id__hwm(N, H), B := reveal(H).
entry_id(N, X, I) :- entry_id__num(N, X, I), accepted(N, X).
entry_id__num(N, X, I)@next :- entry_id(N, X, I).                       % numbers assigned this tick
entry_id__num(N, X, I)@next :- entry_id__num(N, X, I).                   % keep them
entry_id__num__k(N, X) :- entry_id__num(N, X, _).
entry_id__hwm(N, 0) :- boot(N).                                          % hwm: lmax<u64>, merges
entry_id__hwm(N, H + C)@next :- entry_id__hwm(N, H), entry_id__cnt(N, C).
entry_id__hwm(N, H)@next :- entry_id__hwm(N, H).                         % lattice identity frame
```
The high-water mark is an `lmax` cell, so the boot rule is a harmless merge when a durable mark is reloaded.
`seq() release` adds `accepted(N, X)` to the keep rule, so a number is dropped when its tuple leaves; it is
never reused, because the mark only grows. ANA-011 requires `durable` when the numbers escape the node.

**Ordered folds (LANG-110) and `reduce` (LANG-109).**
```blossom
fn apply(s: KvState, e: (u64, Cmd)) -> KvState = s.apply(e.1);
view applied(s = fold_ordered(apply, KvState::empty(), (i, c) by i)) = to_apply(i, c);
on to_apply(i, c), sm_state(s) { next sm_state(fold_ordered(apply, s, (i, c) by i)); }   // carried form
```
```
applied__r(N, I, C, R) :- <index of to_apply by (I)>.
applied__acc(N, 0, A0) :- to_apply(N, _, _), A0 := KvState::empty().
applied__acc(N, R + 1, A2) :- applied__acc(N, R, A), applied__r(N, I, C, R), A2 := apply(A, (I, C)).
applied__n(N, count<I, C>) :- to_apply(N, I, C).
applied(N, A) :- applied__acc(N, K, A), applied__n(N, K).
```
In the carried form, the initial accumulator `s` must be functionally determined by the group (checked
through `sm_state`'s empty key). ANA-011 lints a carried fold over a persistent input. `reduce(f, init, x)` is
`fold_ordered` by canonical order unless `f` is declared and proved commutative and associative.

**Quorum sugar (LANG-111).** `majority(s, Server)` is the monotone threshold
`ge(size(S ∩ members(Server)), floor(|Server| / 2) + 1)`. The verifier maps it to a quorum sort with the
intersection axiom (VER-008). `quorum(s, Server, k)` is the general k-of-n form.

**Estimators and progressive outputs (LANG-113, LANG-139).** `ola_sum(x)`, `ola_count(x)` and `ola_avg(p, x)`
are aggregates that return `(estimate, lo, hi)`, and `sum(x) scale_by progress` is HOP's scale-up. All of them
are typed `nondet "progressive"`. `snapshot s of counts at progress every 0.1 upto 0.9;` declares LANG-139's
snapshot relation. It lowers to `reveal` gated by a threshold on the progress lattice, plus the class label
computed by ANA-036.

**In-tick greedy choice (LANG-099)** is P2 and has no syntax here. It would be a `choose_least` inside a
recursive view, which SEM-086 currently rejects.

### 3.8 Lattices (LANG-120..142, LANG-280..284)

**Built-in lattice types (LANG-124, 130..134).** `lbool`, `lmax<T>`, `lmin<T>`, `lset<T>`, `lmap<K, L>`,
`lbag<T>`, `lpset<T>`, `Pair<A, B>`, `WithBot<L>`, `WithTop<L>`, `Conflict<T>`, `Point<T>`, `Unit`,
`VecUnion<L>`, `UnionFind<T>`, `Lex<K, L>` (a proper lexicographic pair with a chain key, LANG-131), `ldom<V, L>`
(antichain / MV-register, LANG-132), `Tomb<L, S>` (tombstone set and map union, LANG-133) and
`Causal<DotSet | DotFun<L> | DotMap<K, L>>` (LANG-134). `DomPair<A, B>` exists only after `unsafe
"reason"` (LANG-136, CR-25). The library defines `type VClock = lmap<Node, lmax<u64>>;` (LANG-130),
`type Ballot = Lex<lmax<u64>, lmax<Node>>;` and `type Lww<T> = Lex<lmax<(u64, Node)>, Point<T>>;`. Numeric
lattices have an adjoined ⊥ distinct from every value, so `size(∅) = 0` is a real value (LANG-281, ODD-50).

**Lattice columns and cells (LANG-120/121/128).**
```blossom
table votes(term: u64) -> (from: lset<Node>);        // key: the non-lattice columns
scratch heard_now(term: u64) -> (from: lset<Node>);  // tick-scoped lattice: ⊥ at every tick start
cell clock: VClock;                                  // 0-ary lattice (Bloom^L identifier)
```
A lattice column may not be a key, a join key or a group key. Two derivations with the same key merge
(CR-51); two with different non-lattice payloads under the same key are the SEM-050 error. A relation may have
several lattice columns, and then its value is their product (SEM-100).

**Writing merges (LANG-122/123).** `emit votes(t, {v});` merges now and `next votes(t, {v});` merges at t+1.
A collection becomes a lattice through this implicit fold: every derivation contributes a singleton, and the
FD merges them.
```
[h.1] votes(N, T, {V}) :- vote(N, T, true, V).          % ⊔ under key (N, T)
      votes(N, T, S)@next :- votes(N, T, S).            % identity frame: persistent lattices only grow
```
`delete`, `upsert` and `resolve` on lattice relations are compile errors (LANG-284). A lattice is reset
monotonically with `Lex<epoch, L>`: raising the epoch discards the old component.

**Reading: generator, lookup, cell (LANG-280).** `votes(t, s)` ranges over non-⊥ cells and binds `s` to the
cell value. `votes[t]` is a lookup with `t` bound; it returns ⊥ when the cell is absent. A `cell` is read by
name (`clock`), and its value is ⊥ until something is merged in.

**Operations and their classes (LANG-125/126/127).** Every lattice operation has a declared class for each
argument (R04 §2.4, normative). The syntax follows the class:

| Written as | Class | Example |
|---|---|---|
| method morphisms | M / BM | `m.at(k)`, `m.key_set()`, `s.intersect(t)`, `x + 1` on `lmax`, `a.join(b)` |
| monotone methods | Mon | `s.size()`, `m.sum_values()` (LANG-282), `m.size()` |
| thresholds (clauses) | monotone, boolean | `s.size() >= k`, `x in s`, `m.has_key(k)`, `l.is_top()`, `majority(s, R)`, an `lbool` used as a clause |
| exact reads | negative edge | `reveal(x)`; `==` and `!=` on lattice values; comparisons in the non-monotone direction; `a.le(b)` |

Comparisons are thresholds only in their monotone direction: `x >= c` and `x > c` on `lmax`, `x <= c` and
`x < c` on `lmin`, with `c` a bound scalar or constant. Any other comparison involving a lattice value is a
compile error: "this read is not monotone; wrap the value in `reveal(…)` to read it exactly (a point of
order)". An exact read is always spelled `reveal`. The polarity analysis of SEM-102 classifies each occurrence.
An exact occurrence becomes a negative edge in the same-tick graph, and only deductive rules must stratify.
```blossom
while votes(t, s), s.size() >= QUORUM { emit won(t); }     // monotone: THRESH-final
on probe(t), votes(t, s) { emit tally(t, reveal(s).len()); }   // exact: negative edge, marked
```
```
won(N, T) :- votes(N, T, S), ge(size(S), QUORUM).
tally(N, T, X) :- probe(N, T), votes(N, T, S), X := len(reveal(S)).      % edge votes → tally is exact (−)
```

**Lattice to collection (LANG-123).** `x in s` over an `lset` and `(k, v) in m` over an `lmap` are generators
(M). An `lbool` used as a clause is `when_true`. `m.at(k)` on a missing key is ⊥ of the value type (LANG-129).
`m.at(n).join(0)` gives a monotone default (LANG-283); there is no non-monotone `??` operator.

**Lattices in messages (LANG-137).** Channel columns may be lattice-typed:
`channel gossip(state: VClock): Node -> Node;`. Same-key messages merge at the sender and within one delivered
batch, never across ticks without a persistent sink (CR-52, SEM-105). The lowering is the ordinary async head;
the FD on the channel key does the merge.

**User-defined lattices (LANG-135, ODD-09 (c)).**
```blossom
lattice Interval { lo: lmin<i64>, hi: lmax<i64> }        // product of lattices: verified constructor
lattice Tally = lmap<Node, lmax<u64>>;                   // alias of a composed lattice
lattice Hll = extern "tide::lattices::Hll";              // Rust Merge impl; must pass the law harness
monotone fn width(i: Interval) -> lmax<i64> = lmax(reveal(i.hi) - reveal(i.lo));
```
A record-bodied `lattice` is the product of its fields: ⊥ is all-⊥ and merge is pointwise, so the laws hold by
construction ("proven"). `extern` lattices go through TEST-083/TEST-087 and are labelled "tested". A function
annotated `monotone`, `morphism` or `antitone` may use exact reads inside its body. The annotation is then a
proof obligation, discharged by SMT where the fragment allows (VER-014), otherwise tested, and reported with
its status. Only proved or tested claims let callers treat the function by its class. A refuted claim is an
error.

**Weighted collections, groups and wrapped channels (LANG-138, 142, 158).**
```blossom
zset table clicks(url: string);                       // ℤ weights; `bag table` for ℕ weights (insert-only)
on click{url} { emit clicks(url) weight 1; }
on unclick{url} { emit clicks(url) weight -1; }
view popular(url) = clicks(url) weight w, w >= 100;   // raw-weight read; `clicks(url)` alone is weight > 0
channel click_deltas(url: string): Node -> Node carries zset<string> via exactly_once(dots);
group Money = { zero = 0i64; plus(a, b) = a + b; neg(a) = -a; };   // LANG-142: never declarable as a lattice
```
The IR for weighted relations is the Z-set stratum of ENG-062: `clicks(N, U) : ℤ` with `+w` contributions.
The atom suffix `weight w` binds the weight, and the bare atom is the set view (weight > 0). Sending a
weighted payload through a plain channel is a compile error (ANA-015). The `exactly_once` wrapper is inserted
by the compiler (DIST-015..017) and unwrapped into the receiver's Z-set stratum (ENG-070).

### 3.9 Locations, channels, choreographies and modules (LANG-003..009, LANG-150..158)

**Location (LANG-150/151/152).** Local state lives at `self` implicitly (CR-14). The only way to put a fact at
another node is `send … to d`, whose target must be a channel. `self` has the node type of the enclosing role
(`Node` in a plain module). Body locality needs no syntax, because no body atom can name a location except
under `localize` (§3.6) and in specs.

**Channels (LANG-042, LANG-155).** Two declaration forms exist.
```blossom
channel ack(origin: Node, id: u64): Node -> Node;                 // arrow form: implicit destination column
channel pipe(@dst: Node, src: Node, id: u64) -> (payload: bytes); // column form (CR-14): one `@` column, any position
```
Arrow-form sends name the destination with `to`: `send ack{origin, id} to s;` → `ack(D, O, I)@async :- …,
D := S.` Column-form sends put `@` on the location argument: `send pipe(@d, self, i, p);`. You write the
columns you declared, and a receiver may omit the location column in the named form. The channel's key (all
columns by default, or the columns before `->`) is checked at the sender (LANG-042, SEM-050). Options state
the delivery guarantee (LANG-155) and set the receiving relation's stream properties for ANA-030 and for the
simulator:

| Option | Meaning |
|---|---|
| (none) / `fair` | TPLP fair delivery, the reasoning default (SEM-040) |
| `reliable` | reliable, unordered (fail-stop transport) |
| `reliable ordered` | reliable ordered prefix per sender |
| `lossy` | may be lost (lossy-delayed-forever) |
| `sealed by (k)` | the stream carries seals on key `k` (Blazes `Seal_k`, §3.13) |
| `carries zset<T> via exactly_once(w)` | wrapped group payload (LANG-158) |
| `accept from R \| external \| principal in rel` | explicit ACL (LANG-242), narrows the inferred one |

**Receiving and the sender (LANG-241).** `on ack{origin, id} from s` binds `s` to the runtime-populated sender
column. The implicit column is read only when a `from` names it; otherwise it is projected away (SEM-091).
`principal p` binds the authenticated principal (§3.14).

**Choreographies (LANG-009, LANG-153).**
```blossom
choreography Ping {
  role Client: external;
  role Server: cluster;
  channel ping(n: u64): Client -> Server;
  channel pong(n: u64): Server -> Client;
  at Server {
    on ping{n} from c { send pong{n} to c; }
  }
}
```
Role kinds: `process` (one node, the default), `cluster` (SPMD, size fixed at deploy time) and `external`
(clients: sessions, not nodes; no rules). A role name used as an expression is its member set: `p in Server`,
`Server.size`, `Server.by_hash(k)` (rendezvous-style deterministic partition over the canonical member order,
LANG-154), `majority(s, Server)`. Inside `at Server`, `self: Server`. `at R { … }` sections may be reopened, so
a protocol can be written in message order. Items outside every `at` section (types, constants, statics,
channels) are shared by all roles.

*Projection.* For each non-external role R, the compiler builds the module `Ping.R` from:
1. every `at R` item;
2. every shared item that the projected rules reference;
3. for each channel `c: A -> B`, the send side if R = A and the receive side if R = B;
4. the static membership relation `R'__members(N, M)` for every role R' that the projected rules mention.

The projected IR for `Ping.Server` is:
```
static Server__members(N, M).
[Ping.Server.h1.1] pong(D, Nn)@async :- ping(N, Nn, C), D := C.
acl(ping) = { role Client (external sessions) }        % inferred: the only senders of `ping` (LANG-242 P0)
```
The ACL is default-deny (ODD-33): a channel accepts frames only from the roles that have a `send` into it.
A cluster role's projection is deployed on every member. External roles produce typed client stubs, not rules.

**Modules and instances (LANG-003/004).**
```blossom
import ReliableBroadcast(RETRY = 500ms) as control;
while peer(n) { emit control.member(n); }
on command{id, payload} { emit control.bcast(id, payload); }
on control.deliver(o, id, p) { emit urgent(o, id, p); }
```
Lowering: every relation `r` of the instance becomes `control.r` in the flat IR, with parameters substituted.
Channels become `control.msg`, so two instances of one module never exchange messages: the instance path is
part of the channel's identity and of its wire schema id. From outside, only interface relations are visible.
The importer writes the instance's `input`s and reads its `output`s, and naming anything else is an error.
Reusing an alias is an error. Nested instances are `a.b.r`. Imported modules bootstrap first (LANG-190).

**Protocols and implementations (LANG-006).**
```blossom
protocol Delivery {
  input  pipe_in(dst: Node, src: Node, ident: u64) -> (payload: bytes);
  output pipe_sent(dst: Node, src: Node, ident: u64) -> (payload: bytes);
  output pipe_out(dst: Node, src: Node, ident: u64) -> (payload: bytes);
}
module ReliableDelivery(RETRY: Duration = 2s) implements Delivery { … }
module Multicast(D: Delivery) { import D as del; … }        // choose the implementation at composition
import Multicast(D = ReliableDelivery) as mc;
```
`implements P` brings P's interface declarations into the module, as Bloom's `include DeliveryProtocol` does
(E2 relies on this). A module may repeat them for readability, and a repeated declaration must match P's
exactly. A module may not add interfaces that P lacks unless it implements another protocol that has them.
A protocol-typed parameter is instantiated by `import`, which gives functor-style composition. Protocols
produce no rules; they are catalog entries.

**Include and override (LANG-005/007).** `include Base;` copies Base's items into this module flat (Bloom's
`include`). `include "util.bls";` is textual and resolves relative to the including file. A labelled handler
in the includer with the same label as an included one must be written `override label: …`. That removes
every rule `Base.label.*` and adds the new ones. A same-label handler without `override`, or an `override`
with no matching label, is an error. Bloom silently replaced blocks; here the replacement is explicit.
```blossom
module QuietBroadcast {
  include ReliableBroadcast;                 // flat copy of E2, parameters at their defaults
  table muted(n: Node);
  override retransmit: on retry, outbox(dst, id, payload), not muted(dst) {
    send msg{origin: self, id, payload} to dst;
  }
}
```
Lowering: the rule `retransmit.1` of E2 is dropped and replaced by
`[retransmit.1] msg(D, N, I, P)@async :- retry(N, _, _), outbox(N, D, I, P), notin muted(N, D).`; every other
included rule is unchanged.

**Interposition (LANG-008).**
```blossom
interpose data.bcast as (outside, inside) {
  on outside(id, p), p.len() <= MAX_PAYLOAD { emit inside(id, p); }
  on outside(id, p), p.len() > MAX_PAYLOAD { emit rejected(id); }
}
```
For an input interface `a.i`, `outside` is the relation everyone else now writes when they write `a.i`, and
`inside` is what the component actually reads. For an output interface, `inside` is what the component writes
and `outside` is what everyone else reads. The lowering is a renaming. Writers outside the interposer are
redirected to `a.i__outside`, and the interposer's rules connect `a.i__outside` to `a.i`:
```
data.bcast__outside(N, I, P) :- <every outside write of data.bcast>.
[interpose.h1.1] data.bcast(N, I, P) :- data.bcast__outside(N, I, P), len(P) <= MAX_PAYLOAD.
[interpose.h2.1] rejected(N, I) :- data.bcast__outside(N, I, P), len(P) > MAX_PAYLOAD.
```
BOOM's LATE scheduler, Paxos insertion and metering all fit this shape.

**Partitioning (LANG-154).** `table kv(key: string) -> (val: bytes) partition by hash(key) over Server;`
declares that `kv(k, _)` lives at `Server.by_hash(k)`. Writes at a non-owner are a runtime violation, and a
static error when the compiler can prove the ownership test false. `owner(k)` is available in expressions.
The partitioning rewrites of ANA-082 use the declaration.

### 3.10 Time, timers and randomness (LANG-170..175)

```blossom
timer retry every 2s;                  // physical periodic: relation retry(k: u64, at: Timestamp)
timer lease every 500ms times 20;      // stops after 20 firings
timer poll every 5 ticks;              // logical: every 5th tick
timer kick once after 300ms;           // one-shot
```
Physical timers are runtime inputs: `retry(N, K, At)` is delivered as an event at the tick in which it fires,
and under simulation the clock is virtual (ODD-16: mapped to rounds by default, overridable per module).
Logical timers lower to counter rules:
```
poll__c(N, 0) :- boot(N).
poll__c(N, C1)@next :- poll__c(N, C), C1 := (C + 1) % 5.
poll(N, K, T) :- poll__c(N, 0), tick(N, K), now(N, T).
```
A logical timer keeps the node ticking, because its counter is a staged state change (SEM-009).

`tick()` reads `tick(N, K)`, and a rule that uses it is time-dependent (LANG-170). `now()` reads the per-tick
sample `now(N, T)` (LANG-171). `random()` is `rand(())` (LANG-174). `rand(k)`, `rand_float(k)` and
`rand_range(lo, hi, k)` (LANG-175) lower to the builtin `prf(σ_node, "rand", incarnation, tick,
fingerprint(k))`, with `rand_range` unbiased. A random value that must stay fixed is captured into state with
`next`/`upsert` (E3 does this with its election deadline). ANA-011 lints an uncaptured use over persistent
input.

### 3.11 Functions, UDFs, services and host interop (LANG-180..186)

```blossom
fn majority_of(n: u32) -> u32 = n / 2 + 1;                                     // pure, total, non-recursive
extern fn sha256(b: bytes) -> bytes pure injective = "blossom_std::hash::sha256";  // LANG-181/182
extern table fn file_lines(path: string) -> (lineno: u64, text: string) = "blossom_std::io::lines";  // LANG-183
service geocode(addr: string) -> (lat: f64, lon: f64) = "app::geo";           // LANG-184
```
- Built-ins (LANG-180) cover math, strings, hashing, list/set/map (`len`, `head`, `tail`, `cons`, `contains`,
  `concat`, `enumerate`, `words`), `to_string` and id generation.
- `fn` bodies are expressions: no recursion, no relation access. They are inlined or called as builtins.
- An `extern fn` must be declared `pure`. Its properties feed Blazes FDs (`injective`), lattice
  classification (`monotone`, `morphism`) and fold legality (`commutative`, `associative`, `idempotent`), each
  with the proved, tested or refuted status of TEST-087.
- A table function is used as an atom with its inputs bound, e.g. `file_lines(path, n, t)`. It lowers to a
  builtin generator with that binding pattern.
- A `service` declares two relations: `geocode.call(id, addr)`, which rules `emit` into, and
  `geocode.result(id, lat, lon)`, an input delivered at a later tick (the Dedalus rendezvous). Its IR is
  `geocode.call` as an async head addressed to the host plus `geocode.result` as an input.
- Host API (LANG-185/186) is not surface syntax. Hosts subscribe to any `output` with full contents or deltas,
  inject through `input` (always deferred), and step ticks. `atomic output` releases the output only after the
  tick's state is visible to snapshot reads (LANG-206).

### 3.12 Bootstrap (LANG-190)

```blossom
fact peer(B);                                   // static fact: holds at every tick
bootstrap {
  emit current_term(0);
  emit deadline(now() + rand_range(ELECTION_MIN, ELECTION_MAX, 0u64));
}
```
```
peer(N, B).
[bootstrap.1] current_term(N, 0) :- boot(N).
[bootstrap.2] deadline(N, T) :- boot(N), now(N, T0), T := T0 + rand_range(…).
```
Decision: `boot(N)` holds at tick 0 of **every incarnation**, after the durable relations have been reloaded
(SEM-071). Volatile tables therefore get initialized again after a restart, and bootstrap writes into
durable state must be idempotent. Lattice merges and existence-guarded writes are; the compiler warns about
any other bootstrap write into a durable relation. `next` in bootstrap lands at tick 1, and `emit` holds at
tick 0 (LANG-190: a `<+` "in bootstrap" meaning tick 0 is spelled `emit` here). LANG-190 requires imported
instances to bootstrap before their importer. All `boot`-guarded rules run in the same tick-0 fixpoint, so
"before" means that an importer's bootstrap rules can read what an instance's bootstrap derives in that same
tick. Ordinary stratification provides this, and no finer order is observable.

### 3.13 Invariants, seals, finality, specs and annotations (LANG-200..212)

**Runtime invariants (LANG-200).**
```blossom
invariant one_vote_per_term: never voted_for(t, a), voted_for(t, b), a != b;
```
```
violation(N, "one_vote_per_term", (T, A, B)) :- voted_for(N, T, A), voted_for(N, T, B), A != B.
```
`violation` is a built-in output. The deployment chooses the action (abort the node, alert, or log with
provenance) and may ship violations to a remote checker.

**Seals and punctuations (LANG-207, ANA-041/046/065).** A channel declared `sealed by (split)` supports:
- the producer statement `seal occ{split: s} to r;`, meaning "I will send no more `occ` with `split = s` to
  `r`";
- the consumer clause `sealed occ{split: s} from m`, meaning "`m`'s partition `split = s` is complete here".

The seal carries a count digest, so it does not rely on ordered delivery (DIST-011). Producer-side lowering:
```
occ__out(N, R, W, S, L, P) :- <body of every `send occ{…} to r` rule>, R := <its destination>.
occ__out(N, R, W, S, L, P)@next :- occ__out(N, R, W, S, L, P).
occ__cnt(N, R, S, count<W, L, P>) :- occ__out(N, R, W, S, L, P).
occ__mine(N, R, S) :- <body of the `seal` statement>, R := r, S := s.
occ__mine(N, R, S)@next :- occ__mine(N, R, S).
occ__seal(R, S, C)@async :- occ__mine(N, R, S), occ__cnt(N, R, S, C).       % re-sent each tick: idempotent
occ__seal(R, S, 0)@async :- occ__mine(N, R, S), notin occ__cnt(N, R, S, _).
occ__frozen(N, R, S, C)@next :- occ__mine(N, R, S), occ__cnt(N, R, S, C).
violation(N, "send after seal: occ", (R, S)) :- occ__frozen(N, R, S, C), occ__cnt(N, R, S, C2), C2 != C.
```
Consumer-side lowering (`M` is the sender column):
```
occ__in(N, W, S, L, P, M) :- occ(N, W, S, L, P, M).
occ__in(N, W, S, L, P, M)@next :- occ__in(N, W, S, L, P, M).
occ__sm(N, S, C, M) :- occ__seal(N, S, C, M).
occ__sm(N, S, C, M)@next :- occ__sm(N, S, C, M).
occ__rc(N, S, M, count<W, L, P>) :- occ__in(N, W, S, L, P, M).
occ__sealed(N, S, M) :- occ__sm(N, S, C, M), occ__rc(N, S, M, C).
occ__sealed(N, S, M) :- occ__sm(N, S, 0, M), notin occ__rc(N, S, M, _).
```
`sealed occ{split: s} from m` reads `occ__sealed(N, S, M)`. Sealedness is monotone: once it holds it holds
forever (MAR Lemma 5), so `sealed` is a positive clause. It is still reported as a coordination point
(ANA-046). The analyzer marks partition `(S, M)` of `occ` CLOSED once it is sealed (ANA-065, ANA-121). The
logs `occ__out` and `occ__in` are Edelweiss-reclaimable after the seal (ANA-063). Whole-relation seals are
`sealed by ()`, and `seal occ{} to r;` seals everything.

**Finality (LANG-212).**
```blossom
output final word_count(word: string, n: u64);
while prepared(txn, true), final not refused(txn) { … }   // final-absent test
```
`output final r` is a compile error unless ANA-120 classifies `r` POS-, NEG-, TOP-, THRESH-, FINITE- or
SEALED-final. At runtime every emitted tuple carries `provisional`, `final_present` or `final_absent`. The
clauses `final r(x̄)` and `final not r(x̄)` lower to the builtins `final_present(r, X̄)` and `final_absent(r,
X̄)`, which ANA-121/122 maintain. Finality is monotone, so both are positive, threshold-like clauses.
`final(e)` on a lattice expression is LANG-212's `when_final`.

**Accepted nondeterminism (LANG-204).** `nondet "why"` may prefix a handler, a view, a relation or an
expression (`nondet "leader hint may be stale" (reveal(hint))`). It produces no rules; it records
`nondet(site, reason)` in the catalog. The analyzer and simulator track it (SEM-087, ANA-039). An `output
nondet "why" r(…)` passes the label up through the module interface. An importer that reads `r` inherits the
label until it writes its own `nondet`.

**Trusted modules (LANG-205).** `trusted "hand-verified Paxos core" module Paxos { … }` stops CALM reports
inside the module. VER-020 then requires the module's interface spec (a `spec … for Paxos`) to pass.

**Catalog (LANG-202).** Read-only built-in relations: `catalog.rule(label, module, kind)`,
`catalog.depends(head, body, negative, temporal)`, `catalog.stratum(rel, k)`, `catalog.column(rel, name,
type, is_key)` and `catalog.interface(rel, direction)`. Installing rules at runtime (LANG-203) is P2 and an
admin-plane operation, so it has no data-plane syntax.

**Specs (LANG-201, TEST-020..022, VER-001).** A `spec` is a separate item evaluated over a run's trace. It can
never feed protocol relations. Inside a spec:
- every program relation is addressed with an explicit location, `log(@x, pl)`;
- atoms are evaluated at an *evaluation point*: EOT for `pre` and `post`, and every reached global state for
  `invariant`;
- `at k` fixes an absolute tick, and `at t` with `t` unbound binds the tick;
- `ever r(…)` means "at some tick up to the point";
- the oracles are `crashed(n)`, `crash(n, t)` and `hb(n1, t1, n2, t2)` (CR-20, TEST-080).

```blossom
spec Sketch for SimpleDeliv {
  nodes A, B, C;
  input bcast(@A, "data") at 1;
  faults { eot 4; eff 2; crashes 1; }
  pre(x, pl)  = log(@x, pl), not crashed(x);
  post(x, pl) = log(@x, pl);
  invariant no_phantom: never log(@x, pl), not ever bcast(@_, pl);
  liveness spreads: eventually log(@B, "data") within 2 ticks after eff;
  check ldfi expect fails;
}
```
Lowering, with `P` the evaluation point and `r__log(Loc, x̄, T)` the automatic trace relations:
```
pre(X, Pl)  :- log__log(X, Pl, EOT), notin crashed__at(X, EOT).
crashed__at(X, P) :- crash(X, T), T <= P.
post(X, Pl) :- log__log(X, Pl, EOT).
violation("no_phantom", (X, Pl, P)) :- point(P), log__log(X, Pl, P), notin bcast__ever(Pl, P).
bcast__ever(Pl, P) :- bcast__log(_, Pl, T), point(P), T <= P.
violation("spreads") :- EFF + 2 <= EOT, notin spreads__ok.
spreads__ok :- log__log("B", "data", T), EFF <= T, T <= EFF + 2.
```
`faults` gives the LDFI failure spec ⟨EOT, EFF, maxCrashes⟩ (TEST-020). `nodes` names the nodes;
`assign Role = {…}` fixes role membership for choreographies. `check ldfi | bmc | smt | sim | asp {…}` selects
the tool: LDFI (TEST-020..040), bounded model checking (VER-002/003), SMT inductive invariants (VER-006..010),
simulation (TEST-001) or bounded ASP (VER-003). Options such as `ticks 20; delay 3;` set the bounds that the
result certificate states (VER-005). `expect holds | fails` makes the check a CI gate (TEST-039).

### 3.14 Principals, sessions and authorization (LANG-240..245)

```blossom
static admins(p: Principal);
channel del(req: u64, key: string): Client -> Server accept from principal in admins;
at Server {
  on del{req, key} from c principal p, kv(key, v) {
    delete kv(key, v);
    send del_ok{req, existed: true} to c;
    emit audit(p, key);
  }
  on session_closed(s, reason) { emit gone(s); }
}
```
- `Principal` and `Node` are distinct types. `principal_of(n)` and `role_of(n)` are built-ins over the
  directory relation `directory(node, address, principal, role)`. The directory is `static` under static
  membership and epoch-sealed under dynamic membership (LANG-240).
- `from c principal p` binds the two runtime-populated columns: `c` is the sender (a `Node`, or a `Session`
  for external roles) and `p` its principal (LANG-241). The IR atom is `del(N, R, K, C, P)`. Both columns are
  projected away when unread (SEM-091), and neither can be forged, because neither is part of the payload.
- ACLs (LANG-242) are ingress configuration, not rules (CR-40). The inferred ACL comes from projection (§3.9).
  `accept from …` narrows it, reading the named relation at the last committed tick. A rejected frame is an
  omission (SEM-090). ANA-105 rejects self-contradictory ACLs.
- External clients (LANG-243) are sessions. Roles that receive from `external` get the inputs
  `session_open(s: Session, p: Principal, at: Timestamp)` and `session_closed(s: Session, reason: string)`.
  `send … to s` with `s: Session` is egress-only, and a send to a closed session is dropped and counted. A
  forwarded request carries an `on_behalf_of: Principal` field, which is accepted only on channels whose ACL
  admits cluster roles alone (ANA-106).
- Data-dependent authorization (LANG-244) is ordinary rules: `view authorized(p, op, key) = …;` plus a handler
  that sends an error reply and emits `authz_denied(s, op, key, reason)`.
- `signed<T>` (LANG-245, P2) is a value type. `sign(x)` is a `service` (it uses the host keystore) and
  `verify(s): Option<(Principal, T)>` is a pure `extern fn`.

### 3.15 Program versions, schema evolution, migrations (LANG-260..265)

```blossom
program kvstore version 3;
durable table kv(#1 key: string) -> (#2 val: bytes, #3 ver: u64 = 0 since 2, #5 owner: Principal since 3)
  reserved #4;
channel put(#1 req: u64, #2 key: string, #3 val: bytes, #4 ttl: Option<Duration> = None since 3): Client -> Server;

migrate from 2 {
  while old.kv(key, val, ver) { emit kv(key, val, ver, principal_of(self)); }
}
translate put to 2 {                       // emitting to a v2 peer: drop the v3-only field
  while put(req, key, val, _) { emit old.put(req, key, val); }
}
translate put from 2 {                     // accepting from a v2 peer
  while old.put(req, key, val) { emit put(req, key, val, None); }
}
on put{req, key, val, ttl: Some(d)} from c, cluster_version.at_least(3) { … }   // gated feature (LANG-264)
```
- Field numbers `#n` are optional in source. The compiler assigns missing ones and records every number in
  `schema.lock`. `reserved` lists retired numbers. `since`, `deprecated since` and `semantics_changed since`
  are column attributes (LANG-261/265).
- A `migrate from N` block is a separate program M_N. Its handlers may use only `while` and `emit`: no
  temporal or async rules, and no `now()` or `random()`. It reads `old.r` typed by the lock's version-N
  schema and writes current durable relations. Lowering: `kv(N, K, V, W, P) :- old.kv(N, K, V, W), P :=
  principal_of(N).` It runs at recovery (DIST-082), one version step at a time. A key collision is a hard
  error naming both source tuples (LANG-262). Migrations that only add defaulted fields are synthesized and
  need no block.
- `translate c to N` and `translate c from N` contain tuple-local rules: one channel atom, pure functions, no
  state (ANA-103). They compile into the codec layer (DIST-087), not the tick. A tuple that no `to` rule
  matches is disallowed and counted as an omission (LANG-263).
- `cluster_version.at_least(v)` is a threshold clause over the built-in `lmax<u32>` input, so gates are
  monotone (SEM-092). Writing or sending a `since V` feature without a gate is ANA-102's error, unless the item
  is marked `unsafe_ungated "reason"`.

### 3.16 What makes CALM facts visible

1. **Points of order are keywords.** The complete list of non-monotone constructs is: `not`, `outer`,
   `inserted`, `deleted`, `reveal`, `else`, `delete`, `upsert`, `resolve`, every aggregate in a head (`count`,
   `sum`, `min`, `max`, …), `choose…`, `index()`, `seq()`, `fold_ordered`, `top`/`bottom`/`percentile`, and
   non-threshold comparisons on lattice values, which must be written with `reveal`. No operator symbol,
   method or `fn` call is non-monotone unless its declaration says so, and a declared non-monotone `fn` can only
   be applied to `reveal`ed values. `grep -wE 'not|outer|inserted|deleted|reveal|else|delete|upsert|resolve|choose'`
   plus the aggregate names therefore finds every candidate point of order. The LSP highlights them (TEST-092).
2. **`monotone` is a checkable promise.** `monotone view`, `monotone on …` and `monotone module` are compile
   errors if any point of order occurs inside (lattice polarity included, SEM-102). A `monotone` module whose
   channels are all guarded gets ANA-141's certificate in the build output.
3. **The verb is the temporal edge.** `emit` is a same-tick edge, `next` crosses a tick and `send` crosses an
   async edge. Temporal stratification (SEM-020) accepts a negation cycle exactly when some edge on it is a
   `next` or `send`, so the reader can check acceptance by eye.
4. **`on`/`while` shows re-derivation.** Level-triggered sends are resends, and they look like it.
5. **Lattice reads default to thresholds.** A monotone read is the easy path; an exact read has to be written
   out as `reveal`.
6. **Coordination is named.** `seal`/`sealed`, `majority`/`quorum`, `final`, `nondet` and `trusted` are the
   coordination and escape-hatch vocabulary, each tied to an analysis (ANA-046, VER-008, ANA-120, ANA-039,
   VER-020).

Lints that support this (all in `--strict` as errors, ODD-10 (c)):
- **negating your own same-tick write**: a handler that both `emit`s `r` and tests `not r` in one tick almost
  always meant `next r` (see E2's `seen`);
- **`on` without an event**, and **`while` with an event**;
- **unlabelled handlers containing a seed-dependent site**;
- **ANA-007 key conflicts** between two statements that may upsert one key in one tick.

### 3.17 Coverage of FEATURES §2 (P0 and P1)

| LANG | Construct (section) |
|---|---|
| 001 002 | unordered items, standalone grammar (§2, §3.1) |
| 003 004 | `input`/`output` interfaces, `import … as` (§3.9) |
| 005 006 007 008 | `include`, `protocol`/`implements`, `override label:`, `interpose` (§3.9) |
| 009 | `choreography`, `role`, `at`, projection (§3.9) |
| 010 | `const`, `param`, module parameters (§3.1) |
| 020..028 | schemas with `->` keys, inference, scalars, compound values, total order, `Option`, `u160` and `ring`, `extern type`, `blob` (§3.2) |
| 040..053 | `table`, `scratch`, `channel`, `input`/`output`, `durable`, `static`, `loopback`, `view` as temp, `soft`, `sealed`, `range`, `stdio`/file/`readonly`, `halt`, materialization (§3.3) |
| 060..068 | `emit`, `next`, `send`, `delete`, `upsert`, explicit persistence, legality matrix, deferred host insertion, labels (§3.4) |
| 069 070 071 | `fact … at k`, spec `at k`/`ever`, `inserted`/`deleted` (§3.5) |
| 080..098 | atoms, constants, `not`, anti-joins, expressions, `let`, joins, `outer`, unnest, `any`/`if`/`else`, `in`/`exists`, lookup and range scans, generators, order-sensitive aggregates, projections, `localize`, `index`, `seq` (§3.6, §3.7) |
| 100..118 | aggregates, `default … over`, collection/exemplary/statistical aggregates, `aggregate`, `choose` family, `reduce`, `fold_ordered`, `majority`, combiners, estimators, sticky and multi-goal choice, `resolve`, canonical order (§3.7) |
| 120..142, 280..284 | lattice types, columns, merges, conversions, classes, thresholds, `reveal`, scratch lattices, ⊥, VClock, `Lex`, `ldom`, tombstones, causal, user lattices, `unsafe` DomPair, lattices in messages, `zset`/`bag`, snapshots, `group` (§3.7, §3.8) |
| 150..158 | `self`, locality, roles and clusters, `partition by`, fault-model options, wrapped channels (§3.9, §3.8) |
| 170..175 | `tick()`, `now()`, `timer … every`/`ticks`/`once`, `random()`, `rand(k)` (§3.10) |
| 180..186 | built-ins, `fn`, `extern fn` properties, table functions, `service`, host API, output handlers (§3.11) |
| 190 | `fact`, `bootstrap` (§3.12) |
| 200..212 | `invariant`, `spec`, catalog, `nondet`, `trusted`, `atomic output`, `seal`/`sealed`, comments, `output final` and `final` clauses (§2.1, §3.13) |
| 220 | Molly `.ded` is a separate frontend onto the same IR (not this grammar) |
| 240..245 | `Principal`, `from … principal`, `accept from`, sessions, authorization views, `signed<T>` (§3.14) |
| 260..265 | `program … version`, `#n`, `since`, `migrate from`, `translate`, `cluster_version.at_least`, `deprecated`/`semantics_changed` (§3.15) |

P2 items with no syntax here: LANG-011, 054, 072 (reserved form only), 096, 099, 107, 203, 221..223.

---

## 4. Example corpus

Every example is complete: nothing is elided. Comments explain the semantics. Where the lowering teaches
something beyond §3, the IR follows the example.

### E1. Key-value store node: put, get, delete with acks, durable table, upsert semantics

```blossom
program kvstore version 2;

choreography KvService {
  role Client: external;
  role Server: cluster;

  static admins(p: Principal);                                   // deploy-time configuration

  // Keyed by `req`: a client may not send two different puts with one request id in one tick (checked at
  // the sender, LANG-042), so `(client, req)` identifies a request.
  channel put(req: u64) -> (key: string, val: bytes): Client -> Server;
  channel get(req: u64) -> (key: string): Client -> Server;
  channel del(req: u64) -> (key: string): Client -> Server accept from principal in admins;
  channel put_ok(req: u64, ver: u64): Server -> Client;
  channel get_ok(req: u64, val: Option<bytes>, ver: u64): Server -> Client;
  channel del_ok(req: u64, existed: bool): Server -> Client;

  at Server {
    // Durable, partitioned by key over the Server cluster. Field 3 was added in version 2.
    durable table kv(#1 key: string) -> (#2 val: bytes, #3 ver: u64 = 0 since 2)
      partition by hash(key) over Server;

    // Several puts to one key can arrive in one tick. Rank them deterministically by (client, req): every put
    // gets its own version, and the highest-ranked one is the value that survives into the next tick.
    view put_rank(key: string, c: Session, req: u64, val: bytes, r = index() per (key) by (c, req)) =
      put{req, key, val} from c;
    view put_count(key: string, n = count(c, req)) = put_rank(key, c, req, _, _);

    put_value: on put_rank(key, c, req, val, r), outer kv(key, _, old) {
      let base = match old { Some(v) => v, None => 0 };
      send put_ok{req, ver: base + r + 1} to c;
      if put_count(key, n), r + 1 == n {
        upsert kv(key, val, base + n);          // exactly one upsert per key per tick: no SEM-051 conflict
      }
    }

    // Reads see the state at the start of the tick (CR-04): a put in the same tick is not visible yet.
    read: on get{req, key} from c, outer kv(key, val, ver) {
      send get_ok{req, val, ver: match ver { Some(x) => x, None => 0 }} to c;
    }

    // Delete removes the exact stored tuple at t+1. A put to the same key in the same tick wins (CR-05).
    remove: on del{req, key} from c, kv(key, v, ver) {
      delete kv(key, v, ver);
      send del_ok{req, existed: true} to c;
    }
    remove_missing: on del{req, key} from c, not kv(key, _, _) {
      send del_ok{req, existed: false} to c;
    }

    // Upgrade from version 1, whose kv had no `ver` column: existing values start at version 1.
    migrate from 1 {
      while old.kv(key, val) { emit kv(key, val, 1); }
    }
  }
}
```

Lowering of `put_value` in the projected module `KvService.Server` (`put` carries the sender column `C`):
```
put_rank__h(N, K, C, R, V) :- put(N, R, K, V, C).
put_rank(N, K, C, R, V, I) :- <index of put_rank__h per (K) by (C, R)>.           % §3.7 expansion
put_count(N, K, count<C, R>) :- put_rank(N, K, C, R, _, _).
[put_value.1a] put_ok(D, R, B + I + 1)@async :- put_rank(N, K, C, R, V, I), kv(N, K, _, W),
                                                B := W, D := C.
[put_value.1b] put_ok(D, R, 0 + I + 1)@async :- put_rank(N, K, C, R, V, I), notin kv__k(N, K), D := C.
[put_value.2a] kv__up(N, K, V, B + Cn) :- put_rank(N, K, C, R, V, I), kv(N, K, _, W), B := W,
                                          put_count(N, K, Cn), I + 1 == Cn.
[put_value.2b] kv__up(N, K, V, 0 + Cn) :- put_rank(N, K, C, R, V, I), notin kv__k(N, K),
                                          put_count(N, K, Cn), I + 1 == Cn.
kv__del(N, K, V0, W0) :- kv__up(N, K, _, _), kv(N, K, V0, W0).
kv(N, K, V, W)@next :- kv__up(N, K, V, W).
kv(N, K, V, W)@next :- kv(N, K, V, W), notin kv__del(N, K, V, W).                 % frame; WAL-logged (durable)
kv__k(N, K) :- kv(N, K, _, _).
```
`outer` splits each statement into a matched rule and an unmatched rule, and the `match` on `old` is resolved
per branch. `kv__up` is keyed on `K`. Because only the last-ranked put reaches it, the key constraint holds by
construction, and ANA-140 can prove it from `put_count` and `index`.

### E2. Reliable broadcast with retry timer and acks, as a reusable module

```blossom
// The interface (bud-sandbox names, ODD-23). Membership is an input so the importer decides who is in the group.
protocol Broadcast {
  input  member(n: Node);
  input  bcast(id: u64) -> (payload: bytes);
  output deliver(origin: Node, id: u64) -> (payload: bytes);
  output bcast_done(id: u64);
}

// At-least-once delivery with receiver-side dedup, so every member delivers each broadcast exactly once.
// The origin retransmits to each member until that member acks, like bud-sandbox ReliableDelivery.
module ReliableBroadcast(RETRY: Duration = 2s) implements Broadcast {
  channel msg(origin: Node, id: u64, payload: bytes): Node -> Node;
  channel ack(origin: Node, id: u64): Node -> Node;

  table outbox(dst: Node, id: u64) -> (payload: bytes);   // unacknowledged (destination, message) pairs
  table mine(id: u64);                                     // broadcasts this node originated
  table done(id: u64);                                     // broadcasts already reported complete
  table seen(origin: Node, id: u64);                       // messages this node already delivered
  timer retry every RETRY;

  view pending(id: u64) = outbox(_, id, _);

  originate: on bcast{id, payload} {
    emit mine(id);
    emit deliver(self, id, payload);                       // the origin delivers to itself immediately
    next seen(self, id);
    for member(n), n != self {
      emit outbox(n, id, payload);                         // `emit`: pending already holds in this tick
      send msg{origin: self, id, payload} to n;
    }
  }

  // Level-triggered on purpose: while an entry is unacked, every timer firing resends it.
  retransmit: on retry, outbox(dst, id, payload) {
    send msg{origin: self, id, payload} to dst;
  }

  receive: on msg{origin, id, payload} from s {
    send ack{origin, id} to s;                             // ack every copy: the previous ack may be lost
    if not seen(origin, id) {
      emit deliver(origin, id, payload);
      next seen(origin, id);                               // `next`, not `emit`: see the note below
    }
  }

  acked: on ack{origin: self, id} from d, outbox(d, id, p) {
    delete outbox(d, id, p);
  }

  complete: while mine(id), not pending(id), not done(id) {
    emit bcast_done(id);
    next done(id);
  }
}
```

Notes.
- `seen` is written with `next`. Had it been `emit seen(origin, id)`, then `seen` would hold in the same tick as
  the message, `not seen` would be false, and nothing would ever be delivered. The "negating your own same-tick
  write" lint (§3.16) catches this. The rules would still stratify, because `seen` does not depend on `deliver`,
  so without the lint the bug would be silent.
- `complete` is a `while` handler, but it fires once: `done` becomes true at the next tick. With no other
  members, `pending` is empty in the `bcast` tick and completion is reported one tick later.
- The ANA-024 guarded-asynchrony check passes. `msg` is consumed through `seen`/`deliver`, and the only
  negations are over local persistent state. The report lists `not seen`, `not pending`, `not done` and
  `delete outbox` as the module's points of order.

IR of `receive` and `acked` (sender columns shown):
```
[receive.1] ack(D, O, I)@async :- msg(N, O, I, P, S), D := S.
[receive.2] deliver(N, O, I, P) :- msg(N, O, I, P, S), notin seen(N, O, I).
[receive.3] seen(N, O, I)@next  :- msg(N, O, I, P, S), notin seen(N, O, I).
[acked.1]   outbox__del(N, D, I, P) :- ack(N, O, I, D), O == N, outbox(N, D, I, P).
```

### E3. Raft leader election as rules

Election only: persistent `currentTerm`/`votedFor`, randomized timeouts, RequestVote and its response, majority
counting and stepping down on a higher term. There is no step function. The rules follow R07 §11.1: *term
first* (the tick's effective term is the largest term known), *at most one vote per term per tick* (seeded
choice), and *all reads from the pre-tick snapshot*. Roles are views over term-indexed facts, so stepping down
takes no rule of its own. Log replication would add entries to `append_entries` and write `log`; E3 only reads
the log, for the election restriction.

```blossom
choreography RaftElection {
  role Server: cluster;

  const HEARTBEAT: Duration = 50ms;
  const ELECTION_MIN: Duration = 150ms;
  const ELECTION_MAX: Duration = 300ms;
  const POLL: Duration = 10ms;

  channel request_vote(term: u64, last_idx: u64, last_term: u64): Server -> Server;
  channel vote(term: u64, granted: bool): Server -> Server;
  channel append_entries(term: u64): Server -> Server;          // heartbeat form
  channel append_reply(term: u64, ok: bool): Server -> Server;

  at Server {
    // ---- persistent state (Raft Fig. 2); on disk before any reply of the same tick leaves (SEM-072)
    durable table current_term() -> (t: lmax<u64>);             // terms only grow, so a lattice
    durable table voted_for(term: u64) -> (cand: Server);       // key = term: at most one vote per term
    durable table log(idx: u64) -> (term: u64, cmd: bytes);     // written by replication, read here

    // ---- volatile state
    table won(term: u64);                                        // this node won the election of `term`
    table leader_of(term: u64) -> (l: Server);                   // the leader heard for `term`
    table votes(term: u64) -> (from: lset<Server>);              // granted votes received
    table deadline() -> (at: Timestamp);                         // randomized election deadline
    timer poll every POLL;
    timer heartbeat every HEARTBEAT;

    bootstrap {
      emit current_term(0);                                      // lattice merge: harmless after a restart
      emit deadline(now() + rand_range(ELECTION_MIN, ELECTION_MAX, 0u64));
    }

    // ---- term first
    view heard_term(t: u64) {
      request_vote{term: t};
      vote{term: t};
      append_entries{term: t};
      append_reply{term: t};
    }
    view eff(t = max(t0)) {
      current_term(ct), let t0 = reveal(ct);                     // exact read: a point of order
      heard_term(t0);
    }
    persist_term: while eff(t) { next current_term(t); }        // no change, no delta, when already stored

    // ---- roles as views; a higher term makes every one of them false for the old term
    view leader(t: u64) = eff(t), won(t);
    view candidate(t: u64) = eff(t), voted_for(t, self), not won(t), not leader_of(t, _);
    view follower(t: u64) = eff(t), not leader(t), not candidate(t);

    // ---- election restriction (§5.4.1): compare last term, then last index
    view last_idx(i = max(i0) default 0) = log(i0, _, _);
    view last_log(i: u64, t: u64) {
      last_idx(i), log(i, t, _);
      last_idx(i), i == 0, let t = 0;
    }

    // ---- voter side
    view rv_ok(c: Server, t: u64) =
      request_vote{term: t, last_idx: li, last_term: lt} from c, eff(t), last_log(mi, mt),
      lt > mt || (lt == mt && li >= mi);
    view regrant(c: Server, t: u64) = rv_ok(c, t), voted_for(t, c);        // a candidate's retry
    view fresh_grant(t: u64, c = choose(c0)) = rv_ok(c0, t), not voted_for(t, _);
    view grant(c: Server, t: u64) {
      regrant(c, t);
      fresh_grant(t, c);
    }
    view granted_now() = grant(_, _);

    grant_vote: on grant(c, t) {
      next voted_for(t, c);                                      // durable before the reply is released
      send vote{term: t, granted: true} to c;
    }
    refuse_vote: on request_vote from c, eff(t), not grant(c, t) {
      send vote{term: t, granted: false} to c;                   // our term: a stale candidate steps down
    }

    // ---- election timeout
    view heard_leader_now() = append_entries{term: t}, eff(t);
    view timed_out(t: u64) =
      poll, eff(t), deadline(d), now() >= d, not leader(t), not granted_now(), not heard_leader_now();
    view reset_timer() {
      granted_now();
      heard_leader_now();
      timed_out(_);
    }
    rearm: on reset_timer(), eff(t) {
      upsert deadline(now() + rand_range(ELECTION_MIN, ELECTION_MAX, t));   // the draw is captured in state
    }

    start_election: on timed_out(t), last_log(li, lt) {
      let nt = t + 1;
      next current_term(nt);
      next voted_for(nt, self);                                  // vote for self, durably
      next votes(nt, {self});
      for p in Server, p != self {
        send request_vote{term: nt, last_idx: li, last_term: lt} to p;
      }
    }

    // ---- candidate side
    count_vote: on vote{term: t, granted: true} from v { emit votes(t, {v}); }

    become_leader: while candidate(t), votes(t, s), majority(s, Server), not won(t) {
      next won(t);
      next leader_of(t, self);
      for p in Server, p != self { send append_entries{term: t} to p; }   // announce at once
    }

    // ---- leader heartbeats and the follower's side of them
    send_heartbeat: on heartbeat, leader(t) {
      for p in Server, p != self { send append_entries{term: t} to p; }
    }
    accept_leader: on append_entries{term: t} from l, eff(t) {
      emit leader_of(t, l);            // two leaders for one term would break the key: a SEM-050 error
      send append_reply{term: t, ok: true} to l;
    }
    reject_stale: on append_entries{term: at} from l, eff(t), at < t {
      send append_reply{term: t, ok: false} to l;
    }

    invariant one_leader_per_term_seen: never leader_of(t, a), leader_of(t, b), a != b;
  }
}
```

Why this is safe as rules:
- *Step-down.* A message with a higher term raises `eff` in the tick it arrives. `leader` and `candidate`
  are defined at `eff`, so they become false in that same tick, and `persist_term` makes the new term durable
  before any reply leaves.
- *One vote per term.* `voted_for` is keyed by term and durable. `fresh_grant` requires that no vote exists
  for the term and picks exactly one requester per tick (seeded choice, SEM-085). `regrant` repeats only the
  existing vote. `start_election` is suppressed in a tick in which this node grants a vote (`granted_now`),
  so the two writes never target the same term.
- *Majority.* `majority(s, Server)` is a monotone threshold over a grow-only set: it cannot become false once
  true, which is what makes it a CALM-safe winning condition and a quorum sort for the verifier (VER-008).

Selected IR (projected module `RaftElection.Server`; `S` is a sender column):
```
eff__u(N, T0) :- current_term(N, Ct), T0 := reveal(Ct).                         % [eff.1] exact read
eff__u(N, T0) :- heard_term(N, T0).                                             % [eff.2]
eff(N, max<T0>) :- eff__u(N, T0).
[persist_term.1] current_term(N, T)@next :- eff(N, T).                       % ⊔ into lmax
fresh_grant__cand(N, T, C0) :- rv_ok(N, C0, T), notin voted_for__k(N, T).
fresh_grant__pick(N, T, argmin<(P, C0)>) :- fresh_grant__cand(N, T, C0), P := prf(σc, "fresh_grant.1", T, C0).
fresh_grant(N, T, C) :- fresh_grant__pick(N, T, (_, C)).
[grant_vote.1] voted_for(N, T, C)@next :- grant(N, C, T).
[grant_vote.2] vote(D, T, true)@async :- grant(N, C, T), D := C.
[start_election.1] current_term(N, Nt)@next :- timed_out(N, T), last_log(N, Li, Lt), Nt := T + 1.
[start_election.4] request_vote(D, Nt, Li, Lt)@async :- timed_out(N, T), last_log(N, Li, Lt), Nt := T + 1,
                                                       Server__members(N, P), P != N, D := P.
[become_leader.1] won(N, T)@next :- candidate(N, T), votes(N, T, S), majority(S, Server__members), notin won(N, T).
```

### E4. Two-phase commit: coordinator and participants in one choreography, with timeout-abort

The protocol is written in message order by reopening `at` sections. Durable writes happen in the same tick
as the sends that depend on them; the durability barrier orders them (SEM-072), so "log, then send" needs no
extra tick (LIB-041).

```blossom
choreography TwoPhaseCommit {
  role Client: external;
  role Coordinator;
  role Participant: cluster;

  const TIMEOUT: Duration = 5s;
  const RETRY: Duration = 500ms;

  type Decision = enum { Commit, Abort, unknown };     // `unknown`: forward-compatible fallback (LANG-261)

  channel begin(txn: u64): Client -> Coordinator;
  channel prepare(txn: u64): Coordinator -> Participant;
  channel vote(txn: u64, yes: bool): Participant -> Coordinator;
  channel decide(txn: u64, d: Decision): Coordinator -> Participant;
  channel ack(txn: u64): Participant -> Coordinator;
  channel outcome(txn: u64, d: Decision): Coordinator -> Client;

  // ---------------- phase 1a: the coordinator asks every participant
  at Coordinator {
    durable table txn(id: u64) -> (client: Session, started: Timestamp) resolve choose;
    durable table decision(id: u64) -> (d: Decision);
    table yes(id: u64) -> (from: lset<Participant>);
    table no(id: u64) -> (from: lset<Participant>);
    table acked(id: u64) -> (from: lset<Participant>);
    timer retry every RETRY;

    start: on begin{txn: id} from c, not txn(id, _, _) {
      next txn(id, c, now());              // two clients racing on one id: `resolve choose` keeps one
      for p in Participant { send prepare{txn: id} to p; }
    }
    repeat_outcome: on begin{txn: id} from c, decision(id, d) {
      send outcome{txn: id, d} to c;       // a retrying client learns the decision
    }
    re_prepare: on retry, txn(id, _, _), not decision(id, _), p in Participant,
                not p in yes[id], not p in no[id] {
      send prepare{txn: id} to p;
    }
  }

  // ---------------- phase 1b: participants log their vote and answer
  at Participant {
    input refuse(txn: u64);                           // host: this participant cannot commit `txn`
    table refused(txn: u64);
    durable table prepared(txn: u64) -> (yes: bool);  // the vote, logged before it is sent
    durable table decided(txn: u64) -> (d: Decision);
    output commit_txn(txn: u64);
    output abort_txn(txn: u64);

    note_refusal: on refuse{txn} { emit refused(txn); }

    first_vote: on prepare{txn} from co, not prepared(txn, _), not decided(txn, _) {
      if refused(txn)     { next prepared(txn, false); send vote{txn, yes: false} to co; }
      if not refused(txn) { next prepared(txn, true);  send vote{txn, yes: true}  to co; }
    }
    repeat_vote: on prepare{txn} from co, prepared(txn, y) {
      send vote{txn, yes: y} to co;                   // idempotent: the logged vote, never a new one
    }
  }

  // ---------------- phase 2a: the coordinator decides, logs, announces
  at Coordinator {
    count_yes: on vote{txn: id, yes: true} from p  { emit yes(id, {p}); }
    count_no:  on vote{txn: id, yes: false} from p { emit no(id, {p}); }

    view all_yes(id: u64) = yes(id, s), s.size() >= Participant.size;          // monotone threshold
    view any_no(id: u64)  = no(id, s), s.size() >= 1;                           // monotone threshold
    view timed_out(id: u64) =                                                   // non-monotone: time, negation
      txn(id, _, t0), now() - t0 > TIMEOUT, not all_yes(id), not decision(id, _);
    view verdict(id: u64, d: Decision) {
      all_yes(id), not any_no(id), let d = Decision::Commit;
      any_no(id), let d = Decision::Abort;
      timed_out(id), let d = Decision::Abort;
    }

    decide_once: while verdict(id, d), not decision(id, _) {
      next decision(id, d);                           // durable: logged before `announce` runs
    }
    announce: on inserted decision(id, d), txn(id, c, _) {
      send outcome{txn: id, d} to c;
      for p in Participant { send decide{txn: id, d} to p; }
    }
    redecide: on retry, decision(id, d), p in Participant, not p in acked[id] {
      send decide{txn: id, d} to p;
    }
    count_ack: on ack{txn: id} from p { emit acked(id, {p}); }
  }

  // ---------------- phase 2b: participants apply and acknowledge
  at Participant {
    apply_decision: on decide{txn, d} from co {
      send ack{txn} to co;
      if not decided(txn, _) {
        next decided(txn, d);
        if d == Decision::Commit { emit commit_txn(txn); }
        if d == Decision::Abort  { emit abort_txn(txn); }
      }
    }
    invariant no_unknown_decision: never decided(txn, Decision::unknown);
  }
}
```

Notes.
- `verdict` can hold both Commit and Abort for one id only if one participant voted both ways. The durable
  `prepared` table forbids that, and if it happened anyway, `decision`'s key would turn it into a SEM-050
  error.
- The analyzer reports the points of order: `not all_yes`, `not decision`, `not p in acked[id]` and the other
  negations, plus `now()` in `timed_out`. It labels `decision` "coordinated at Coordinator by timeout",
  because of the time-dependent abort (ANA-029). `all_yes` and `any_no` alone would be THRESH-final.
- Projection gives two programs. `TwoPhaseCommit.Coordinator` receives `begin`, `vote` and `ack` and sends
  `prepare`, `decide` and `outcome`. `TwoPhaseCommit.Participant` receives `prepare` and `decide` and sends
  `vote` and `ack`. The inferred ACLs are: `prepare` and `decide` accepted only from Coordinator; `vote` and
  `ack` only from Participant; `begin` only from external Client sessions.

IR of `re_prepare` and `timed_out` in the Coordinator projection:
```
[re_prepare.1] prepare(D, I)@async :- retry(N, _, _), txn(N, I, _, _), notin decision__k(N, I),
                                      Participant__members(N, P), notin yes__has(N, I, P),
                                      notin no__has(N, I, P), D := P.
yes__has(N, I, P) :- yes(N, I, S), contains(S, P).   % an absent cell is ⊥ = ∅: no row, so `notin` holds
no__has(N, I, P)  :- no(N, I, S), contains(S, P).
timed_out(N, I) :- txn(N, I, _, T0), now(N, T), T - T0 > TIMEOUT, notin all_yes(N, I), notin decision__k(N, I).
```

### E5. Lattices: vector clocks, a monotone shopping cart with a user lattice, a quorum threshold

```blossom
// ======================= (a) vector clocks =======================
type VClock = lmap<Node, lmax<u64>>;

module CausalStamps {
  input  local_event(id: u64) -> (data: bytes);
  input  peer(n: Node);                                   // fed by the importer at every tick
  output stamped(origin: Node, id: u64, data: bytes) -> (at: VClock);
  output final heard_ten(n: Node);                        // THRESH-final

  channel gossip(id: u64, data: bytes, at: VClock): Node -> Node;
  cell clock: VClock;                                     // grows only
  table history(origin: Node, id: u64) -> (at: VClock);

  // A tick with local events or receipts is one step of the clock. Every event of the tick gets the same
  // stamp: Dedalus treats facts of one tick as simultaneous (CR-02).
  view stepped() { local_event; gossip; }
  scratch next_clock() -> (v: VClock);                    // tick-scoped lattice
  bump: on stepped() {
    emit next_clock(clock.join({self: clock.at(self) + 1}));   // join and at(): morphisms; +1 on lmax: morphism
  }
  absorb: on gossip{at: m} { emit next_clock(m); }        // merge every received clock
  advance: on next_clock(v) { next clock(v); }

  local: on local_event{id, data}, next_clock(v) {
    emit stamped(self, id, data, v);
    emit history(self, id, v);
    for peer(n), n != self { send gossip{id, data, at: v} to n; }
  }
  remote: on gossip{id, data, at: m} from o {
    emit stamped(o, id, data, m);
    emit history(o, id, m);
  }

  // Monotone threshold on a lattice read: once 10 events from n are known, this never changes.
  ten: while peer(n), clock.at(n) >= 10 { emit heard_ten(n); }

  // Happened-before is an exact comparison of two clocks. It is a point of order and has to say so.
  view concurrent(a1: Node, i1: u64, a2: Node, i2: u64) =
    history(a1, i1, v1), history(a2, i2, v2), (a1, i1) < (a2, i2),
    let r1 = reveal(v1), let r2 = reveal(v2),
    !map_le(r1, r2), !map_le(r2, r1);                     // std: pointwise ≤, missing entries = 0
}

// ======================= (b) monotone shopping cart (Bloom^L style) =======================
module MonotoneCart {
  type Op = { item: string, delta: i32 };

  // User-defined lattice: a product of built-in lattices, so its laws hold by construction (LANG-135).
  lattice Cart {
    ops: lmap<u64, Point<Op>>,      // op id -> op; a second, different op under one id becomes ⊤
    expect: Point<u64>,             // number of ops in the session, fixed by checkout
  }

  // A declared-monotone function: the claim is a proof obligation (VER-014, else TEST-087 "tested").
  // Monotone in `expect` (⊥ < n < ⊤ maps to false ≤ size ≥ n ≤ true) and in `ops` (size only grows).
  monotone fn complete(c: Cart) -> lbool =
    match reveal(c.expect) {
      PointVal::Bot    => false,
      PointVal::Val(n) => c.ops.size() >= n,
      PointVal::Top    => true,
    };

  channel add(session: u64, op: u64, item: string, delta: i32): Node -> Node;
  channel checkout(session: u64, nops: u64): Node -> Node;
  channel receipt(session: u64, lines: List<(string, i32)>): Node -> Node;

  table carts(session: u64) -> (c: Cart);
  table reply_to(session: u64) -> (client: Node);
  table sent(session: u64);

  record_op: on add{session, op, item, delta} {
    emit carts(session, Cart{ ops: {op: Point(Op{item, delta})} });   // fields left out are ⊥
  }
  record_checkout: on checkout{session, nops} from c {
    emit carts(session, Cart{ expect: Point(nops) });
    emit reply_to(session, c);
  }

  // The exact read (`reveal`) is guarded by the monotone threshold `complete`: the cart can no longer
  // change in a way that alters the summary ("monotone, then immutable"; ANA-142 certifies it).
  view done_cart(session: u64) = carts(session, c), complete(c);
  view line(session: u64, item: string, qty = sum(d)) =
    carts(session, c), complete(c),
    (id, PointVal::Val(op)) in reveal(c.ops),             // bind `id`: each op is one distinct contribution
    let item = op.item, let d = op.delta;
  view receipt_lines(session: u64, lines = collect((item, qty) by item) default [] over done_cart(session)) =
    line(session, item, qty), qty != 0;

  respond: while receipt_lines(session, lines), reply_to(session, c), not sent(session) {
    send receipt{session, lines} to c;
    next sent(session);
  }

  invariant op_conflict: never carts(s, c), (_, PointVal::Top) in reveal(c.ops);
  invariant too_many_ops: never carts(s, c), let PointVal::Val(n) = reveal(c.expect), c.ops.size() > n;
}

// ======================= (c) quorum threshold: size(lset) >= k =======================
module QuorumWrite(W: u32 = 2) {
  input  replica(n: Node);
  input  write(req: u64) -> (key: string, val: bytes);
  output final committed(req: u64);                       // accepted: THRESH-final (ANA-120)

  channel store(req: u64, key: string, val: bytes): Node -> Node;
  channel stored(req: u64): Node -> Node;

  table pending(req: u64) -> (key: string, val: bytes);
  table acks(req: u64) -> (from: lset<Node>);
  table held(req: u64) -> (key: string, val: bytes);     // replica side
  timer retry every 1s;

  fan_out: on write{req, key, val} {
    emit pending(req, key, val);
    for replica(r) { send store{req, key, val} to r; }
  }
  resend: on retry, pending(req, key, val), replica(r), not r in acks[req] {
    send store{req, key, val} to r;
  }
  collect_ack: on stored{req} from r { emit acks(req, {r}); }

  // Monotone: once W replicas have acked, the threshold holds forever, so `committed` is final on emission.
  quorum_reached: while acks(req, s), s.size() >= W { emit committed(req); }
  gc: while acks(req, s), s.size() >= W, pending(req, k, v) { delete pending(req, k, v); }

  keep: on store{req, key, val} from c {
    emit held(req, key, val);
    send stored{req} to c;
  }
}
```

Lowering highlights:
```
% (a) cell write through a tick-scoped lattice; `clock.at(self) + 1` is a morphism chain
[bump.1] next_clock(N, V) :- stepped(N), clock(N, C), V := join(C, {N: at(C, N) + 1}).
[advance.1] clock(N, V)@next :- next_clock(N, V).
[ten.1] heard_ten(N, P) :- peer(N, P), clock(N, C), ge(at(C, P), 10).            % threshold: positive edge
concurrent(N, A1, I1, A2, I2) :- history(N, A1, I1, V1), history(N, A2, I2, V2), (A1, I1) < (A2, I2),
    R1 := reveal(V1), R2 := reveal(V2), !map_le(R1, R2), !map_le(R2, R1).          % exact: negative edges
% (b) collection -> lattice by implicit fold; a partial record literal fills ⊥
[record_op.1] carts(N, S, Cart{ops: {Op: Point(O)}, expect: ⊥}) :- add(N, S, Op, It, D), O := Op{It, D}.
line(N, S, It, sum<D>) :- carts(N, S, C), is_true(complete(C)),
    elem(reveal(ops(C)), (Id, Val(O))), It := item(O), D := delta(O).
% (c) the quorum test
[quorum_reached.1] committed(N, R) :- acks(N, R, S), ge(size(S), W).
```

### E6. Word-count MapReduce: hash-partitioned shuffle, reducer aggregation, seals end the input

```blossom
choreography WordCount {
  role Mapper: cluster;
  role Reducer: cluster;

  const HOT: u64 = 1000;

  // Job plan, supplied at deploy time: which mapper reads each input split.
  static split_of(split: u32) -> (mapper: Mapper);

  // One tuple per word occurrence. (split, line, pos) keeps occurrences distinct under set semantics, so
  // at-least-once delivery cannot double-count (CR-35).
  channel occ(word: string, split: u32, line: u64, pos: u32): Mapper -> Reducer
    reliable sealed by (split);

  at Mapper {
    input line(split: u32, lineno: u64) -> (text: string);   // the host streams each split's lines
    input eof(split: u32);                                    // the host: the split has been read fully

    map: on line{split, lineno, text}, (pos, w) in enumerate(words(text)) {
      send occ{word: w, split, line: lineno, pos} to Reducer.by_hash(w);     // hash-partitioned shuffle
    }
    punctuate: on eof{split}, split_of(split, self) {
      for r in Reducer { seal occ{split} to r; }   // digest: how many occ this mapper sent r for `split`
    }
  }

  at Reducer {
    table seen(word: string, split: u32, line: u64, pos: u32);
    table occs(word: string) -> (ids: lset<(u32, u64, u32)>);
    output final hot(word: string);                  // THRESH-final: may be emitted before any seal
    output final word_count(word: string, n: u64);   // SEALED-final: after the seal of every split

    keep: on occ{word, split, line, pos} {
      emit seen(word, split, line, pos);
      emit occs(word, {(split, line, pos)});
    }
    early: while occs(word, ids), ids.size() >= HOT { emit hot(word); }

    // A split is complete here when its mapper's seal has arrived and the digest matches what arrived.
    view split_done(s: u32) = split_of(s, m), sealed occ{split: s} from m;
    view split_pending(s: u32) = split_of(s, _), not split_done(s);
    view all_done() = not split_pending(_);

    finish: on inserted all_done(), seen(word, s, l, p) {
      emit word_count(word, count(s, l, p));
    }
  }
}
```

Notes.
- `sealed occ{split: s} from m` is the Blazes `Seal_split` annotation made operational (§3.13). The finality
  analysis marks each `(split, mapper)` partition of `occ` CLOSED once sealed. That makes `split_pending`,
  `all_done` and the count exact, and `output final word_count` is accepted as SEALED-final (FLAG-141).
  `hot` needs no seal: it is a monotone threshold over a grow-only set.
- `finish` fires in exactly one tick, the first in which every split is complete, so each count is emitted
  once with status `final_present`.
- If the host delivers a `line` for a split after its `eof`, the mapper's generated "send after seal" check
  fires (§3.13). That makes an ordering bug in the input adapter a loud error instead of a silently wrong count.
- A mapper-side combiner would send partial counts. Those are group values, not identity-keyed facts, so they
  would have to travel `carries zset<(string, u32)> via exactly_once(dots)` (LANG-158, CR-35). Identity-keyed
  occurrences need no wrapper.

IR of `map` and `finish`:
```
[map.1] occ(D, W, S, L, P)@async :- line(N, S, L, T), elem(enumerate(words(T)), (P, W)),
                                    D := by_hash(Reducer__members, W).
occ__out(N, D, W, S, L, P) :- line(N, S, L, T), elem(enumerate(words(T)), (P, W)),
                              D := by_hash(Reducer__members, W).                      % seal log (§3.13)
all_done__prev(N)@next :- all_done(N).
[finish.1] word_count(N, W, count<S, L, P>) :- all_done(N), notin all_done__prev(N), seen(N, W, S, L, P).
```

### E7. Single-node analytics: transitive closure, shortest paths, stratified negation

```blossom
module GraphAnalytics {
  static node(n: u32);
  static edge(src: u32, dst: u32) -> (w: u64);
  static source(n: u32);
  const MAX_HOPS: u64 = 6;                     // |node| - 1: every simple path fits

  fact node(1); fact node(2); fact node(3); fact node(4); fact node(5); fact node(6); fact node(7);
  fact edge(1, 2, 7);  fact edge(1, 3, 9);  fact edge(1, 6, 14); fact edge(2, 3, 10);
  fact edge(2, 4, 15); fact edge(3, 4, 11); fact edge(3, 6, 2);  fact edge(4, 5, 6);
  fact edge(6, 5, 9);  fact edge(5, 1, 3);
  fact source(1);                              // node 7 has no edges: it is the unreachable one

  output distance(target: u32) -> (d: Option<u64>);
  output first_hop(target: u32, via: u32);
  output unreachable_node(n: u32);
  output rank_of(target: u32) -> (r: u64);

  // Transitive closure: positive recursion, one stratum; `monotone` makes the compiler check it.
  monotone view reach(a: u32, b: u32) {
    edge(a, b, _);
    reach(a, c), edge(c, b, _);
  }

  // Shortest distances on a lattice: recursion through a morphism (lmin + w). Monotone, one stratum.
  monotone view dist(a: u32, b: u32, d: lmin<u64>) {
    edge(a, b, w), let d = lmin(w);
    dist(a, c, d0), edge(c, b, w), let d = d0 + w;
  }

  // The same answer with a stratified min aggregate over hop-bounded paths (finite because h is bounded).
  view hop_path(a: u32, b: u32, d: u64, h: u64) {
    edge(a, b, w), let d = w, let h = 1;
    hop_path(a, c, d0, h0), h0 < MAX_HOPS, edge(c, b, w), let d = d0 + w, let h = h0 + 1;
  }
  view shortest(a: u32, b: u32, d = min(d0)) = hop_path(a, b, d0, _);

  // First hop on a shortest path: argmin keeps every tied exemplar.
  view via_cost(a: u32, b: u32, c: u32, cost: u64) {
    edge(a, b, w), let c = b, let cost = w;
    edge(a, c, w), shortest(c, b, d), let cost = w + d;
  }
  view next_hop(a: u32, b: u32, via = argmin(c by cost)) = via_cost(a, b, c, cost);

  // Stratified negation: nodes the source cannot reach.
  view unreachable(n: u32) = source(s), node(n), n != s, not reach(s, n);

  // Dense ranking of targets by distance (canonical tie-break).
  view ranked(b: u32, r = index() by (d)) = source(s), shortest(s, b, d);

  // Out-degree with an explicit default (LANG-106).
  view out_degree(n: u32, k = count(m) default 0 over node(n)) = edge(n, m, _);

  report_distance: while source(s), node(n), n != s, outer shortest(s, n, d) { emit distance(n, d); }
  report_hop: while source(s), next_hop(s, b, v) { emit first_hop(b, v); }
  report_unreachable: while unreachable(n) { emit unreachable_node(n); }
  report_rank: while ranked(b, r) { emit rank_of(b, r); }

  // The two shortest-path formulations must agree.
  invariant formulations_agree: never shortest(a, b, d), dist(a, b, dl), reveal(dl) != d;
}
```

Stratification (SEM-022) puts `reach`, `dist` and `hop_path` in stratum 0 (positive; `dist` recurses through
a morphism, SEM-031). `shortest` and `unreachable` go in stratum 1 (aggregate edge, negation edge);
`via_cost` and `out_degree` in 1; `next_hop` and `ranked` in 2; and the `outer` report and the invariant, with
their exact reads, in 2. The static inputs mean all the work happens at tick 0. The node then idles, and the
`while` reports are re-derived at any later tick without changing (SEM-009).

IR of the lattice recursion and the negation:
```
[dist.1] dist(N, A, B, D) :- edge(N, A, B, W), D := lmin(W).                    % ⊔ = min under key (A, B)
[dist.2] dist(N, A, B, D) :- dist(N, A, C, D0), edge(N, C, B, W), D := D0 + W.
[unreachable.1] unreachable(N, X) :- source(N, S), node(N, X), X != S, notin reach(N, S, X).
```

### E8. Soft-state heartbeat failure detector

```blossom
module FailureDetector(PERIOD: Duration = 1s, TTL: Duration = 3500ms) {
  input  member(n: Node);                          // fed by the importer at every tick
  output alive(n: Node);
  output suspect(n: Node);
  output newly_suspected(n: Node);
  output recovered(n: Node);

  channel hb(sent_at: Timestamp): Node -> Node;
  soft table heard(n: Node) ttl TTL max 4096;      // refreshed by each heartbeat; expires at tick boundaries
  table started() -> (at: Timestamp);
  timer beat every PERIOD;                         // also keeps the node ticking, so expiry is observed

  bootstrap { emit started(now()); }

  beat_out: on beat, member(n), n != self { send hb{sent_at: now()} to n; }
  beat_in: on hb from n { emit heard(n); }         // re-deriving a live tuple resets its birth (SEM-060)

  // Grace period: nobody is suspected before one TTL has passed since this incarnation started.
  view settled() = started(t0), now() - t0 > TTL;
  view up(n: Node) = member(n), n != self, heard(n);
  view down(n: Node) = member(n), n != self, settled(), not heard(n);

  report_up: on up(n) { emit alive(n); }           // `member` is an input, so these views are events
  report_down: on down(n) { emit suspect(n); }
  edge_down: on inserted down(n) { emit newly_suspected(n); }
  edge_up: on deleted down(n), member(n), heard(n) { emit recovered(n); }
}
```

The soft table lowers as in §3.3. With `TTL = 3.5 × PERIOD`, three consecutive lost heartbeats are needed
before a node is suspected. Expiry is checked against each tick's sampled `now`, so replay reproduces
suspicions exactly (CR-17). `suspect` is non-monotone: `not heard`, and `heard` shrinks through expiry. The
analyzer reports it as time-dependent, which is correct for a failure detector.

### E9. Two instances of E2's module, with interposition on one interface

```blossom
module DualBroadcast(MAX_PAYLOAD: u64 = 65536) {
  import ReliableBroadcast(RETRY = 2s) as data;          // bulk traffic: slow retries
  import ReliableBroadcast(RETRY = 200ms) as control;    // control traffic: fast retries

  static peer(n: Node);
  input  publish(id: u64) -> (payload: bytes);
  input  command(id: u64) -> (payload: bytes);
  output received(origin: Node, id: u64, urgent: bool) -> (payload: bytes);
  output published(id: u64);
  output rejected(id: u64);

  table accepted(id: u64) -> (size: u64);                // metering of admitted bulk payloads
  view total_bytes(t = sum(sz) default 0) = accepted(id, sz);   // bind `id`: see the note below

  // Both instances see the same group. Instance inputs are tick-local, so they are fed at every tick.
  membership: while peer(n) {
    emit data.member(n);
    emit control.member(n);
  }

  publish_bulk: on publish{id, payload} { emit data.bcast(id, payload); }
  publish_ctrl: on command{id, payload} { emit control.bcast(id, payload); }
  deliver_bulk: on data.deliver(o, id, p) { emit received(o, id, false, p); }
  deliver_ctrl: on control.deliver(o, id, p) { emit received(o, id, true, p); }
  done_bulk: on data.bcast_done(id) { emit published(id); }

  // Interposition on data.bcast: every write to it, including `publish_bulk` above, goes to `outside`.
  // Only what this block forwards to `inside` reaches the broadcast module.
  interpose data.bcast as (outside, inside) {
    admit: on outside(id, p), p.len() <= MAX_PAYLOAD {
      emit inside(id, p);
      emit accepted(id, p.len());
    }
    refuse: on outside(id, p), p.len() > MAX_PAYLOAD {
      emit rejected(id);
    }
  }
}
```

Note: `total_bytes` binds `id` although it never uses it. Written as `accepted(_, sz)`, two payloads of equal
size would be summed once, because `_` is projected away before aggregation (§3.7). The compiler's
wildcard-under-aggregate lint flags that form.

Lowering highlights:
```
% instance renaming: two independent copies of E2, with disjoint channel identities
[data.originate.5] data.msg(D, O, I, P)@async :- data.bcast(N, I, P), data.member(N, M), M != N, O := N, D := M.
[control.originate.5] control.msg(D, O, I, P)@async :- control.bcast(N, I, P), control.member(N, M), M != N,
                                                       O := N, D := M.
data.retry: runtime timer every 2s      control.retry: runtime timer every 200ms
% interposition: writers of data.bcast are redirected to data.bcast__outside
[publish_bulk.1] data.bcast__outside(N, I, P) :- publish(N, I, P).
[admit.1] data.bcast(N, I, P) :- data.bcast__outside(N, I, P), len(P) <= MAX_PAYLOAD.
[admit.2] accepted(N, I, S) :- data.bcast__outside(N, I, P), len(P) <= MAX_PAYLOAD, S := len(P).
[refuse.1] rejected(N, I) :- data.bcast__outside(N, I, P), len(P) > MAX_PAYLOAD.
[membership.1] data.member(N, X) :- peer(N, X).
[membership.2] control.member(N, X) :- peer(N, X).
```
The instance path is part of each channel's wire schema id (§3.9), so a `data.msg` frame can never be decoded
as `control.msg`. `control.bcast` is not interposed, so writes to it reach the instance directly.

### E10. Verification specs: simple broadcast under LDFI, and Raft election safety

```blossom
// Molly's simple-deliv (LDFI paper Fig. 2) in Blossom.
module SimpleDeliv {
  static node(n: Node);
  input bcast(payload: string);
  table log(payload: string);
  channel deliv(payload: string): Node -> Node;

  originate: on bcast{payload} {
    emit log(payload);
    for node(n) { send deliv{payload} to n; }
  }
  receive: on deliv{payload} { emit log(payload); }
}

// Molly's redun-deliv: every node relays what it has logged, at every tick (redundancy in space and time).
module RedunDeliv {
  include SimpleDeliv;
  relay: while log(payload), node(n) { send deliv{payload} to n; }     // level-triggered on purpose
}

spec SimpleDelivFaults for SimpleDeliv {
  nodes A, B, C;
  fact node(@A, B); fact node(@A, C);
  fact node(@B, A); fact node(@B, C);
  fact node(@C, A); fact node(@C, B);
  input bcast(@A, "data") at 1;

  faults { eot 4; eff 2; crashes 1; }              // Fspec ⟨EOT, EFF, maxCrashes⟩ (TEST-020)

  // Molly's deliv_assert.ded: "someone has a log, but not me".
  view missing_log(a: Node, pl: string) = log(@x, pl), node(@x, a), not log(@a, pl);
  pre(x, pl)  = log(@x, pl), not bcast(@x, pl) at 1, not crashed(x);
  post(x, pl) = log(@x, pl), not missing_log(_, pl);

  check ldfi expect fails;         // one omission A->B at tick 1 falsifies `post` (Molly round 1)
}

spec RedunDelivFaults for RedunDeliv {
  nodes A, B, C;
  fact node(@A, B); fact node(@A, C);
  fact node(@B, A); fact node(@B, C);
  fact node(@C, A); fact node(@C, B);
  input bcast(@A, "data") at 1;

  faults { eot 4; eff 2; crashes 1; }

  view missing_log(a: Node, pl: string) = log(@x, pl), node(@x, a), not log(@a, pl);
  pre(x, pl)  = log(@x, pl), not bcast(@x, pl) at 1, not crashed(x);
  post(x, pl) = log(@x, pl), not missing_log(_, pl);

  // Bounded liveness (VER-001): within 2 ticks after the last omission, nobody who could have the payload
  // is missing it. Vacuous when the origin crashed before sending anything.
  liveness no_gaps: eventually not missing_log(_, "data") within 2 ticks after eff;

  check ldfi expect holds;         // the failure-free run's lineage already covers every hypothesis (Molly round 3)
}

spec RaftElectionSafety for RaftElection {
  nodes S1, S2, S3;
  assign Server = {S1, S2, S3};

  // Election Safety (Raft §5.2): at most one leader per term, over the whole history of the run.
  invariant election_safety: never ever won(@a, t), ever won(@b, t), a != b;

  // Supporting invariants that make election_safety inductive for the SMT check (VER-006..010).
  invariant vote_once: never ever voted_for(@n, t, c1), ever voted_for(@n, t, c2), c1 != c2;
  view voter_set(c: Node, t: u64, s = collect_set(v)) = voted_for(@v, t, c);
  view voter_quorum(c: Node, t: u64) = voter_set(c, t, s), majority(s, Server);
  invariant won_by_quorum: never won(@c, t), not voter_quorum(c, t);

  check bmc { ticks 30; delay 3; crashes 1; round 10ms; } expect holds;   // VER-002; timers mapped to rounds
  check smt expect holds;                                                // VER-006..010, quorum sort (VER-008)
  check sim { runs 20000; seed 7; crashes 1; } expect holds;             // TEST-001 deterministic simulation
}
```

What each piece means for the tools:
- `faults` is the LDFI Fspec. An omission may occur only at a send tick `1 ≤ t < EFF` (CR-21), and `crashes`
  bounds the crash variables (TEST-027). `pre` and `post` are required and are evaluated at EOT (TEST-022,
  CR-30). A run in which `pre` fails is vacuous.
- `ever won(@a, t)` ranges over the trace relation `won__log(a, t, τ)` with `τ ≤` the evaluation point.
  `election_safety` is checked at every global state that BMC or simulation reaches. SMT uses it as the goal
  of an inductive invariant, conjoined with `vote_once` and `won_by_quorum`. `majority` becomes the quorum
  sort with the intersection axiom (VER-008), which is exactly the step that proves two quorums share a voter,
  who by `vote_once` voted for one candidate.
- Crashed nodes' frozen state stays visible to specs (CR-20). That is why `voted_for(@v, …)` of a crashed
  voter still counts toward `voter_quorum`. It also makes `voted_for` being durable essential: without it, a
  crash-restart could make `vote_once` fail, and BMC with `crashes 1` finds that counterexample.

Lowering of `election_safety` and `won_by_quorum` to trace queries:
```
violation("election_safety", (A, B, T, P)) :- point(P), won__log(A, T, P1), P1 <= P,
                                              won__log(B, T, P2), P2 <= P, A != B.
voter_set(C, T, collect_set<V>, P) :- point(P), voted_for__log(V, T, C, P).
voter_quorum(C, T, P) :- voter_set(C, T, S, P), majority(S, Server__members).
violation("won_by_quorum", (C, T, P)) :- point(P), won__log(C, T, P), notin voter_quorum(C, T, P).
```

---

## 5. Self-critique

### 5.1 Decisions this proposal takes (and the alternative it rejected)

| Decision | Rationale | Rejected alternative |
|---|---|---|
| Five verbs (`emit`, `next`, `send`, `delete`, `upsert`) name the rule kind | The rule kind is the most important fact about a Dedalus rule, so it should be the first word | Bloom's operator arrows `<= <+ <~ <- <+-`: compact, but easy to misread |
| `on` requires an event, `while` forbids one | Makes ODD-05 re-derivation and resends visible | One keyword for both, with a lint |
| Views are closed definitions; scratches are open | A reader finds every rule of a view in one place | Letting any handler `emit` into a view |
| `else` only after scalar conditions | An `else` after an atom condition is a hidden anti-join | Allowing it and marking it in the LSP |
| Exact lattice reads require `reveal` | The monotone read is the easy path, and the non-monotone one is visible | Implicit coercion with a warning |
| `#` is a comment unless a digit follows | Satisfies LANG-208 and LANG-261 together | Dropping `#` comments |
| `min`/`max` are only aggregates; scalars are `least`/`greatest` | Keeps aggregates recognizable by name in heads | Deciding by arity |
| `bootstrap` runs at tick 0 of every incarnation | Volatile state must be re-initialized after a restart (SEM-071), and lattice merges into durable state are idempotent | Once per node lifetime, with a separate `on restart` hook |
| Aggregates range over distinct bindings of named variables; `_` is projected first | Datalog/Soufflé set semantics (CR-03); matches Bloom collections | Bag semantics over body matches |
| Channel identity includes the instance path | Two imports of one module can never exchange messages (E9) | Global channel names with explicit renaming |
| Seals carry a count digest | Order-independent completion (DIST-011), enough under set semantics with honest nodes | Hash digests always (offered as an option below) |
| `r__prev` for delta atoms inherits durability | No spurious `inserted` burst after recovery | Deltas relative to an empty pre-restart state |

### 5.2 Weaknesses

1. **Handlers look sequential, but they are per-batch set operations.** An engineer coming from Erlang or P will
   write `on put{key, val} { upsert kv(key, val); }` and meet SEM-051 the first time two clients write one key
   in one tick. E1 needed `index()` to linearize same-tick puts. The syntax makes the per-message mental model
   feel right, and it is subtly wrong (CR-02). Mitigations: ANA-007 as a default warning with a
   concrete two-message counterexample, a `fold_ordered` recipe in the error text, and simulator schedules
   that deliberately batch conflicting messages (TEST-003). The cost remains: this is the angle's central
   trade-off.
2. **`emit` versus `next` into persistent state is a trap.** E2's `seen` shows it: `emit seen` plus `not seen`
   in one tick silently disables delivery, and the program still stratifies. The lint has to follow negation
   through views transitively to catch every instance. A stricter rule, forbidding `emit` into a relation the
   same handler negates, would reject legitimate same-tick patterns.
3. **The event/standing classification is global.** A change far away, such as making a view read a table, can
   turn an `on` into an error somewhere else. The error must print the derivation chain that made the
   relation standing. If this proves too brittle in practice, it can be downgraded to a warning with errors only
   under `--strict`, as ODD-10 (c) does for other stream properties.
4. **Rule explosion is hidden.** One handler with `outer`, `any` and nested `if`s can become many rules: each
   `outer` doubles its statements, and each `any` alternative multiplies them. Provenance and coverage report
   rule labels (`h.3a`, `h.3b`) that the user never wrote. CSE of shared bodies keeps runtime cost down, but
   explanations get longer.
5. **Choreographies can scatter again.** Reopenable `at` sections let a protocol read in message order, which is
   the point, but a large protocol can end up with one role's state spread across many sections. The formatter
   should offer both groupings (by role, by message), and the LSP should show the projected module for a role.
6. **Positional labels are unstable.** Unlabelled handlers are numbered by position, and seeded-choice site ids
   come from labels. Inserting a handler above an unlabelled `choose` changes its seeds, and with them replay.
   The lint asks for labels, but only where a seed-dependent site exists. Requiring a label on every handler
   would be safer, and noisier.
7. **Lattice reads are verbose.** `current_term(ct), let t0 = reveal(ct)` is the price of visibility. The E3
   `eff` view shows it. Common patterns (the largest term known) may deserve library views.
8. **The spec time model has three modes.** EOT for `pre`/`post`, every state for `invariant`, and `ever`/`at`
   for history. Writing a state invariant when a history invariant was meant is easy. E10's
   `election_safety` needs `ever`, because `won` is volatile and disappears on restart. The checker should warn
   when an invariant mentions a volatile relation without `ever`.
9. **The seal protocol costs memory.** The producer keeps a log of everything it sent on a sealed channel until
   Edelweiss reclaims it (ANA-063). The count digest detects loss and over-send by honest nodes only. A
   `sealed by (k) digest hash` option would add a content hash for adversarial or buggy producers, at CPU cost.
10. **`Session` versus `Node` senders.** The type of `from c` depends on the channel's declared source role. A
    channel accepting `A | external` gives a union type, and this proposal does not say how a handler
    discriminates it. Suggested resolution: split such channels, or bind with a `match` on
    `Sender::Node(n) | Sender::Session(s)`.

### 5.3 Ambiguities left open, with the default this proposal assumes

1. **`reveal` of a possibly-⊥ value.** A generator binds only non-⊥ cells, so `reveal` there yields the
   carrier type (`u64` for `lmax<u64>`). A lookup (`votes[t]`) or a `cell` read may be ⊥. Default: for numeric
   lattices, `reveal` of a maybe-⊥ value has type `WithBot<T>` (`Bot | Val(T)`); for collection lattices it is
   the empty collection. This needs flow-sensitive typing of "known non-⊥", which is simple for generators and
   lookups but must be specified for `match` arms and function returns.
2. **Head deduplication for `index()` when the `by` key is not a head column** (E7 `ranked`). Default: the key
   expression is added to the deduplicated head projection, and the compiler errors if it is not functionally
   determined by the head columns (ANA-080), because the rank would otherwise be ill-defined.
3. **`on` with an event that only appears under `not`** (`on tick, not x(…)`). A positive event atom is
   required, so a negated event does not count. The timer in E4's `re_prepare` is positive, so this is only a
   question of how to phrase the error.
4. **Output interfaces written by `while`** (E5 `committed`, E7 reports) re-emit at every tick in which the
   node ticks. Hosts that want one notification should subscribe to deltas (LANG-185). An `output … once`
   modifier, lowering to `inserted`, would make the intent explicit. It is not included, to keep one mechanism.
5. **Interposition on outputs.** The direction of `outside`/`inside` flips between inputs and outputs. That is
   consistent ("outside" is always what the rest of the program sees), but it is easy to misread. An
   alternative spelling is `interpose a.i as (from_callers, to_component)` for inputs and
   `(from_component, to_callers)` for outputs.
6. **Named atoms are open by default.** Omitted fields are wildcards in bodies. In heads every field without a
   default is required. A misspelled field is an error, but a forgotten join field silently widens the match.
   A lint could require `..` to acknowledge omitted fields in atoms over relations with more than N columns.
7. **`let` scoping is textual while statements are unordered.** Moving a statement above the `let` it uses is a
   scope error, not a change of meaning. That is the intended reading, but it sits uneasily with "order carries
   no meaning". The alternative, handler-wide `let` scope regardless of position, was rejected because it makes
   shadowing confusing.
8. **Static membership.** `Participant.size`, `by_hash` and `majority(s, Server)` read static role membership
   (ODD-21 (c)). With the epoch-based dynamic membership library, role sets become epoch-indexed relations and
   these expressions need an epoch argument. That syntax is not designed here.
9. **`final` has two uses.** It is an output modifier (`output final r`) and a clause (`final r(x)`). Both mean
   "final" in the SEM-016 sense, but a reader may expect `final` in a body to be a declaration.
10. **The Molly frontend and Blossom specs coexist.** A `.ded` program's `pre`/`post` rules map to a Blossom
    `spec`. When both exist for one program, the rule for which one the LDFI gate uses is left to the tooling
    design.
