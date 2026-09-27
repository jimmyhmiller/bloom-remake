# Blossom surface syntax, proposal C: "Modernized Datalog"

Status: design proposal (angle C of the syntax bake-off). Normative inputs: `docs/DECISIONS.md`,
`docs/research/FEATURES.md` §1 (CR-xx) and §2 (LANG-xxx). Nothing here overrides a CR; where this proposal has to
bend a LANG item, §5 says so.

Contents:

1. Design philosophy and overview
2. Lexical structure and full EBNF grammar
3. Every construct, with an example and its exact Dedalus lowering
4. The required example corpus, E1–E10
5. Self-critique

---

## 1. Design philosophy and overview

### 1.1 Philosophy

Blossom C keeps Datalog's central sentence, `head :- body.`, so a Soufflé, Dedalus, Logica or Molly programmer can
read a Blossom program on first sight. Around that sentence it changes what 2026 programmers expect to be
changed:

- **Time is a word, not a suffix.** Dedalus writes `p(X)@next :- …` and `p(X)@async :- …`. Blossom writes
  `next p(x) :- …` and `async p(@n, x) :- …`. The rule kind is the first token of the rule, so the three
  Dedalus rule kinds, and the two deferred mutations (`delete`, `upsert`), are visible at the left margin. A reader
  scanning a file sees which rules happen *now*, *next tick* and *somewhere else later*.
- **Schemas are declared, typed and keyed.** `table kv(key: str -> val: bytes)` states the key with the
  functional-dependency arrow. Lattice-typed columns are ordinary columns, and they merge.
- **Names beat positions.** Atoms are positional (`vote(t, v)`) or by name (`vote(term: t, voter: v, ..)`), and the
  named form puns (`vote(term, ..)` binds variable `term` to field `term`).
- **Aggregates are expressions.** `count{ v | vote(term: t, voter: v, ..) }`, `max{ x | p(x) }`, and a pipeline form
  `… |> group by w { n = count() }` for classic GROUP BY. Relational aggregates and lattice folds are spelled
  differently (`max{…}` vs `lmax{…}`), because one is non-monotone and the other is not.
- **CALM is legible.** Every operator that can break monotonicity is a keyword from one short list: `not`,
  `optional`, `group`, relational aggregates, `choose`, `index()`, `seq()`, `fold`, `reveal`, `inserted`,
  `removed`, `delete`, `upsert`, `resolve`. Everything else (joins, `let`, `for`, lattice merges, morphisms,
  thresholds) is monotone. A `monotone` modifier on a rule, block, module or output turns "I believe this is
  coordination-free" into a compile-time check. The compiler's points-of-order report points at exactly these
  keywords.
- **Nothing is hidden.** Rule bodies contain no closures and no ambient effects (LANG-002, CR-18). `now()`,
  `random()` and `rand(k…)` are per-tick sampled inputs. The textual order of rules never matters (LANG-001).
- **Parser-friendly.** Items start with a keyword or with `ident (`/`ident :`; expressions are Pratt-parsed; the
  whole grammar is LL(2) at the item level with no backtracking. Every rule ends with a Prolog-style end-dot, and
  every block ends with `}`, so the parser can resynchronize after an error at the next `.` or `}`.

The lowering target is the Dedalus core IR (ENG-001). Every surface construct in §3 is shown with the Dedalus rules
it becomes. Syntax sugar never adds semantics that the IR cannot state.

### 1.2 Names at a glance

| Concept | Blossom C spelling |
|---|---|
| Deductive rule (Dedalus `:-`, Bloom `<=`) | `p(x) :- q(x).` |
| Inductive rule (`@next`, `<+`) | `next p(x) :- q(x).` |
| Async rule (`@async`, `<~`) | `async c(@n, x) :- q(x), peer(n).` |
| Deferred delete (`<-`) | `delete p(x) :- gone(x).` |
| Deferred upsert (`<+-`) | `upsert reg(k, v) :- set(k, v).` |
| Fact / timestamped fact | `edge(1, 2).` / `bcast(@A, "hi") at 1.` |
| Bootstrap | `bootstrap { … }` (every incarnation), `bootstrap fresh { … }` (first start only) |
| Named rule | `grant: next voted_for(t, c) :- grant(t, c).` |
| Collections | `table`, `durable table`, `scratch`, `channel`, `loopback channel`, `input`, `output`, `static`, `soft table … ttl … max …`, `sealed table`, `range table … range(col)`, `cell`, `timer` |
| Key | `table kv(key: str -> val: bytes)` (FD arrow; no arrow = all columns are key) |
| Negation | `not p(x)`, `not { p(x, y), y > 3 }` |
| Let / unnest | `let n = len(s)`, `for w in words(line)` |
| Outer join | `optional q(x, y)` (binds `y: Option<T>`) |
| Aggregates | `count{…}`, `sum{…}`, `max{…}`, `argmin{…}`, … and `\|> group by g { a = sum(x) }` |
| Aggregate with default (LANG-106) | `count{ v \| vote(t, v) } default 0` |
| Choice | `choose k -> v`, `choose k -> v least c`, `choose sticky k -> v`, `choose random k -> v` |
| Numbering / ordered fold | `index() per (g) by (k)`, `seq() by (k)`, `fold(init, step){ … } order by k` |
| Lattice column / cell | `table votes(term: u64 -> who: LSet<Node>)`, `cell cnt: LMax<u64>` |
| Lattice read | generator `votes(t, s)`; lookup `votes[t]`, `cnt[]`; threshold `s.size >= 3`; raw `reveal(s)` |
| Monotone lattice folds | `lset{…}`, `lmax{…}`, `lmin{…}`, `lbool{…}`, `lmap{…}` |
| Delta pseudo-relations | `inserted r(x)`, `removed r(x)` |
| Seals | `seal c(@r, part: p) count k` (head), `sealed c(part: p)` (body) |
| Modules | `module M(…) implements P { … }`, `protocol P { … }`, `import M(…) as m`, `include`, `extends`, `override block`, `interpose m.r as orig { … }` |
| Choreography | `choreography C { role a: process  role b: cluster  on a { … } on b { … } }` |
| Constants / params | `const QUORUM: u64 = 3`, `param RETRY: Duration = 2s` |
| Principals | `req(…) from s principal p`, `channel c(…) accept from external` |
| Versions | `program kv version 3`, `#n` field numbers, `since 3`, `migrate from 2 { … }`, `translate c to 2 { … }` |
| Specs | `spec S for M { nodes …  faults { … }  pre/post rules  invariant name: never … }` |

Source files use `.bls`. Dedalus/Molly files keep `.ded` and go through the compatibility frontend (LANG-220).

---

## 2. Lexical structure and grammar

### 2.1 Lexical structure

**Encoding and whitespace.** Source is UTF-8. Whitespace separates tokens and is otherwise insignificant, except in
the end-dot rule below.

**Comments.** `// …` to end of line; `/* … */`, which nests; `/// …` doc comments, which attach to the next item.
`#` is **not** a comment in `.bls` files (it introduces field numbers, §2.3); `#` comments are accepted by the
`.ded` and Overlog frontends, where they occur in practice (see §5, deviation D1).

**Identifier classes.** The lexer classifies identifiers by case. The class decides the role, so there is never a
"is this a variable or a constant?" question.

| Class | Regex | Used for |
|---|---|---|
| `lower` | `[a-z][A-Za-z0-9_]*` or `_[A-Za-z0-9_]+` | variables, relations, fields, functions, instance aliases, roles, rule labels |
| `Upper` | `[A-Z][A-Za-z0-9]*` containing a lowercase letter, or a single capital letter | types, lattices, modules, protocols, choreographies, enum variants |
| `CONST` | `[A-Z][A-Z0-9_]+` containing no lowercase letter | constants, parameters |
| `_` | `_` | wildcard |

Node constants in specs and tests (`A`, `Coord`) are `Upper` identifiers declared by `nodes`; programs refer to
nodes through `self`, membership relations, received `from` columns and `const`/`param` values.

So `x`, `vote_resp`, `bed` are `lower`; `LSet`, `Role`, `Follower`, `T` are `Upper`; `QUORUM`, `RETRY_MS` are
`CONST`. A `lower` identifier in term position is always a variable. Relations and functions share one namespace per
module, so a variable can never be mistaken for a relation: a relation only appears as `name(` or `name[` in atom or
lookup position.

**Reserved keywords.**

```
module protocol choreography implements extends include import as use override block bootstrap on role
table scratch channel input output cell static durable loopback soft sealed range timer
type enum lattice aggregate extern fn const param program
next async delete upsert temp not optional inserted removed seal choose fold reveal group
let for in if then else match where and or true false self
interpose final monotone nondet trusted spec invariant never always once migrate translate
```

**Contextual keywords** (keywords only in the listed position, ordinary identifiers elsewhere): `from`,
`principal`, `at`, `by`, `per`, `order`, `desc`, `asc`, `default`, `least`, `most`, `sticky`, `random`, `count`
(after a `seal` head), `ttl`, `max`, `every`, `after`, `times`, `ticks`, `delivery`, `accept`, `partition`,
`resolve`, `via`, `exactly_once`, `materialized`, `recompute`, `since`, `deprecated`, `semantics_changed`,
`reserved`, `version`, `to`, `process`, `cluster`, `external`, `rel`, `morphism`, `bimorphism`, `antitone`,
`stable`, `nodes`, `faults`, `expect`, `holds`, `fails`, `prove`, `using`, `release`, `atomic`, `localize`,
`unsafe_ungated`, `weight`, `is`, `bag`, `zset`, `ring`, `distinct`, `exists`, `implies`, `induction`, `init`,
`step`, `merge`, `finish`, `properties`, `lossy`, `reliable`, `ordered`, `lossy_forever`, `producer`, `like`,
`unsafe`, `threshold`, `index`, `seq`, `ticks`, `snapshot`, `of`, `progress`, `upto`, `mode`, `committed_only`, `include_tentative`,
`estimate`, `liveness`, `eventually`, `within`, `eff`, `sent`, `handler`, `fresh`. After a `.` (member position) every identifier or keyword is a member name, so
`m.at(k)` and `log.count` are legal.

**Literals.**

```
INT       ::= DEC | HEX | BIN                      (underscores allowed: 1_000_000)
DEC       ::= [0-9][0-9_]* [INTSUFFIX]
HEX       ::= 0x[0-9A-Fa-f_]+ [INTSUFFIX]
BIN       ::= 0b[01_]+ [INTSUFFIX]
INTSUFFIX ::= u8|u16|u32|u64|u128|i8|i16|i32|i64|i128|r160|r256      (rN: N-bit ring id, LANG-026)
FLOAT     ::= DEC '.' [0-9]+ [(e|E)[+-]?[0-9]+]    (a '.' followed by a digit is part of the number)
DURATION  ::= DEC (ns|us|ms|s|m|h|d)                 e.g. 150ms, 2s, 1h
STRING    ::= '"' { char | escape } '"'              escapes: \n \t \r \\ \" \0 \u{hex}
RAWSTRING ::= 'r"' … '"' | 'r#"' … '"#'
BYTES     ::= 'b"' { byte | escape } '"'
BOOL      ::= true | false
```

A number immediately followed by letters that are not a valid suffix is a lexical error ("unknown numeric suffix
`kb`"), never an identifier.

**Punctuation and operators.**

```
:-  ->  =>  |>  ..  ..=  ::  @  .  ,  :  ;  (  )  [  ]  {  }  =
==  !=  <  <=  >  >=  +  -  *  /  %  **  ++  &  |  ^  ~  <<  >>  !  \/  ⊔
```

`\/` and `⊔` are the same token (lattice join). `;` is used only inside `faults { … }` style option lists as an
alternative separator; it is never a rule terminator.

**The end-dot rule.** A `.` is an `END` token (rule terminator) when it is followed by whitespace, end of file, or
the start of a comment. Otherwise it is a `DOT` (member access, qualified name). `..` and `..=` are lexed greedily
before this check, and a `.` followed by a digit inside a number is part of the number. This is Prolog's rule; it
lets `p(x) :- q(x), n > s.size.` end correctly while `a.b.rel(x)` and `s.size` stay member access.

### 2.2 Precedence (Pratt table)

From loosest to tightest binding. All binary operators are left-associative unless stated.

| Level | Operators | Notes |
|---|---|---|
| 1 | `if c then a else b`, `match e { … }` | prefix forms, extend as far right as possible |
| 2 | `or` | boolean |
| 3 | `and` | boolean |
| 4 | `==` `!=` `<` `<=` `>` `>=` `in` | non-associative; on lattice types `<=`/`>=` are ⊑/⊒ (§3.5) |
| 5 | `\/` (`⊔`) | lattice join |
| 6 | `\|` | bitwise or (disabled at depth 0 of an aggregate element, §2.3) |
| 7 | `^` | bitwise xor |
| 8 | `&` | bitwise and |
| 9 | `<<` `>>` | shifts |
| 10 | `..` `..=` | ranges, non-associative |
| 11 | `+` `-` `++` | `++` is string/list concatenation |
| 12 | `*` `/` `%` | |
| 13 | `**` | right-associative |
| 14 | `as` | numeric cast, `x as i64` |
| 15 | prefix `-` `!` `~` | `!` is boolean not |
| 16 | postfix `.m`, `.m(args)`, `(args)`, `[args]`, `::V` | member, method, call, lookup, variant path; a zero-argument method may drop its parentheses (`s.size`) |

### 2.3 Grammar (EBNF)

Notation: `{ x }` is zero or more, `[ x ]` is optional, `|` separates alternatives, `'x'` is a terminal. `lower`,
`Upper`, `CONST`, `INT`, `STRING`, `END` are tokens from §2.1. `sep(X, s)` abbreviates `X { s X } [ s ]`
(a trailing separator is allowed).

```ebnf
(* ---------- files and items ---------- *)
File          = [ ProgramDecl ] { UseDecl } { Item } ;
ProgramDecl   = 'program' lower 'version' INT ;
UseDecl       = 'use' Path [ '::' '{' sep(Upper, ',') '}' ] ;          (* brings definitions into scope *)
Path          = lower { '::' lower } [ '::' Upper ] ;
Item          = TypeDecl | EnumDecl | LatticeDecl | GroupDecl | AggregateDecl | ExternDecl
              | ConstDecl | ParamDecl | ModuleDecl | ProtocolDecl | ChoreoDecl
              | SpecDecl | MigrateDecl | TranslateDecl | ModuleItem ;

(* ---------- types ---------- *)
Type          = Upper [ '<' sep(TypeArg, ',') '>' ]                      (* Option<T>, LMap<K, LMax<u64>> *)
              | PrimType | '(' [ sep(Type, ',') ] ')'                    (* tuple; () is unit *)
              | lower ;                                                  (* a role name used as a Node refinement *)
TypeArg       = Type | INT ;                                             (* Ring<160> *)
PrimType      = 'bool' | 'str' | 'bytes' | 'u8' | 'u16' | 'u32' | 'u64' | 'u128'
              | 'i8' | 'i16' | 'i32' | 'i64' | 'i128' | 'f64' ;          (* lowercase prim names are reserved in type position *)
TypeDecl      = 'type' Upper [ TypeParams ] '=' ( Type | RecordType ) ;
RecordType    = '{' sep(FieldDecl, ',') '}' ;
TypeParams    = '<' sep(Upper, ',') '>' ;
EnumDecl      = 'enum' Upper [ TypeParams ] '{' sep(Variant, ',') '}' ;
Variant       = Upper [ RecordType | '(' sep(Type, ',') ')' ] { FieldAnn } | 'unknown' ;
FieldDecl     = [ '@' ] lower ':' Type [ '=' Expr ] { FieldAnn } ;
FieldAnn      = FieldNum | 'since' INT | 'deprecated' 'since' INT | 'semantics_changed' 'since' INT ;
FieldNum      = '#' INT ;

(* ---------- lattices, groups, aggregates, host functions ---------- *)
LatticeDecl   = 'lattice' Upper [ TypeParams ]
                ( '=' Type                                                (* composition of built-ins: verified *)
                | '{' { LatField } { LatMethod } '}'                     (* product DSL: verified merge *)
                | 'extern' STRING '{' { LatMethodSig } '}' ) ;           (* Rust Merge impl: law-tested *)
LatField      = lower ':' Type ',' ;
LatMethod     = LatMethodSig '=' Expr ;
LatMethodSig  = [ MethodClass ] 'fn' lower '(' 'self' { ',' Param } ')' '->' Type [ 'after' lower ] ;
MethodClass   = 'morphism' | 'bimorphism' | 'monotone' | 'antitone' | 'stable' ;   (* none = non-monotone *)
Param         = lower ':' Type ;
GroupDecl     = 'group' Upper [ TypeParams ] '=' Type [ 'ring' ] ;       (* LANG-142 *)
AggregateDecl = 'aggregate' lower '(' sep(Param, ',') ')' '->' Type '{'
                  'init' '=' Expr
                  'step' '(' lower ',' sep(lower, ',') ')' '=' Expr
                  [ 'merge' '(' lower ',' lower ')' '=' Expr ]
                  [ 'finish' '(' lower ')' '=' Expr ]
                  [ 'properties' sep(lower, ',') ]
                '}' ;
ExternDecl    = 'extern' ( 'fn' lower '(' [ sep(Param, ',') ] ')' '->' Type { lower }   (* pure, monotone, injective … *)
                         | 'table' 'fn' lower '(' [ sep(Param, ',') ] ')' '->' '(' sep(Param, ',') ')'
                         | 'service' lower '(' [ sep(Param, ',') ] ')' '->' '(' sep(Param, ',') ')' )
                'is' STRING ;                                            (* host path *)
ConstDecl     = 'const' CONST ':' Type '=' Expr ;
ParamDecl     = 'param' CONST ':' Type '=' Expr ;                        (* deploy-time overridable *)

(* ---------- modules ---------- *)
ModuleDecl    = [ Trust ] 'module' Upper [ ModParams ] [ 'implements' sep(Upper, ',') ]
                [ 'extends' Upper ] '{' { ModuleItem } '}' ;
Trust         = 'trusted' '(' STRING ')' ;
ModParams     = '(' sep(ModParam, ',') ')' ;
ModParam      = CONST ':' Type [ '=' Expr ]                             (* value parameter *)
              | lower ':' 'rel' '(' [ FieldList ] ')'                    (* relation parameter *)
              | Upper ':' Upper [ '=' Upper ] ;                          (* module parameter of a protocol type *)
ProtocolDecl  = 'protocol' Upper [ ModParams ] '{' { InterfaceDecl | ConstSig } '}' ;
ConstSig      = 'const' CONST ':' Type ;
ChoreoDecl    = 'choreography' Upper [ ModParams ] '{' { RoleDecl | ModuleItem | OnBlock } '}' ;
RoleDecl      = 'role' lower ':' ( 'process' | 'cluster' | 'external' ) ;
OnBlock       = 'on' lower '{' { ModuleItem } '}' ;

ModuleItem    = RelDecl | CellDecl | TimerDecl | ImportDecl | IncludeDecl | InterposeDecl
              | BlockDecl | BootstrapBlock | ConstDecl | ParamDecl | TypeDecl | EnumDecl
              | InvariantDecl | SnapshotDecl | Rule ;
SnapshotDecl  = 'snapshot' lower 'of' QualName 'at' 'progress'
                ( 'every' Expr 'upto' Expr | '(' sep(Expr, ',') ')' )
                [ 'mode' ( 'committed_only' | 'include_tentative' ) ] [ 'estimate' lower ] ;   (* LANG-139 *)
ImportDecl    = 'import' Upper [ '(' sep(ModArg, ',') ')' ] 'as' lower ;
ModArg        = ( CONST | lower | Upper ) ':' ( Expr | QualName | Upper ) ;
IncludeDecl   = 'include' ( Upper | STRING ) ;
BlockDecl     = [ 'override' ] 'block' lower '{' { ModuleItem } '}' ;
InterposeDecl = 'interpose' QualName 'as' lower '{' { Rule } '}' ;
BootstrapBlock= 'bootstrap' [ 'fresh' ] '{' { Rule } '}' ;

(* ---------- relation declarations ---------- *)
RelDecl       = RelKind lower '(' [ FieldList ] ')' { RelClause } ;
RelKind       = [ 'durable' | 'soft' | 'sealed' | 'range' ] 'table'
              | 'scratch' | [ 'loopback' ] 'channel' | 'input' | [ 'final' | 'atomic' ] 'output'
              | 'static' | 'zset' 'table' | 'bag' 'table' ;
InterfaceDecl = ( 'input' | 'output' ) lower '(' [ FieldList ] ')' { RelClause } ;
FieldList     = sep(FieldDecl, ',') [ '->' sep(FieldDecl, ',') ]
              | '->' sep(FieldDecl, ',')                                 (* empty key: a register *)
              | 'like' QualName ;                                        (* reuse another relation's schema *)
CellDecl      = [ 'scratch' | 'durable' ] 'cell' lower ':' Type ;
RelClause     = 'ttl' Expr | 'max' Expr | 'range' '(' lower ')'
              | 'partition' 'by' Expr
              | 'delivery' ( 'lossy' | 'reliable' | 'ordered' | 'lossy_forever' )
              | 'accept' 'from' sep(AclTerm, '|')
              | 'seal' 'on' '(' sep(lower, ',') ')' [ 'per' 'producer' ]
              | 'exactly_once' '(' lower ')'
              | 'resolve' ResolvePolicy
              | 'materialized' | 'recompute'
              | 'nondet' '(' STRING ')'
              | 'unsafe' '(' STRING ')'                                  (* required to use DomPair, LANG-136 *)
              | 'handler' STRING                                         (* output-event host handler, LANG-186 *)
              | 'reserved' sep(FieldNum, ',') ;
AclTerm       = lower | 'external' | 'principal' 'in' lower ;
ResolvePolicy = 'choose' [ 'sticky' ] | 'choose' 'random' [ 'sticky' ]
              | ( 'least' | 'most' ) '(' sep(lower, ',') ')' | 'merge' ;
TimerDecl     = 'timer' lower ( 'every' Period [ 'times' Expr ] | 'after' Period | 'once' ) ;
Period        = Expr                                                     (* a Duration: physical timer *)
              | Expr 'ticks' ;                                           (* a tick count: logical timer *)

(* ---------- rules ---------- *)
Rule          = { RuleMod } [ lower ':' ] [ HeadKind ] Head [ ':-' Body ] [ 'at' Expr ] END ;   (* modifiers, label, kind *)
RuleMod       = 'monotone' | 'temp' | 'localize' | 'nondet' '(' STRING ')' | 'unsafe_ungated' '(' STRING ')' ;
HeadKind      = 'next' | 'async' | 'delete' | 'upsert' [ 'resolve' ResolvePolicy ] ;
Head          = Atom [ 'weight' Expr ]
              | 'seal' Atom 'count' Expr ;
Atom          = QualName '(' [ sep(AtomArg, ',') ] ')' ;
QualName      = lower { '.' lower } ;                                    (* inst.sub.rel *)
AtomArg       = [ '@' ] Pattern                                          (* positional *)
              | lower ':' Pattern                                        (* named *)
              | '@' lower ':' Pattern                                    (* named location field *)
              | lower                                                    (* pun, named mode only *)
              | '..' ;                                                   (* rest wildcard, named mode only *)
Body          = Conj { '|>' Stage } ;
Conj          = sep(Literal, ',') [ 'where' sep(Expr, ',') ] ;
Literal       = Expr { AtomSuffix }                                      (* unmarked: an atom or a guard, see below *)
              | 'not' ( Atom { AtomSuffix } | 'sealed' Atom | '{' Conj '}' )
              | 'sealed' Atom
              | 'optional' Atom
              | ( 'inserted' | 'removed' ) Atom
              | 'once' Atom { AtomSuffix }                               (* spec rules only *)
              | 'let' Pattern '=' Expr
              | 'for' Pattern 'in' Expr
              | 'choose' [ 'sticky' ] [ 'random' ] VarTuple '->' VarTuple [ ( 'least' | 'most' ) VarTuple ] ;
AtomSuffix    = 'from' Pattern | 'principal' Pattern | 'at' Expr ;
VarTuple      = lower | '(' [ sep(lower, ',') ] ')' ;
Stage         = 'where' sep(Expr, ',')
              | 'let' Pattern '=' Expr
              | 'group' [ 'by' ( sep(lower, ',') | '(' [ sep(lower, ',') ] ')' ) ] '{' sep(AggBind, ',') '}' ;
AggBind       = lower '=' AggCall ;
AggCall       = lower '(' [ [ 'distinct' ] sep(Expr, ',') ] ')' [ 'order' 'by' OrderKeys ] ;

(* ---------- expressions ---------- *)
Expr          = PrattExpr ;                                              (* precedence table §2.2 *)
Primary       = Literal_ | lower | CONST | 'self' | '_'
              | QualName '(' [ sep(AtomArg, ',') ] ')'                   (* call, or an atom in literal position *)
              | QualName '[' [ sep(Expr, ',') ] ']'                      (* lookup *)
              | AggExpr
              | 'reveal' '(' Expr ')'
              | Numbering                                                (* head columns only *)
              | Upper [ '::' Upper ] [ '(' sep(Expr, ',') ')' | '{' sep(FieldInit, ',') '}' ]
              | '(' [ sep(Expr, ',') ] ')'                               (* parenthesized / tuple / unit *)
              | '[' [ sep(Expr, ',') ] ']'                               (* list *)
              | '{' [ sep(Expr, ',') ] '}'                               (* set; {} is the empty set *)
              | '{' ( '=>' | sep(Expr '=>' Expr, ',') ) '}'              (* map; {=>} is the empty map *)
              | '[' Expr 'for' Pattern 'in' Expr [ 'if' Expr ] ']'       (* list comprehension over a value *)
              | 'ring' ( '(' | '[' ) Expr ',' Expr ( ')' | ']' )         (* ring interval, LANG-026 *)
              | 'if' Expr 'then' Expr 'else' Expr
              | 'match' Expr '{' sep(Pattern '=>' Expr, ',') '}' ;
FieldInit     = lower ':' Expr | lower ;
Numbering     = [ 'durable' ] ( 'index' | 'seq' ) '(' ')'                (* LANG-097/098; `index`, `seq` contextual *)
                [ 'per' '(' sep(Expr, ',') ')' ] [ 'by' '(' OrderKeys ')' ] [ 'release' ] ;
AggExpr       = lower [ '(' sep(Expr, ',') ')' ] '{' [ AggElem '|' ] Conj '}'
                [ 'order' 'by' OrderKeys ] [ 'default' Expr ] ;
AggElem       = [ 'distinct' ] Expr ;                                    (* parsed with `|` disabled at depth 0 *)
OrderKeys     = sep(Expr [ 'asc' | 'desc' ], ',') ;
Pattern       = '_' | lower | Literal_ | CONST | '(' sep(Pattern, ',') ')'
              | Upper [ '::' Upper ] [ '(' sep(Pattern, ',') ')' | '{' sep(FieldPat, ',') [ ',' '..' ] '}' ]
              | Expr ;                                                   (* an expression pattern is an equality test *)
FieldPat      = lower ':' Pattern | lower ;
Literal_      = INT | FLOAT | DURATION | STRING | RAWSTRING | BYTES | 'true' | 'false' ;

(* ---------- specs, invariants, versions ---------- *)
InvariantDecl = 'invariant' lower ':' ( 'never' Conj | 'always' Formula ) END ;
Formula       = 'forall' sep(Param, ',') ':' Formula
              | 'exists' sep(Param, ',') ':' Formula
              | Formula ( 'implies' | 'and' | 'or' ) Formula
              | 'not' Formula | '(' Formula ')'
              | 'sent' Atom [ 'from' Pattern ]                           (* the message is in the network *)
              | Atom | Expr ;
SpecDecl      = 'spec' Upper 'for' Upper [ '(' sep(ModArg, ',') ')' ] '{' { SpecItem } '}' ;
SpecItem      = 'nodes' sep(Upper, ',')                                   (* node constants *)
              | 'faults' '{' sep(FaultEntry, ( ',' | ';' )) '}'
              | 'expect' ( 'holds' | 'fails' )
              | 'prove' lower 'by' 'induction' [ 'using' '{' { InvariantDecl } '}' ]
              | 'liveness' lower ':' 'eventually' Conj 'within' Expr 'after' ( 'eff' | Expr ) END
              | InvariantDecl | RelDecl | Rule | ConstDecl ;
FaultEntry    = lower ':' ( Expr | 'async' ) ;                           (* keys: eot eff crashes model delay round; model: sync | async *)
MigrateDecl   = 'migrate' 'from' INT '{' { Rule } '}' ;
TranslateDecl = 'translate' lower ( 'to' | 'from' ) INT '{' { Rule } '}' ;
```

**Literal classification (the one non-LL step, done after parsing, not by backtracking).** A body literal that is not
introduced by a keyword is parsed as an expression. If the resulting tree is a bare application `q.r(args)` (no
operator applied to it) it is an **atom**; otherwise it is a **guard** and must have type `bool`. An `AtomSuffix`
(`from`, `principal`, `at`) after a guard is an error. After a keyword that demands an atom (`not`, `sealed`,
`optional`, `inserted`, `removed`, `once`, `seal`) the parser reads `Atom` directly. `AtomArg` forms
that only make sense in atoms (`@`, `name:`, `..`, puns) are parsed everywhere and rejected in real function calls
by the resolver. Parenthesized `or` / `and` inside a literal are classified the same way: if any operand is an atom,
the literal is a *disjunction of conjunctions* and splits into several rules (§3.3.6); otherwise it is a boolean
guard. Inside parentheses conjunction is written `and`, because `,` inside parentheses builds a tuple.

**Named mode.** An atom is in *named mode* if any argument is `name: p`, `..`, or `@name: p`. In named mode a bare
`lower` argument is a pun for `lower: lower`, and every field that is not mentioned is a wildcard only if `..` is
present (otherwise a missing field is an error). In positional mode every column must be given, `_` included. Heads
are never allowed `..`; a head in named mode must give every field that has no declared default.

**Error recovery.** Synchronizing tokens are `END`, a `}` that closes the current block, and any reserved keyword
that can start an item when it appears at the start of a line. The parser records the error, skips to the next
synchronizing token, and continues. The end-dot rule makes a forgotten `.` detectable ("a new rule seems to start
here: `next …` at line 12").

**Parsing complexity.** Items are LL(1) on their first keyword, except that a rule may begin with a label
(`lower ':'`), which needs a second token of lookahead: `lower ':'` is a label, `lower '('` or `lower '.'` is a head.
Expressions are Pratt. Nothing needs backtracking.

---

## 3. Constructs and their Dedalus lowering

### 3.0 The Dedalus IR notation used in this section

Lowerings are written in the textual form of the Dedalus core IR (ENG-001). Conventions:

- IR variables are Capitalized, to keep IR listings visually distinct from surface code.
- Every IR relation has the **location as its first column** (CR-14). In a local rule every atom shares one location
  variable, written `L`. The executing node is `L`; `self` lowers to `L`.
- Rule kinds: `h(L, …) :- body.` is deductive; `h(L, …)@next :- body.` is inductive; `h(D, …)@async :- body.` is
  async, where `D` is the destination and the body is evaluated at `L`. Time is implicit, as in Dedalus sugar.
- `notin r(…)` is negation. IR head aggregates `h(L, Ḡ, agg<E>) :- b.` fold `E` over the **rows** of the body, where
  the body's rows are its distinct valuations (set semantics, CR-03). `count<*>` counts rows.
- Lattice relations use Dedalus^L notation (R11/G1 §3): `R(L, k̄; e)` as a head merges `e` into the cell; as a body
  atom it is a *generator* that ranges over non-⊥ cells (N4); `X = R[L, k̄]` is a *lookup* that returns ⊥ for an
  absent cell (N5).
- Per-tick built-in inputs: `now(L, T)`, `tick(L, N)`; `boot(L)`, which holds only at the first tick of an
  incarnation (tick 0 of a fresh node, the first tick after a restart, SEM-071); and `recovered(L)`, which holds
  at that tick only if durable state was reloaded. `rand(k̄)` is the
  pure keyed PRF of SEM-084, implicitly keyed by node, incarnation and tick.
- `.decl` lines are IR metadata, not rules: columns, key, storage class (`durable`, `static`, `channel`, `input`,
  `output`), value lattice. `error(Code, …)` is the engine's hard-error pseudo-relation: deriving it aborts the tick
  with a located message.
- Compiler-generated relations contain `$`, which user identifiers cannot, so generated names never collide.

Every lowering below is *the* meaning of the construct (CR-26: the engine must be observationally identical to naive
evaluation of this expansion). Engines are free to implement it natively (ENG-003, ENG-004).

### 3.1 Declarations and collection kinds

#### 3.1.1 Types, schemas and keys (LANG-020–028, CR-28)

```blossom
type ReqId = u64
type Line = { item: str, action: Action, n: u32 }
enum Action { Add, Remove, unknown }
enum Msg { Put { key: str, val: bytes } #1, Get { key: str } #2, unknown }

table kv(key: str -> val: bytes)                 // key(key)
table edge(src: u32, dst: u32)                   // no arrow: every column is a key
table leader(-> node: Node)                      // empty key: a single-row register
table log(idx: u64 -> term: u64, cmd: bytes #3 since 2, note: Option<str> = None)
table kv_backup(like kv)                         // LANG-020: reuse kv's schema, key included
```

The `->` is the functional dependency *key → rest*. Scalar types are `bool`, `u8…u128`,
`i8…i128`, `f64` (not orderable as a lattice), `str`, `bytes`, `()`, `Node`, `Principal`, `Session`, `Duration`,
`Time`, `Ring<N>`; compound types are tuples, `List<T>`, `Set<T>`, `Map<K, V>`, `Option<T>`, records and enums. Every
type has one canonical total order (LANG-024) used by `<` on values, by `choose`, `index()` and every
order-sensitive aggregate. There is no null: a nullable column is `Option<T>` (LANG-025).

Lowering:

```
.decl kv(L: Node, Key: str, Val: bytes)  key(L, Key)
.decl edge(L: Node, Src: u32, Dst: u32)  key(L, Src, Dst)
.decl leader(L: Node, Node: Node)        key(L)
```

The key is enforced by the engine (SEM-050): two distinct tuples with the same key at one tick raise
`error(KeyConflict, kv, K, V1, V2)` naming both derivations, unless every differing column is lattice-typed, in which
case they merge (CR-07, CR-51). Field numbers `#n`, `since`, defaults and `deprecated`/`semantics_changed` are schema
metadata recorded in `schema.lock` (LANG-260/261); they do not produce rules.

#### 3.1.2 `table` (LANG-040, LANG-065)

```blossom
table member(n: Node)
member(n) :- add_member(n).
delete member(n) :- remove_member(n).
```

Lowering (the mutable-persistence rule, R02 §3.3):

```
member(L, N)@next :- member(L, N), notin member$del(L, N).
member(L, N)      :- add_member(L, N).
member$del(L, N)  :- remove_member(L, N).
```

A deductive rule into a table means "present now, and persisted from now on". `member$del` is tick-local and is read
only by the inductive persistence rule, so deletion never creates a same-tick cycle. Explicit Dedalus persistence is
also accepted verbatim (LANG-065): `scratch p(x: u32)` plus `next p(x) :- p(x), not p_del(x).` is legal, and the
compiler recognizes it as storage (ENG-003).

#### 3.1.3 `durable table` (LANG-044)

```blossom
durable table voted_for(term: u64 -> cand: Node)
```

Lowering: exactly the `table` rules, plus `.decl voted_for(…) key(L, Term) durable`. The `durable` attribute adds no
rule. It obliges the runtime to commit the tick's durable deltas (the staged `@next` inserts and `$del` deletes)
before releasing the tick's outbox (SEM-072), and puts the relation in the WAL, checkpoints and `schema.lock`
(DIST-020, DIST-081).

#### 3.1.4 `scratch` (LANG-041) and `temp` (LANG-047)

```blossom
scratch stale(id: u64)
stale(i) :- req(i, t), current_term(c) where t < c.
temp acked(d, i) :- buf(d, _, i, _), ack(_, d, i).        // declares and defines a scratch, schema inferred
```

Lowering: a scratch has no persistence rule, so it holds only at the tick in which it is derived (SEM-008):

```
stale(L, I) :- req(L, I, T), current_term(L, C), T < C.
acked(L, D, I) :- buf(L, D, _, I, _), ack(L, D, I).
```

A `next` into a scratch shows up only at the next tick (LANG-041). A `temp` name may not shadow a declared name.

#### 3.1.5 `channel` and `loopback channel` (LANG-042, LANG-046, LANG-150)

```blossom
channel ack(@src: Node, dst: Node, ident: u64)
async ack(@s, self, i) :- got(s, i).

loopback channel retry_later(@me: Node, id: u64)
async retry_later(@self, i) :- busy(i).
```

A channel has exactly one `@` field, in any position. Only `async` rules may derive into it (LANG-066); its received
contents are tick-local. Lowering normalizes the location to the first column:

```
.decl ack(L: Node, Dst: Node, Ident: u64) channel
ack(S, L, I)@async :- got(L, S, I).
.decl retry_later(L: Node, Id: u64) channel loopback
retry_later(L, I)@async :- busy(L, I).
```

On the receiver, `ack(_, d, i)` in a body reads the batch delivered at the current tick (whose location column is the
receiver). A `loopback` channel must be addressed to `self` (checked statically); it goes through the network path,
so it arrives at a later tick. The built-in `localtick()` is a nullary loopback: `next localtick() :- more_work().`
requests another tick without sending anything (LANG-046); it lowers to `localtick(L)@next :- more_work(L).`, and a
staged fact triggers the next tick (SEM-009).

#### 3.1.6 `input` and `output` (LANG-043, LANG-067, LANG-185)

```blossom
input put(id: u64 -> key: str, val: bytes)
output put_done(id: u64)
```

Lowering: `.decl put(…) input` and `.decl put_done(…) output`; no rules. Inputs are tick-local EDB facts fed by the
host or by an importing module; the host can only insert for a *future* tick (LANG-067). Outputs are tick-local
IDB relations exported to the host or to the importer. The host API (subscribe to contents or deltas, `sync_do`,
`async_do`, single-step `tick()`) lives outside the language (LANG-185). An `atomic output` releases its callbacks
only after the tick's durable commit (LANG-206). `final output` is §3.11.4.

#### 3.1.7 `static` (LANG-045, LANG-049, CR-16)

```blossom
static peers(n: Node)
peers(a). peers(b). peers(c).        // or supplied by the deployment configuration
```

Lowering: `.decl peers(…) static`, and each fact is a body-less deductive rule, true at every tick:

```
peers(L, a).  peers(L, b).  peers(L, c).
```

A static relation may be written only by facts and by deployment configuration; a rule with a static head is a
compile error. Every unannotated program fact holds at every tick (CR-16), in any kind of relation. Initial values of
mutable state therefore go in `bootstrap { … }` (§3.2.6), not in bare facts, and the compiler warns when a rule
deletes a fact that is re-asserted every tick.

#### 3.1.8 `soft table` (LANG-048, CR-17, SEM-060, SEM-061)

```blossom
soft table last_heard(peer: Node -> at: Time) ttl 3s max 1024
upsert last_heard(p, now()) :- heartbeat(..) from p.
```

Lowering. The store keeps a hidden birth column; visibility is a pure function of the tick's sampled `now`, so expiry
happens deterministically at tick boundaries (CR-17):

```
.decl last_heard$store(L, Peer, At, Birth)
% visible relation: alive under TTL, and among the MAX newest by (birth, canonical order)
last_heard$live(L, P, A, B) :- last_heard$store(L, P, A, B), now(L, T), T - B < 3s.
last_heard$n(L, count<*>)   :- last_heard$live(L, _, _, _).
last_heard$rk(L, P, A, I)   :- last_heard$live(L, P, A, B), I = index() by (B, P, A).   % LANG-097, oldest first
last_heard(L, P, A)         :- last_heard$rk(L, P, A, I), last_heard$n(L, N), I >= N - 1024.
% insertions (from `last_heard(…) :-` rules or from `upsert`) are written to last_heard$ins
last_heard$store(L, P, A, T)@next :- last_heard$ins(L, P, A), now(L, T).
last_heard$store(L, P, A, B)@next :- last_heard$store(L, P, A, B), now(L, T), T - B < 3s,
                                     notin last_heard$del(L, P, A), notin last_heard$ins(L, P, A).
% `upsert last_heard(p, x) :- body.` lowers through §3.2.5 into $ins and $del
```

Re-deriving an identical tuple is a refresh: it rewrites the birth time without being an insertion (SEM-060).
A soft head derived from soft body tuples is refreshed whenever its body is (SEM-061), which falls out of the rules
above because the head is re-derived. ANA-006's lints (head TTL ≥ every soft body TTL) apply.

#### 3.1.9 `sealed table` (LANG-049) and `range table` (LANG-050)

```blossom
sealed table config(k: str -> v: str)
bootstrap { config("replicas", "3"). }

range table acked(src: Node, seq: u64) range(seq)
```

A sealed table lowers to a `table` plus a whole-relation seal from tick 1 on:

```
config(L, K, V)@next :- config(L, K, V).          % no $del: deletes are rejected statically
config(L, "replicas", "3") :- boot(L).
config$sealed(L)@next :- boot(L).
config$sealed(L)@next :- config$sealed(L).
```

Any rule other than a `bootstrap` rule with head `config` is a compile error. `config$sealed` feeds the seal machinery
(§3.8.4), so reads of `config` never count as points of order. A range table has every column in its key and stores
the `range(col)` column as disjoint `[lo, hi]` intervals per value of the other columns. That is a representation
choice, so the lowering is the `table` lowering; `delete` on a range table is a compile error (LANG-050).

#### 3.1.10 `cell`: 0-ary lattice relations (LANG-120, LANG-128, LANG-280)

```blossom
cell votes: LSet<Node>                 // persistent, starts at ⊥
scratch cell seen_now: LSet<u64>       // tick-scoped: resets to ⊥ every tick (CR-24)
```

`cell c: 𝓛` is exactly `table c(-> v: 𝓛)`: a relation with an empty key and a lattice value, that is, a Bloom^L
lattice identifier. It is read by lookup, `votes[]` (N5). Lowering: `.decl votes(L; lset<Node>)` plus the implicit
identity rule of a persistent lattice (SEM-104):

```
votes(L; X)@next :- votes(L; X).
```

A `scratch cell` has no identity rule.

#### 3.1.11 `zset table` and `bag table` (LANG-138)

```blossom
zset table pane(window: u64, key: str)
pane(w, k) weight 1  :- click(w, k).
pane(w, k) weight -1 :- retract_click(w, k).
output hot(window: u64, key: str)
hot(w, k) :- pane(w, k) weight n where n >= 100.
```

Weighted relations carry a signed `i64` weight per tuple (checked arithmetic: overflow is a hard error). `weight e`
in a head contributes `e`; `weight n` in a body binds the current weight. A `bag table` is the insert-only ℕ variant,
and a negative contribution into a `bag` is a compile error when ANA-030 cannot rule it out. Lowering is to the IR's
Z-set stratum (ENG-062): `.decl pane(L, W, K) zset` with head weights `pane(L, W, K) += 1 :- click(L, W, K).`.
Crossing nodes requires an `exactly_once` channel (§3.8.3).

#### 3.1.12 Built-in relations (LANG-051, LANG-052, LANG-202)

| Built-in | Kind | Meaning |
|---|---|---|
| `stdin(line: str)` | input | lines read since the last tick |
| `stdout(line: str)` | output | written at the end of the tick, in canonical order (LANG-118) |
| `halt(reason: str)` | output | inserting stops the node at the end of the tick |
| `localtick()` | loopback | requests another tick |
| `catalog.rule(…)`, `catalog.depends(…)`, `catalog.stratum(…)`, `catalog.schema(…)` | static | the program's own catalog (LANG-202) |
| `session_open(s: Session, p: Principal, t: Time)`, `session_closed(s: Session, why: str)` | input | external sessions (LANG-243) |

File sources are table functions (`extern table fn file_lines(path: str) -> (lineno: u64, text: str) is "…"`,
§3.3.8). Writing into an input-only built-in is a compile error (LANG-066).

#### 3.1.13 Materialization hints (LANG-053)

`scratch v(…) materialized` or `… recompute` chooses whether a view is maintained incrementally or recomputed. It
produces no rule and cannot change meaning.

### 3.2 Rules and temporal operators

#### 3.2.1 Deductive rules (LANG-060)

```blossom
reach(a, b) :- edge(a, b).
reach(a, c) :- reach(a, b), edge(b, c).
next_slot(s + n) :- slot(s), batch_size(n).          // expressions are allowed in heads
```

```
reach(L, A, B) :- edge(L, A, B).
reach(L, A, C) :- reach(L, A, B), edge(L, B, C).
next_slot(L, X) :- slot(L, S), batch_size(L, N), X = S + N.
```

Same node, same tick, recursion allowed (CR-11). A head expression is lowered to a fresh variable bound by `=` in
the body. Every head variable must be bound by the body (ANA-001).

#### 3.2.2 Inductive rules: `next` (LANG-061)

```blossom
next counter(x + 1) :- counter(x), request(..).
```

```
counter(L, Y)@next :- counter(L, X), request(L, _, _), Y = X + 1.
```

Evaluated once, on the completed fixpoint of the tick (SEM-003), so it may negate or aggregate any stratum and never
needs stratifying.

#### 3.2.3 Async rules: `async` (LANG-062, LANG-150, LANG-151)

```blossom
async vote_resp(@c, t, true) :- grant(t, c).
```

```
vote_resp(C, T, true)@async :- grant(L, T, C).
```

The head must be a channel, and its `@` argument names the destination. The body is evaluated at the sender, and
every body atom is local (ANA-004). The message is delivered at some later tick of `C` chosen by the network (SEM-040),
with sender-side merge for lattice payloads (CR-52). Messages derived from persistent state are re-sent every tick
the body holds (ODD-05); suppression is an optimization the compiler may prove safe (DIST-007).

**Bodies that span nodes, as sugar (LANG-095, CR-15).** Only under the `localize` modifier, body atoms may carry an
explicit `@loc` argument:

```blossom
localize path(@s, d) :- link(@s, x), path(@x, d).
```

The compiler applies the chain rewrite (one async hop per location change) and emits a lint. Here the body is at
`s` and `x`, so it ships `link` to `x`, joins there, and ships results back:

```
.decl link$at(L: Node, Src: Node)                % persistent mirror of link at its target
.decl link$ship(L: Node, Src: Node) channel
.decl path$in(L: Node, Dst: Node) channel
link$ship(X, L)@async :- link(L, X).
link$at(L, S) :- link$ship(L, S).
link$at(L, S)@next :- link$at(L, S).
path$in(S, D)@async :- link$at(L, S), path(L, D).
path(L, D) :- path$in(L, D).
```

The lint states what the sugar costs: two hops per derivation, and deletions of `link` are not propagated to
`link$at` (the rewrite is only exact for insert-only `link`; ANA-004 rejects `localize` when `link` has a `delete`
rule).

#### 3.2.4 Deferred delete: `delete` (LANG-063, CR-06)

```blossom
delete buf(d, s, i, p) :- buf(d, s, i, p), acked(d, i).
```

```
buf$del(L, D, S, I, P) :- buf(L, D, S, I, P), acked(L, D, I).
% together with the table's persistence rule
buf(L, D, S, I, P)@next :- buf(L, D, S, I, P), notin buf$del(L, D, S, I, P).
```

`delete` removes the exact tuple at t+1; the tuple is still visible at t (SEM-005). If a rule also inserts it for
t+1, the insert wins (CR-05). To delete by key, join on the key (`kv(k, v)` above binds the value). `delete` into a
scratch, channel, static, range table or lattice-valued relation is a compile error (LANG-066).

#### 3.2.5 Deferred upsert: `upsert` (LANG-064, CR-07, SEM-051)

```blossom
upsert kv(key, val) :- put_win(key, val).
```

```
kv$ups(L, K, V)    :- put_win(L, K, V).
kv$del(L, K, V0)   :- kv$ups(L, K, _), kv(L, K, V0).
kv(L, K, V)@next   :- kv$ups(L, K, V).
error(UpsertConflict, kv, K, V1, V2) :- kv$ups(L, K, V1), kv$ups(L, K, V2), V1 != V2.
```

At t+1 every tuple with the key is replaced by the new one, atomically. Upserting the value already stored is a
no-op (delete and insert of the same tuple: insert wins). Two different upserts for one key in one tick are a hard
error; a program that wants a deterministic winner says so, either with `choose` in the body (§3.4.6) or with
`upsert … resolve <policy>` (§3.4.9).

#### 3.2.6 Facts, timestamped facts and bootstrap (LANG-069, LANG-190, CR-13, CR-16)

```blossom
edge(1, 2).                          // holds at every tick
bcast(@A, "hello") at 1.             // an input event at node A, tick 1 (tests, specs, the .ded frontend)

bootstrap fresh {                    // only when the node starts with no durable state
    current_term(0).
}
bootstrap {                          // at the first tick of every incarnation
    role(Follower).
    deadline(d) :- let d = now() + rand_duration(150ms, 300ms, ("deadline", 0)).
}
```

```
edge(L, 1, 2).
bcast(A, "hello")@1.
current_term(L, 0)  :- boot(L), notin recovered(L).
role(L, Follower)   :- boot(L).
deadline(L, D)      :- boot(L), now(L, T), D = T + rand_duration(150ms, 300ms, ("deadline", 0)).
```

Bootstrap rules are deductive rules guarded by `boot(L)`, which holds at tick 0 (SEM-012) and at the first tick of
every later incarnation, when volatile state is empty again (SEM-071). `bootstrap fresh` adds `notin recovered(L)`,
so durable initial values are written only on a node's first start and never collide with reloaded state; writing a
`durable` relation in a plain `bootstrap` block is a compile error for that reason. `recovered` is an input, so the
guard creates no negation cycle. Because the targets are tables, the values persist.

`next`, `async`, `delete` and `upsert` are allowed inside `bootstrap` and mean what they mean at the boot tick (so
`next x(…)` inside bootstrap lands one tick later). Imported modules' bootstrap rules are part of
the same tick-0 fixpoint; their relative order is decided by stratification, not by import order, which is the
Dedalus reading of LANG-190's "imported modules bootstrap first".

#### 3.2.7 Named rules, blocks and rule modifiers (LANG-068, LANG-204)

```blossom
grant_vote: next voted_for(t, c) :- grant(t, c).

block retransmit {
    bed.pipe_in(d, s, i, p) :- buf(d, s, i, p), retry(_).
}

monotone deliver: log(p) :- log_msg(payload: p).
nondet("leader hint is advisory") redirect: async hint(@c, l) :- req(..) from c, believed_leader(l).
```

A label names one rule; a block names a set of rules and declarations and is the unit of `override` (§3.7.4). Both
become IR rule identifiers (`Module::grant_vote`, `Module::retransmit#1`) used by provenance, tracing, coverage,
plan hints, and the stable choice-site ids of SEM-084. Labels are unique per module (duplicate: compile error).

`monotone` asserts that the rule's head depends monotonically on every body occurrence: no `not`, `optional`,
relational aggregate, `choose`, `index()`, `seq()`, `fold`, `reveal`, `inserted`/`removed`, Anti/NM lattice
operation, `delete` or `upsert`. The assertion is checked by ANA-020/021; failure is a compile error that names the
offending keyword. It produces no IR rule. `nondet("…")` marks accepted nondeterminism with a mandatory reason
(LANG-204); it is IR metadata that the analyzer and simulator propagate.

#### 3.2.8 The operator × collection legality matrix (LANG-066)

| head form → target | `table`, `durable`, `soft` | `scratch`, `temp`, `output` | `channel` | lattice-valued relation or `cell` | `static`, `input`, timers, `stdin` |
|---|---|---|---|---|---|
| deductive | insert now, persist | derive now | error | merge now | error |
| `next` | insert at t+1 | derive at t+1 | error | merge at t+1 | error |
| `async` | error | error | send | error | error |
| `delete` | delete at t+1 (not `range`) | error | error | error (LANG-284) | error |
| `upsert` | replace at t+1 (not `range`) | error | error | error (LANG-284) | error |

`sealed table` accepts writes only from `bootstrap`. The matrix is checked statically (ANA-005).

### 3.3 Rule bodies

#### 3.3.1 Atoms: positional, named, puns (LANG-080, LANG-081, LANG-086)

```blossom
table vote(term: u64, voter: Node, granted: bool)

tally(t, v) :- vote(t, v, true).                        // positional: every column
tally(t, v) :- vote(term: t, voter: v, granted: true).  // named: order-free
tally(term, voter) :- vote(term, voter, granted: true). // named mode with puns: `term` means `term: term`
yes(t) :- vote(term: t, granted: true, ..).             // `..`: unmentioned fields are wildcards
self_vote(t) :- vote(t, v, _), leader(v).               // repeated variable = equality join
```

All of these lower to the same positional IR atom, e.g. `vote(L, T, V, true)`; wildcards become fresh anonymous
variables. Constants in atoms are real selections (LANG-081). A named-mode atom that names an unknown field, or
omits a field without `..`, is a compile error; this is what makes renaming or adding a column safe.

#### 3.3.2 Guards and `where` (LANG-084)

```blossom
big(k) :- kv(k, v), size(k, n) where len(v) > 1024 and n % 2 == 0.
```

```
big(L, K) :- kv(L, K, V), size(L, K, N), len(V) > 1024, N % 2 == 0.
```

`where` starts the filter section of a conjunction; after it only expressions are allowed. Guards may also appear as
ordinary literals. Expression precedence is §2.2; Molly's right-nested parse is not reproduced.

#### 3.3.3 `let` (LANG-085)

```blossom
bucket(k, b) :- kv(k, _), let b = hash(k) % 64.
pairs(x, y) :- pt(p), let (x, y) = p.
puts(k, v) :- msg(m), let Msg::Put{key: k, val: v} = m.     // refutable: fails for other variants
```

```
bucket(L, K, B) :- kv(L, K, _), B = hash(K) % 64.
pairs(L, X, Y)  :- pt(L, P), X = P.0, Y = P.1.
puts(L, K, V)   :- msg(L, M), variant(M) == Put, K = M.key, V = M.val.
```

`let` binds fresh variables. The planner orders literals by which variables they need (LANG-085), so `let` may
appear anywhere; using a variable before any literal can bind it is a range-restriction error.

#### 3.3.4 Negation and anti-joins (LANG-082, LANG-083, SEM-021)

```blossom
unreachable(n) :- node(n), source(s), not reach(s, n).          // whole-tuple anti-join
lonely(n) :- node(n), not edge(n, _).                           // key anti-join (existential wildcard)
cold(k) :- kv(k, v), not { hits(k, h), h > 10 }.                // anti-join with a predicate
```

```
unreachable(L, N) :- node(L, N), source(L, S), notin reach(L, S, N).
edge$p1(L, N) :- edge(L, N, _).
lonely(L, N) :- node(L, N), notin edge$p1(L, N).
not$1(L, K) :- hits(L, K, H), H > 10.
cold(L, K) :- kv(L, K, V), notin not$1(L, K).
```

`not` is the only negation (no `!p(…)`; `!` is boolean not on values). Every variable in a negated literal must be
bound by a positive literal, except wildcards and variables local to a `not { … }` block, which are existential.
Negation is a negative edge; a same-tick cycle through it is rejected with the cycle as witness (SEM-020, ANA-002).

#### 3.3.5 Left outer join: `optional` (LANG-087)

```blossom
async get_resp(@s, id, v) :- get_req(_, id, key) from s, optional kv(key, v).     // v: Option<bytes>
```

```
kv$has(L, K) :- kv(L, K, _).
get_resp(S, Id, some(V))@async :- get_req(L, Id, K, S), kv(L, K, V).
get_resp(S, Id, none)@async    :- get_req(L, Id, K, S), notin kv$has(L, K).
```

Each variable first bound by an `optional` atom has type `Option<T>`; either all of them are `Some` or all are `None`.
`optional` is non-monotone and is reported as a point of order. `from s` binds the channel's implicit sender
column, which the IR appends after the declared columns (§3.9).

#### 3.3.6 Disjunction and conditional values (LANG-089)

```blossom
msg_term(t) :- (request_vote(term: t, ..) or vote_resp(term: t, ..) or heartbeat(term: t, ..)).
alert(k) :- reading(k, v), (v > HIGH or (v < LOW and armed(k))).
label(k, if v > 0 then "pos" else "nonpos") :- reading(k, v).
```

```
msg_term(L, T) :- request_vote(L, T, _, _, _).
msg_term(L, T) :- vote_resp(L, T, _).
msg_term(L, T) :- heartbeat(L, T, _).
alert(L, K) :- reading(L, K, V), V > HIGH.
alert(L, K) :- reading(L, K, V), V < LOW, armed(L, K).
label(L, K, X) :- reading(L, K, V), X = ite(V > 0, "pos", "nonpos").
```

A parenthesized `or` containing an atom is a disjunction of conjunctions and splits into one IR rule per disjunct
(the split is shown for the atom case even though `v > HIGH` alone would be a guard, to show mixed disjuncts). A
variable used outside the disjunction must be bound in every branch. When the rule also has `|>` stages, the split applies to
the conjunction only: the disjuncts are unioned into the stage's generated `$in` relation and the stages run once
over that union, so a `group` sees all branches together. `if … then … else` is a pure IR expression (`ite`), which
is equivalent to splitting the rule and cheaper.

#### 3.3.7 Unnest and destructuring: `for` (LANG-088)

```blossom
word(w) :- line(_, text), for w in split_words(text).
occurrence(ln, pos, w) :- line(ln, text), for (pos, w) in enumerate(split_words(text)).
```

```
word(L, W) :- line(L, _, Text), unnest(split_words(Text), W).
occurrence(L, Ln, Pos, W) :- line(L, Ln, Text), unnest(enumerate(split_words(Text)), (Pos, W)).
```

`unnest(Xs, P)` is the built-in generator relation with binding pattern (in, out): it ranges over the elements of the
bound collection value `Xs` (lists in order, sets and maps canonically) and matches each against the pattern.
Results are sets, so repeated elements collapse; `enumerate` keeps duplicates apart. `for` is positive and monotone.

#### 3.3.8 Generators, ranges and table functions (LANG-092, LANG-183)

```blossom
slot(i) :- window(lo, hi), for i in lo..hi.
extern table fn file_lines(path: str) -> (lineno: u64, text: str) is "blossom_io::file_lines"
line(n, t) :- input_file(p), file_lines(p, n, t).
```

```
slot(L, I) :- window(L, Lo, Hi), range(Lo, Hi, I).
line(L, N, T) :- input_file(L, P), file_lines(P, N, T).      % binding pattern b f f
```

Generator relations are infinite relations usable only with their input positions bound (checked statically).

#### 3.3.9 Membership and existence (LANG-090)

```blossom
member_ok(x) :- req(x, allowed) where x.user in allowed.     // `in` on a bound collection value
has_votes(t) :- term(t) where exists{ vote(term: t, ..) }.    // semijoin
flag(t, b) :- term(t), let b = exists{ vote(term: t, ..) }.   // boolean value: exact read
```

```
member_ok(L, X) :- req(L, X, Allowed), contains(Allowed, X.user).
ex$1(L, T) :- vote(L, T, _, _).
has_votes(L, T) :- term(L, T), ex$1(L, T).
flag(L, T, true)  :- term(L, T), ex$1(L, T).
flag(L, T, false) :- term(L, T), notin ex$1(L, T).
```

`x in xs` is a guard and requires `x` bound (for the generator, write `for x in xs`). `exists{ … }` in guard
position is a monotone semijoin; used as a value it is an exact read and lowers through `notin`.

#### 3.3.10 Lookups and range scans (LANG-091)

```blossom
entry(i, t, c) :- want(i), let (t, c) = log[i].                       // key lookup on a set relation
suffix(i, t, c) :- next_index(ni), last_index(li), log(i, t, c) where i in ni..=li.
```

```
entry(L, I, T, C)  :- want(L, I), log(L, I, T, C).
suffix(L, I, T, C) :- next_index(L, Ni), last_index(L, Li), log(L, I, T, C), I >= Ni, I <= Li.
```

On a set relation `r[k̄]` requires every key column and binds the value columns; it fails when the key is absent (a
join, so monotone). On a lattice relation it returns ⊥ for an absent cell (§3.5.3). Range guards on key columns
compile to index range scans; that is a planner fact, not a semantic one.

#### 3.3.11 Delta pseudo-relations: `inserted`, `removed` (LANG-071)

```blossom
newly_suspected(p) :- inserted suspect(p).
recovered(p) :- removed suspect(p).
```

```
suspect$was(L, P)@next :- suspect(L, P).
newly_suspected(L, P) :- suspect(L, P), notin suspect$was(L, P).
recovered(L, P) :- suspect$was(L, P), notin suspect(L, P).
```

`inserted r(…)` holds at the tick at which the fact became present, `removed r(…)` at the tick at which it became
absent (JOL's `#insert`/`#delete`). Both read `r` exactly, so both are negative edges. At tick 0 all of `r` is
`inserted`. For stored tables the engine reads its native birth/death intervals instead of materializing `$was`
(ENG-003). Re-staging an identical `$was` is not a state change, so it does not wake an idle node (SEM-009).

#### 3.3.12 Functions, UDFs and async services (LANG-180–184)

```blossom
extern fn crc32(b: bytes) -> u32 pure is "blossom_std::crc32"
extern fn ballot_max(a: Ballot, b: Ballot) -> Ballot pure commutative associative idempotent is "blossom_paxos::ballot_max"
extern service dns(name: str) -> (addr: Option<str>) is "blossom_net::resolve"

check(k, c) :- kv(k, v), let c = crc32(v).
dns.call(h) :- need_host(h).
host_addr(h, a) :- dns.result(h, a).
```

```
check(L, K, C) :- kv(L, K, V), C = crc32(V).
dns$call(L, H) :- need_host(L, H).
dns$result(L, H, A)@async :- dns$call(L, H), host$dns(H, A).   % host function as external EDB
host_addr(L, H, A) :- dns$result(L, H, A).
```

Every host function must be `pure`; its other declared properties (`monotone`, `morphism`, `injective`,
`commutative`, `associative`, `idempotent`) feed the analyses and are checked by TEST-087. Impure work is an
`extern service`: a call is emitted at the end of the tick and its result arrives as an input at a later tick, which
is the Dedalus rendezvous (R02 §3.3). Built-in functions (math, strings, hashing, collection operations, `to_string`)
are ordinary pure functions (LANG-180).

### 3.4 Aggregation, choice, numbering and folds

#### 3.4.1 What an aggregate folds over

One rule covers both aggregate syntaxes: **an aggregate folds its element expression over the distinct valuations
of the named (non-wildcard) variables in scope**. This is set semantics on bindings (CR-03), and it avoids the
classic "SUM(DISTINCT)" trap: `sum{ n | partial(split, word, n) }` adds `n` once per distinct `(split, n)`, not once
per distinct `n`. `distinct e` folds over the distinct values of `e` instead. Wildcards are not variables, so
`count{ v | vote(term: t, voter: v, ..) }` counts distinct voters. Relational aggregates are non-monotone, so each
one is a negative edge (CR-09, SEM-021); monotone aggregation is written with lattices (§3.4.4, §3.5).

#### 3.4.2 Pipeline GROUP BY: `|> group by` (LANG-100, LANG-101, CR-08)

```blossom
word_count(w, n) :- occurrence(ln, pos, w) |> group by w { n = count() }.
stats(k, n, avg_v, top) :- sample(k, t, v)
    |> group by k { n = count(), total = sum(v), top = max(v) }
    |> where n >= 10
    |> let avg_v = total / n.
```

```
word_count$g1$in(L, W, Ln, Pos) :- occurrence(L, Ln, Pos, W).
word_count(L, W, count<*>) :- word_count$g1$in(L, W, Ln, Pos).
stats$g1$in(L, K, T, V) :- sample(L, K, T, V).
stats$g1(L, K, count<*>, sum<V>, max<V>) :- stats$g1$in(L, K, T, V).
stats(L, K, N, A, Top) :- stats$g1(L, K, N, Total, Top), N >= 10, A = Total / N.
```

The line number `ln` is deliberately a named variable. Had it been written `occurrence(_, pos, w)`, the valuations
would be `(pos, w)` and two occurrences at the same position on different lines would collapse into one. This is the
one place where set semantics bites, so the compiler lints "aggregate input drops a wildcard column that is not
functionally determined by the named variables" for every `count()`/`sum` group.

Semantics: the stage's input is the set of valuations of every variable in scope before it; `group by ḡ` partitions
them; an empty group produces no row (CR-08); after the stage only `ḡ` and the aggregate names are in scope. `group`
with no `by` is one global group. Several `group` stages in one rule lower to a chain of generated relations. The IR
split into `$in` and the aggregate rule is Molly's (R02 §12.2), which keeps provenance bindings from changing the
grouping.

#### 3.4.3 Aggregate expressions and `default` (LANG-106)

```blossom
tally(t, n) :- term(t), let n = count{ v | vote(term: t, voter: v, ..) } default 0.
busy(t) :- term(t) where count{ v | vote(term: t, voter: v, ..) } > 3.
```

Variables of the enclosing rule used inside the braces (`t`) are *correlated*: they are the group key, exactly as in
Soufflé. Lowering:

```
agg$1$in(L, T, V) :- vote(L, T, V, _).
agg$1(L, T, count<*>) :- agg$1$in(L, T, V).
agg$1$has(L, T) :- agg$1(L, T, _).
tally(L, T, N) :- term(L, T), agg$1(L, T, N).
tally(L, T, 0) :- term(L, T), notin agg$1$has(L, T).          % only because of `default 0`
agg$2$in(L, T, V) :- vote(L, T, V, _).
agg$2(L, T, count<*>) :- agg$2$in(L, T, V).
busy(L, T) :- term(L, T), agg$2(L, T, N), N > 3.
```

Without `default`, an empty group makes the literal fail (GROUP BY, CR-08). With `default d`, the rest of the rule
supplies the driving tuples and each one gets a row, which is Overlog's per-event aggregate (ODD-03). If a correlated
variable is not bound by a positive literal *inside* the braces, the compiler adds a driver relation `drv$k(L, Ō)`
built from the rule's other positive literals and joins it into `agg$k$in`; this is exact, and it is only added when
range restriction requires it.

#### 3.4.4 The aggregate catalogue

| Aggregate | Group-stage form | Expression form | Class |
|---|---|---|---|
| count, sum, min, max, avg | `count()`, `count(distinct e)`, `sum(e)`, `min(e)`, `max(e)`, `avg(e)` | `count{…}`, `sum{ e \| … }`, … | NM (negative edge) |
| collections (LANG-102) | `set(e)`, `list(e)` (canonically sorted), `accum_pair(a, b)` | `set{ e \| … }`, `list{ e \| … }` | NM |
| exemplary (LANG-103) | `argmin(k, e)`, `argmax(k, e)` (all ties, as a set), `bool_and(c)`, `bool_or(c)` | `argmin{ e by k \| … }` | NM |
| order-sensitive (LANG-093, LANG-104, LANG-118) | `sort(e) order by k`, `topk(n, e) order by k desc`, `bottomk`, `limit(n, e)`, `percentile(p, e)` | `topk(3){ e \| … } order by k desc` | NM, ties broken by canonical order |
| quorum (LANG-111) | — | `majority{ n \| peers(n) }` = ⌊\|S\|/2⌋+1 | NM over its input; the verifier maps it to a quorum sort |
| estimators (LANG-113) | `ola_sum(e)`, `ola_count()`, `ola_avg(p, e)` | same with braces | NM, `nondet "progressive"` |
| choice (LANG-108) | `choose(e)` | `choose{ e \| … }` | NM, seed-dependent |
| folds (LANG-109, LANG-110) | `reduce(f, e)`, `fold(init, step, e) order by k` | `reduce(f){ e \| … }`, `fold(init, step){ e \| … } order by k` | NM |
| **lattice folds** (LANG-123) | `lset(e)`, `lmax(e)`, `lmin(e)`, `lbool(c)`, `lmap(k, v)`, `lbag(e)` | `lset{ e \| … }`, `lmax{ … }`, … | **monotone** (a morphism from the input set) |

The last row is the CALM-visible split: `max{ x | p(x) }` is a plain `u64` that can go down if `p` loses a tuple, so
it is a negative edge; `lmax{ x | p(x) }` is an `LMax<u64>` whose reads must go through thresholds or `reveal`, so it
is monotone. Lowering of a lattice fold is a lattice-valued generated relation that merges singletons (LANG-123):

```blossom
quorum_ok(t) :- term(t) where lset{ v | vote(term: t, voter: v, ..) }.size >= QUORUM.
```

```
agg$3(L, T; lset{V}) :- vote(L, T, V, _).                 % tick-scoped lattice relation; heads merge
quorum_ok(L, T) :- term(L, T), S = agg$3[L, T], S.size >= QUORUM.   % lookup: ⊥ (size 0) when no votes
```

#### 3.4.5 User-defined aggregates (LANG-105, LANG-112)

```blossom
aggregate distinct_count(x: bytes) -> u64 {
    init = Hll::new(14)
    step(s, x) = s.insert(x)
    merge(a, b) = a.union(b)
    finish(s) = s.estimate()
    properties commutative, associative, idempotent
}
uniq(page, n) :- visit(page, user) |> group by page { n = distinct_count(user) }.
```

A UDA with `commutative, associative` may be combined in any order and gets a derived partial combiner for
partitioned channels (LANG-112); `idempotent` additionally allows at-least-once inputs (ANA-015). Without those
declarations it **is** `fold(init, step){…} order by` canonical order (LANG-110), so it is still deterministic.
Declarations are checked by the law harness and shuffle tests (TEST-015, TEST-087). Lowering: the IR aggregate
`distinct_count<User>` with the UDA's functions attached; when not declared commutative and associative, the
§3.4.8 fold expansion.

#### 3.4.6 Choice: `choose` (LANG-108, LANG-114–116, SEM-085, CR-45)

```blossom
grant(t, c) :- may_vote(c, t), choose t -> c.                    // one c per t per tick
put_win(key, val) :- put_ok(s, id, key, val), choose key -> val most (s, id).   // greedy: highest (s, id)
leader_hint(l) :- candidate(l), choose () -> l.                  // one global choice per tick
next held(k, v) :- offer(k, v), choose sticky k -> v.            // keep the previous choice while it is a candidate
probe(n) :- peers(n), choose random () -> n.                     // uniform per tick, per node
matched(a, b) :- likes(a, b), choose a -> b, choose b -> a.      // several FDs: greedy matching
```

`choose X̄ -> Ȳ` enforces the functional dependency X̄ → Ȳ over the rule's candidates at this tick; node and tick
are always part of the determinant (SEM-085). `least C̄` / `most C̄` pick the least/greatest cost first (GZ01
`choice_least`), with FD X̄ → Ȳ ∪ C̄. Lowering of the first rule, with site id `s` = `M::grant#1`:

```
choose$s$cand(L, T, C)      :- may_vote(L, C, T).
choose$s$pmin(L, T, min<P>) :- choose$s$cand(L, T, C), P = prio(s, (T), (C)).
choose$s$chosen(L, T, C)    :- choose$s$cand(L, T, C), choose$s$pmin(L, T, P), P == prio(s, (T), (C)).
grant(L, T, C)              :- may_vote(L, C, T), choose$s$chosen(L, T, C).
```

`prio(s, X̄, Ȳ) = (PRF_σc(s, fp(X̄), fp(Ȳ)), Ȳ)` is SEM-084's seeded priority, so the choice is deterministic given
the seed and independent of plan order. For `most (s, id)` the candidates are first narrowed to the maximal cost:

```
choose$s$cand(L, K, V, S, Id) :- put_ok(L, S, Id, K, V).
choose$s$best(L, K, max<C>)   :- choose$s$cand(L, K, V, S, Id), C = (S, Id).
choose$s$top(L, K, V, S, Id)  :- choose$s$cand(L, K, V, S, Id), choose$s$best(L, K, C), C == (S, Id).
choose$s$pmin(L, K, min<P>)   :- choose$s$top(L, K, V, S, Id), P = prio(s, (K), (V, S, Id)).
choose$s$chosen(L, K, V, S, Id) :- choose$s$top(L, K, V, S, Id), choose$s$pmin(L, K, P), P == prio(s, (K), (V, S, Id)).
```

`choose sticky` adds the carried `held` relation of R12/G2 §5.5, which is the only state a choice ever has:

```
choose$s$keep(L, K, V)   :- choose$s$held(L, K, V), choose$s$cand(L, K, V), notin choose$s$forced(L, K).
choose$s$kept(L, K)      :- choose$s$keep(L, K, _).
choose$s$pminf(L, K, min<P>) :- choose$s$cand(L, K, V), notin choose$s$kept(L, K), P = prio(s, (K), (V)).
choose$s$fresh(L, K, V)  :- choose$s$cand(L, K, V), notin choose$s$kept(L, K), notin choose$s$forced(L, K),
                            choose$s$pminf(L, K, P), P == prio(s, (K), (V)).
choose$s$chosen(L, K, V) :- choose$s$keep(L, K, V).
choose$s$chosen(L, K, V) :- choose$s$fresh(L, K, V).
choose$s$chosen(L, K, V) :- choose$s$ovr(L, K, V).
choose$s$held(L, K, V)@next :- choose$s$chosen(L, K, V).
```

`choose random` replaces `prio` with `rprio`, keyed additionally by node, incarnation and tick (schedule-dependent,
SEM-087). Several `choose` literals in one rule use the greedy scan of R12/G2 §5.2 (a `fold` over candidates in
seeded priority order that accepts each candidate FD-consistent with those already accepted). The simulation-only
override hook (`choose$s$ovr`, `choose$s$forced`, `__choice`) is R12/G2 §5.6. Every choice site is a negative edge
and may not sit on a same-tick cycle (SEM-086). Lattice-typed variables may not appear in Ȳ.

#### 3.4.7 Numbering: `index()` and `seq()` (LANG-097, LANG-098)

```blossom
indexed(p, index() by (p.client, p.req)) :- client_in(p).         // dense 0-based rank within the tick
ranked(g, x, index() per (g) by (score desc)) :- entry(g, x, score).
numbered(x, seq() by (x)) :- arrival(x).                           // stable arrival numbering
```

`index()` and `seq()` are only legal as head columns, because they number the *deduplicated head tuples* (not body
rows). Lowering of `index()`:

```
index$s$h(L, P) :- client_in(L, P).
indexed(L, P, I) :- index$s$h(L, P), I = count{ P2 | index$s$h(L, P2), (P2.client, P2.req, P2) < (P.client, P.req, P) } default 0.
```

(and the aggregate expression lowers by §3.4.3). `seq()` lowers to R12/G2 §5.9's high-water-mark expansion:

```
seq$s$h(L, X)            :- arrival(L, X).
seq$s$assigned(L, X, I)@next :- seq$s$assigned(L, X, I).
seq$s$has(L, X)          :- seq$s$assigned(L, X, _).
seq$s$new(L, X)          :- seq$s$h(L, X), notin seq$s$has(L, X).
seq$s$nrank(L, X, J)     :- seq$s$new(L, X), J = count{ X2 | seq$s$new(L, X2), X2 < X } default 0.
seq$s$ncount(L, count<*>) :- seq$s$new(L, X).
seq$s$hwm(L, 0)          :- boot(L).                  % `durable seq()`: boot(L), notin recovered(L)
seq$s$hwm(L, H2)@next    :- seq$s$hwm(L, H), seq$s$ncount(L, C), H2 = H + C.
seq$s$hwm(L, H)@next     :- seq$s$hwm(L, H), notin seq$s$ncount(L, _).
numbered(L, X, I)        :- seq$s$h(L, X), seq$s$assigned(L, X, I).
numbered(L, X, I)        :- seq$s$nrank(L, X, J), seq$s$hwm(L, H), I = H + J.
seq$s$assigned(L, X, I)@next :- seq$s$nrank(L, X, J), seq$s$hwm(L, H), I = H + J.
```

`seq() release` drops a tuple's number when it leaves the head (the number is still never reused). If a `seq` number
reaches an `async` head or an output, the site's state must be `durable` (`durable seq() by (x)`), or ANA-011 rejects
it. ANA-011 also lints `index()` over persistent input ("ranks shift on insertion; use `seq()`").

#### 3.4.8 Ordered folds: `fold` (LANG-110) and `reduce` (LANG-109)

```blossom
next sm(s2) :- sm(s), let s2 = fold(s, apply){ (i, c) | to_apply(i, c) } order by i.
```

`fold(init, step){ e | body } order by k̄` left-folds the pure `step` over the distinct valuations in (k̄, canonical)
order. In expression form an empty input yields `init` (LANG-110's carried form); inside a `group` stage an empty
group has no row (the aggregate form). Lowering (R12/G2 §5.11; the site id is `f`):

```
fold$f$row(L, I, C)        :- to_apply(L, I, C).
fold$f$rk(L, I, C, K)      :- fold$f$row(L, I, C), K = index() by (I).          % §3.4.7
fold$f$acc(L, 0, S0)       :- sm(L, S0).                                         % drive: the carried state
fold$f$acc(L, K2, S2)      :- fold$f$acc(L, K, S1), fold$f$rk(L, I, C, K), K2 = K + 1, S2 = apply(S1, (I, C)).
fold$f$n(L, count<*>)      :- fold$f$row(L, I, C).
fold$f$out(L, S)           :- fold$f$acc(L, N, S), fold$f$n(L, N).
fold$f$nonempty(L)         :- fold$f$n(L, _).
fold$f$out(L, S0)          :- sm(L, S0), notin fold$f$nonempty(L).
sm(L, S2)@next             :- sm(L, S), fold$f$out(L, S2).
```

The recursion on `fold$f$acc` is positive and bounded by the row count, so it is accepted (SEM-020). A carried fold
over a *persistent* input re-applies every row every tick, as Dedalus demands; ANA-011 lints it, and the idiom is to
fold only the new rows (E3 and FLAG-005 use `last_applied < i <= commit`). `reduce(f){ e | … }` is `fold` without an
`init` over a non-empty input; if `f` is declared `commutative associative` the engine may combine in any order.

#### 3.4.9 Relation-level conflict resolution: `resolve` (LANG-117, ODD-02)

```blossom
durable table reg(k: str -> v: bytes, ts: (u64, Node)) resolve most (ts)       // an LWW register
reg(k, v, ts) :- write(k, v, ts).
upsert resolve choose sticky owner(r, n) :- claim(r, n).                       // first-writer-wins per key
```

Without `resolve`, two tuples with one key at one tick are an error (SEM-050, ODD-02 (a)). With it, the relation's
contents at each tick are the per-key winner among its candidates: the persisted tuples plus this tick's deductive
derivations, where `next` inserts and upserts become next tick's candidates (R12/G2 §5.12). Lowering of the first
declaration:

```
reg$raw(L, K, V, Ts)  :- write(L, K, V, Ts).                                   % every deductive rule into reg
reg$raw(L, K, V, Ts)  :- reg$kept(L, K, V, Ts).
reg$kept(L, K, V, Ts)@next :- reg(L, K, V, Ts), notin reg$del(L, K, V, Ts), notin reg$ups$k(L, K).
reg$kept(L, K, V, Ts)@next :- reg$staged(L, K, V, Ts).                         % `next reg(…)` and `upsert reg(…)`
reg$best(L, K, max<Ts>) :- reg$raw(L, K, V, Ts).
reg(L, K, V, Ts)      :- reg$raw(L, K, V, Ts), reg$best(L, K, Ts).            % + seeded tie-break if Ts can tie
```

`upsert resolve <policy>` applies the policy to the tick's conflicting upserts instead of raising SEM-051. A resolved
relation may not be on a same-tick cycle (SEM-086). `resolve merge` is only legal when every non-key column is a
lattice, where it is the default anyway.

### 3.5 Lattices

#### 3.5.1 Built-in lattice types (LANG-124, LANG-130–134, LANG-136, LANG-281)

| Type | ⊥ | Merge | Notes |
|---|---|---|---|
| `LBool` | `false` | or | used directly as a guard = `when_true` |
| `LMax<T>`, `LMin<T>` | adjoined −∞ / +∞ (ODD-50) | max / min | `T` must have a total order; not `f64` |
| `LSet<T>`, `LPSet<T>` (non-negative numbers) | `{}` | union | `LPSet.sum()` is monotone |
| `LBag<T>` | empty | per-element max multiplicity | |
| `LMap<K, V: lattice>` | `{=>}` | key union, value merge | entries equal to ⊥ are absent (SEM-034) |
| `Pair<A, B>`, records of lattices | (⊥, ⊥) | pointwise | product |
| `Lex<K, V>` | (⊥, ⊥) | lexicographic, `K` a chain (LANG-131) | `Ballot`, LWW registers |
| `Dom<Ver, Val>` | ∅ | keep non-dominated pairs (LANG-132) | MV-register; `version` M, `value` NM |
| `WithBot<L>`, `WithTop<L>`, `Conflict<T>`, `Point<T>`, `Unit`, `VecUnion<L>`, `UnionFind<T>` | | | as in LANG-124; merging two different `Point`s is a hard error |
| `TombSet<T>`, `TombMap<K, V>` | | union with tombstones (LANG-133) | |
| `Causal<DotSet>`, `Causal<DotFun<V>>`, `Causal<DotMap<K, L>>` | | causal dot-store join (LANG-134) | context compressible to a version vector |
| `DomPair<K, V>` | | **not associative** | only in a declaration carrying `unsafe("reason")` (LANG-136, CR-25) |

`type VClock = LMap<Node, LMax<u64>>` gives vector clocks (LANG-130); `a <= b`, `a < b` and
`a.concurrent_with(b)` are the happens-before tests.

#### 3.5.2 Lattice-valued relations (LANG-121, SEM-100, CR-24)

```blossom
table votes(term: u64 -> who: LSet<Node>)          // persistent (the default for lattices)
scratch seen(round: u64 -> ids: LSet<u64>)         // tick-scoped: ⊥ at every tick
```

A relation whose value columns are all lattices is a lattice relation: its key is the non-lattice columns, and two
derivations for one key merge instead of conflicting (CR-51). A lattice column may not be a key, a join column, a
group key or an argument of `==` (ANA-005). Lowering:

```
.decl votes(L: Node, Term: u64; lset<Node>)
votes(L, T; X)@next :- votes(L, T; X).            % implicit identity rule of a persistent lattice (SEM-104)
.decl seen(L: Node, Round: u64; lset<u64>)       % no identity rule
```

A relation with lattice columns *and* non-key non-lattice columns keeps SEM-050's error for the non-lattice part.

#### 3.5.3 Writing and reading lattice cells (LANG-122, LANG-129, LANG-280)

```blossom
votes(t, {v}) :- vote_resp(term: t, granted: true, ..) from v.     // merge now      (Bloom <=)
next votes(t, {self}) :- start_election(t).                        // merge at t+1   (Bloom <+)
won(t) :- votes(t, who), current_term(t) where who.size >= QUORUM.    // generator: non-⊥ cells only
lag(n) :- peers(n), let c = vc[].at(n) \/ 0 where reveal(c) < 10.     // lookup of a cell, default by join
```

```
votes(L, T; {V}) :- vote_resp(L, T, true, V).
votes(L, T; {L})@next :- start_election(L, T).
won(L, T) :- votes(L, T; Who), current_term(L, T), Who.size >= QUORUM.
lag(L, N) :- peers(L, N), X = vc[L], C = X.at(N) \/ 0, reveal(C) < 10.
```

A head value is merged into the cell (both sides must have the same lattice type). Literal coercion: `{v}` in an
`LSet` column is a singleton, `0` in an `LMin<u64>` column is `LMin(0)`, `{k => v}` is an `LMap` singleton. A
generator `r(k̄, x)` ranges only over non-⊥ cells (N4). A lookup `r[k̄]` (all keys bound) or `c[]` (a cell) returns
the value, ⊥ when absent (N5). `x \/ c` joins with a constant, the monotone way to give a lattice read a default
(LANG-283); there is no `??` operator.

#### 3.5.4 Operations, polarity and thresholds (LANG-125, LANG-126, LANG-127)

Every lattice method has a declared class per argument: morphism (M), bimorphism (BM), monotone (Mon), antitone (Anti)
or non-monotone (NM). The normative table is R04 §2.4; a few:

| Expression | Result | Class |
|---|---|---|
| `s.contains(x)`, `m.has_key(k)`, `m.at(k)`, `m.keys()`, `s.intersect(t)` | `LBool`, `LBool`, `V`, `LSet<K>`, `LSet<T>` | M (BM for `intersect`) |
| `s.size`, `m.size`, `p.sum()`, `m.sum_values()` | `LMax<u64>` | Mon |
| `x + c` on `LMax`/`LMin` (c ≥ 0) | same | M |
| `a \/ b` | same | BM |
| `x >= c`, `x > c` on `LMax`; `x <= c`, `x < c` on `LMin`; `a >= b` (a ⊒ b) on any lattice | `bool` threshold | Mon in the "growing" side, Anti in the other |
| `x < c` on `LMax`, `a <= b` in `a` | `bool` | Anti |
| `a.concurrent_with(b)` on `VClock` | `bool` | NM |
| `reveal(x)` | the raw value | NM |

A **threshold** is a monotone map into `bool`; thresholds are the only monotone way to get a plain value out of a
lattice. Used as guards they are positive literals. `threshold(x, t1, …, tn)` returns `Option<u32>`, the index of
the one threshold `x` has reached; the `tᵢ` must be pairwise incompatible (checked), so the result never changes
once set (THRESH-FINAL, ANA-120). `reveal(x)` is the only way to read a raw value and is always a negative edge.
`==`/`!=` on lattice values are compile errors that suggest `reveal`. All of this is the IR's polarity analysis
(SEM-102); the surface adds no rule, only class-tagged expressions.

#### 3.5.5 Monotone reset (LANG-284)

```blossom
table round_votes(key: str -> v: Lex<LMax<u64>, LSet<Node>>)
next round_votes(k, Lex(e + 1, {})) :- new_epoch(k, e).     // bumping the epoch discards the old set
```

`delete` and `upsert` on lattice relations are compile errors; the reset is a larger epoch under `Lex`.

#### 3.5.6 Lattices in messages (LANG-137, CR-52, SEM-105)

```blossom
channel gossip(@to: Node, clock: VClock)
async gossip(@n, vc[]) :- beat(_), peers(n).
vc(c) :- gossip(clock: c).
```

```
gossip(N; C)@async :- beat(L, _, _), peers(L, N), C = vc[L].
vc(L; C) :- gossip(L; C).
```

The IR treats the channel as a lattice relation keyed by (destination, non-lattice columns): values sent by one node
in one tick to one destination are merged at the sender into one message, and same-key arrivals in one delivered
batch merge (CR-52). Accumulation across ticks needs a persistent sink (here `vc`).

#### 3.5.7 User-defined lattices (LANG-135, ODD-09)

Three forms, from most to least verified:

```blossom
// (1) composition of built-ins: merge, ⊥ and laws are inherited, nothing to check
lattice GCounter = LMap<Node, LMax<u64>>

// (2) record-of-lattices DSL: the product merge is derived (verified by construction);
//     method classes are claims checked by the law harness / SMT (TEST-083, TEST-087, VER-014)
lattice Cart {
    lines: LMap<u64, Point<Line>>,
    checkout: WithBot<Point<u64>>,

    monotone fn is_complete(self) -> LBool =
        match self.checkout.get() {
            Some(k) => all([self.lines.has_key(i) for i in 1..k]),
            None => false,
        }
    stable fn summary(self) -> Map<str, i64> after is_complete =
        sum_by_key([(l.item, if l.action == Add then l.n as i64 else -(l.n as i64))
                    for (i, l) in self.lines.entries() if i < self.checkout.get_or(0)])
}

// (3) a Rust type implementing the Merge trait: every claim is "tested", never "proven"
lattice Hll extern "blossom_sketch::HllLattice" {
    monotone fn estimate(self) -> LMax<u64>
    morphism fn contains(self, x: bytes) -> LBool
}
```

Method classes: `morphism` (join-preserving, CR-23), `bimorphism`, `monotone`, `antitone`, plain `fn`
(non-monotone), and `stable … after t`, a claim that once threshold method `t` holds the method's value never changes
as `self` grows (Bloom^L's "monotonic-then-immutable" pattern). A read of a `stable` method is classified monotone
when the same rule body guards it with `t`. Method bodies are pure expressions over the representation and may read
it freely; the *class* is what the analysis trusts, and it is checked: proved where the SMT fragment allows,
otherwise tested (TEST-087). A refuted claim is a compile error. Lowering: the IR registers the lattice type with its
merge, ⊥ and per-method class; the method bodies become pure IR functions.

#### 3.5.8 Group and ring types (LANG-142, CR-35)

```blossom
group Delta = ZSet<(str, u64)>
group Money = i64 ring
```

A `group` provides `0`, `+`, `neg`; `ring` adds `*` and `1`. A group or ring can never be declared a lattice (the
law harness rejects `+` as a merge). Group-typed payloads cross nodes only through `exactly_once` channels (§3.8.3).

### 3.6 Time, timers and randomness

#### 3.6.1 Clock, tick and randomness (LANG-170, LANG-171, LANG-174, LANG-175, CR-18)

```blossom
stamped(i, now()) :- event(i).
late(i) :- event(i), deadline(d) where now() > d.
next deadline(now() + rand_duration(150ms, 300ms, ("election", t))) :- start_election(t).
probe_target(n) :- peers(n), let r = rand("probe", n) where r % 4 == 0.
```

```
stamped(L, I, T) :- event(L, I), now(L, T).
late(L, I) :- event(L, I), deadline(L, D), now(L, T), T > D.
deadline(L, D)@next :- start_election(L, Tm), now(L, T), D = T + rand_duration(150ms, 300ms, ("election", Tm)).
probe_target(L, N) :- peers(L, N), R = rand("probe", N), R % 4 == 0.
```

`now()` is one value per tick, sampled and recorded (LANG-171). `tick()` is the local tick number (and marks the rule
time-dependent). `rand(k̄)` is `PRF_σnode("rand", incarnation, tick, fp(k̄))` (LANG-175); `random()` is `rand(())`;
`rand_float`, `rand_range(lo, hi, k̄)` (unbiased) and `rand_duration` are helpers. Nothing is recorded per draw
(SEM-084). A value that must stay fixed is captured with `next` into state (ANA-011 lints uncaptured `rand` over
persistent inputs).

#### 3.6.2 Timers (LANG-172, LANG-173, CR-19, ODD-16)

```blossom
timer beat every 500ms                  // physical, periodic: beat(id: u64, at: Time)
timer probe every 1s times 10           // stops after 10 firings
timer warmup after 5s                   // one-shot, 5 s after start
timer start once                        // fires once, at tick 0
timer retry every 3 ticks               // logical: counts local ticks
```

Physical timers are runtime inputs: `.decl beat(L, Id: u64, At: Time) input timer(physical, every 500ms)`; the
timer wheel (DIST-030) feeds them and triggers ticks, and under simulation they run on the virtual clock. Under LDFI
they are mapped to rounds by the spec's `round:` duration unless the module overrides it (ODD-16). A `once` timer is
`start(L, 0, T) :- boot(L), now(L, T).` A logical timer is its own Dedalus expansion:

```
retry$left(L, 3) :- boot(L).
retry$left(L, K2)@next :- retry$left(L, K), K > 1, K2 = K - 1.
retry$left(L, 3)@next  :- retry$left(L, 1).
retry(L, N, T)         :- retry$left(L, 1), tick(L, N), now(L, T).
```

Because `retry$left` changes every tick it keeps the node ticking (SEM-009); logical timers are intended for
simulation and LDFI, and the compiler warns when a deployed program uses one.

### 3.7 Modules, protocols and choreographies

#### 3.7.1 Modules, interfaces and instances (LANG-001, LANG-003, LANG-004)

```blossom
module Counter(START: u64 = 0) {
    input  bump(id: u64 -> amount: u64)          // ids keep equal amounts apart under set semantics
    output value(n: u64)
    table total(-> n: u64)
    bootstrap { total(START). }
    upsert total(n + s) :- total(n), bump(i, amt) |> group by n { s = sum(amt) }.
    value(n) :- total(n).
}

module Two {
    import Counter as a
    import Counter(START: 100) as b
    a.bump(i, 1) :- tick_evt(i).
    b.bump(i, 2) :- tick_evt(i).
    input tick_evt(id: u64)
    output both(x: u64, y: u64)
    both(x, y) :- a.value(x), b.value(y).
}
```

`import M(args) as a` creates an independent instance; the same module can be imported again under another alias,
and reusing an alias is an error. The importer may only write an instance's `input`s and read its `output`s;
everything else is private (qualified access `a.b.rel` to nested interfaces is allowed only inside `interpose`).
Lowering is flattening: instance-qualified names are mangled with `$`, value parameters are substituted, and relation
parameters are aliased to the argument relation. The example becomes, among others:

```
a$total(L, N)@next :- a$total(L, N), notin a$total$del(L, N).
a$total(L, 0) :- boot(L).
b$total(L, 100) :- boot(L).
a$bump(L, I, 1) :- tick_evt(L, I).
both(L, X, Y) :- a$value(L, X), b$value(L, Y).
```

Interfaces are tick-local relations in the flattened IR; the catalog records their direction (LANG-003).

#### 3.7.2 Protocols and module parameters (LANG-006)

```blossom
protocol Delivery {
    input  pipe_in(dst: Node, src: Node, ident: MsgId -> payload: bytes)
    output pipe_sent(dst: Node, src: Node, ident: MsgId -> payload: bytes)
    output pipe_out(dst: Node, src: Node, ident: MsgId -> payload: bytes)
}
module BestEffortDelivery implements Delivery { … }
module Multicast(D: Delivery = BestEffortDelivery, members: rel(n: Node)) {
    import D as d
    …
}
import Multicast(D: ReliableDelivery, members: peers) as mc
```

A protocol is an interface-only contract. `implements P` includes `P`'s interface declarations into the module (as
Bloom's `include DeliveryProtocol` does); redeclaring one is allowed only with the identical schema and direction. A module parameter of protocol type is filled with an implementation at import time; the
lowering monomorphizes (the instance `mc$d` is a `ReliableDelivery`). A relation parameter `members: rel(n: Node)`
binds to any relation of the importer with that schema.

#### 3.7.3 Include and mixins (LANG-005)

`include Base` copies `Base`'s declarations and rules into the current module without a prefix (Bloom's `include`).
`include "common/types.bls"` is textual, resolved relative to the including file. Lowering: none beyond the copy;
duplicate names are compile errors.

#### 3.7.4 Extension and override (LANG-007)

```blossom
module LoudDelivery extends ReliableDelivery {
    output audit(ident: MsgId)
    override block rcv {
        pipe_out(d, s, i, p) :- bed.pipe_out(d, s, i, p).
        async ack(@s, d, i) :- bed.pipe_out(d, s, i, _).
        audit(i) :- bed.pipe_out(_, _, i, _).
    }
}
```

`extends B` includes `B`; a block declared `override block n` replaces `B`'s block `n` wholesale. Overriding a block
that does not exist, or declaring two blocks with one name, is an error. Lowering: the flattened rule set with the
replacement applied.

#### 3.7.5 Interposition (LANG-008)

```blossom
interpose data.d.bed.pipe_in as orig {
    data.d.bed.pipe_in(dst, src, m, p) :- orig(dst, src, m, p), not fd.suspect(dst).
    suppressed(dst, {m}) :- orig(dst, _, m, _), fd.suspect(dst).
}
```

Inside the instance, every rule whose head is the interposed interface is re-targeted to a fresh relation, which the
block sees as `orig`; the interface itself is now defined only by the block's rules. Lowering:

```
data$d$bed$pipe_in$orig(L, D, S, M, P) :- … (each original rule body, unchanged) …
data$d$bed$pipe_in(L, D, S, M, P) :- data$d$bed$pipe_in$orig(L, D, S, M, P), notin fd$suspect(L, D).
suppressed(L, D; {M}) :- data$d$bed$pipe_in$orig(L, D, _, M, _), fd$suspect(L, D).
```

An interface can be interposed once per program; a second `interpose` on it is an error. Interposition on an
`output` intercepts what the instance publishes; on an `input`, what it receives.

#### 3.7.6 Choreographies: multi-role modules (LANG-009, LANG-153, LANG-242, CR-14)

```blossom
choreography Ping {
    role client: process
    role server: cluster
    channel ping(@to: server, n: u64)
    channel pong(@to: client, n: u64)
    on client {
        input go(n: u64)
        output got(n: u64)
        async ping(@s, n) :- go(n), let s = server.route(n).
        got(n) :- pong(_, n).
    }
    on server {
        async pong(@c, n) :- ping(_, n) from c.
    }
}
```

`role r: process` is one node, `cluster` a set of nodes running the same code (SPMD), `external` clients that are
not `Node`s (sessions, LANG-243). Each role gets a static membership relation `r(n: Node)` visible to every role,
plus `r.route(k)` (rendezvous hash over the members, deterministic) and `r.size()`. A channel's `@` column may be
typed by a role, which constrains destinations and yields the inferred default-deny ACL: a role may send on a channel
only if it has an `async` rule into it (LANG-242, ANA-105). Every rule and every declaration other than a channel or
a `static` relation must be inside an `on` block; choreography-level statics are replicated to every role. The Dedalus meaning is TPLP's heterogeneous-roles encoding: one program in which each rule is
guarded by its role:

```
ping(S, N)@async :- role$client(L), go(L, N), S = route(server, N).
got(L, N)        :- role$client(L), pong(L, N, _).
pong(C, N)@async :- role$server(L), ping(L, N, C).
```

Projection to one program per role drops the rules whose guard is false on that role and the guard itself; that is an
optimization whose correctness is immediate from the guards (R02 §4.2).

#### 3.7.7 Constants and parameters (LANG-010)

```blossom
const QUORUM: u64 = 3
param RETRY: Duration = 2s               // overridable: blossom run --param RETRY=500ms, or the deployment spec
module ReliableDelivery(RETRY: Duration = 2s) { … }
```

Constants and parameters are substituted before lowering; they produce no IR rules. A parameter's value is part of
the program digest recorded in traces (TEST-010).

#### 3.7.8 Trusted modules and interface nondeterminism (LANG-204, LANG-205)

`trusted("hand-verified Paxos; see docs/proofs/paxos.md") module Paxos { … }` exempts the module's internals from
the CALM report; VER-020 checks it against its interface spec instead. `nondet("…")` on an `output` declaration
exports accepted nondeterminism through the interface so importers see it in their own reports.

### 3.8 Distribution

#### 3.8.1 Channel fault models (LANG-155, CR-12)

```blossom
channel shuffle(@r: reducer, split: u32, word: str, n: u64) delivery reliable
channel hint(@to: Node, leader: Node) delivery lossy
```

`delivery` is one of `lossy` (the default: a message may never arrive), `lossy_forever` (loss modeled as delay
forever), `reliable` (unordered, eventually delivered), `ordered` (reliable ordered prefix per sender). It sets the
receiving relation's stream properties (ANA-030) and drives the simulator; the normative semantics is still fair
async delivery with the declared fault model (CR-12). It adds no rules: `.decl shuffle(…) channel delivery(reliable)`.

#### 3.8.2 Partitioning (LANG-154)

```blossom
durable table kv(key: str -> val: bytes) partition by hash(key)
async fwd(@kv.owner(k), k, v) :- put(k, v) where kv.owner(k) != self.
```

`partition by e` declares the routing function; `r.owner(k̄)` is the pure function that maps a key to its node
(through the static or epoch-sealed membership). It lowers to an IR function `owner(kv, K)` and a relation
attribute used by ANA-082 (co-hashing) and Blazes (ANA-043).

#### 3.8.3 Exactly-once channels for group payloads (LANG-158, CR-35, DIST-015–017)

```blossom
group Delta = ZSet<(str, u64)>
channel view_delta(@to: Node, d: Delta) exactly_once(dots)
async view_delta(@r, d) :- replicas(r), local_change(d).
```

A group-typed payload on a channel without `exactly_once(dots | cumulative | tree)` is a compile error (ANA-015).
The compiler inserts the wrapper of DIST-015/016/017: a durable per-destination out-buffer keyed by dot
`(origin, incarnation, seq)`, retransmission on a timer, cumulative causal-context acks, and receiver-side `unwrap`
that applies each dot once. The wrapper is itself a lattice stratum (a grow-only dot set), so its IR is ordinary
Dedalus^L; LIB-093 is its executable reference expansion, and the built-in is differential-tested against it.

#### 3.8.4 Seals and punctuations (LANG-207, LIB-066, ANA-065, CR-27)

```blossom
channel shuffle(@r: reducer, split: u32, word: str, n: u64) seal on (split)
async seal shuffle(@r, split: s) count k :- inserted ready(s), reducer(r),
    let k = count{ w | counts(s, w, _), reducer.route(w) == r } default 0.
all_splits_in() :- not { splits(s), not sealed shuffle(split: s) }.
```

A channel declared `seal on (k̄)` gets a companion seal channel. A `seal` head promises that the sender has sent
exactly `count` distinct tuples with key k̄ to that destination and will send no more. `sealed r(k̄)` in a body holds
once the promise can be checked complete. Lowering (sender column `P` is the implicit sender, §3.9):

```
.decl shuffle$seal(L: Node, Split: u32, Count: u64) channel
shuffle$seal(R, S, K)@async :- ready$ins(L, S), reducer(L, R), agg$k(L, S, R, K).     % the head, after §3.4.3
shuffle$rcv(L, S, W, N, P)       :- shuffle(L, S, W, N, P).                % receive log (persistent, GC'd by ANA-063)
shuffle$rcv(L, S, W, N, P)@next  :- shuffle$rcv(L, S, W, N, P).
shuffle$sl(L, S, P, K)           :- shuffle$seal(L, S, K, P).
shuffle$sl(L, S, P, K)@next      :- shuffle$sl(L, S, P, K).
shuffle$got(L, S, P, count<*>)   :- shuffle$rcv(L, S, W, N, P).
shuffle$done(L, S, P)            :- shuffle$sl(L, S, P, K), shuffle$got(L, S, P, K).
shuffle$done(L, S, P)            :- shuffle$sl(L, S, P, 0).
sealed$shuffle(L, S)             :- shuffle$done(L, S, _).
error(SealViolated, shuffle, S, P) :- shuffle$sl(L, S, P, K), shuffle$got(L, S, P, C), C > K.
```

With `seal on (k̄) per producer`, a key is sealed only when every member of the channel's sending role has sealed it
(Blazes' unanimous vote; skipped when there is one producer per partition, as here):

```
shuffle$keyseen(L, S) :- shuffle$sl(L, S, _, _).
shuffle$missing(L, S) :- shuffle$keyseen(L, S), mapper(L, P), notin shuffle$done(L, S, P).
sealed$shuffle(L, S)  :- shuffle$keyseen(L, S), notin shuffle$missing(L, S).
```

Although the expansion uses `count` and `notin`, a `sealed` literal is **positive** for the CALM analysis: by Lemma 5
of R05 it never becomes false again once true (and the `SealViolated` error enforces the promise). The analysis marks
the sealed partition CLOSED (ANA-121), and outputs guarded by seals are certified "confluent given seals S"
(ANA-029). An `input` may also be declared `seal on (k̄)`; the host then supplies the seals. A `sealed table` emits
its whole-relation seal after bootstrap (§3.1.9).

### 3.9 Principals, sessions and authorization (LANG-240–245, CR-40, SEM-091)

```blossom
channel put_req(@srv: Node, id: u64, key: str, val: bytes) accept from external
channel append(@to: server, term: u64, entries: List<Entry>)             // inferred ACL: only `server` sends
static admins(p: Principal)
static writers(p: Principal)

put_ok(s, id, k, v) :- put_req(_, id, k, v) from s principal p, writers(p).
authz_denied(s, "put", k) :- put_req(_, _, k, _) from s principal p, not writers(p).
reconfig(x) :- admin_cmd(x) from n principal p, admins(p).
```

`from s` binds the transport-authenticated sender (a `Node`, or a `Session` for external clients) and
`principal p` binds its principal; neither is part of the payload, so neither can be forged (LANG-241). Lowering:
the received channel relation gets two trailing implicit columns, projected away when no rule reads them (SEM-091):

```
.decl put_req(L: Node, Id: u64, Key: str, Val: bytes, Sender: Session, Principal: Principal) channel
put_ok(L, S, Id, K, V) :- put_req(L, Id, K, V, S, P), writers(L, P).
```

ACLs are ingress metadata, not rules (CR-40): the default is inferred from the choreography (roles with an `async`
rule into the channel); `accept from r1 | external | principal in admins` narrows or opens it (LANG-242). A rejected
message is dropped before the tick, which is an omission (SEM-090). Data-dependent authorization is ordinary rules
(LANG-244) and its denials are program outputs. `principal_of(n)` and `role_of(n)` read the node directory
(LANG-240). External clients are the `external` role; replies go to `@s` with `s: Session`, and `session_open`/
`session_closed` are built-in inputs (LANG-243). `Signed<T>` with `sign(x)` and `verify(s)` is the P2 opt-in for
end-to-end authenticity (LANG-245).

### 3.10 Program versions, schema evolution and migrations (LANG-260–265)

```blossom
program kvstore version 3

durable table kv(key: str #1 -> val: bytes #2, ttl: Option<Duration> = None #4 since 3) reserved #3
channel put_req(@srv: Node, id: u64 #1, key: str #2, val: bytes #3, ttl: Option<Duration> = None #4 since 3)
enum Op { Put #1, Del #2, Expire #3 since 3, unknown }

migrate from 2 {
    kv(k, v, None) :- old.kv(k, v, _).                 // v2's #3 column is dropped (and reserved)
}
translate put_req to 2 {
    old.put_req(@s, id, k, v) :- put_req(@s, id, k, v, _).
}
translate put_req from 2 {
    put_req(@s, id, k, v, None) :- old.put_req(@s, id, k, v).
}
expire(k) :- kv(k, _, Some(t)), stored_at(k, a) where cluster_version() >= 3, now() - a > t.
unsafe_ungated("read-only probe, safe in mixed clusters") probe(k) :- kv(k, _, Some(_)).
```

Field numbers `#n` are stable identities (the compiler assigns and locks them if omitted); `since N`, defaults,
`reserved`, `deprecated since N`, `semantics_changed since N` and enum `unknown` are `schema.lock` metadata checked by
ANA-100. A `migrate from N` block is a separate, deterministic, restartable program run at recovery (DIST-082): its
rules read `old.r` (typed by version N's locked schema) and write current durable relations; `next`, `async`,
`now()` and `random()` are forbidden. Lowering: `kv(L, K, V, none) :- old$kv(L, K, V, _).` evaluated to a fixpoint
once. `translate c to/from N` blocks must be tuple-local (one channel atom, pure functions) and run in the codec
(LANG-263, DIST-087). `cluster_version()` is a built-in `LMax<u32>` input sampled once per tick; `cluster_version() >= 3` is a threshold, so
version gates add no point of order (SEM-092). A rule that writes a `since 3` field without such a gate is rejected
unless it carries `unsafe_ungated("…")` (ANA-102).

### 3.11 Specs, invariants and verification

#### 3.11.1 Runtime invariants (LANG-200)

```blossom
invariant one_vote_per_term: never voted_for(t, a), voted_for(t, b) where a != b.
```

```
violation(L, "one_vote_per_term", (T, A, B)) :- voted_for(L, T, A), voted_for(L, T, B), A != B.
```

Inside a module an invariant is checked at every tick of the running node; the action (abort, alert, log with
provenance, ship to a checker) is deployment configuration. `violation` feeds nothing else.

#### 3.11.2 Spec blocks (LANG-201, LANG-070, TEST-020–022, TEST-080, VER-001)

```blossom
spec Delivery for SimpleBcast(peers: node) {
    nodes A, B, C
    faults { eot: 4, eff: 2, crashes: 0, model: sync }
    node(@x, y) :- for x in [A, B, C], for y in [A, B, C].
    bcast(@A, "hello") at 1.
    missing(x, pl) :- log(@y, pl), node(@y, x), not log(@x, pl).
    pre(x, pl) :- log(@x, pl), not bcast(@x, pl) at 1, not crash(x, _).
    post(x, pl) :- log(@x, pl), not missing(_, pl).
    expect fails
}
```

A spec is a separate program evaluated over the *trace* of the module under test, never on a node. Inside a spec:

- `nodes A, B, C` declares the simulated nodes. Node names are `Upper` identifiers, so they are constants and can
  never be confused with variables.
- Every located relation is written with its location explicitly, `log(@x, pl)`. Spec rules may join across
  locations (LANG-201). Spec-local relations are declared by their rules, with inferred schemas as for `temp`;
  they are unlocated (`missing`, `pre`, `post`) unless their head marks a location (`node(@x, y)`), in which case
  they are per-node inputs to the module under test and can be bound to its relation parameters
  (`SimpleBcast(peers: node)`).
- An atom with no time qualifier is read at the evaluation point: EOT for `pre`/`post` (TEST-022), every tick for
  `invariant … never`. `r(@n, …) at k` reads tick `k` (LANG-070). `once r(@n, …)` means "at some tick up to the
  evaluation point" (past-time *once*).
- Oracles are available only here (CR-20, ANA-010): `crash(n, t)`, `hb(n1, t1, n2, t2)`, and the trace relations.
- Facts with `at k` are input events (tick `k`, node given by `@`); facts without `at` are static at every node.
- `faults { eot, eff, crashes, model: sync|async, delay, round }` is the failure spec (TEST-020, CR-21); `round`
  maps physical timers to rounds (ODD-16).
- `expect holds | fails` turns the LDFI verdict into a test assertion (TEST-039).

Lowering: every protocol relation `r` has the trace relation `r$log(N, X̄, T)` (TEST-080). `log(@x, pl)` at EOT
becomes `log$log(X, Pl, EOT)`; `bcast(@x, pl) at 1` becomes `bcast$log(X, Pl, 1)`; `once r(@n, x̄)` becomes
`r$log(N, X̄, T), T <= Tnow`. The spec's rules are then plain stratified Datalog over those relations (TEST-081).

#### 3.11.3 Safety over histories, bounded liveness, and inductive proofs (VER-001, VER-006–010)

```blossom
invariant one_leader_per_term:
    never once leader_of(@a, t, a), once leader_of(@b, t, b) where a != b.
liveness all_delivered: eventually post(x, pl) within 2 after eff.
invariant leader_has_quorum:
    always forall a: Node, t: u64:
        (role(@a, Leader) and current_term(@a, t))
        implies count{ v | voted_for(@v, t, a) } >= majority{ v | server(@a, v) }.
prove leader_has_quorum by induction
```

`never` conjunctions are denial constraints; `always` formulas are first-order and go to the VER-006 transition
system and SMT (VER-010); `majority{…}` maps to the quorum sort with its intersection axiom (VER-008). A `liveness`
item is VER-001's bounded liveness. `prove name by induction` asks for an inductive proof; an optional
`using { invariant … }` block names auxiliary invariants (E10 has a complete one).

#### 3.11.4 Final outputs (LANG-212, ANA-120–122)

```blossom
final output word_count(word: str -> n: u64)
early(w) :- word_count(w, n) where when_final(n >= 1000).
```

`final output` is a compile error unless ANA-120 classifies the output as POS-, NEG-, TOP-, THRESH-, FINITE- or
SEALED-final; emission is gated at runtime (ANA-121/122) and every tuple carries its status (`provisional`,
`final_present`, `final_absent`). `is_final(atom)` and `when_final(cond)` are threshold built-ins. No rules are added;
the IR carries the classification.

#### 3.11.5 Progressive snapshots (LANG-139)

```blossom
snapshot partial of word_count at progress every 0.1 upto 0.9 mode committed_only
```

This desugars to a `reveal` of the output's lattice gated by a threshold on the progress lattice (the mean of the
producers' `LMax<progress>`), emitting `partial(point, actual_progress, class, attempt, value)` rows; it is typed
`nondet("progressive")` and carries the ANA-036 class.

### 3.12 CALM at a glance

| Monotone (positive edges; no coordination needed) | Non-monotone (negative edges; points of order) |
|---|---|
| joins, `let`, `for`, guards on bound values, head expressions | `not`, `not { … }` |
| `async` into channels (the send itself) | `optional` |
| lattice merges (`:-` and `next` into lattice relations) | relational aggregates and `group` stages |
| morphisms, bimorphisms, monotone lattice methods | `choose` (all forms), `index()`, `seq()`, `fold`, `reduce` |
| thresholds: `s.size >= k`, `x >= c`, `LBool` guards, `threshold(…)`, `when_final`, `cluster_version() >= n` | `reveal`, Anti/NM lattice methods, `==` via `reveal` |
| lattice folds `lset{…}`, `lmax{…}`, … | `inserted`, `removed` |
| `sealed r(k)` | `delete`, `upsert`, `resolve` |
| `stable` methods guarded by their threshold | `rand`, `now()`, `tick()` (time-varying, schedule-dependent) |

Every right-column keyword gets a stable site id, shows up in the points-of-order report (ANA-022) with its source
span, and is what the `monotone` assertion forbids. `final`, `nondet("…")` and `trusted("…")` are the three
annotations by which the programmer states CALM facts the compiler then checks or tracks.

### 3.13 Coverage of FEATURES.md §2 (P0 and P1)

| Area | LANG items | Where |
|---|---|---|
| Program structure | 001–010 | §3.7, §1.2 |
| Types and schemas | 020–028 | §3.1.1, §2.1 (`r160` ring ids, `ring(a, b]`), §3.3.12 (opaque host values through `extern` types), blob handles are a `Blob` library type |
| Collections | 040–053 | §3.1 |
| Rules and time | 060–071 | §3.2, §3.3.11, §3.11.2 (`at k`); LANG-072 entanglement (P2) is not provided |
| Bodies | 080–098 | §3.3, §3.4.7 |
| Aggregation and choice | 100–118 | §3.4 (LANG-107, P2, is not provided) |
| Lattices | 120–142, 280–284 | §3.5, §3.4.4 |
| Locations | 150–158 | §3.2.3, §3.7.6, §3.8 |
| Time and randomness | 170–175 | §3.6 |
| Functions and host | 180–186 | §3.3.12, §3.1.6 (LANG-186 handlers: `output r(…) handler "rust::path"` clause, same shape as `nondet`) |
| Bootstrap | 190 | §3.2.6 |
| Assertions, specs | 200–212 | §3.11, §3.2.7, §3.1.12 (catalog) |
| Compatibility | 220–223 | `.ded` files (P1); others P2 |
| Security | 240–245 | §3.9 |
| Versions | 260–265 | §3.10 |

---

## 4. Example corpus

Each example is a complete `.bls` file. Where an example uses another (E9 uses E2 and E8, E10 uses E3), it says
`use` and does not repeat the other file.

### E1. Key-value store node

Put, get and delete with acknowledgements; a durable table; upsert semantics. Clients are external sessions.

```blossom
// file: kvstore/kv_node.bls
program kvstore version 1

module KvNode(MAX_VAL: u64 = 1_048_576) {
    // ---------- wire protocol (clients are external sessions, LANG-243)
    channel put_req(@srv: Node, id: u64, key: str, val: bytes) accept from external
    channel get_req(@srv: Node, id: u64, key: str)             accept from external
    channel del_req(@srv: Node, id: u64, key: str)             accept from external
    channel put_ack(@to: Session, id: u64, ok: bool)
    channel get_resp(@to: Session, id: u64, val: Option<bytes>)
    channel del_ack(@to: Session, id: u64, ok: bool, existed: bool)

    // ---------- state
    durable table kv(key: str -> val: bytes)
    static writers(p: Principal)                       // supplied by the deployment configuration

    // ---------- puts: authorize, validate, one winner per key per tick, upsert at t+1
    scratch put_ok(s: Session, id: u64, key: str, val: bytes)
    put_ok(s, id, key, val) :-
        put_req(_, id, key, val) from s principal p,
        writers(p)
        where len(val) <= MAX_VAL.

    // Puts to one key in one tick are concurrent; the greatest (session, id) wins, deterministically.
    scratch put_win(key: str -> val: bytes)
    pick_put: put_win(key, val) :- put_ok(s, id, key, val), choose key -> val most (s, id).

    apply_put: upsert kv(key, val) :- put_win(key, val).

    async put_ack(@s, id, true)  :- put_ok(s, id, _, _).
    async put_ack(@s, id, false) :- put_req(_, id, _, _) from s, not put_ok(s, id, _, _).

    // ---------- deletes: remove the exact current tuple at t+1
    scratch del_ok(s: Session, id: u64, key: str)
    del_ok(s, id, key) :- del_req(_, id, key) from s principal p, writers(p).

    apply_del: delete kv(key, v) :- del_ok(_, _, key), kv(key, v).
    // A put and a delete of one key in one tick: the upsert re-inserts, and insert wins (CR-05).

    async del_ack(@s, id, true, v.is_some()) :- del_ok(s, id, key), optional kv(key, v).
    async del_ack(@s, id, false, false) :- del_req(_, id, _) from s, not del_ok(s, id, _).

    // ---------- gets: read the state as of this tick (CR-04); a put in this tick is visible from t+1
    async get_resp(@s, id, v) :- get_req(_, id, key) from s, optional kv(key, v).
}
```

Selected lowering (the put path; `S`, `P` are the implicit sender and principal columns):

```
put_ok(L, S, Id, K, V)    :- put_req(L, Id, K, V, S, P), writers(L, P), len(V) <= 1048576.
choose$pick_put$cand(L, K, V, S, Id)   :- put_ok(L, S, Id, K, V).
choose$pick_put$best(L, K, max<C>)     :- choose$pick_put$cand(L, K, V, S, Id), C = (S, Id).
choose$pick_put$top(L, K, V, S, Id)    :- choose$pick_put$cand(L, K, V, S, Id), choose$pick_put$best(L, K, C),
                                          C == (S, Id).
choose$pick_put$pmin(L, K, min<P>)     :- choose$pick_put$top(L, K, V, S, Id), P = prio(pick_put, (K), (V, S, Id)).
choose$pick_put$chosen(L, K, V, S, Id) :- choose$pick_put$top(L, K, V, S, Id), choose$pick_put$pmin(L, K, P),
                                          P == prio(pick_put, (K), (V, S, Id)).
put_win(L, K, V)          :- put_ok(L, S, Id, K, V), choose$pick_put$chosen(L, K, V, S, Id).
kv$ups(L, K, V)           :- put_win(L, K, V).
kv$del(L, K, V0)          :- kv$ups(L, K, _), kv(L, K, V0).
kv$del(L, K, V)           :- del_ok(L, _, _, K), kv(L, K, V).
kv(L, K, V)@next          :- kv$ups(L, K, V).
kv(L, K, V)@next          :- kv(L, K, V), notin kv$del(L, K, V).
put_ack(S, Id, true)@async :- put_ok(L, S, Id, _, _).
```

`kv` is durable, so each tick's `kv$ups`/`kv$del` effects are fsynced before any `put_ack` leaves
(SEM-072). CALM report: points of order are `choose` (seed-dependent winner), `optional`, `not`, `upsert`, `delete`;
all are local to the node, so the node is deterministic given the seed.

### E2. Reliable broadcast as a reusable module (bud-sandbox ReliableDelivery style)

```blossom
// file: lib/delivery.bls
type MsgId = { origin: Node, seq: u64 }

protocol Delivery {
    input  pipe_in(dst: Node, src: Node, ident: MsgId -> payload: bytes)
    output pipe_sent(dst: Node, src: Node, ident: MsgId -> payload: bytes)   // sender: delivery complete
    output pipe_out(dst: Node, src: Node, ident: MsgId -> payload: bytes)    // receiver: delivered
}

module BestEffortDelivery implements Delivery {
    channel pipe_chan(@dst: Node, src: Node, ident: MsgId -> payload: bytes)
    snd:  async pipe_chan(@d, s, i, p) :- pipe_in(d, s, i, p).
    rcv:  pipe_out(d, s, i, p) :- pipe_chan(d, s, i, p).
    done: pipe_sent(d, s, i, p) :- pipe_in(d, s, i, p).      // "more like an effort"
}

module ReliableDelivery(RETRY: Duration = 2s) implements Delivery {
    import BestEffortDelivery as bed

    table buf(dst: Node, src: Node, ident: MsgId -> payload: bytes)
    channel ack(@src: Node, dst: Node, ident: MsgId)
    timer retry every RETRY

    block remember {
        buf(d, s, i, p) :- pipe_in(d, s, i, p).
        bed.pipe_in(d, s, i, p) :- pipe_in(d, s, i, p).
        bed.pipe_in(d, s, i, p) :- buf(d, s, i, p), retry(..).      // retransmit everything unacked
    }
    block rcv {
        pipe_out(d, s, i, p) :- bed.pipe_out(d, s, i, p).
        async ack(@s, d, i) :- bed.pipe_out(d, s, i, _).
    }
    block done {
        temp msg_acked(d, s, i, p) :- buf(d, s, i, p), ack(s, d, i).
        pipe_sent(d, s, i, p) :- msg_acked(d, s, i, p).
        delete buf(d, s, i, p) :- msg_acked(d, s, i, p).
    }
}
```

```blossom
// file: lib/broadcast.bls
use lib::delivery::{Delivery, ReliableDelivery, MsgId}

protocol Broadcast {
    input  bcast(seq: u64 -> payload: bytes)
    output deliver(origin: Node, seq: u64 -> payload: bytes)   // exactly once per message at every node
    output bcast_done(seq: u64)                                // at the origin: every peer has acknowledged
}

module ReliableBroadcast(peers: rel(n: Node), D: Delivery = ReliableDelivery) implements Broadcast {
    import D as d

    table delivered(ident: MsgId -> payload: bytes)
    table acked(seq: u64 -> who: LSet<Node>)
    scratch others(n: Node)
    scratch complete(seq: u64)

    others(n) :- peers(n) where n != self.

    // The origin delivers its own message at once; everyone else learns it from any copy, origin or relay.
    originate: delivered(MsgId { origin: self, seq: i }, p) :- bcast(i, p).
    receive:   delivered(m, p) :- d.pipe_out(_, _, m, p).

    // First sight of a message: hand it to the application once, and relay it to every other peer.
    // Relaying is what makes this *reliable* broadcast: if the origin crashes after reaching one correct
    // node, that node's retransmissions reach everybody (classic RB).
    hand_off: deliver(m.origin, m.seq, p) :- inserted delivered(m, p).
    relay:    d.pipe_in(n, self, m, p) :- inserted delivered(m, p), others(n).

    // Completion at the origin: its copy was acknowledged by every other peer.
    acked(m.seq, {n}) :- d.pipe_sent(n, _, m, _) where m.origin == self.
    complete(m.seq) :- delivered(m, _) where m.origin == self, acked[m.seq] >= lset{ n | others(n) }.
    bcast_done(i) :- inserted complete(i).
}
```

Notes. `acked[m.seq]` is a lookup, so it yields ⊥ for a message nobody has acknowledged yet, and the threshold
`⊥ ⊒ lset{}` makes a one-node cluster complete immediately. `acked` and the threshold are monotone; the only points
of order are the two `inserted` literals (exactly-once hand-off to the application) and ReliableDelivery's
`delete buf` (garbage collection after acknowledgement). Lowering of `relay`, with `d` a ReliableDelivery instance:

```
delivered$was(L, M, P)@next :- delivered(L, M, P).
delivered$ins(L, M, P)      :- delivered(L, M, P), notin delivered$was(L, M, P).
d$pipe_in(L, N, L, M, P)    :- delivered$ins(L, M, P), others(L, N).
d$buf(L, N, L, M, P)        :- d$pipe_in(L, N, L, M, P).
d$bed$pipe_in(L, N, L, M, P) :- d$pipe_in(L, N, L, M, P).
d$bed$pipe_chan(N, L, M, P)@async :- d$bed$pipe_in(L, N, L, M, P).
```

### E3. Raft leader election, as rules

Persistent `current_term`/`voted_for`, randomized election timeout, RequestVote and its response, majority count,
step-down on a higher term. No step function: every decision is a rule, serialized within a tick by "term first" and
"one vote per term per tick" (R07 §11.1, FLAG-001/002).

```blossom
// file: raft/election.bls
enum Role { Follower, Candidate, Leader }

module RaftElection(
    peers: rel(n: Node),                         // every server, self included (static membership)
    log: rel(idx: u64 -> term: u64),             // the log as seen by the replication module
    ELECTION_MIN: Duration = 150ms,
    ELECTION_MAX: Duration = 300ms,
    HEARTBEAT: Duration = 50ms,
) {
    channel request_vote(@to: Node, term: u64, cand: Node, last_idx: u64, last_term: u64)
    channel vote_resp(@to: Node, term: u64, granted: bool)
    channel heartbeat(@to: Node, term: u64, leader: Node)

    durable table current_term(-> term: u64)
    durable table voted_for(term: u64 -> cand: Node)
    table role(-> r: Role)
    table deadline(-> at: Time)
    table votes(term: u64 -> who: LSet<Node>)

    output leader_of(term: u64 -> leader: Node)
    output became_leader(term: u64)

    timer clock every 10ms
    timer beat every HEARTBEAT

    bootstrap fresh {
        current_term(0).                              // durable: only on first start, never after recovery
    }
    bootstrap {
        role(Follower).                               // volatile: at the first tick of every incarnation
        deadline(d) :- let d = now() + rand_duration(ELECTION_MIN, ELECTION_MAX, ("deadline", 0)).
    }

    // ------------------------------------------------ term first (FLAG-001)
    scratch msg_term(t: u64)
    msg_term(t) :- (request_vote(term: t, ..) or vote_resp(term: t, ..) or heartbeat(term: t, ..)).

    scratch eff_term(-> t: u64)
    eff_term(t) :- (current_term(x) or msg_term(x)) |> group { t = max(x) }.

    scratch stepped_down()
    stepped_down() :- current_term(c), eff_term(e) where e > c.

    // ------------------------------------------------ the log's tip, for the election restriction (FLAG-003)
    scratch last_log(-> idx: u64, term: u64)
    last_log(i, t) :- log(i, t) where i == max{ j | log(j, _) }.
    last_log(0, 0) :- not log(_, _).

    // ------------------------------------------------ voter side
    scratch up_to_date(cand: Node, term: u64)
    up_to_date(c, t) :-
        request_vote(term: t, cand: c, last_idx: li, last_term: lt, ..) from c,   // cand must be the sender
        eff_term(t), last_log(mi, mt)
        where lt > mt or (lt == mt and li >= mi).

    scratch may_vote(cand: Node, term: u64)
    may_vote(c, t) :- up_to_date(c, t), not voted_for(t, _).
    may_vote(c, t) :- up_to_date(c, t), voted_for(t, c).                       // re-grant to the same candidate

    scratch grant(term: u64 -> cand: Node)
    one_vote: grant(t, c) :- may_vote(c, t), choose t -> c.                    // at most one vote per term per tick

    record_vote: next voted_for(t, c) :- grant(t, c).                          // durable: fsynced before the reply
    async vote_resp(@c, t, true)  :- grant(t, c).
    async vote_resp(@c, e, false) :- request_vote(term: t, ..) from c, eff_term(e), not grant(t, c).

    // ------------------------------------------------ candidate side
    tally: votes(t, {v}) :- vote_resp(term: t, granted: true, ..) from v, current_term(t), role(Candidate).

    scratch won(term: u64)
    won(t) :- votes(t, who), current_term(t), role(Candidate)
              where who.size >= majority{ n | peers(n) }.

    // ------------------------------------------------ election timer
    scratch heard_leader()
    heard_leader() :- heartbeat(term: t, ..), eff_term(t).

    scratch start_election(term: u64)
    timeout: start_election(e + 1) :-
        clock(..), deadline(d), role(r), eff_term(e),
        not heard_leader(), not won(_), not grant(_, _)
        where now() >= d, r != Leader.

    async request_vote(@n, t, self, li, lt) :- start_election(t), peers(n), last_log(li, lt) where n != self.

    scratch reset_timer()
    reset_timer() :- (heard_leader() or grant(_, _) or start_election(_)).
    upsert deadline(d) :- reset_timer(), eff_term(t),
        let d = now() + rand_duration(ELECTION_MIN, ELECTION_MAX, ("deadline", t)).

    // ------------------------------------------------ state transitions (all take effect at t+1)
    upsert current_term(t) :- start_election(t).
    upsert current_term(e) :- stepped_down(), eff_term(e), not start_election(_).
    next voted_for(t, self) :- start_election(t).
    next votes(t, {self}) :- start_election(t).

    upsert role(Candidate) :- start_election(_).
    upsert role(Follower)  :- stepped_down(), not start_election(_).
    upsert role(Follower)  :- heard_leader(), role(Candidate), not stepped_down(), not start_election(_), not won(_).
    upsert role(Leader)    :- won(_), not stepped_down(), not start_election(_).

    // ------------------------------------------------ leadership
    became_leader(t) :- won(t), not stepped_down().
    leader_of(t, self) :- role(Leader), current_term(t), not stepped_down().
    leader_of(t, l)    :- heartbeat(term: t, leader: l, ..) from l, eff_term(t).
    async heartbeat(@n, t, self) :- became_leader(t), peers(n) where n != self.
    async heartbeat(@n, t, self) :- beat(..), role(Leader), current_term(t), peers(n), not stepped_down()
                                    where n != self.
}
```

Why this is safe within one tick (R07 §11.1):

- Every message older than `eff_term` is ignored or refused, which is the serialization "highest term first".
- `choose t -> c` grants at most one candidate per term per tick. `may_vote` refuses any other candidate once a vote
  is on disk, and `voted_for`'s key makes a second vote a hard error rather than a silent overwrite.
- The vote reply leaves only after `voted_for` is durable (SEM-072), so a restarted voter cannot vote twice.
- The candidate's own vote is recorded with `next`, which breaks the same-tick cycle
  `start_election → votes → won → not won`. Every `upsert role(…)` rule excludes the others, so SEM-051 never fires.
- `leader_of` is keyed by term. A node that believes it leads term t and hears another leader of t derives two
  values for one key, which is a hard error: the "one leader per term" invariant is checked at runtime for free, and
  E10 states it as a spec.

Stratification: `eff_term`, `last_log`, `won`, `one_vote`, `heard_leader` are below `timeout`; the only negative
edges are local (`not`, `max`, `group`, `choose`, `majority`), and the node-to-node edges are all `async`, so the
program is accepted (SEM-020). Lowering of the timeout rule (negated atoms with wildcards are projected first,
§3.3.4):

```
won$any(L) :- won(L, _).
grant$any(L) :- grant(L, _, _).
start_election(L, E1) :- clock(L, _, _), deadline(L, D), role(L, R), eff_term(L, E), now(L, T),
                         notin heard_leader(L), notin won$any(L), notin grant$any(L),
                         T >= D, R != Leader, E1 = E + 1.
```

### E4. Two-phase commit as one choreography, with timeout-abort

```blossom
// file: commit/two_phase.bls
enum Vote { Yes, No }
enum Outcome { Commit, Abort }

choreography TwoPhaseCommit(TIMEOUT: Duration = 5s, RESEND: Duration = 500ms) {
    role coordinator: process
    role participant: cluster

    channel prepare(@to: participant, xid: u64)
    channel vote(@to: coordinator, xid: u64, v: Vote)
    channel decision(@to: participant, xid: u64, o: Outcome)
    channel decision_ack(@to: coordinator, xid: u64)

    on coordinator {
        input  begin(xid: u64)
        output decided(xid: u64 -> o: Outcome)

        durable table running(xid: u64 -> started: Time)
        durable table outcome(xid: u64 -> o: Outcome)
        table voted(xid: u64, p: Node)
        table yes_votes(xid: u64 -> who: LSet<Node>)
        table acked(xid: u64, p: Node)
        timer resend every RESEND

        start: next running(x, now()) :- begin(x), not running(x, _), not outcome(x, _).

        // phase 1: ask every participant; re-ask those that have not voted
        scratch send_prepare(xid: u64, p: Node)
        send_prepare(x, p) :- inserted running(x, _), participant(p).
        send_prepare(x, p) :- resend(..), running(x, _), participant(p), not outcome(x, _), not voted(x, p).
        async prepare(@p, x) :- send_prepare(x, p).

        voted(x, p) :- vote(xid: x, ..) from p, running(x, _).
        yes_votes(x, {p}) :- vote(xid: x, v: Yes, ..) from p, running(x, _).

        // decide exactly once per transaction
        scratch any_no(xid: u64)
        any_no(x) :- vote(xid: x, v: No, ..), running(x, _).
        scratch all_yes(xid: u64)
        all_yes(x) :- running(x, _) where yes_votes[x] >= lset{ p | participant(p) }.
        scratch timed_out(xid: u64)
        timed_out(x) :- resend(..), running(x, t0) where now() - t0 >= TIMEOUT.

        scratch decide(xid: u64 -> o: Outcome)
        commit: decide(x, Commit) :- all_yes(x), not outcome(x, _).
        abort:  decide(x, Abort)  :- (any_no(x) or timed_out(x)), not all_yes(x), not outcome(x, _).
        log_outcome: next outcome(x, o) :- decide(x, o).   // durable: on disk before any decision message leaves
        decided(x, o) :- decide(x, o).

        // phase 2: tell every participant; re-tell those that have not acknowledged
        scratch send_decision(xid: u64, p: Node, o: Outcome)
        send_decision(x, p, o) :- decide(x, o), participant(p).
        send_decision(x, p, o) :- resend(..), outcome(x, o), participant(p), not acked(x, p).
        async decision(@p, x, o) :- send_decision(x, p, o).
        acked(x, p) :- decision_ack(xid: x, ..) from p.

        // the transaction is no longer running once its outcome is durable
        delete running(x, t) :- running(x, t), outcome(x, _).
    }

    on participant {
        input  can_commit(xid: u64 -> ok: bool)          // the local resource manager's answer
        output apply(xid: u64 -> o: Outcome)

        table local_ok(xid: u64 -> ok: bool)
        durable table prepared(xid: u64 -> v: Vote)
        durable table done(xid: u64 -> o: Outcome)

        local_ok(x, ok) :- can_commit(x, ok).

        // vote once, durably; a re-sent prepare gets the logged vote again
        scratch my_vote(xid: u64 -> v: Vote)
        my_vote(x, v) :- prepare(xid: x, ..), prepared(x, v).
        my_vote(x, if ok then Yes else No) :- prepare(xid: x, ..), not prepared(x, _), local_ok(x, ok).
        next prepared(x, v) :- my_vote(x, v), not prepared(x, _).
        async vote(@c, x, v) :- my_vote(x, v), prepare(xid: x, ..) from c.

        // learn the decision once; always acknowledge (the ack may have been lost)
        apply(x, o) :- decision(xid: x, o, ..), not done(x, _).
        next done(x, o) :- decision(xid: x, o, ..), not done(x, _).
        async decision_ack(@c, x) :- decision(xid: x, ..) from c.

        invariant agreement: never done(x, o1), decision(xid: x, o: o2, ..) where o1 != o2.
    }
}
```

The coordinator's timeout rule is what makes this Molly's `2pc_timeout` rather than blocking 2PC; participants still
block if the coordinator dies after they vote Yes, which is 2PC's known limitation (E10-style LDFI finds it with one
crash). The inferred ACLs (LANG-242) are: only the coordinator may send `prepare` and `decision`, only participants
may send `vote` and `decision_ack`. Lowering puts a role guard on every rule, e.g.

```
decide(L, X, Commit) :- role$coordinator(L), all_yes(L, X), notin outcome$any(L, X).
outcome$any(L, X) :- outcome(L, X, _).
vote(C, X, V)@async  :- role$participant(L), my_vote(L, X, V), prepare(L, X, C).
```

and projection drops the guard in each role's binary.

### E5. Lattices: vector clocks, a Bloom^L shopping cart, a quorum threshold, one user lattice

```blossom
// file: lattices/demo.bls
type VClock = LMap<Node, LMax<u64>>
enum Action { Add, Remove }
type Line = { item: str, action: Action, n: u32 }

// ---------------------------------------------------------------- the user-defined lattice
// A cart is the set of numbered operations plus the id of the checkout operation. Operations may
// arrive in any order and more than once; two different operations with one id are a client bug and
// a hard error (Point). The cart is complete when the checkout is known and every earlier id is present.
lattice Cart {
    lines: LMap<u64, Point<Line>>,
    checkout: WithBot<Point<u64>>,

    monotone fn is_complete(self) -> LBool =
        match self.checkout.get() {
            Some(k) => all([self.lines.has_key(i) for i in 1..k]),
            None => false,
        }

    // "monotonic then immutable": once complete, the summary never changes as the cart grows
    stable fn summary(self) -> Map<str, i64> after is_complete =
        sum_by_key([(l.item, if l.action == Add then l.n as i64 else -(l.n as i64))
                    for (i, l) in self.lines.entries() if i < self.checkout.get_or(0)])
}

// ---------------------------------------------------------------- vector clocks
module VectorClocks(peers: rel(n: Node)) {
    input  local_event(id: u64 -> payload: str)
    output happened_before(a: u64, b: u64)
    output concurrent(a: u64, b: u64)

    channel gossip(@to: Node, clock: VClock)
    cell vc: VClock                                   // this node's clock (persistent lattice)
    scratch cell next_vc: VClock                      // the clock after this tick's event
    table stamped(id: u64 -> clock: VClock)

    // every tick that has an event: merge what arrived, then increment our own entry once
    next_vc(vc[]) :- (local_event(..) or gossip(..)).
    next_vc(c) :- gossip(clock: c).
    next_vc({self => (vc[].at(self) \/ 0) + 1}) :- (local_event(..) or gossip(..)).
    next vc(next_vc[]) :- (local_event(..) or gossip(..)).

    stamped(i, next_vc[]) :- local_event(i, _).
    async gossip(@n, next_vc[]) :- local_event(..), peers(n) where n != self.

    happened_before(i, j) :- stamped(i, a), stamped(j, b) where a < b.
    concurrent(i, j) :- stamped(i, a), stamped(j, b) where i < j, a.concurrent_with(b).
}

// ---------------------------------------------------------------- the monotone shopping cart (SoCC Fig. 10)
module CartReplica {
    channel cart_action(@replica: Node, session: u64, op: u64, line: Line)
    channel cart_checkout(@replica: Node, session: u64, op: u64, reply_to: Node)
    channel cart_response(@to: Node, session: u64, items: Map<str, i64>)

    table sessions(session: u64 -> cart: Cart)
    table reply_to(session: u64 -> to: Node)
    scratch complete(session: u64)

    sessions(s, Cart { lines: {op => Point(line)} }) :- cart_action(session: s, op, line, ..).
    sessions(s, Cart { checkout: Point(op) }) :- cart_checkout(session: s, op, ..).
    reply_to(s, t) :- cart_checkout(session: s, reply_to: t, ..).

    complete(s) :- sessions(s, c) where c.is_complete().
    // One response per completed cart. The `inserted` is the rule's only point of order; dropping it gives
    // the fully monotone Bloom^L original, which re-sends the response every tick (ODD-05).
    respond: async cart_response(@t, s, c.summary()) :- inserted complete(s), sessions(s, c), reply_to(s, t).
}

// ---------------------------------------------------------------- the monotone quorum (SoCC Fig. 3)
module QuorumVote(QUORUM: u64 = 5, RESULT_ADDR: Node) {
    channel vote_chn(@coord: Node)
    channel result_chn(@to: Node)
    cell votes: LSet<Node>

    monotone collect: votes({v}) :- vote_chn(..) from v.
    monotone decide: async result_chn(@RESULT_ADDR) :- votes[].size >= QUORUM.
}
```

The `monotone` assertions on `collect` and `decide` are checked: the first is a lattice merge, the second is a lookup
of a 0-ary lattice (monotone, N5) under a `size ≥ k` threshold. Its lowering is exactly R11/G1 §3.6:

```
votes(L; {V}) :- vote_chn(L, V).                 % V is the implicit sender column
votes(L; X)@next :- votes(L; X).
result_chn(RESULT_ADDR)@async :- X = votes[L], X.size >= 5.
```

Replacing the threshold with `count{ v | vote_log(v) } >= QUORUM` over a set table would compile, but `decide` would
then fail its `monotone` assertion with "relational aggregate `count` is non-monotone; use `lset{…}.size`".

### E6. Word-count MapReduce with a sealed shuffle

Mappers count words per split, hash-partition the counts to reducers over a channel, and punctuate each split at
every reducer with a seal that carries the number of tuples sent there. A reducer's counts are final once every
split is sealed.

```blossom
// file: mr/wordcount.bls
choreography WordCount {
    role mapper: cluster
    role reducer: cluster

    static splits(split: u32)                                  // the job's input splits (job configuration)

    channel shuffle(@r: reducer, split: u32, word: str, n: u64) delivery reliable seal on (split)

    on mapper {
        // the host feeds each assigned split line by line, then seals it with the line count
        input line(split: u32, lineno: u64 -> text: str) seal on (split)

        table lines(split: u32, lineno: u64 -> text: str)
        scratch counts(split: u32, word: str -> n: u64)
        scratch ready(split: u32)

        lines(s, ln, t) :- line(s, ln, t).
        // `ln` and `pos` are named so that repeated words are counted, not collapsed (§3.4.1)
        counts(s, w, n) :- lines(s, ln, text), for (pos, w) in enumerate(split_words(text))
                           |> group by s, w { n = count() }.
        ready(s) :- sealed line(split: s).

        // map output: once per split, pre-combined per (split, word), routed by hash of the word
        emit: async shuffle(@reducer.route(w), s, w, n) :- inserted ready(s), counts(s, w, n).

        // punctuation: to every reducer, the number of shuffle tuples of this split it was sent
        punctuate: async seal shuffle(@r, split: s) count k :-
            inserted ready(s), reducer(r),
            let k = count{ w | counts(s, w, _) where reducer.route(w) == r } default 0.

        // the split has been shipped; its lines are garbage
        delete lines(s, ln, t) :- lines(s, ln, t), ready(s).
    }

    on reducer {
        table partials(split: u32, word: str -> n: u64)
        scratch all_in()
        final output word_count(word: str -> n: u64)

        partials(s, w, n) :- shuffle(_, s, w, n).

        // ∀ s ∈ splits: sealed(s), written as a double negation over the static split list
        all_in() :- not { splits(s), not sealed shuffle(split: s) }.

        reduce: word_count(w, total) :- all_in(), partials(s, w, n) |> group by w { total = sum(n) }.
    }
}
```

Why `final` compiles (ANA-120): `word_count` depends on `partials`, which only grows and is fed only by `shuffle`,
and on `all_in`, which holds only when every split's `shuffle` partition is sealed, hence CLOSED; the `sum` is then
SEALED-final. Early per-word results would be a threshold (`when_final(total >= 1000)`, §3.11.4) and could be
emitted before the seals (THRESH). Each partial is keyed by `(split, word)` and sent once, so the sum over the set of
`(split, n)` valuations is exact and no exactly-once wrapper is needed (CR-35: the reducer never integrates a
stream; it sums a keyed table). Lowering of the reducer's barrier:

```
not$1(L, S) :- role$reducer(L), splits(L, S), notin sealed$shuffle(L, S).
not$1$any(L) :- not$1(L, _).
all_in(L) :- role$reducer(L), notin not$1$any(L).
word_count$g1$in(L, W, S, N) :- role$reducer(L), all_in(L), partials(L, S, W, N).
word_count(L, W, sum<N>) :- word_count$g1$in(L, W, S, N).
```

with `sealed$shuffle` expanded as in §3.8.4 (one producer per split, so no unanimous vote).

### E7. Single-node analytics: transitive closure, shortest paths, stratified negation

```blossom
// file: analytics/graph.bls
module GraphAnalytics {
    static node(n: u32)
    static edge(src: u32, dst: u32 -> w: u64)
    static source(n: u32)

    node(1). node(2). node(3). node(4). node(5). node(6). node(7). node(8).
    edge(1, 2, 7). edge(1, 3, 9). edge(1, 6, 14). edge(2, 3, 10). edge(2, 4, 15).
    edge(3, 4, 11). edge(3, 6, 2). edge(4, 5, 6). edge(6, 5, 9). edge(5, 3, 1). edge(7, 8, 3).
    source(1).

    // ---- transitive closure (positive recursion, one stratum)
    scratch reach(src: u32, dst: u32)
    reach(a, b) :- edge(a, b, _).
    reach(a, c) :- reach(a, b), edge(b, c, _).

    // ---- shortest distances: recursion through the monotone min lattice; converges because w >= 0
    scratch dist(src: u32, dst: u32 -> d: LMin<u64>)
    monotone dist(s, s, 0) :- source(s).
    monotone dist(s, b, d + w) :- dist(s, a, d), edge(a, b, w).

    output shortest(src: u32, dst: u32 -> d: u64)
    shortest(s, n, reveal(d)) :- dist(s, n, d).

    // ---- relational min aggregation over the result (a negative edge, stratified above `dist`)
    output closest_distance(src: u32 -> d: u64)
    closest_distance(s, m) :- source(s), let m = min{ d | shortest(s, n, d) where n != s }.

    // one nearest node per source: ties broken by the smaller node id
    output nearest(src: u32 -> dst: u32, d: u64)
    nearest(s, n, d) :- shortest(s, n, d), choose s -> (n, d) least (d, n) where n != s.

    // the shortest-path tree: a predecessor that realizes the distance, smallest id first
    output parent(src: u32, dst: u32 -> via: u32)
    parent(s, b, a) :- dist(s, a, da), edge(a, b, w), dist(s, b, db), choose (s, b) -> a least a
                       where reveal(da) + w == reveal(db), b != s.

    // ---- stratified negation
    output unreachable(src: u32, n: u32)
    unreachable(s, n) :- source(s), node(n), not reach(s, n) where n != s.

    output on_cycle(n: u32)
    on_cycle(n) :- reach(n, n).
}
```

Strata: {`reach`}, {`dist`} (monotone lattice recursion), then `shortest` (`reveal`), then `closest_distance`,
`nearest`, `parent` (aggregate and choice), and `unreachable` (`not reach`). Expected output for the facts above:
`shortest(1, ·)` = {1:0, 2:7, 3:9, 4:20, 5:20, 6:11}; `closest_distance(1, 7)`; `nearest(1, 2, 7)`;
`unreachable(1, 7)`, `unreachable(1, 8)`; `on_cycle` = {3, 4, 5, 6}. (Paths: 1→3→6 = 11; 6→5 = 20; 3→4 = 20; 5→3
closes the cycle 3→6→5→3, and 3→4→5→3.) Lowering of the lattice recursion (one stratum, semi-naive over `LMin`):

```
dist(L, S, S; lmin(0)) :- source(L, S).
dist(L, S, B; X)       :- dist(L, S, A; D), edge(L, A, B, W), X = D + W.
shortest(L, S, N, R)   :- dist(L, S, N; D), R = reveal(D).
```

### E8. Soft-state heartbeat failure detector

```blossom
// file: lib/heartbeat.bls
module HeartbeatFD(
    peers: rel(n: Node),
    PERIOD: Duration = 1s,
    TTL: Duration = 3500ms,
    CAPACITY: u64 = 4096,
) {
    output suspect(peer: Node)
    output newly_suspected(peer: Node)
    output recovered(peer: Node)

    channel heartbeat(@to: Node, sent_at: Time)
    timer beat every PERIOD
    soft table last_heard(peer: Node -> at: Time) ttl TTL max CAPACITY

    bootstrap {
        last_heard(p, now()) :- peers(p) where p != self.        // grace period: everybody starts alive
    }

    async heartbeat(@n, now()) :- beat(..), peers(n) where n != self.
    refresh: upsert last_heard(p, now()) :- heartbeat(..) from p, peers(p).     // new birth time, TTL restarts

    suspect(p) :- peers(p), not last_heard(p, _) where p != self.
    newly_suspected(p) :- inserted suspect(p).
    recovered(p) :- removed suspect(p).
}
```

A peer is suspected at the first tick whose sampled `now` is 3.5 s past its last heartbeat's arrival tick (expiry
is decided at tick boundaries, CR-17), and recovers at the tick after a heartbeat arrives (the upsert lands at t+1).
The timer keeps the node ticking every second, so expiry is observed within one period. Lowering of `refresh` and the
view, following §3.1.8 and §3.2.5:

```
last_heard$ups(L, P, T)      :- heartbeat(L, _, P), peers(L, P), now(L, T).
last_heard$del(L, P, A0)     :- last_heard$ups(L, P, _), last_heard(L, P, A0).
last_heard$ins(L, P, A)      :- last_heard$ups(L, P, A).
last_heard$store(L, P, A, T)@next :- last_heard$ins(L, P, A), now(L, T).
last_heard$store(L, P, A, B)@next :- last_heard$store(L, P, A, B), now(L, T), T - B < 3500ms,
                                     notin last_heard$del(L, P, A), notin last_heard$ins(L, P, A).
last_heard$live(L, P, A, B)  :- last_heard$store(L, P, A, B), now(L, T), T - B < 3500ms.
last_heard$n(L, count<*>)    :- last_heard$live(L, _, _, _).
last_heard$rk(L, P, A, I)    :- last_heard$live(L, P, A, B), I = index() by (B, P, A).
last_heard(L, P, A)          :- last_heard$rk(L, P, A, I), last_heard$n(L, N), I >= N - 4096.
suspect(L, P)                :- peers(L, P), notin last_heard$p1(L, P), P != L.
last_heard$p1(L, P)          :- last_heard(L, P, _).
```

### E9. Two instances of E2 and an interposition

```blossom
// file: apps/dual_broadcast.bls
use lib::broadcast::{ReliableBroadcast}
use lib::delivery::{MsgId}
use lib::heartbeat::{HeartbeatFD}

module DualBroadcast(peers: rel(n: Node), MAX_DATA: u64 = 65_536) {
    input  send_control(seq: u64 -> payload: bytes)
    input  send_data(seq: u64 -> payload: bytes)
    output recv(kind: str, origin: Node, seq: u64 -> payload: bytes)
    output data_done(seq: u64)
    output suppressed_count(dst: Node -> n: LMax<u64>)

    // E2's module, twice: two independent instances with separate buffers, timers and channels
    import ReliableBroadcast(peers: peers) as control
    import ReliableBroadcast(peers: peers) as data
    import HeartbeatFD(peers: peers) as fd

    table suppressed(dst: Node -> ids: LSet<MsgId>)

    control.bcast(i, p) :- send_control(i, p).
    data.bcast(i, p)    :- send_data(i, p) where len(p) <= MAX_DATA.

    recv("control", o, i, p) :- control.deliver(o, i, p).
    recv("data", o, i, p)    :- data.deliver(o, i, p).
    data_done(i) :- data.bcast_done(i).

    // Interpose on the data instance's best-effort layer, *below* ReliableDelivery's retry buffer:
    // while a peer is suspected, skip (re)transmissions to it. The buffer keeps the message, so it is
    // retransmitted after the peer recovers, and delivery stays reliable. The control instance is untouched.
    interpose data.d.bed.pipe_in as orig {
        data.d.bed.pipe_in(dst, src, m, p) :- orig(dst, src, m, p), not fd.suspect(dst).
        suppressed(dst, {m}) :- orig(dst, _, m, _), fd.suspect(dst).
    }

    suppressed_count(d, s.size) :- suppressed(d, s).
}
```

Flattening gives two disjoint copies (`control$…`, `data$…`), each with its own `d$buf`, `d$retry` timer and
`d$bed$pipe_chan` channel. The interposition re-targets the one rule in `data$d` that derives `bed.pipe_in`
(ReliableDelivery's `remember` block has two), as in §3.7.5:

```
data$d$bed$pipe_in$orig(L, D, S, M, P) :- data$d$pipe_in(L, D, S, M, P).
data$d$bed$pipe_in$orig(L, D, S, M, P) :- data$d$buf(L, D, S, M, P), data$d$retry(L, _, _).
data$d$bed$pipe_in(L, D, S, M, P)      :- data$d$bed$pipe_in$orig(L, D, S, M, P), notin fd$suspect(L, D).
suppressed(L, D; {M})                  :- data$d$bed$pipe_in$orig(L, D, _, M, _), fd$suspect(L, D).
control$d$bed$pipe_in(L, D, S, M, P)   :- control$d$pipe_in(L, D, S, M, P).                 % unchanged
control$d$bed$pipe_in(L, D, S, M, P)   :- control$d$buf(L, D, S, M, P), control$d$retry(L, _, _).
```

The interposition adds one point of order (`not fd.suspect`); the report attributes it to this module, not to
`ReliableBroadcast`, and notes it depends on a timer-driven soft-state input (schedule-dependent).

### E10. Verification specs: LDFI for simple broadcast, and Raft election safety

```blossom
// file: specs/broadcast_specs.bls
use lib::broadcast::{ReliableBroadcast}

// Molly's simplog, in Blossom: one attempt, no retries
module SimpleBcast(peers: rel(n: Node)) {
    input  bcast(payload: str)
    channel log_msg(@to: Node, payload: str)
    table log(payload: str)

    log(p) :- bcast(p).
    async log_msg(@n, p) :- bcast(p), peers(n) where n != self.
    log(p) :- log_msg(_, p).
}

// LDFI: Molly's deliv_assert against SimpleBcast
spec SimpleDelivery for SimpleBcast(peers: node) {
    nodes A, B, C
    faults { eot: 4, eff: 2, crashes: 0, model: sync }

    node(@x, y) :- for x in [A, B, C], for y in [A, B, C].
    bcast(@A, "hello") at 1.

    // someone has the entry, but not me
    missing(x, pl) :- log(@y, pl), node(@y, x), not log(@x, pl).
    pre(x, pl)  :- log(@x, pl), not bcast(@x, pl) at 1, not crash(x, _).
    post(x, pl) :- log(@x, pl), not missing(_, pl).

    expect fails        // Molly: simplog + deliv_assert, EOT 4, EFF 2, 0 crashes: counterexample
}

// LDFI with a crash budget, against E2 (relay + retry): the analogue of Molly's ack_rb verdict
spec ReliableDeliverySpec for ReliableBroadcast(peers: node) {
    nodes A, B, C
    faults { eot: 12, eff: 6, crashes: 1, model: sync, round: 1s }

    node(@x, y) :- for x in [A, B, C], for y in [A, B, C].
    bcast(@A, 1, b"hello") at 1.

    got(x, o, i) :- once deliver(@x, o, i, _).
    missing(x, o, i) :- got(y, o, i), node(@y, x), not got(x, o, i), not crash(x, _).
    pre(x, o, i)  :- got(x, o, i), not crash(x, _).
    post(x, o, i) :- got(x, o, i), not missing(_, o, i).

    liveness everyone_delivers: eventually post(x, o, i) within 4 after eff.
    expect holds
}
```

```blossom
// file: specs/raft_election_specs.bls
use raft::election::{RaftElection, Role}

spec ElectionSafety for RaftElection(peers: server, log: empty_log) {
    nodes N1, N2, N3
    faults { eot: 30, eff: 20, crashes: 1, model: async, delay: 1..3, round: 10ms }

    static empty_log(idx: u64 -> term: u64)
    server(@x, y) :- for x in [N1, N2, N3], for y in [N1, N2, N3].

    // The property, over whole histories: checked on every run LDFI and bounded model checking explore.
    invariant one_leader_per_term:
        never once leader_of(@a, t, a), once leader_of(@b, t, b) where a != b.

    // Its state form, proved for all runs by induction (VER-006..010).
    invariant no_two_leaders:
        never role(@a, Leader), current_term(@a, t), role(@b, Leader), current_term(@b, t) where a != b.

    prove no_two_leaders by induction using {
        invariant vote_once:
            always forall v: Node, t: u64, a: Node, b: Node:
                (voted_for(@v, t, a) and voted_for(@v, t, b)) implies a == b.
        invariant replies_follow_votes:
            always forall c: Node, t: u64, v: Node:
                (sent vote_resp(@c, t, true) from v) implies voted_for(@v, t, c).
        invariant counted_votes_are_cast:
            always forall c: Node, t: u64, v: Node:
                (exists w: LSet<Node>: votes(@c, t, w) and w.contains(v)) implies voted_for(@v, t, c).
        invariant leader_has_quorum:
            always forall a: Node, t: u64:
                (role(@a, Leader) and current_term(@a, t))
                implies count{ v | voted_for(@v, t, a) } >= majority{ v | server(@a, v) }.
    }

    expect holds
}
```

How the pieces map to the tools: `pre`/`post` and `faults` are LDFI's outcome oracle and failure spec (TEST-020,
TEST-022, CR-21, CR-30); `crash(x, _)` is the spec-only oracle (CR-20); `once` and `at 1` read the trace relations
(TEST-080); `liveness` is VER-001's bounded liveness; `never` invariants are checked on every explored run; `always`
formulas, `sent` (the network as a grow-only set) and `prove … by induction` go to the first-order transition system
and Z3 (VER-006, VER-010), where `majority{…}` becomes the quorum sort with its intersection axiom (VER-008). The
proof sketch: `leader_has_quorum` and `vote_once` for two leaders of term t give two majorities of `voted_for(t, ·)`
that intersect in some v who voted for both, contradicting `vote_once`. `replies_follow_votes` holds because the vote
reply is released only after `voted_for` is durable (SEM-072), and `counted_votes_are_cast` because `votes` is fed
only by granted replies and by the candidate's own `voted_for(t, self)`.

---

## 5. Self-critique

### 5.1 Deviations from FEATURES.md §2

| # | Item | What this proposal does | Why |
|---|---|---|---|
| D1 | LANG-208 (`#` comments, P0) | `#` is not a comment in `.bls`; it introduces field numbers `#n`. `#` comments stay in the `.ded` and Overlog frontends, where the corpora use them. | One sigil, one meaning. The P0 need behind LANG-208 is Dedalus compatibility, which the frontend covers. |
| D2 | LANG-082 (`!p(…)`) | only `not p(…)` | `!` is boolean not on values; a single, wordy negation is easier to spot in a CALM review. |
| D3 | LANG-085 (`X := e`) | `let x = e` | A keyword makes binding sites greppable and frees `=` for defaults and aggregate bindings. |
| D4 | LANG-100 (`count<*>`, `count<X>`) | `count()`, `count(distinct x)`, `count{ x \| … }` | Aggregates are expressions; the angle brackets are reserved for types. |
| D5 | LANG-190 ("imported modules bootstrap first") | all bootstrap rules run in the tick-0 fixpoint, ordered by stratification | Under set semantics the textual/import order has no observable meaning; stratification gives the only order that matters. |
| D6 | LANG-072, LANG-099, LANG-107 (all P2) | not provided | Out of scope for P0/P1; entanglement would need a new time-binding form (`@N` on body atoms) that this syntax deliberately does not reserve. |

### 5.2 Weaknesses

1. **Two aggregate syntaxes.** `|> group by g { … }` and `agg{ e | … }` compute the same things. The pipeline reads well
   for GROUP BY and the braces read well for correlated subqueries and thresholds, but a style guide has to say
   which to use, and the lowering has two entry points to keep equivalent.
2. **Set semantics leaks through aggregates.** Aggregates fold over distinct valuations of the *named* variables, so
   a wildcard in the wrong place silently collapses rows (§3.4.2's `occurrence(_, pos, w)`). The lint catches the
   common case only when FD inference can prove the dropped column is not determined; the rest relies on the
   programmer.
3. **Positional vs named atoms.** `vote(t, v, true)` binds by position and `vote(term, voter, ..)` by name. In named
   mode every bare identifier must be a field (so typos fail), but a positional atom whose variables happen to be
   named after *other* fields (`vote(voter, term, true)`) is legal and misleading. A lint for "variable named after a
   different column" is needed.
4. **Case carries meaning.** Variables, relations and roles are `lower`; types, modules and node constants `Upper`;
   constants `CONST`. That removes the Rust "is this a binding or a constant pattern?" hazard, but acronyms suffer
   (`IO`, `ID` lex as `CONST`, so the type must be spelled `Io`), and module parameter kinds (`peers: rel`,
   `D: Delivery`, `RETRY: Duration`) are distinguished by case alone.
5. **The Prolog end-dot.** `p(x) :- q(x).` versus `q(x).r` is decided by the character after the dot. It is robust in
   practice, but `x.` at the end of a line inside an expression that the programmer meant to continue produces a
   confusing "rule ends here" error, and `1.e5` is not a float. The error-recovery heuristics need care.
6. **Lattice comparisons are type-directed.** `x >= 5` is a monotone threshold on `LMax` and an antitone read on
   `LMin`; `a <= b` is ⊑ on lattices and ≤ on scalars. The polarity is correct and checked, but it is invisible in the
   text. Editors must color monotone and non-monotone comparisons differently (TEST-092). The alternative — distinct
   operators `⊒`/`⊑` for lattices — was rejected as too alien, and is the first thing to revisit.
7. **`delete` and `upsert` do not say `next`.** Both take effect at t+1, like `next`, but their keywords do not show
   it. `next delete` was considered and rejected as noise; newcomers will expect `delete` to be immediate.
8. **`inserted`/`removed` depend on tick boundaries.** They are deterministic given a node's inputs, but whether two
   changes land in one tick or two depends on delivery timing, so over async inputs they are schedule-dependent.
   The analysis must classify them that way (SEM-087), and the syntax gives no hint.
9. **Two spellings of one relation.** Protocol rules read `log(pl)`; spec rules read `log(@x, pl)`. Specs need
   explicit locations and protocols must not have them, but it means code cannot be pasted between the two.
10. **Location columns in bodies are noise.** Received channel atoms must still mention the location column
    (`put_req(_, id, key, val)`), which is always `self`. Named mode avoids it (`put_req(id, key, val, ..)`); positional
    mode does not. Making the location column implicit in body atoms was considered and rejected because heads and
    spec atoms need it, and one rule for all positions is simpler.
11. **Hidden state behind `sealed` and `seq()`.** A `seal on` channel makes the compiler keep a receive log and
    per-producer counts; `seq()` keeps a high-water mark. Both are in the Dedalus expansion, so nothing is hidden
    semantically, but memory cost is invisible in the source and depends on Edelweiss reclamation (ANA-063).
12. **Interposition breaks encapsulation.** `interpose data.d.bed.pipe_in` reaches two levels into an instance's
    internals. That is exactly what BOOM needed (LANG-008), but it means an internal refactor of `ReliableDelivery`
    can break an importer. Only interface relations can be interposed, and the error names the path, but the
    coupling is real.
13. **`localize` is only exact for insert-only bodies.** The chain rewrite mirrors remote relations and does not
    propagate deletions; the compiler rejects `localize` when a mirrored relation has a `delete` rule, which makes
    the sugar less useful than NDlog's.
14. **Special forms the verifier depends on.** `majority{…}` is recognized syntactically to become the quorum sort;
    `stable fn … after t` is a new method class whose soundness rests on `Point` conflicts being fatal errors. Both
    are small, but both are places where the surface and the verifier are coupled.
15. **Choreography roles are static.** Role membership relations are `static`; dynamic membership by epochs
    (DIST-042, ODD-21) works through a library whose membership relation replaces `r(n)`, but `r.route(k)` and role
    typing of channels then need an epoch argument that this syntax does not yet spell.
16. **Literal Dedalus resend.** An `async` rule whose body reads persistent state re-sends every tick (ODD-05). The
    corpus gates such rules with timers or `inserted`; a newcomer who writes the obvious rule floods the network until
    DIST-007 proves suppression safe. A lint ("async from persistent state without a timer or delta") is essential.
17. **Commas inside parentheses build tuples.** Inside `( … )` conjunction must be written `and`. Users will write
    commas; the parser must recognize "a tuple of atoms" and suggest `and`.

### 5.3 Ambiguities, and how this proposal resolved them

- **Unannotated facts in mutable tables.** CR-16 says they hold at every tick, which makes them undeletable; initial
  values therefore go in `bootstrap`, and the compiler warns when a `delete` targets a re-asserted fact.
- **`fold` on an empty input.** In expression form it returns `init` (LANG-110's carried form); inside `group` an empty
  group has no row (the aggregate form). Other expression aggregates fail on empty input unless given `default`.
- **Disjunction plus pipeline.** Disjuncts are unioned into the pipeline's input relation, so a `group` sees all
  branches together (E3's `eff_term`).
- **`choose … least c`.** The FD is X̄ → Ȳ ∪ c̄, so the chosen tuple is one exemplar, unlike `argmin`, which returns all
  ties (LANG-103 vs LANG-114).
- **`implements P`** includes `P`'s interface declarations; redeclaring them is allowed only verbatim.
- **Soft-table refresh.** A deductive re-derivation of an identical tuple refreshes it; a new value for the same key
  must be an `upsert` (E8), because two values for one key in one tick are a SEM-050 error even in a soft table.
- **Spec access to internals.** A spec may read every relation of the module under test, including private ones and
  nested instances, because it observes a trace, not an interface.
- **Durability and same-tick replies.** Writes staged with `next` into a `durable` relation are committed before the
  tick's outbox is released (SEM-072), so "log, then reply" can be written in one tick (E3's vote, E4's outcome), and
  "log, then act next tick" (LIB-041) is only needed when the *action* must observe the logged state.
