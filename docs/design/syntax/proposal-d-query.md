# Blossom surface syntax, proposal D: "query-oriented / comprehension"

Status: design proposal (one of several competing angles). Nothing here overrides `docs/DECISIONS.md` or the
normative semantics in `docs/research/FEATURES.md` §1 (CR-xx) and §3 (SEM-xxx). Where this proposal fixes a
spelling, it is the spelling of an already-decided semantics. Where it adds a semantic rule (for example what
`count()` counts), the rule is marked **[new]** and justified.

Contents

1. Design philosophy and name-level overview
2. Lexical structure and full EBNF grammar
3. Every construct: example and exact Dedalus lowering
4. Required example corpus (E1–E10)
5. Self-critique

---

## 1. Design philosophy and name-level overview

### 1.1 Philosophy

A Blossom program is a set of **queries whose results land somewhere at some time**. Every rule reads as a
LINQ/PRQL-style comprehension, top to bottom in data-flow order, and every rule *starts* with a **sink** that
states, in its first two tokens, where the result goes and when:

```
into       store  from p in put select {key: p.key, val: p.val};   // same tick      (Dedalus deductive)
into next  store  from p in put select {key: p.key, val: p.val};   // next tick     (Dedalus @next)
send       ack    from p in put select {to: p.$sender, id: p.id};  // some later tick elsewhere (@async)
delete! next buf  from b in buf join a in ack on a.id == b.id select b;       // deferred delete
upsert! next kv   from p in put select {key: p.key, val: p.val};              // deferred upsert
```

Five principles drive the design.

1. **The sink is the rule kind.** The analyzer, the reader and `grep` all learn the Dedalus rule kind from the
   first token of the statement. `into` is deductive, `into next` is inductive, `send` is async, and `delete!` and
   `upsert!` are the deferred mutations. There is no separate arrow vocabulary to learn.
2. **A `!` marks every point of non-monotonicity.** A bang appears on exactly the constructs that SEM-021 counts as
   negative edges: negation (`not!`), non-lattice aggregation (`group!`, `count!(…)`), deletion (`delete!`,
   `upsert!`), outer join (`left join!`), order-sensitive operators (`enumerate!`, `seq!`, `top!`, `fold!`), choice
   (`choose!`, `resolve!`), antitone or non-monotone lattice reads (`reveal!`, `x.method!()`) and delta
   pseudo-relations (`r.inserted!`, `r.deleted!`). A rule with no `!` is monotone, so a `grep '!'` over a module
   lists its CALM points of order. Omitting a required bang is a compile error with a fix-it, and writing a
   superfluous one is a warning. This is the syntactic face of ANA-022.
3. **Comprehension clauses are keywords, never punctuation.** `from`, `join`, `where`, `let`, `unnest`, `group`,
   `choose!`, `select` all begin with a reserved word, so the grammar is LL(1) at the clause level, an expression
   parser (Pratt) handles everything between keywords, and a missing clause keyword gives an exact diagnostic.
   Statements end in `;` and blocks in `}`, which are the error-recovery synchronization points.
4. **Records, not positions.** Rows are named records. Binders (`from p in put`) give each row a name, fields are
   read as `p.key`, and heads are record literals (`{key: p.key}`, with punning `{p.key}`). Positional tuples are
   still accepted for short relations. This is what data engineers expect from SQL/LINQ/PRQL, and it fixes the
   "long positional join" problem that BOOM called out (LANG-080).
5. **Lattices are just column types, and folds are just `select`.** A lattice-typed column merges under the key
   functional dependency, so `select {term: v.term, voters: lset(v.voter)}` *is* the Bloom^L fold. Monotone
   lattice reads (`>=` on `lmax`, `.contains`, `.size`, `.at`) are plain expressions; only non-monotone reads need
   `reveal!`. Monotone lattice grouping uses `group by` (no bang), and non-monotone SQL grouping uses `group!`.

Everything above is syntax. The meaning is always the Dedalus^L program shown in §3, and there is exactly one
lowering per construct.

### 1.2 Name-level overview

| Concept | Spelling | Dedalus meaning (short) |
|---|---|---|
| Program header | `program kv version 3;` | LANG-260 |
| Module / protocol / choreography | `module M(p: T) implements P { … }`, `protocol P { … }`, `choreography C { role r: cluster; at r { … } }` | namespaced rule sets; interface-only contract; per-role projection |
| Import as instance | `import ReliableDelivery(retry: 2s) as rd: Delivery;` | renamed copy of the module's rules and relations |
| Flat include / textual include | `mixin M;` / `include "util.bls";` | LANG-005 |
| Interposition | `interpose rd.pipe_out as raw { … }` | rename the instance's writes, splice rules in between |
| Named rule / block / override | `rule remember: …;`, `block done { … }`, `override block done { … }` | LANG-068, LANG-007 |
| Collections | `table`, `durable table`, `scratch`, `channel`, `input`, `output`, `static`, `loopback`, `soft table … ttl … max …`, `sealed table`, `range table`, `zset table`, `bag table` | §3.2 |
| Lattice identifier | `lattice votes: lset<Node>;`, `scratch lattice seen: lmax<u64>;` | 0-ary lattice relation |
| Timers | `periodic hb every 1s;`, `periodic lt every 3 ticks;`, `once start;` | runtime EDB events |
| Schema | `table kv(key: string => val: bytes, ver: u64 = 0);` | keys left of `=>` |
| Rule sinks | `into`, `into next`, `send … [to e]`, `delete! next`, `upsert! next [resolve! p]`, `seal … on (…)`, `fail "msg"` | deductive, inductive, async, `del_r`, delete-by-key + insert, punctuation, violation |
| Views | `let reach = from … select …;` | scratch (temp) with inferred schema |
| Clauses | `from p in r`, `join q in s on e`, `left join! q in s on e`, `where e`, `let x = e`, `unnest x in e`, `group by`, `group! by`, `choose! per (…)`, `enumerate! as i`, `seq! as i`, `top! k by …`, `select e`, `union` | body atoms, conditions, aggregates, choice sites |
| Relational predicates | `exists (q in s where e)`, `s(f: e)`, `e in s`, `all x in s: e`, `not! …` | semi-join, anti-join, ∀ |
| Correlated aggregate | `count!(v in vote where v.x == t.x) default 0` | aggregate with default, driven by the outer row (LANG-106) |
| Lattice reads | `l.size`, `l >= 3`, `l.contains(x)`, `m.at(k)`, `m[k]`, `merge(l, c)`, `reveal!(l)` | morphisms, monotone functions, thresholds, exact read |
| Implicit message columns | `m.$sender`, `m.$principal`, `m.$session` | SEM-091, LANG-241 |
| Time and randomness | `now()`, `tick()`, `random()`, `rand(k)`, `rand_range(lo, hi, k)` | sampled once per tick (CR-18) |
| Deltas | `r.inserted!`, `r.deleted!` | LANG-071 |
| Seals and finality | `seal r on (k) counted …;`, `is_sealed(r, k: v)`, `output final r(…)`, `is_final(…)` | LANG-207, LANG-212 |
| Spec and verification | `spec S for M { failures {…} pre …; post …; invariant n: never …; eventually post within k after eff; check …; }` | TEST-020..029, VER-001 |
| Versions | `migrate from 2 { … }`, `emit c to 2 { … }`, `accept c from 2 { … }`, `since 3`, `cluster_version.at_least(3)` | LANG-260..265 |
| Annotations | `nondet "why"`, `unsafe_ungated "why"`, `trusted module`, `atomic { … }`, `localize` | LANG-204, 205, 206, 095 |

### 1.3 The bang table (normative for this proposal)

| Bang construct | Negative edge per SEM-021 | Note |
|---|---|---|
| `not! atom`, `not! exists (…)`, `not! (e in r)`, `not! lbool_expr` | `notin` / antitone | scalar boolean `not` has no bang |
| `group! by …`, `count!(…)`, `sum!(…)`, … | non-lattice aggregate | `group by` with only lattice folds is monotone and has no bang |
| `delete! next r`, `upsert! next r` | deletion | |
| `left join!` | outer join | |
| `enumerate!`, `seq!`, `top!`, `fold!(…)`, `sort!(…)`, `percentile!(…)` | order-sensitive operator | |
| `choose!`, `resolve!` | choice site | seed- or schedule-dependent (SEM-087) |
| `reveal!(e)`, `l.m!(…)` for a method declared `antitone` or plain | antitone / NM lattice operation, `reveal` | |
| `r.inserted!`, `r.deleted!` | expansion contains `notin r_prev` | |
| `all! x in r: e` over an open relation | antitone in `r` | `all x in r: e` without a bang is legal only when `r` is `static`, `sealed` or sealed on the quantified key, because ∀ over a closed domain of a monotone predicate is monotone |

The compiler checks the table in both directions: it rejects a negative edge written without `!` and warns on a `!`
that lowers to no negative edge.

---

## 2. Lexical structure and grammar

### 2.1 Lexical structure

Source files are UTF-8 with extension `.bls`. Whitespace (space, tab, CR, LF) separates tokens and is otherwise
insignificant; there is no layout rule.

**Comments (LANG-208).**
- `// …` to end of line.
- `/* … */`, nestable.
- `# …` to end of line, **only** when `#` is not immediately followed by an ASCII digit. `#12` is a field-number
  token (LANG-261). A `#` comment therefore needs a space or a letter after the `#`.

**Identifiers.** `IDENT = [A-Za-z_][A-Za-z0-9_]*`, excluding reserved words. `_` alone is the wildcard. Relation,
field, variable and function names share one lexical class; name resolution, not the lexer, decides which namespace
a name lives in.

**Bang identifiers.** `BANG_IDENT = IDENT "!"` when the `!` is immediately adjacent to the identifier and is **not**
followed by `=`. So `group!` is one token, `x!=y` is `x`, `!=`, `y`, and `x ! y` is a lexical error. The bang keywords
below are `BANG_IDENT` tokens whose stem is reserved; any other `BANG_IDENT` is a call of a user method or function
declared non-monotone (`summary!(…)`).

**Implicit-column identifiers.** `DOLLAR_IDENT = "$" IDENT`. The legal ones are `$sender`, `$principal`,
`$session`, `$birth` (soft tables), and, in specs only, `$node` and `$tick`.

**Reserved words.**

```
accept all antitone as associative at atomic bag block bootstrap bot by check choreography commutative const
counted default delivery deprecated durable else emit enum eventually every exists extends extern fail
false final fn for from group idempotent if implements import in include injective input interpose into
invariant join lattice left let like localize loopback match materialized migrate mixin module monotone
morphism never next nondet not old on once or output over override param partition per periodic plain post
pre program protocol pure recomputed record release ring role rule scratch seal sealed select self
semantics_changed send service since snapshot soft spec static table then ticks times to true trusted ttl
type union unnest unsafe unsafe_ungated values via where within zset
```

**Contextual keywords** are reserved only where the grammar expects them and are ordinary identifiers elsewhere:
`acl asc best_effort carries choose choose_least choose_most choose_rand client cluster desc down estimate eff
exactly_once expect external failures forever include_tentative least lossy max merge mode most nodes ordered
principal process progress rand range reliable sticky tick trace unknown upto version committed_only`.

**Keywords as field names.** Wherever the grammar expects a *field name* (after `.`, `?.` or `$`; before `:` in a
schema field, a record literal, a field pattern or a named argument) every reserved word is accepted as an
identifier. So `{to: p.n}`, `d.at`, `r.role` and `v.version()` are legal. Punning (`{to}`) is not allowed for
reserved words.

**Reserved bang words.** `not! group! join! delete! upsert! resolve! choose! enumerate! seq! top! reveal! all!
inserted! deleted! fn! count! sum! min! max! avg! fold! sort! percentile! argmin! argmax! set! list!
bool_and! bool_or!` (the aggregate
names ending in `!` are the correlated-subquery forms).

**Literals.**
- Integers: `42`, `0x2A`, `0b1010`, `1_000_000`, with optional type suffix `42u64`, `-3i32`. Wide modular ids:
  `0x…I` (LANG-026).
- Floats: `3.5`, `1e-9` (`f64`).
- Strings: `"…"` with Rust escapes; byte strings `b"…"`.
- Durations: `INT` immediately followed by a unit, `ns us ms s m h` (`150ms`, `2s`). Type `duration`.
- Booleans: `true`, `false`. Unit: `()`. Lattice bottom: `bot` (typed by context).
- Field numbers: `#` DIGITS, directly adjacent (`#3`).

**Operators and punctuation.**
`( ) [ ] { } , ; : . .. => -> @ ? ?. == != < <= > >= + - * / % ** ++ & | ^ ~ << >> =`.

### 2.2 Operator precedence (Pratt table, loosest first)

| Level | Operators | Assoc. | Notes |
|---|---|---|---|
| 1 | `if e then e else e` | prefix | ternary (LANG-084, LANG-089) |
| 2 | `or` | left | |
| 3 | `and` | left | |
| 4 | `not`, `not!` | prefix | `not` on `bool`; `not!` on relational predicates and `lbool` |
| 5 | `== != < <= > >= in` | none | comparisons do not chain |
| 6 | `\|` | left | bit or |
| 7 | `^` | left | bit xor |
| 8 | `&` | left | bit and |
| 9 | `<< >>` | left | |
| 10 | `+ - ++` | left | `++` is string/list concat |
| 11 | `* / %` | left | |
| 12 | `**` | right | |
| 13 | unary `-`, `~` | prefix | |
| 14 | `.f`, `.m(…)`, `.m!(…)`, `[e]`, `[e..e]`, `?.f`, `(…)` | postfix | |

Molly's right-nested, precedence-free expressions are not reproduced (LANG-084).

### 2.3 Full grammar (EBNF)

Notation: `{ x }` is zero or more, `[ x ]` is optional, `|` separates alternatives, terminals are quoted, and
`UPPER` names are tokens from §2.1. The grammar is LL(1) except in the four places listed after it ("LL(2) spots"),
each of which needs one extra token of lookahead.

```ebnf
(* ===================== files ===================== *)
file            = [ program_hdr ] { top_item } EOF ;
program_hdr     = "program" qname "version" INT ";" ;
top_item        = module_def | protocol_def | choreo_def | spec_def | migrate_def | translate_def
                | include_item | module_item ;               (* a file is an implicit anonymous module *)
include_item    = "include" STRING ";" ;
qname           = IDENT { "." IDENT } ;

(* ===================== modules ===================== *)
module_def      = [ "trusted" ] "module" IDENT [ params ] [ "implements" qname { "," qname } ]
                  [ "extends" qname [ args ] ] "{" { module_item } "}" ;
protocol_def    = "protocol" IDENT [ params ] [ "extends" qname { "," qname } ] "{" { protocol_item } "}" ;
protocol_item   = iface_decl | type_decl | enum_decl | const_decl ;
choreo_def      = [ "trusted" ] "choreography" IDENT [ params ] [ "implements" qname { "," qname } ]
                  "{" { choreo_item } "}" ;
choreo_item     = role_decl | at_block | module_item ;
role_decl       = "role" IDENT ":" ( "process" | "cluster" | "external" ) ";" ;
at_block        = "at" IDENT "{" { module_item } "}" ;

params          = "(" [ param { "," param } ] ")" ;
param           = IDENT ":" ( type | "static" schema ) [ "=" expr ] ;    (* relation parameter *)
args            = "(" [ arg { "," arg } ] ")" ;
arg             = [ IDENT ":" ] expr [ "by" order_list ] ;    (* LL(2): IDENT ":" vs expr; "by" only in aggregates *)

module_item     = decl | stmt | import_item | mixin_item | interpose_item | block_def | override_def
                | bootstrap_block | atomic_block | include_item ;
import_item     = "import" qname [ args ] "as" IDENT [ ":" qname ] [ acl_block ] ";" ;
acl_block       = "acl" "{" { qname ":" "accept" "from" acl_clause ";" } "}" ;
mixin_item      = "mixin" qname [ args ] ";" ;
interpose_item  = "interpose" qname "as" IDENT "{" { module_item } "}" ;
block_def       = "block" IDENT "{" { stmt } "}" ;
override_def    = "override" "block" IDENT "{" { stmt } "}" ;
bootstrap_block = "bootstrap" "{" { stmt } "}" ;
atomic_block    = "atomic" "{" { stmt } "}" ;

(* ===================== declarations ===================== *)
decl            = rel_decl | lattice_ident | timer_decl | type_decl | enum_decl | lattice_type
                | const_decl | param_decl | fn_decl | extern_decl | service_decl | snapshot_decl ;

rel_decl        = { rel_mod } rel_kind IDENT [ "@" IDENT ] schema { rel_opt } [ "=" values_expr ] ";" ;
iface_decl      = ( "input" | "output" [ "final" ] ) IDENT [ "@" IDENT ] schema { rel_opt } ";" ;
rel_mod         = "durable" | "soft" | "sealed" | "range" | "zset" | "bag" | "materialized" | "recomputed" ;
rel_kind        = "table" | "scratch" | "channel" | "input" | "output" [ "final" ] | "static" | "loopback" ;
schema          = "(" "like" qname ")"
                | "(" [ field { "," field } ] [ "=>" [ field { "," field } ] ] ")" ;
field           = [ "@" ] IDENT ":" type [ FIELDNUM ] [ "=" expr ] { field_ann } ;
field_ann       = "since" INT | "deprecated" "since" INT | "semantics_changed" "since" INT ;
rel_opt         = "ttl" expr | "max" expr
                | "resolve!" policy
                | "partition" "by" expr [ "over" source ]
                | "delivery" delivery_mode
                | "accept" "from" acl_clause
                | "carries" type "via" "exactly_once" "(" IDENT ")"
                | "since" INT | "deprecated" "since" INT ;
delivery_mode   = "reliable" [ "ordered" ] | "lossy" [ "forever" ] | "best_effort" ;
acl_clause      = acl_src { "," acl_src } ;
acl_src         = IDENT                                        (* a role *)
                | "external" "client" [ "where" expr ]
                | "principal" "in" qname ;
policy          = "choose" [ "sticky" ] | "choose_rand" [ "sticky" ]
                | ( "choose_least" | "choose_most" ) "(" expr ")" | "merge" ;

lattice_ident   = { "durable" | "scratch" } "lattice" IDENT ":" type [ "=" expr ] ";" ;   (* LL(2) with lattice_type *)
lattice_type    = "lattice" "type" IDENT [ tparams ] "=" lat_ctor [ "{" { lat_method } "}" ] ";"
                | "extern" "lattice" IDENT [ tparams ] STRING "{" { lat_sig } "}" ;
lat_ctor        = type                                         (* composition of built-in constructors *)
                | "record" "{" IDENT ":" type { "," IDENT ":" type } "}" ;
lat_method      = fn_class ( "fn" | "fn!" ) IDENT "(" "self" { "," param } ")" "->" type "=" expr ";" ;
lat_sig         = fn_class "fn" IDENT "(" "self" { "," param } ")" "->" type ";" ;
fn_class        = "morphism" | "monotone" | "antitone" | "plain" ;

timer_decl      = "periodic" IDENT "every" expr [ "ticks" ] [ "times" expr ] ";"
                | "once" IDENT ";" ;
type_decl       = [ "group" | "ring" ] "type" IDENT [ tparams ] "=" type ";"
                | "extern" "type" IDENT [ tparams ] STRING ";" ;          (* LANG-027, LANG-142 *)
enum_decl       = "enum" IDENT "{" variant { "," variant } [ "," ] "}" ;
variant         = IDENT [ "(" type { "," type } ")" ] [ FIELDNUM ] | "unknown" ;
const_decl      = "const" IDENT ":" type "=" expr ";" ;
param_decl      = "param" IDENT ":" type [ "=" expr ] ";" ;
fn_decl         = { fn_prop } ( "fn" | "fn!" ) IDENT [ tparams ] "(" [ param { "," param } ] ")" "->" type "=" expr ";" ;
fn_prop         = "pure" | "monotone" | "morphism" | "antitone" | "injective" | "commutative"
                | "associative" | "idempotent" ;
extern_decl     = "extern" { fn_prop } "fn" IDENT "(" [ param { "," param } ] ")" "->" type STRING ";"
                | "extern" "table" "fn" IDENT "(" [ param { "," param } ] ")" "->" schema STRING ";"
                | "extern" { fn_prop } "aggregate" IDENT "(" type ")" "->" type STRING ";" ;
service_decl    = "service" IDENT "(" [ param { "," param } ] ")" "->" schema STRING ";" ;
snapshot_decl   = "snapshot" IDENT "of" qname "at" "progress"
                  ( "every" expr "upto" expr | "(" expr { "," expr } ")" )
                  [ "mode" ( "committed_only" | "include_tentative" ) ] [ "estimate" IDENT ] ";" ;

tparams         = "<" IDENT { "," IDENT } ">" ;
type            = [ "unsafe" ] IDENT [ "<" type { "," type } ">" ] [ "@" IDENT ]   (* Node@role; unsafe dompair *)
                | "(" [ type { "," type } ] ")"                          (* tuple / unit *)
                | "{" IDENT ":" type { "," IDENT ":" type } "}" ;         (* record *)

(* ===================== statements (rules) ===================== *)
stmt            = [ "rule" IDENT ":" ] { stmt_mod } rule_core ";"
                | view_stmt | seal_stmt | invariant_stmt ;
stmt_mod        = "nondet" STRING | "unsafe_ungated" STRING | "localize" ;
rule_core       = sink query ;
sink            = "into" [ "next" ] qname
                | "send" qname [ "to" expr ]
                | "delete!" "next" qname
                | "upsert!" "next" qname [ "resolve!" policy ]
                | "fail" STRING ;
seal_stmt       = [ "rule" IDENT ":" ] "seal" qname "on" "(" [ IDENT { "," IDENT } ] ")" [ "counted" ]
                  query ";" ;
view_stmt       = [ "materialized" | "recomputed" ] "let" IDENT [ schema ] "=" query ";" ;
invariant_stmt  = "invariant" IDENT ":" "never" query ";" ;

(* ===================== queries ===================== *)
query           = core_query { "union" core_query } ;
core_query      = values_expr | { clause } select_clause ;
values_expr     = "values" value_row { "," value_row } ;
value_row       = "(" [ expr_list ] ")" | "{" [ rec_field { "," rec_field } ] "}" ;   (* ("a") is a 1-row *)
clause          = "from" binding { "," binding }
                | "join" pattern "in" source "on" expr
                | "left" "join!" pattern "in" source "on" expr
                | "where" expr
                | "let" pattern "=" expr
                | "unnest" pattern "in" expr
                | "group" "by" group_keys
                | "group!" "by" group_keys
                | "choose!" "per" "(" [ expr_list ] ")" [ choose_pol ]
                | "enumerate!" "as" IDENT [ "per" "(" expr_list ")" ] [ "by" order_list ]
                | "seq!" "as" IDENT [ "per" "(" expr_list ")" ] [ "by" order_list ] [ "release" ]
                | "top!" expr [ "per" "(" expr_list ")" ] "by" order_list ;
binding         = pattern "in" source ;
group_keys      = "(" [ expr_list ] ")" | expr ;
choose_pol      = "least" expr | "most" expr | "sticky" | "rand" [ "sticky" ] ;
order_list      = "(" order_key { "," order_key } ")" | order_key ;
order_key       = expr [ "asc" | "desc" ] ;
select_clause   = "select" ( expr | "*" ) ;
source          = rel_ref [ "@" postfix_expr ] [ "at" "tick" expr ] ;
rel_ref         = IDENT { "." ( IDENT | "inserted!" | "deleted!" ) } [ args ] [ "[" expr [ ".." expr ] "]" ]
                                                                (* a delta suffix must come last *)
                | "(" query ")"
                | "old" "." qname                               (* migrations and translations *)
                | "trace" "(" qname ")" ;                        (* specs *)

pattern         = IDENT | "_" | literal
                | "(" pattern { "," pattern } ")"
                | "{" field_pat { "," field_pat } [ "," ".." ] "}"
                | IDENT "(" [ pattern { "," pattern } ] ")" ;   (* enum variant *)
field_pat       = IDENT [ ":" pattern ] ;

(* ===================== expressions (Pratt; see §2.2) ===================== *)
expr            = "if" expr "then" expr "else" expr
                | "match" expr "{" match_arm { "," match_arm } [ "," ] "}"
                | or_expr ;
match_arm       = pattern [ "if" expr ] "=>" expr ;
(* or_expr … postfix_expr follow the precedence table in §2.2 *)
unary_expr      = ( "not" | "not!" | "-" | "~" ) unary_expr | postfix_expr ;
postfix_expr    = primary { "." IDENT [ args ] | "." BANG_IDENT args | "?." IDENT
                          | "[" expr [ ".." expr ] "]" | args } ;
primary         = literal | IDENT | BANG_IDENT args | DOLLAR_IDENT | "self" | "bot"
                | "(" expr ")" | tuple_or_rec | list_lit | map_lit
                | "exists" "(" pattern "in" source [ "where" expr ] ")"
                | ( "all" | "all!" ) pattern "in" source ":" expr
                | agg_sub
                | "[" expr "for" pattern "in" expr [ "if" expr ] "]" ;   (* list comprehension, fn bodies *)
agg_sub         = BANG_IDENT "(" [ expr "," expr "," ] pattern "in" source [ "where" expr ]
                  [ ":" expr ] [ "by" order_list ] ")" [ "default" expr ] ;   (* init, step: fold! only *)
tuple_or_rec    = "(" expr "," [ expr_list ] ")"
                | "{" [ rec_field { "," rec_field } ] "}"
                | qname "{" [ rec_field { "," rec_field } ] "}" ;          (* typed record / lattice ctor *)
rec_field       = IDENT ":" expr | postfix_expr | ".." expr ;               (* punning; spread *)
list_lit        = "[" [ expr_list ] "]" ;
map_lit         = "map" "[" [ expr "=>" expr { "," expr "=>" expr } ] "]" ;
expr_list       = expr { "," expr } ;
literal         = INT | FLOAT | STRING | BYTES | DURATION | "true" | "false" | "(" ")" ;

(* ===================== specs, migrations, translations ===================== *)
spec_def        = "spec" IDENT "for" qname [ args ] "{" { spec_item } "}" ;
spec_item       = "nodes" IDENT { "," IDENT } ";"
                | "failures" "{" { IDENT ":" expr [ "," ] } "}"
                | "at" "tick" expr "on" IDENT "{" { stmt } "}"          (* timestamped input facts *)
                | "on" IDENT "{" { stmt } "}"                           (* per-node facts at every tick *)
                | ( "pre" | "post" ) query ";"
                | view_stmt | invariant_stmt
                | "eventually" ( "post" | IDENT ) "within" expr "after" ( "eff" | expr ) ";"
                | "check" IDENT [ "{" { IDENT ":" expr [ "," ] } "}" ] ";"
                | "expect" IDENT ":" IDENT ";" ;                        (* expected certificate *)
migrate_def     = "migrate" "from" INT [ "down" ] "{" { stmt } "}" ;
translate_def   = "emit" qname "to" INT "{" { stmt } "}"
                | "accept" qname "from" INT "{" { stmt } "}" ;
```

**LL(2) spots.** (1) `arg`: `IDENT ":"` starts a named argument. (2) `lattice IDENT ":"` versus
`lattice type`. (3) inside `rec_field`, `IDENT ":"` versus a punned `postfix_expr`. (4) the contextual modifier
`range` is recognized only when the next token is `table`. Everything else is decided by the first token. `rule_core` in particular is decided by the sink keyword, and each `clause` by its keyword.

**Error recovery.** The parser synchronizes on `;`, on `}`, and on any statement-initial keyword (`into send
delete! upsert! seal let rule table channel … module at spec`) that appears at the start of a line. Inside a query
it also synchronizes on clause keywords, so one malformed `where` loses only that clause. Missing `;` before a
statement-initial keyword on a new line is reported and inserted.

**Static rules that the grammar does not express (checked after parsing).**
- A `query` used by a sink must end in `select` or be a `values_expr`.
- After `group! by` / `group by`, binders other than the group keys may appear only inside aggregate calls.
- `group by` (no bang) may use only lattice constructors as aggregates (`lset(x)`, `lmax(x)`, …).
- `left join!` makes the joined binder `Option<Row>`; its fields are read with `?.` or `match`.
- `$node`, `$tick`, `trace(…)`, `at tick k` and `crash` are legal only inside `spec`.
- `old.r` is legal only inside `migrate` / `emit` / `accept`.

---

## 3. Constructs and their exact lowering to Dedalus

### 3.0 IR notation used in this section

The lowering target is the Dedalus^L core IR (ENG-001, SEM-100..109). It is printed here in Molly-like concrete
syntax:

- `h(ā) :- b₁, …, bₙ.` is a **deductive** rule (same node, same tick).
- `h(ā)@next :- … .` is an **inductive** rule (same node, tick t+1).
- `h(D, ā)@async :- … .` is an **async** rule; the first head column `D` is the destination (CR-14 normal form).
- The local location column is elided everywhere else; every body atom is at the evaluating node (LANG-151).
- `notin p(ā)` is negation; `X := e` binds; `e₁ op e₂` is a condition; `agg<X>` is a head aggregate.
- A lattice-valued relation is written `r(k̄; v)`: `k̄` are the key columns and `v` the lattice value (a product if
  there are several lattice columns). A head `r(k̄; v)` **joins** `v` into the cell (SEM-103); a set relation is the
  case `v ∈ 𝔹`, written without `;`.
- `persist[r]` abbreviates `r(x̄)@next :- r(x̄), notin del_r(x̄).` (LANG-065).
- `unit` is the built-in 0-ary fact that holds at every tick; it makes rules with no positive atom range-restricted.
- Names that end in `$n` (`remember$1`) are compiler-generated and scoped to one rule site. Their ids are stable
  and derived from module, rule label and ordinal (SEM-084), so provenance and seeds never depend on source order.

### 3.1 The general lowering of a query rule

A rule `SINK q;` is lowered in three steps. Every later subsection only specializes a step.

**Step 1: clauses to a body.** Clauses are processed left to right with an environment `Γ` of in-scope names.

| Clause | Adds to the body | Environment |
|---|---|---|
| `from p in R` | one atom `R(x̄₁,…,x̄ₖ)` with a fresh variable per column; literals in `p` become constants | binds `p`'s names to the variables |
| `join p in R on e` | the same atom, plus the conditions of `e` | same |
| `where e` | `e` split into conjunctive literals (§3.6) | unchanged |
| `let v = e` | `V := e` | binds `v` |
| `unnest p in e` | generator atom `elem(E, x̄)` (binding pattern: `E` bound) | binds `p` |
| `left join!`, `group!`, `choose!`, `enumerate!`, `seq!`, `top!`, correlated aggregates | cut the body: everything so far becomes an auxiliary relation, then the operator's own expansion (§3.7–3.9) | the operator's outputs |

Field access `p.f` is replaced by the variable bound for column `f`. Equality conditions between variables are
unified away (`join q in s on q.k == p.k` binds both to one variable), which gives the natural Datalog join.

**Step 2: `select` to a head.** `select {f₁: e₁, …}` (or a positional tuple, or `select p` for a row with the same
field names) is matched against the target schema by field name. Missing fields take their declared defaults;
any other missing field is an error. Every non-variable head expression `eᵢ` becomes a fresh variable with
`Vᵢ := eᵢ` in the body, because Dedalus heads are variables and constants only. Lattice-typed columns become the
`; v` part of the head.

**Step 3: the sink picks the rule kind.**

| Sink | Target kinds allowed | IR |
|---|---|---|
| `into R` | table, durable table, scratch, output, lattice, soft, range, `halt`, `stdio` | `R(ā) :- body.` |
| `into next R` | table, durable table, scratch, lattice, soft, range, `localtick` | `R(ā)@next :- body.` |
| `send C [to e]` | channel, loopback, service request, `stdio` | `C(D, ā)@async :- body.` with `D := e` or the `@` field |
| `delete! next R` | table, durable table, soft, range | `del_R(ā) :- body.` |
| `upsert! next R` | table, durable table, soft | §3.5 |
| `seal R on (k̄) …` | table, durable table, channel-fed tables | §3.13 |
| `fail "m"` | none (writes `violation`) | `violation(site, "m", ā) :- body.` |

The legality matrix of LANG-066 is this table read in reverse: `into`/`into next` on a channel, `send` on a table
or lattice, `delete!`/`upsert!` on a scratch or lattice, and any sink on a `periodic`, `static` (after bootstrap),
`input` or `readonly` relation are compile errors (ANA-005).

**Example of all three steps.**

```
rule ack_put: send put_ack from p in kv_put join s in sessions on s.id == p.$session
              where p.key != "" select {to: p.$session, reqid: p.reqid};
```

```
put_ack(D, R)@async :- kv_put(R, K, V, S), sessions(S, _), K != "", D := S.     // ack_put$0
```

`kv_put`'s implicit `$session` column appears in the IR atom only because the rule reads it (SEM-091).

### 3.2 Collections (storage classes)

Every collection declaration is `{modifier} kind name(schema) {option};`. Keys are the fields left of `=>`
(§3.3). The lowering of each kind is its persistence behavior; rules into it are lowered by §3.1.

| Declaration | Example | IR produced by the declaration | IDs |
|---|---|---|---|
| `table` | `table buf(id: u64 => payload: bytes);` | `persist[buf]` | LANG-040 |
| `durable table` | `durable table log(idx: u64 #1 => term: u64 #2, cmd: bytes #3);` | `persist[log]` + relation flagged `durable` (WAL, commit before outbox, SEM-072) | LANG-044 |
| `scratch` | `scratch hot(word: string);` | nothing: tick-local | LANG-041 |
| `let` view (temp) | `let hot = from w in counts where w.n > 9 select {w.word};` | scratch with inferred schema + its rule | LANG-047 |
| `channel` | `channel ack(@src: Node, dst: Node, id: u64);` | relation whose only writers are `@async` rules; tick-local at the receiver | LANG-042 |
| `input` / `output` | `input put(key: string => val: bytes);` | tick-local interface relations; direction recorded in the catalog | LANG-043, 003 |
| `static` | `static peers(n: Node) = values ("a"), ("b");` | facts that hold at every tick: `peers("a") :- unit.` (CR-16) | LANG-045 |
| `loopback` | `loopback retry(id: u64);` | a channel whose `@` is always `self`: `retry(Self, I)@async :- …, Self := self.` | LANG-046 |
| `soft table` | `soft table heard(peer: Node) ttl 4s max 1024;` | §3.2.1 | LANG-048 |
| `sealed table` | `sealed table split(line: u64 => text: string);` | `persist[split]`, `violation("write after bootstrap", …)` for writes at tick > 0, and a whole-relation seal fact `seal_split() :- tick() >= 1.` | LANG-049 |
| `range table` | `range table seen(src: Node, seq: u64);` | same IR as `table`; storage uses interval buckets on the last integer column; never reclaimed | LANG-050 |
| `zset table` / `bag table` | `zset table view(k: string => v: i64);` | weighted relation (LANG-138) in the DBSP stratum | LANG-138 |
| `durable lattice` / `lattice` / `scratch lattice` | `durable lattice term: lmax<u64>;` | 0-ary lattice relation `term(; v)`; persistent ones get the identity inductive rule `term(; V)@next :- term(; V).` (SEM-104); scratch ones get none | LANG-120, 128 |
| `periodic` / `once` | `periodic hb every 1s;` | §3.4 | LANG-172, 173 |
| built-ins | `stdio`, `halt`, `localtick`, `file_reader(path)` | runtime relations (LANG-051, 052, 046) | |

`materialized` / `recomputed` on a `let` view is a plan hint only (LANG-053); it changes no IR rule.

#### 3.2.1 Soft state (CR-17, SEM-060, SEM-061)

`soft table heard(peer: Node) ttl 4s max 1024;` adds a hidden lattice column `$birth: lmax<timestamp>`. Every
derivation into `heard` supplies `$birth = now()`; because the column is a lattice, re-deriving an existing tuple
merges to the newer birth, which is exactly "re-derivation refreshes" (SEM-060). Rules may read `h.$birth`.

```
heard(P; B) :- <body of each rule into heard>, B := now().                     // refresh by lmax merge
heard(P; B)@next :- heard(P; B), notin del_heard(P), now(N), N - B < 4s.       // TTL-guarded persist
heard_n$(count<P>) :- heard(P; _).
heard_rank$(P, I)  :- heard(P; B), I = index() by (B, P).                       // oldest first
del_heard(P) :- heard_rank$(P, I), heard_n$(C), C > 1024, I < C - 1024.        // evict oldest (birth, canonical)
```

A soft head derived from soft body atoms inherits the minimum body birth instead of `now()` when the rule is marked
`refresh cascade` (SEM-061, P1); ANA-006 checks that head TTL ≥ body TTL.

### 3.3 Types, schemas and keys

```
type Entry = {term: u64, cmd: bytes};
enum Role { Follower #1, Candidate #2, Leader #3, unknown }
durable table log(idx: u64 #1 => term: u64 #2, cmd: bytes #3, client: Option<Principal> #4 = None since 2);
table current( => term: u64);                    // empty key: a singleton register
table votes(term: u64 => voters: lset<Node>);    // lattice column; key = the non-lattice columns
table rename_me(like log);                        // schema reuse (LANG-020)
```

- Keys are the fields left of `=>`. With no `=>`, every field is a key; with nothing left of `=>`, the relation is a
  singleton (LANG-020). A lattice-typed field is never a key (LANG-121): putting one left of `=>` is an error.
- Two distinct tuples with the same key in one tick are a runtime error unless every differing field is a lattice,
  in which case they merge (SEM-050, CR-51). `resolve! policy` on the declaration replaces the error by a choice
  (LANG-117, §3.8).
- Scalars (LANG-022): `bool`, `i8…i64`, `u8…u64`, `f64` (never a lattice order), `string`, `bytes`, `unit`, `Node`,
  `Node@role`, `Principal`, `Session`, `duration`, `timestamp`, `id<160>` with ring intervals `x in (a, b]`
  (LANG-026). Compounds (LANG-023): tuples, records, `list<T>`, `set<T>`, `map<K,V>`, enums, `Option<T>`
  (LANG-025). Opaque host types: `extern type Blob "crate::Blob";` (LANG-027); blob handles `blob` (LANG-028).
- Field numbers `#n`, defaults, `since`, `deprecated since` and `semantics_changed since` are LANG-261/265.
  The compiler assigns missing numbers and records them in `schema.lock`.
- Types inside rules are inferred by unification; errors list every piece of evidence with its span (LANG-021).

The IR has positional columns; the lowering fixes the order as declared (keys first, then values), and the location
column first for channels.

### 3.4 Timers, time and randomness (LANG-170..175, CR-18, CR-19)

```
periodic hb every 1s;              // physical: hb(id: u64, at: timestamp), id counts firings
periodic lt every 3 ticks;         // logical
periodic retry every 500ms times 10;
once start;                        // fires once, at tick 0
```

Lowering: a physical `periodic` is a runtime-fed EDB event relation (`hb(I, At)` arrives as a batch input at a tick
chosen by the timer wheel, DIST-030; virtual time in simulation, ODD-16). A logical timer is plain IR:

```
lt_ctr$(0) :- bootstrap.                                     // bootstrap is true only at tick 0
lt_ctr$(C2)@next :- lt_ctr$(C), C2 := (C + 1) % 3.
lt(I, T) :- lt_ctr$(0), T := tick(), I := T / 3.
localtick()@next :- unit.                                    // a logical timer keeps the node ticking
```

`once start` is `start(0, T) :- bootstrap, T := now().`. `times n` adds `I < n` to the event relation's filter.

`now()`, `tick()`, `random()`, `rand(k̄)`, `rand_float(k̄)`, `rand_range(lo, hi, k̄)` are ordinary expressions. In
the IR `now()` and `tick()` are the per-tick inputs `now(N)` and `tick(T)` joined into the body; `rand(k̄)` is the
pure keyed PRF of SEM-084 (node seed, incarnation, tick, fingerprint of `k̄`). The analyzer marks every rule reading
them as time-dependent and schedule-dependent (SEM-087). A value that must stay fixed is captured with `into next`:

```
upsert! next deadline from e in start_election
  select {at: now() + rand_range(election_min, election_max, ("election", e.term))};
```

### 3.5 Rule kinds and mutations (LANG-060..067, CR-05..07)

**Deductive, `into`.**
```
into reach from l in link select {src: l.src, dst: l.dst};
```
```
reach(S, D) :- link(S, D).
```

**Inductive, `into next`.** Evaluated once on the completed fixpoint (SEM-003); may read any stratum.
```
into next pending from r in request where not! done(id: r.id) select r;
```
```
pending(I, P)@next :- request(I, P), notin done(I).
```

**Async, `send`.** The destination is the `to` expression or, without `to`, the channel's `@` field in the
`select` record. The receiving node sees the tuple at a later tick (SEM-040) as a tick-local fact.
```
send ack to m.$sender from m in data select {id: m.id};       // channel ack(@to: Node, id: u64)
```
```
ack(D, I)@async :- data(I, _, S), D := S.
```

**Deferred delete, `delete! next`.** Exact-tuple deletion at t+1 (CR-06). Deleting by key is a key join.
```
delete! next buf from b in buf join a in ack on a.id == b.id select b;
```
```
del_buf(I, P) :- buf(I, P), ack(_, I).
```
Insert wins over delete for the same fact at t+1 (CR-05, SEM-006), which is the IR's natural behavior because
`persist[buf]` and any insertion rule are separate rules.

**Deferred upsert, `upsert! next`** (LANG-064). At t+1, delete every tuple with the selected key and insert the
selected tuple.
```
upsert! next kv from p in put select {key: p.key, val: p.val};
```
```
kv(K, V)@next :- put(K, V).
del_kv(K, V0) :- put(K, _), kv(K, V0).
```
If the stored value already equals the new one, the delete and the insert name the same fact and insert wins, so
the tuple stays. Two different upserts to the same key in one tick are the SEM-051 error, unless the sink carries a
resolution:

```
upsert! next kv resolve! choose_most(reqid) from p in put select {key: p.key, val: p.val, reqid: p.reqid};
```
```
cand_kv$(K, V, R) :- put(K, V, R).
best_kv$(K, max<R>) :- cand_kv$(K, _, R).
kv(K, V, R)@next :- cand_kv$(K, V, R), best_kv$(K, R).
del_kv(K, V0, R0) :- cand_kv$(K, _, _), kv(K, V0, R0).
```
The policy expression ranges over the **target's** fields (here `kv.reqid`), because the candidates are rows of the
target.
`choose_least/most(c)` ties are broken by the seeded priority and then canonical order (LANG-114), which the
expansion adds as a second `min<(PRF, W̄)>` stage when `c` is not provably unique (ANA-038 D5). `choose`,
`choose sticky` and `choose_rand` use the R12 §5.1/§5.5/§5.7 expansions over `cand_kv$`.

**Host input** is only ever deferred (LANG-067): `input` relations are filled by the runtime between ticks.

**Explicit persistence** (LANG-065) is available for Dedalus purists: `into next r from x in r where not! del_r(…) select x;`
together with `scratch del_r(…)` is accepted and is exactly `persist[r]`; the lint suggests `table`.

### 3.6 Bodies: joins, conditions, negation, quantifiers (LANG-080..092)

**Joins and cross products.** `from p in a join q in b on q.k == p.k` is a natural join; `from p in a from q in b`
is a Cartesian product, filtered by any later `where`. N-way joins are clause sequences. Equalities are unified;
other conditions stay as IR conditions (LANG-086).

**Patterns.** Binders may destructure and match constants (LANG-080, LANG-081, LANG-088):
```
into leaders from {role: Leader, term} in my_role select {term};
```
```
leaders(T) :- my_role(Leader, T).
```

**`where` conditions.** A `where` expression is converted to disjunctive normal form over *literals*. Scalar
comparisons stay conditions. Relational predicates become atoms:

| Surface predicate | Meaning | IR literal |
|---|---|---|
| `r(f: e, …)` | some row of `r` has `f == e` (named-field atom; unnamed fields are wildcards) | `r(…, E, …)` |
| `e in r` (unary `r`) / `(e₁, e₂) in r` | membership (LANG-090) | `r(E)` |
| `exists (q in r where c)` | semi-join | `ex$(Ō) :- r(…), c.` then `ex$(Ō)` where `Ō` are the outer variables used in `c` |
| `not! p` for any of the above | anti-join (LANG-082, 083) | `notin r(…)` / `notin ex$(Ō)` |
| `all x in r: p` | ∀ | `miss$(Ō, X̄) :- r(X̄), notin p$(Ō, X̄).`, `all$(Ō) :- outer$(Ō), notin miss$(Ō, _).` |
| lattice threshold (`l >= 3`, `s.contains(x)`, `m.has_key(k)`) | monotone read (§3.10) | threshold literal |

A disjunction whose disjuncts contain relational predicates splits the rule into one IR rule per disjunct
(LANG-089). Negated variables must be bound positively (range restriction, ANA-001); the parser keeps spans so the
error points at the unbound name.

```
into orphan from c in child where not! parent(id: c.parent) or c.parent == 0 select {c.id};
```
```
orphan(I) :- child(I, P), notin parent(P).
orphan(I) :- child(I, P), P == 0.
```

**`let` and `unnest`** (LANG-085, LANG-088).
```
into words from l in line unnest (pos, w) in enumerate(split_ws(l.text))
  let lw = lower(w) select {line: l.no, pos, word: lw};
```
```
words(N, Pos, LW) :- line(N, T), Ws := split_ws(T), elem_idx(Ws, Pos, W), LW := lower(W).
```
`elem_idx` and `elem` are built-in generator relations whose first argument must be bound (LANG-092).
`from i in range(0, n)` is the generator `range(0, N, I)` with the same binding-pattern check.

**Indexed lookup and range scans** (LANG-091). `from e in log[i]` and `from e in log[lo..hi]` range over the
first key column:
```
into to_apply from e in log[last_applied + 1 .. commit + 1] select e;
```
```
to_apply(I, T, C) :- log(I, T, C), last_applied$(A), commit$(M), I >= A + 1, I < M + 1.
```
The planner turns the bound pair into an index range query; the IR is the plain condition.

**Union** of queries into one sink is several IR rules with the same head. **Recursion** needs no keyword: a view or
table may appear in its own defining queries.
```
let reach = from l in link select {src: l.src, dst: l.dst}
      union from r in reach join l in link on l.src == r.dst select {src: r.src, dst: l.dst};
```
```
reach(S, D) :- link(S, D).
reach(S, D) :- reach(S, M), link(M, D).
```

**Utility projections** (LANG-094): record spread `{..p, val: v}`, `p without (dst)`, `schema_of(r)`, and
`payloads(p)` (drops the `@` field) are pure record expressions and compile to column permutations.

### 3.7 Outer join (LANG-087)

```
send get_reply from g in kv_get left join! s in store on s.key == g.key
  select {to: g.$session, reqid: g.reqid, val: s?.val};           // val: Option<bytes>
```
```
lj_match$(R, K, S, V) :- kv_get(R, K, S), store(K, V).
lj_has$(R, K, S)      :- lj_match$(R, K, S, _).
get_reply(D, R, Some(V))@async :- lj_match$(R, K, S, V), D := S.
get_reply(D, R, None)@async    :- kv_get(R, K, S), notin lj_has$(R, K, S), D := S.
```

### 3.8 Aggregation, choice and order (LANG-100..118)

**Non-monotone grouping, `group!`** (LANG-100, 101). SQL GROUP BY under set semantics: an empty group yields no row
(CR-08). After `group! by keys`, other binders may appear only inside aggregates.
```
into word_count from w in words group! by w.word select {word: w.word, n: count()};
```
```
wc_vars$(W, N, P) :- words(N, P, W).           // Molly-style split: grouping sees only the body valuation
word_count(W, count<N, P>) :- wc_vars$(W, N, P).
```
**[new] What `count()` counts.** `count()` counts the distinct valuations of every binder in scope at the `group!`
clause (here `(N, P)` for each `W`), which is `count<*>` over the deduplicated body. `count(e)` counts distinct
values of `e`, and `count(distinct e)` is accepted as a synonym. This is the only reading consistent with set
semantics (CR-03), and it is why word count above binds `pos`. For the same reason `sum(e)` and `avg(e)` range over
the distinct valuations of the in-scope binders, not over the distinct values of `e`: two mappers that report the
same partial count for a word both contribute (E6). SQL users get bag-like results without bags, and Molly's
`sum<X>` (distinct values) is available as `sum(distinct e)`.

Aggregates: `count sum min max avg` (LANG-100); `set list mklist accum_pair` returning canonically sorted values
(LANG-102, 118); `argmin(e by k) argmax(e by k)` returning all tied exemplars, `bool_and bool_or` (LANG-103);
`percentile(p, e)`, `topk_list(k, e by k)`, `quantile_sketch(e)` (LANG-104); declared user aggregates (LANG-105);
estimators `ola_sum ola_count ola_avg(p)` and `scale_by_progress(e)` (LANG-113). Order-sensitive aggregates must be
written with a bang even inside `group!` (`sort!(x by k)`, `percentile!(0.99, x)`, `fold!(…)`), because each is its
own negative edge.

**Monotone grouping, `group by`** (LANG-123). Only lattice constructors may appear as aggregates; the result is the
per-key lattice fold, so it is monotone and has no bang.
```
into votes from v in vote_reply where v.granted group by v.term
  select {term: v.term, voters: lset(v.$sender)};
```
```
votes(T; S) :- vote_reply(T, G, Sender), G == true, S := lset_single(Sender).
```
The same rule without `group by` means the same thing (the key FD merges the singletons), and the formatter
removes a redundant `group by`. It is kept in the grammar because data engineers reach for it.

**Aggregates with a default, driven by an outer row** (LANG-106, CR-08). A correlated aggregate subquery is an
expression. It is evaluated once per outer valuation and yields the default when nothing matches.
```
into yes_cnt from t in txn
  let n = count!(v in vote where v.txn == t.id and v.yes) default 0
  select {txn: t.id, n};
```
```
sub$(T, V) :- txn(T), vote(T, V, Y), Y == true.
subc$(T, count<V>) :- sub$(T, V).
subv$(T, N) :- subc$(T, N).
subv$(T, 0) :- txn(T), notin subc$(T, _).
yes_cnt(T, N) :- txn(T), subv$(T, N).
```
`default` is mandatory for `min! max! avg! argmin! argmax! percentile!` and optional for `count! sum! set! list!
bool_or! bool_and!`, whose monoid identity (0, ∅, [], false, true) is then the default. The FLAG-111 idiom
`count<T> default 0` is `count!(t in task where …)`.

**Quorum sugar** (LANG-111). `majority(s, of: r)` where `s: lset<T>` and `r` is a `static` or sealed relation:
```
into quorate from v in votes where majority(v.voters, of: peers) select {term: v.term};
```
```
peers_n$(count<N>) :- peers(N).                                    // count over a CLOSED relation
quorate(T) :- votes(T; V), peers_n$(C), lset_size(V) >= C / 2 + 1.  // threshold on lset.size (monotone in V)
```
The verifier maps it to a quorum sort with the intersection axiom (VER-008).

**Choice, `choose!`** (LANG-108, 114, 115, 116; CR-45). `choose! per (x̄) [policy]` keeps one valuation per group
`x̄` among the rows reaching this clause. The chosen columns `Ȳ` are the variables that are **live** after the
clause (read by later clauses or `select`), minus `x̄`. Policies: none (seeded priority), `least e`, `most e`,
`sticky`, `rand`, `rand sticky`. Several `choose!` clauses in one rule form one multi-FD site (LANG-116).
```
into grant from rv in request_vote where rv.term == cur
  choose! per (rv.term) least rv.cand
  select {term: rv.term, cand: rv.cand};
```
```
cand$(T, C) :- request_vote(T, C, _, _), cur$(T).
pmin$(T, min<C>) :- cand$(T, C).                           // least: cost = C, unique, so no PRF stage (D5)
grant(T, C) :- cand$(T, C), pmin$(T, C).
```
With no policy the priority is `(PRF_σc(site, X̄, Ȳ), Ȳ)`:
```
cand$(X, Y) :- body.
pmin$(X, min<P>) :- cand$(X, Y), P := prio(site$, X, Y).
chosen$(X, Y) :- cand$(X, Y), pmin$(X, P), P == prio(site$, X, Y).
head :- body, chosen$(X, Y).
```
`sticky` adds R12 §5.5's `held$(X̄, Ȳ)@next` relation; it may be declared `durable` with `sticky durable`.
The FD always includes the node and the tick (SEM-085); there is no syntax for a tick-free FD.

**Numbering and top-k** (LANG-093, 097, 098, 118).
```
into indexed from c in client_in enumerate! as i by (c.client, c.reqid) select {cmd: c, slot_off: i};
into ids     from f in files     seq! as n select {path: f.path, id: n};
into hottest from w in word_count top! 10 by (w.n desc) select w;
```
`enumerate!` is R12 §5.9's per-tick dense rank after head deduplication; `seq!` is the high-water-mark expansion
of R12 §5.9 (its `assigned$`/`hwm$` relations become `durable` automatically when the number reaches a `send` or
an output, ANA-011); `top! k by …` is `index < k`.

**Ordered fold** (LANG-110, 109). `fold!(init, step, e by k)` in a `group!` select is the aggregate form;
the carried form is written as an inductive rule:
```
into next sm_state from s in sm_state
  select {st: fold!(s.st, apply_cmd, a in to_apply: a.cmd by a.idx)};
```
The expansion is R12 §5.11 (`rk$`, `acc$`, `n$`, `out$`); ANA-011 lints a carried fold over a persistent input.
`reduce(init, f)` with `f` declared `commutative associative` needs no bang and no order (LANG-109).

**Relation-level resolution** (LANG-117): `table reg(k: K => v: V, ts: (u64, Node)) resolve! choose_most(ts);`
applies the R12 §5.12 candidate expansion to every derivation of `reg` at t+1.

### 3.9 Lattices (LANG-120..139, 142, 280..284; SEM-030..034, 100..109)

**Built-in lattice types** (LANG-124, 130, 131, 132, 133, 134): `lbool`, `lmax<T>`, `lmin<T>` (⊥ = ∓∞ adjoined,
LANG-281), `lset<T>`, `lpset<T>`, `lbag<T>`, `lmap<K, L>`, `pair<L1, L2>`, `record {…}` (named product), `lex<K, L>`
(proper lexicographic pair with chain key `K`), `withbot<L>`, `withtop<L>`, `conflict<T>`, `point<T>`, `vecunion<L>`,
`unionfind<T>`, `ldom<V, L>` (antichain / MV-register), `tombset<T>`, `tombmap<K, L>`, `causal<dotset|dotfun<L>|dotmap<K,L>>`,
`vclock` (= `lmap<Node, lmax<u64>>` with `happens_before!`, `concurrent!`), `ballot` (= `lex<lmax<u64>, point<Node>>`),
`lww<T>` (= `lex<(timestamp, Node), point<T>>`). `unsafe dompair<L1, L2>` requires the `unsafe` keyword at the use
site (LANG-136, CR-25).

**Where lattices live.** A lattice is either a column type (`table votes(term: u64 => voters: lset<Node>)`) or a
0-ary identifier (`lattice seen: lset<Node>;`). Both are Dedalus^L cells (SEM-100). Persistence is the default;
`scratch lattice` resets to ⊥ each tick (LANG-128, CR-24).

**Merging is `into`** (LANG-122). `into l …` joins now, `into next l …` joins at t+1; `send` carries lattice values
inside channel rows (LANG-137), merged at the sender and within one delivered batch (SEM-105). `delete!` and
`upsert!` on a lattice are compile errors (LANG-284).

**Collections to lattices** (LANG-123). Selecting an element into a lattice position wraps it in the singleton
constructor, and the key FD folds the singletons: `into seen from v in vote select v.$sender;` with
`lattice seen: lset<Node>` lowers to `seen(; S) :- vote(_, Sender), S := lset_single(Sender).` The implicit wrap is
allowed only for `lset lpset lbag lmax lmin lbool`; every other lattice needs its constructor.

**Lattices to collections.** `from x in s.items()` ranges over an `lset` (a morphism); `from (k, v) in m.entries()`
over an `lmap` (to_collection, a morphism); `where b` on an `lbool` is `when_true`. `R(k̄; x)` generators range only
over non-⊥ cells, and `R[k̄]` is a lookup that returns ⊥ for an absent cell (LANG-280, LANG-129).

**Monotonicity is part of the method's type** (LANG-125, 126, 127). Each lattice method has a class from R04 §2.4:
morphism, bimorphism, monotone, antitone, or plain. Calls to antitone and plain methods are written with a bang.
Comparison operators on lattice operands are resolved to methods by type:

| Expression | Resolves to | Class | Bang? |
|---|---|---|---|
| `l >= c`, `l > c` (`l: lmax`, `c` scalar) | `gt_eq`, `gt` | M (threshold) | no |
| `l <= c`, `l < c` (`l: lmin`) | `lt_eq`, `lt` | M (threshold) | no |
| `l <= c` (`l: lmax`) | `lt_eq` | Anti | `not! (l > c)` or `reveal!(l) <= c` |
| `s.size`, `m.size` | size | Mon | no |
| `s.contains(x)`, `m.has_key(k)`, `m.at(k)`, `m.key_set()` | | M | no |
| `a + b` (`lmax`, `lmin`) | tropical `+` | BM | no |
| `l + c` (`lmax`, constant `c`) | shift | M | no |
| `merge(l, c)` | ⊔ with a constant: the monotone default (LANG-283) | M | no |
| `l == c`, `l != c` | exact comparison | NM | `reveal!(l) == c` |
| `reveal!(l)` | raw value | NM | yes |
| `v.version()` / `v.value!()` (`ldom`) | | M / NM | on `value!` |
| `l.is_top()`, `threshold(l, t₁, …, tₙ)` (pairwise incompatible `tᵢ`) | | M | no |

In the IR each method is a built-in function whose class the analyzer reads from the catalog (ANA-020, SEM-102);
the bang is purely syntactic confirmation of that class.

**Thresholds as conditions.**
```
send result to RESULT_ADDR where votes.size >= QUORUM select {};
```
```
result(D)@async :- votes(; S), Sz := lset_size(S), Sz >= 5, D := RESULT_ADDR.
```
`lset_size` is monotone and `>= 5` on an `lmax` is a threshold, so SEM-102 labels the occurrence monotone and the
program is certified by ANA-141.

**Mixing `bool` and `lbool` in `where`.** A `where` expression is split into literals before typing (§3.6), so
`p.n != self and cluster_version.at_least(3)` is two literals, one scalar and one threshold. Outside `where` (for
example in a `select` field), `and`/`or`/`not` require both operands to have the same type, and an `lbool` is
converted to `bool` only by `reveal!`.

**Monotone reset** (LANG-284): write `lex<lmax<u64>, L>` and raise the epoch.

**User-defined lattices** (LANG-135, ODD-09 (c)). Two forms.

(1) *Verified constructors.* The representation is a composition of built-in constructors, so merge, ⊥ and the
order are derived and proved by construction; only the methods need checking.
```
lattice type Interval = pair<lmin<i64>, lmax<i64>> {
  morphism fn lo(self) -> lmin<i64> = self.0;
  morphism fn hi(self) -> lmax<i64> = self.1;
  monotone fn width(self) -> lmax<i64> = self.1 - self.0;        // claim: checked (TEST-087)
  plain fn! midpoint(self) -> f64 = (reveal!(self.0) + reveal!(self.1)) / 2.0;
}
```
(2) *Rust trait implementation*, labeled "tested" after the law harness (TEST-083) passes:
```
extern lattice Hll<T> "blossom_sketch::Hll" {
  monotone fn estimate(self) -> lmax<u64>;
  morphism fn contains_maybe(self, x: T) -> lbool;
}
```
Lowering: a lattice type is catalog data (⊥, join, order, method classes), not IR rules. Method calls become
built-in function applications classified as declared. A `monotone`/`morphism` claim that the law harness refutes is
a compile error; one that is only fuzzed is reported as "tested, not proven" (TEST-087).

**Monotone functions vs morphisms on user functions** (LANG-182):
`morphism fn f(x: lset<u64>) -> lset<u64> = …;` may be applied to deltas (semi-naive); `monotone fn` must see the
whole value each round (R04 §3.8). The analyzer uses the class for both CALM and semi-naive planning.

**Group and ring types** (LANG-142) are declared with `group type Delta = zset<Row>;` and are rejected as lattices by
the law harness. `zset table` / `bag table` collections (LANG-138) take view modes `distinct`, `clamped`, `raw`:
`let live = from x in stock.distinct() select x;`. A group payload crosses nodes only on a wrapped channel:
`channel deltas(@to: Node, d: zset<Row>) carries zset<Row> via exactly_once(dots);` (LANG-158); `send` of a group
type on a plain channel is an error (ANA-015).

**Progressive snapshots** (LANG-139):
`snapshot partial of word_count at progress every 0.1 upto 0.9 mode committed_only estimate scale_by_progress;`
declares an output relation `partial(point, actual_progress, class, attempt, value)` defined by R14 §6's
threshold-gated `reveal`; it is typed `nondet "progressive"`.

### 3.10 Delta pseudo-relations (LANG-071)

`r.inserted!` and `r.deleted!` are the facts that became present or absent at this tick boundary.
```
into decided from o in outcome_log.inserted! select o;
```
```
outcome_log_prev$(X, O)@next :- outcome_log(X, O).
outcome_log_ins$(X, O) :- outcome_log(X, O), notin outcome_log_prev$(X, O).
decided(X, O) :- outcome_log_ins$(X, O).
```
`r.deleted!` is `r_prev$(x̄), notin r(x̄)`. The `_prev$` relation is shared by all delta reads of `r`.

### 3.11 Locations, distribution and roles (LANG-150..158, LANG-009, LANG-095)

- **One `@` field per channel** (CR-14, LANG-150). `channel ack(@to: Node, id: u64)`; the address field may sit in any
  position and is moved first in the IR. `self` is the local `Node` (LANG-152).
- **Body locality** (LANG-151) is structural: every relation in a `from`/`join` is local, and remote heads need
  `send`.
- **Cluster roles** (LANG-153). Inside a choreography, `role workers: cluster;` provides `members(workers)` (a static
  relation of `Node@workers`), `self` typed `Node@workers` inside `at workers { … }`, and the membership stream
  `membership(workers)` with `joined`/`left` events when dynamic membership (LIB-023) is linked.
- **Partitioning** (LANG-154). `channel shuffle(@to: Node@reducer, word: string => n: u64) partition by hash(word)
  over members(reducer);` lets `send shuffle` omit the address; the compiler fills `to := owner(shuffle, word)`,
  which is also callable as `owner(shuffle, e)` in expressions:
  `shuffle(D, W, N)@async :- …, members_reducer_ranked$(D, I), I == hash(W) % C, members_reducer_n$(C).`
- **Fault model** (LANG-155): `delivery reliable ordered | reliable | lossy | lossy forever | best_effort`. It
  changes only analysis and simulation, never the IR.
- **Multi-location bodies** (LANG-095, CR-15) require the `localize` statement modifier and explicit `@loc` on
  remote sources; the compiler performs the chain rewrite, one `@async` hop per location change, and lints:
```
localize into both_have from a in item join b in item@a.peer on b.id == a.id select {a.id};
```
```
fwd$(P, I, Self)@async :- item(I, P), Self := self.            // hop to a.peer
both_have$(I, Back)   :- fwd$(I, Back), item(I, _).            // evaluated at the peer
both_have(Back, I)@async :- both_have$(I, Back).               // hop back
```
  (`both_have` is then a channel-fed relation; the rewrite changes its timing, which is why the modifier is required.)

**Choreographic modules** (LANG-009). A `choreography` declares roles and puts rules in `at role { … }` blocks.
Channel address types name the role (`@to: Node@participant`), so a `send` from one role to another is type-checked.
The compiler *projects* one Dedalus program per role: the rules in `at r`, the declarations visible to `r`, and the
channels `r` sends or receives. Nothing else changes in the IR. Projection also yields `senders(c)`, the inferred
default-deny ACL of every channel (LANG-242, ANA-105).

### 3.12 Functions, host interop and services (LANG-180..186)

```
const QUORUM: u64 = 3;                                       // compile-time constant (LANG-010)
param retry: duration = 2s;                                  // deploy-time parameter, from config or CLI
fn key_of(p: Put) -> string = p.tenant ++ "/" ++ p.key;      // pure, in-language (inlined)
extern pure injective fn hash64(b: bytes) -> u64 "blossom_std::hash64";      // LANG-181, 182
extern commutative associative aggregate hll_add(bytes) -> Hll<bytes> "blossom_sketch::add";  // LANG-105
extern table fn tokens(s: string) -> (pos: u64, w: string) "blossom_tok::tokens";           // LANG-183
service geoip(ip: string) -> (ip: string => city: string) "svc::geoip";                        // LANG-184
```
- In-language `fn` bodies are pure expressions; they are inlined before analysis, so rule bodies never contain
  opaque closures (LANG-002). `extern fn` calls are `F(Args, Out)` built-in atoms with binding pattern
  `(in…, out)`; declared properties feed ANA-043, ANA-080 and semi-naive planning.
- A table function is used as a source with its inputs bound: `from t in tokens(l.text)` lowers to the atom
  `tokens(T, Pos, W)` with binding pattern `(in, out, out)`. (`split_ws`, used in §3.6 and E6, is the built-in
  scalar function `string -> list<string>`; `enumerate(xs)` is the built-in `list<T> -> list<(u64, T)>`.)
- A `service` is a pair of relations: `send geoip.request from … select {ip: …};` and
  `from r in geoip.response`. The request is an `@async` to the service endpoint; the response is a later input
  (the Dedalus rendezvous, R02 §3.3).
- Host callbacks subscribe to outputs with `full` or `delta` mode from the embedding API (LANG-185, 186); this is
  not surface syntax.

### 3.13 Seals, punctuations and finality (LANG-207, LANG-212, ANA-065, ANA-120..122)

A seal is a statement that no more tuples of `r` with the given key values will ever appear. It is written as a
sink over a query whose rows name the sealed key values. With `counted`, each row also carries `count`, the number
of distinct `r` tuples that the partition must contain before the seal takes effect. That is the Blazes digest
(R05 §4.5), and it makes a seal that overtakes its data harmless.

```
seal got on (mapper) counted
  from s in shuffle_done select {mapper: s.$sender, count: s.digest};
```
```
seal_got$(M, C) :- shuffle_done(C, S), M := S.
seal_got$(M, C)@next :- seal_got$(M, C).                                // seals persist
got_rows$(M, W) :- seal_got$(M, _), got(M, W, _).
got_cnt$(M, count<W>) :- got_rows$(M, W).
got_cnt$(M, 0) :- seal_got$(M, _), notin got_rows$(M, _).
sealed_got(M) :- seal_got$(M, C), got_cnt$(M, C).
sealed_got(M)@next :- sealed_got(M).                                    // once effective, stays effective
violation("seal_broken", M) :- sealed_got(M), got_cnt$(M, N), seal_got$(M, C), N != C.
violation("seal_conflict", M) :- seal_got$(M, C1), seal_got$(M, C2), C1 != C2.
```

- `is_sealed(r, k: v)` is the built-in predicate over `sealed_r`. It is monotone in time (it is persisted and never
  deleted), so it is a threshold and needs no bang. Its negation `not! is_sealed(…)` needs one.
- A seal with no `on` list seals the whole relation; `sealed table` gets one automatically after bootstrap
  (LANG-049).
- The analyzer treats a sealed partition as CLOSED for ANA-121 and certifies exact reads guarded by it
  (ANA-142). `all m in members(mapper): is_sealed(got, mapper: m)` is the standard "every producer has sealed" guard.
  Because `members(mapper)` is static and `sealed` is monotone, the ∀ is monotone and has no bang.
- A seal is ordinary data, so it travels on ordinary channels, retries like any message, and has provenance.

**Final outputs** (LANG-212). `output final word_count(word: string => n: u64);` is accepted only if ANA-120
classifies every rule into `word_count` as POS-, NEG-, TOP-, THRESH-, FINITE- or SEALED-final. At runtime each row
carries `provisional | final_present | final_absent`. `is_final(r(f: e))` and `when_final(e)` are threshold
predicates (finality is monotone). The declaration adds no IR rules; it adds an analysis obligation and the runtime
gate of ANA-121/122.

### 3.14 Modules, instances, protocols, overrides, interposition (LANG-001..010)

```
protocol Delivery {
  input  pipe_in(dst: Node, src: Node, ident: u64 => payload: bytes);
  output pipe_sent(like pipe_in);
  output pipe_out(like pipe_in);
}

module Loopback implements Delivery {                 // the smallest complete implementation
  rule sent: into pipe_sent from p in pipe_in select p;
  rule out:  into pipe_out from p in pipe_in where p.dst == self select p;
}
```
(E2 gives the full `BestEffortDelivery` and `ReliableDelivery`.)

- **Program structure** (LANG-001). Statements in a module are an unordered set; textual order means nothing.
- **Typed interfaces** (LANG-003). `input`/`output` declarations are the only relations visible from outside an
  instance. Reading an instance's non-interface relation (`rd.buf`) is an error, except inside `spec`.
- **Import creates an instance** (LANG-004). `import M(args) as a [: P];` copies every declaration and rule of `M`
  with every relation renamed `a.rel`, substitutes `args` for `M`'s `param`s, and (with `: P`) restricts the
  importer's view to protocol `P`'s interface. Importing the same module twice under two aliases makes two
  disjoint copies; reusing an alias is an error. Nested instances are `a.b.rel`. Lowering is pure renaming:
  `a.pipe_in` is an IR relation named `a.pipe_in`, and the importer's rules that write `a.pipe_in` are just more
  rules for it.
- **Implementation choice at composition time** (LANG-006). A module may import a protocol-typed parameter:
  `module Kvs(D: Delivery = ReliableDelivery) { import D as d; … }`, and a composition picks the implementation:
  `import Kvs(D: BestEffortDelivery) as kv;`.
- **Mixin and include** (LANG-005). `mixin M;` includes `M`'s declarations and rules flat (no renaming), which is
  Bloom's `include`; `include "file.bls";` is textual and resolves relative to the including file.
- **Named rules and blocks, override** (LANG-068, LANG-007). `rule name: …;` labels one rule; `block name { … }`
  labels a set. `module Majority extends Voting { override block summary { … } }` replaces the base's block of that
  name. Overriding a block the base does not have, or defining a block name twice, is an error.
- **Interposition** (LANG-008). `interpose a.iface as local { … }` reroutes one interface of an instance through the
  importer's rules. For an **output** `a.o`, the instance's own rules that wrote `a.o` now write `local` (same
  schema), and the rules inside the braces define `a.o` for every other reader. For an **input** `a.i`, the
  importer's writes to `a.i` go to `local`, and the rules inside the braces define what the instance really sees
  as `a.i`. Lowering: rename one relation in the instance's rule copy, then add the braces' rules.

```
interpose rd.pipe_out as raw {
  into rd.pipe_out from m in raw where not! blocked(src: m.src) select m;
}
```
```
raw(D, S, I, P) :- bed.pipe_out(D, S, I, P).          // was: rd.pipe_out(…) :- rd.bed.pipe_out(…)
rd.pipe_out(D, S, I, P) :- raw(D, S, I, P), notin blocked(S).
```

- **Constants and parameters** (LANG-010). `const` is inlined; `param` is resolved at deployment (CLI or config)
  and becomes a `static` 0-ary relation `param_retry(V)` wherever it is used in a rule, so one binary serves many
  deployments. Module parameters (`module M(retry: duration = 2s)`) are bound at import and lowered the same way.
- **Relation parameters [new].** A module parameter may have a relation type, `static(n: Node)`. The importer binds
  it to one of its own `static` or `sealed` relations with the same schema:
  `import ReliableBroadcast(members: cluster) as data;`. Lowering substitutes the importer's relation name into
  the instance's rules. Because the bound relation is closed, `all m in members: …` inside the instance stays
  monotone. This replaces Bloom's `StaticMembership` mixin without giving each instance its own configuration.
- **Bootstrap** (LANG-190). `bootstrap { … }` rules are conjoined with `bootstrap` (true only at tick 0). Imported
  instances bootstrap before their importer; `into next` inside bootstrap lands at tick 0 as LANG-190 requires, so
  the compiler lowers it to a deductive rule guarded by `bootstrap`.
- **Trusted modules** (LANG-205). `trusted module Paxos { … }` suppresses CALM warnings inside it and adds the
  VER-020 interface-check obligation.

### 3.15 Principals, sessions and authorization (LANG-240..245, CR-40)

- Every received channel row has implicit `$sender` (a `Node`, or a `Session` for external clients) and
  `$principal` fields; external-ingress rows also have `$session`. They are read as fields (`m.$sender`), never
  written, and projected away when unread (SEM-091). In the IR they are extra trailing columns of the channel atom
  that exist only in rules that name them.
- `principal_of(n)` and `role_of(n)` are built-in functions over the static `node(Node, Address, Principal, Role)`
  directory (LANG-240).
- ACLs (LANG-242). The default is the inferred, default-deny `senders(c)`; the explicit form narrows it or opens an
  external channel:
```
channel kv_put(@server: Node, reqid: u64 => key: string, val: bytes)
  accept from external client where $principal in writers;
channel admin_cmd(@dst: Node, cmd: AdminCmd) accept from principal in admins;
import Raft as r acl { r.client_req: accept from external client; };
```
  These compile to the ingress admission function (DIST-062), not to rules; `writers` must be a unary `static` or
  `table` relation, read at the last committed tick.
- Rule-level authorization (LANG-244) is plain rules deriving `authorized` and `authz_denied`.
- `signed<T>`, `sign(x)` and `verify!(s)` are P2 (LANG-245); `verify!` carries a bang because its result is
  `Option`, and absence is exact.

### 3.16 Program versions and migrations (LANG-260..265)

```
program kv version 3;
durable table store(key: string #1 => val: bytes #2, ver: u64 #3 = 0 since 3);

migrate from 2 {
  into store from o in old.store select {key: o.key, val: o.val, ver: 0};
}
emit put_ack to 2 { into old.put_ack from a in put_ack select {reqid: a.reqid, to: a.to}; }
accept kv_put from 2 { into kv_put from o in old.kv_put select {..o, ttl: None}; }

static peers(n: Node);
channel replicate(@to: Node, key: string => val: bytes, ver: u64) since 3;
channel probe(@to: Node, key: string);
send replicate from s in store.inserted!, p in peers where p.n != self and cluster_version.at_least(3)
  select {to: p.n, key: s.key, val: s.val, ver: s.ver};                     // gated write (ANA-102)
unsafe_ungated "read-only probe, old nodes ignore it"
  send probe from s in store.inserted!, p in peers where p.n != self select {to: p.n, key: s.key};
```

- `migrate from N { … }` rules are one deductive stratum with no temporal sinks (`into next`, `send`, `delete!`,
  `upsert!` are errors), no `now()`/`random()`, and `old.r` typed by `schema.lock` version N. ANA-103 classifies each
  block as tuple-local, monotone or non-monotone. The IR is an ordinary stratified program over `old$r` relations.
- `emit c to N` and `accept c from N` bodies must be tuple-local: one channel atom, pure functions (ANA-103).
- `cluster_version` is a built-in `lmax<u32>` input; `.at_least(V)` is its threshold (LANG-264).
- `since N`, `deprecated since N` and `semantics_changed since N` are field and declaration annotations (LANG-261,
  265).

### 3.17 Specs, invariants and verification (LANG-069, 070, 200, 201; TEST-020..029, 080, 081; VER-001..010)

A `spec` block is a separate, global program evaluated over a run's trace. It may join across nodes, and may read
oracles, but no protocol rule can read it (ANA-010).

```
spec Delivered for SimpleBroadcast {
  nodes a, b, c;
  failures { eot: 4, eff: 2, crashes: 0 }
  on a { into neighbors values ("b"), ("c"); }
  at tick 1 on a { into bcast values ("hello"); }
  pre  from l in log where not! crash(node: l.$node) select {node: l.$node, payload: l.payload};
  post from l in log where not! exists (m in missing_log where m.payload == l.payload)
       select {node: l.$node, payload: l.payload};
  check ldfi;
}
```

- Inside a spec every relation has `$node`, and `trace(r)` also has `$tick` (TEST-080 `R_log`). In `pre`/`post`,
  plain `r` means `r` at EOT (TEST-022). In an `invariant`, plain `r` means `r` in every global state the checker
  visits (every simulator step, every BMC state, and the arbitrary state of an induction step). `r at tick k` is the absolute-time atom
  (LANG-070). `crash(node, at)` and `hb(n1, t1, n2, t2)` are oracles.
- `at tick k on n { into r values …; }` are timestamped input facts (LANG-069); `on n { … }` gives per-node facts
  that hold at every tick. They may feed `static` relations and `input` interfaces, which protocol rules may not
  write.
- `pre` and `post` must have the same schema; a missing one is an error (CR-30).
- `invariant name: never q;` is a denial constraint: every row of `q` is a violation (LANG-200). Inside a module
  (not a spec), `invariant` is local and checked every tick; inside a spec it is global.
- `eventually post within k after eff;` is VER-001's bounded liveness.
- `failures { eot, eff, crashes, omissions, crash_recovery, nodes }` is the Fspec (TEST-020).
- `check ldfi;`, `check bmc { nodes: 3, ticks: 12, delay: 2, crashes: 1 };`,
  `check inductive { strengthen: [inv1, inv2] };` and `check simulate { seeds: 1000 };` select the verifier.
  `expect delivered: confluent;` asserts the ANA-029 certificate for an output.

Lowering: a spec is lowered like a module but over *global* relations: each protocol relation `r(x̄)` becomes
`r(N, x̄)` (the node column made explicit) evaluated at EOT, and `trace(r)` becomes `r_log(N, x̄, T)`.
`pre q;` lowers to `pre(ā) :- …` and `invariant n: never q;` to `violation("n", ā) :- …`. The TEST-022 oracle
compares `pre`/`post` between the failure-free run and each hypothesis.

### 3.18 Remaining statements

- **Facts.** `static peers(n: Node) = values ("a"), ("b");` holds at every tick (CR-16). `into r values (…);` as a
  statement is a fact rule with body `unit`; inside `bootstrap` it holds only at tick 0.
- **Violations** (LANG-200). `fail "truncating committed entry" from t in truncate where t.idx <= commit select t;`
  lowers to `violation(site, "truncating committed entry", ā) :- …`. The action (abort, alert, log) is deployment
  configuration.
- **`halt`, `localtick`, `stdio`** (LANG-046, 051, 052). `into halt select {process: true};`,
  `into next localtick select ();`, `from l in stdio`, `send stdio select {line: s}`.
- **`nondet "reason"`** (LANG-204) prefixes a rule; the reason is carried into certificates and interfaces.
- **`atomic { … }`** (LANG-206) marks rules whose outputs are released only after their state updates are visible.
- **Catalog** (LANG-202). `catalog.rules`, `catalog.deps`, `catalog.strata`, `catalog.schemas` are read-only
  relations: `from d in catalog.deps where d.nonmonotone select d`.

### 3.19 Coverage of FEATURES §2 (P0 and P1)

| LANG ids | Where |
|---|---|
| 001–005 | §3.14 |
| 006–010 | §3.14, §3.11 (009), §3.12 (010) |
| 020–028 | §3.3 |
| 040–053 | §3.2, §3.2.1, §3.18 (051, 052) |
| 060–068 | §3.5, §3.1 (066), §3.14 (068) |
| 069, 070 | §3.17 |
| 071 | §3.10 |
| 080–095 | §3.6, §3.7 (087), §3.11 (095) |
| 097, 098, 100–118 | §3.8 |
| 120–139, 142, 280–284 | §3.9 |
| 150–158 | §3.11, §3.9 (158) |
| 170–175 | §3.4 |
| 180–186 | §3.12 |
| 190 | §3.14 |
| 200–212 | §3.17, §3.18, §3.13 (207, 212), §3.14 (205) |
| 220 | not surface syntax: Molly `.ded` is a separate frontend onto the same IR |
| 240–244 | §3.15 |
| 260–265 | §3.16 |

---

## 4. Required example corpus

Each example is complete: every relation it reads is declared, and every rule is written out. Comments explain
the semantics where the syntax alone does not. A few examples also show the IR of their most interesting rules.

### E1. Key-value store node: put / get / delete with acks, durable table, upsert semantics

```
program kvstore version 1;

module KvNode {
  static writers(p: Principal);                                   // principals allowed to mutate

  // Requests come from external clients on the client listener (LANG-243). The runtime attaches
  // $session and $principal to every received row; they cannot be forged by the payload.
  channel kv_put(@server: Node, reqid: u64 => key: string, val: bytes)
    accept from external client where $principal in writers;
  channel kv_del(@server: Node, reqid: u64 => key: string)
    accept from external client where $principal in writers;
  channel kv_get(@server: Node, reqid: u64 => key: string)
    accept from external client;

  channel put_ack(@client: Session, reqid: u64 => ok: bool);
  channel del_ack(@client: Session, reqid: u64 => existed: bool);
  channel get_reply(@client: Session, reqid: u64 => key: string, val: Option<bytes>);

  // Durable: changes are in the WAL and fsynced before this tick's replies leave (SEM-072).
  durable table store(key: string #1 => val: bytes #2, writer: Principal #3, reqid: u64 #4);

  // PUT is an upsert at t+1. If two puts hit the same key in one tick, the higher reqid wins
  // (reqids are assigned by the client library from a per-deployment counter); without the
  // resolve! clause that would be the SEM-051 runtime error.
  rule put:
    upsert! next store resolve! choose_most(reqid)
      from p in kv_put
      select {key: p.key, val: p.val, writer: p.$principal, reqid: p.reqid};

  // The ack is derived in the same tick. It is released only after the tick's durable commit,
  // which includes the upsert staged for t+1, so an acked write survives a crash.
  rule ack_put:
    send put_ack from p in kv_put select {client: p.$session, reqid: p.reqid, ok: true};

  // DELETE removes the exact stored tuple at t+1. A put and a delete of the same key in the same
  // tick: both remove the old tuple, the put inserts the new one, and insert wins (CR-05), so the
  // put is ordered after the delete. That is a documented, deterministic outcome.
  rule del:
    delete! next store from d in kv_del join s in store on s.key == d.key select s;

  rule ack_del:
    send del_ack from d in kv_del left join! s in store on s.key == d.key
      select {client: d.$session, reqid: d.reqid, existed: s.is_some()};

  // GET reads the state at t, i.e. before this tick's puts and deletes (CR-04).
  rule get:
    send get_reply from g in kv_get left join! s in store on s.key == g.key
      select {client: g.$session, reqid: g.reqid, key: g.key, val: s?.val};
}
```

IR of `put` (with `$principal` and `$session` columns present because rules read them):

```
cand_store$(K, V, P, R) :- kv_put(R, K, V, _Sess, P).
best_store$(K, max<R>)  :- cand_store$(K, _, _, R).
store(K, V, P, R)@next  :- cand_store$(K, V, P, R), best_store$(K, R).
del_store(K, V0, P0, R0) :- cand_store$(K, _, _, _), store(K, V0, P0, R0).
persist[store]                                          // durable: WAL-logged
```

CALM report: `put`, `del`, `ack_del` and `get` are points of order (the bangs). The node is a single replica, so
nothing needs coordination; the report says so and lists the four sites.

### E2. Reliable delivery and reliable broadcast as reusable modules

```
protocol Delivery {
  input  pipe_in(dst: Node, src: Node, ident: u64 => payload: bytes);
  output pipe_sent(like pipe_in);                   // sender side: delivery is complete
  output pipe_out(like pipe_in);                    // receiver side: delivered
}

module BestEffortDelivery implements Delivery {
  channel pipe_chan(@dst: Node, src: Node, ident: u64 => payload: bytes);

  rule snd:  send pipe_chan from p in pipe_in select p;
  rule rcv:  into pipe_out from c in pipe_chan select c;
  rule done: into pipe_sent from p in pipe_in select p;        // "more like an effort"
}

module ReliableDelivery(retry: duration = 2s) implements Delivery {
  import BestEffortDelivery as bed: Delivery;

  table buf(like pipe_in);                                      // unacked messages
  channel ack(@src: Node, ident: u64);                          // acker = $sender, never a payload field
  periodic clock every retry;

  block remember {
    rule buffer:     into buf from p in pipe_in select p;
    rule first_send: into bed.pipe_in from p in pipe_in select p;
    rule resend:     into bed.pipe_in from b in buf from c in clock select b;
  }

  block rcv {
    rule deliver: into pipe_out from m in bed.pipe_out select m;
    rule ack_it:  send ack from m in bed.pipe_out select {src: m.src, ident: m.ident};
  }

  block done {
    let acked = from b in buf join a in ack on a.ident == b.ident and a.$sender == b.dst select b;
    rule report: into pipe_sent from a in acked select a;
    rule gc:     delete! next buf from a in acked select a;
  }
}

protocol Broadcast {
  input  bcast(ident: u64 => payload: bytes);
  output bcast_done(ident: u64 => payload: bytes);             // every other member acknowledged
  output deliver(src: Node, ident: u64 => payload: bytes);      // exactly once per (src, ident)
}

module ReliableBroadcast(members: static(n: Node), retry: duration = 2s) implements Broadcast {
  import ReliableDelivery(retry: retry) as rd: Delivery;

  table pending(ident: u64 => payload: bytes);
  table acked_by(ident: u64 => who: lset<Node>);
  table seen(src: Node, ident: u64);

  rule fanout:
    into rd.pipe_in from b in bcast from m in members where m.n != self
      select {dst: m.n, src: self, ident: b.ident, payload: b.payload};
  rule remember: into pending from b in bcast select b;
  rule collect:  into acked_by from s in rd.pipe_sent select {ident: s.ident, who: lset(s.dst)};

  // Done when every *other* member acked. `members` is a closed relation and `contains` is a
  // threshold, so this ∀ is monotone and carries no bang.
  let complete = from p in pending join a in acked_by on a.ident == p.ident
                 where all m in members: m.n == self or a.who.contains(m.n)
                 select p;
  rule finished: into bcast_done from c in complete select c;
  rule forget:   delete! next pending from c in complete select c;

  // Receiver: retransmissions make rd.pipe_out at-least-once. `seen` is written with `into next`
  // so that at tick t it holds only earlier deliveries; with a same-tick `into`, the anti-join
  // would always see the current message and nothing would ever be delivered.
  rule deliver_once:
    into deliver from m in rd.pipe_out where not! seen(src: m.src, ident: m.ident)
      select {src: m.src, ident: m.ident, payload: m.payload};
  rule mark: into next seen from m in rd.pipe_out select {src: m.src, ident: m.ident};
}
```

IR of `ReliableDelivery.resend` and `done` (instance prefix `rd.` omitted):

```
bed.pipe_in(D, S, I, P) :- buf(D, S, I, P), clock(_, _).
acked(D, S, I, P) :- buf(D, S, I, P), ack(I, Acker), Acker == D.
pipe_sent(D, S, I, P) :- acked(D, S, I, P).
del_buf(D, S, I, P) :- acked(D, S, I, P).
```

### E3. Raft leader election as rules

```
enum Role { Follower #1, Candidate #2, Leader #3, unknown }

protocol ElectionApi {
  input  last_log( => idx: u64, term: u64);                  // from the log component on this node
  output leader_of(term: u64 => leader: Node);
  output role_now( => role: Role, term: u64);
}

module RaftElection(election_min: duration = 150ms, election_max: duration = 300ms,
                    heartbeat: duration = 50ms) implements ElectionApi {
  static peers(n: Node);                                       // every server, including self

  // Durable Raft state (dissertation Fig. 3.1), fsynced before any reply leaves (SEM-072).
  durable lattice current_term: lmax<u64>;                     // terms only grow: a lattice
  durable table voted_for(term: u64 #1 => cand: Node #2);      // at most one vote per term (key)

  // Volatile state.
  table my_role( => role: Role, term: u64);
  table deadline( => at: timestamp);
  table votes(term: u64 => granted: lset<Node>);

  channel request_vote(@to: Node, term: u64, last_idx: u64, last_term: u64);  // candidate = $sender
  channel vote_reply(@to: Node, term: u64 => granted: bool);                  // voter = $sender
  channel append_entries(@to: Node, term: u64);                               // heartbeat; leader = $sender

  periodic tick_timer every 10ms;
  periodic hb_timer every heartbeat;

  bootstrap {
    into current_term select 0;                                // harmless merge after a restart
    into my_role select {role: Follower, term: 0};
    into deadline select {at: now() + rand_range(election_min, election_max, ("boot", 0))};
  }

  // ---------- term first (R07 §11.1, rule 2)
  scratch lattice seen: lmax<u64>;
  rule seen_cur: into seen select current_term;
  rule seen_rv:  into seen from m in request_vote select m.term;
  rule seen_vr:  into seen from m in vote_reply select m.term;
  rule seen_ae:  into seen from m in append_entries select m.term;
  rule keep:     into next current_term select seen;

  let eff = select {t: reveal!(seen)};                          // the effective term of this tick

  let heard_leader = from m in append_entries, e in eff where m.term == e.t
                     select {leader: m.$sender, term: m.term};

  // ---------- voter side
  let acceptable = from m in request_vote, e in eff, l in last_log
      where m.term == e.t
        and (m.last_term > l.term or (m.last_term == l.term and m.last_idx >= l.idx))
        and not! exists (v in voted_for where v.term == m.term and v.cand != m.$sender)
      select {term: m.term, cand: m.$sender};

  // At most one grant per term per tick: the least candidate id among acceptable requests.
  let grant = from a in acceptable choose! per (a.term) least a.cand select a;

  rule record_vote: into next voted_for from g in grant select g;
  rule yes:         send vote_reply to g.cand from g in grant select {term: g.term, granted: true};
  rule no:          send vote_reply to m.$sender from m in request_vote, e in eff
                      where not! grant(cand: m.$sender) select {term: e.t, granted: false};

  // ---------- role transitions; the three views are mutually exclusive by construction
  let step_down = from e in eff, r in my_role
      where e.t > r.term
         or (r.role == Candidate and exists (h in heard_leader where h.term == r.term))
      select {term: e.t};

  let won = from r in my_role join v in votes on v.term == r.term
      where r.role == Candidate and majority(v.granted, of: peers)
        and not! exists (s in step_down)
      select {term: r.term};

  let start = from t in tick_timer, d in deadline, r in my_role, e in eff
      where now() >= d.at and r.role != Leader
        and not! exists (s in step_down) and not! exists (w in won)
        and not! exists (h in heard_leader) and not! exists (g in grant)
      select {term: e.t + 1};

  rule to_follower:  upsert! next my_role from s in step_down select {role: Follower, term: s.term};
  rule to_leader:    upsert! next my_role from w in won select {role: Leader, term: w.term};
  rule to_candidate: upsert! next my_role from s in start select {role: Candidate, term: s.term};
  rule bump_term:    into next current_term from s in start select s.term;
  rule vote_self:    into next voted_for from s in start select {term: s.term, cand: self};
  rule count_self:   into next votes from s in start select {term: s.term, granted: lset(self)};

  rule ask:
    send request_vote from s in start, p in peers, l in last_log where p.n != self
      select {to: p.n, term: s.term, last_idx: l.idx, last_term: l.term};

  // ---------- candidate side: count votes (monotone lattice fold)
  rule tally:
    into votes from v in vote_reply, r in my_role
      where v.granted and v.term == r.term and r.role == Candidate
      select {term: v.term, granted: lset(v.$sender)};

  // ---------- election timer: re-arm on starting an election, granting a vote, or hearing a
  // leader of the current term. `start` excludes the other two, and they share the term e.t,
  // so all rows of `rearm` produce the same deadline and the upsert never conflicts.
  let rearm = from s in start select {term: s.term}
        union from g in grant select {term: g.term}
        union from h in heard_leader select {term: h.term};
  rule rearm_timer:
    upsert! next deadline from r in rearm
      select {at: now() + rand_range(election_min, election_max, ("rearm", r.term))};

  // ---------- leader: heartbeats, immediately on winning and then periodically
  rule announce:
    send append_entries from w in won, p in peers where p.n != self select {to: p.n, term: w.term};
  rule heartbeat:
    send append_entries from h in hb_timer, r in my_role, p in peers
      where r.role == Leader and p.n != self select {to: p.n, term: r.term};

  // ---------- outputs. Two different leaders for one term in one tick would be a SEM-050 key
  // error on leader_of, i.e. the safety violation is also caught at runtime.
  rule out_role:     into role_now from r in my_role select r;
  rule out_self:     into leader_of from r in my_role where r.role == Leader select {term: r.term, leader: self};
  rule out_heard:    into leader_of from h in heard_leader select {term: h.term, leader: h.leader};
}
```

IR of `grant`, `won` and `start` (the rest follows §3.1 mechanically):

```
eff(T) :- seen(; S), T := reveal(S).
voted_other$(T, C) :- voted_for(T, C2), request_vote(T, _, _, C), C2 != C.
acceptable(T, C) :- request_vote(T, LI, LT, C), eff(T), last_log(MI, MT), LT > MT, notin voted_other$(T, C).
acceptable(T, C) :- request_vote(T, LI, LT, C), eff(T), last_log(MI, MT), LT == MT, LI >= MI,
                    notin voted_other$(T, C).
cand$(T, C) :- acceptable(T, C).
pmin$(T, min<C>) :- cand$(T, C).
grant(T, C) :- cand$(T, C), pmin$(T, C).

peers_n$(count<N>) :- peers(N).
sd$() :- step_down(_).
won(T) :- my_role(Candidate, T), votes(T; V), peers_n$(C), lset_size(V) >= C / 2 + 1, notin sd$().

won$() :- won(_).   hl$() :- heard_leader(_, _).   gr$() :- grant(_, _).     // exists (…) with no outer vars
start(T1) :- tick_timer(_, _), deadline(D), my_role(R, _), eff(T), now(N), N >= D, R != Leader,
             notin sd$(), notin won$(), notin hl$(), notin gr$(), T1 := T + 1.
my_role(Candidate, T)@next :- start(T).
del_my_role(R0, T0) :- start(_), my_role(R0, T0).
```

Strata: `seen` → `eff` → {`heard_leader`, `acceptable`} → `grant` → `step_down` → `won` → `start`. No negation is on
a same-tick cycle, so the program is accepted (SEM-020). Every non-monotone site is local to one node, which is the
Raft interlock ANA-083 must protect.

### E4. Two-phase commit: coordinator and participants in one choreography, with timeout abort

```
enum Outcome { Commit #1, Abort #2, unknown }

choreography TwoPhaseCommit(vote_timeout: duration = 5s, retry: duration = 500ms) {
  role coordinator: process;
  role participant: cluster;

  channel prepare(@to: Node@participant, xid: u64);
  channel vote(@to: Node@coordinator, xid: u64 => yes: bool);                // voter = $sender
  channel decision(@to: Node@participant, xid: u64 => outcome: Outcome);
  channel decision_ack(@to: Node@coordinator, xid: u64);                     // participant = $sender

  at coordinator {
    input  begin(xid: u64);
    output decided(xid: u64 => outcome: Outcome);

    durable table xact(xid: u64 #1 => started: timestamp #2);
    durable table outcome_log(xid: u64 #1 => outcome: Outcome #2);
    table yes_votes(xid: u64 => voters: lset<Node@participant>);
    table no_vote(xid: u64);
    table acks(xid: u64 => ackers: lset<Node@participant>);
    periodic retry_timer every retry;

    // Log first, act next tick (LIB-041): prepares go out from the durably logged row.
    rule log_begin:
      into next xact from b in begin where not! xact(xid: b.xid) select {xid: b.xid, started: now()};

    let open = from x in xact where not! outcome_log(xid: x.xid) select x;

    rule ask_first:
      send prepare from x in xact.inserted!, p in members(participant) select {to: p, xid: x.xid};
    rule ask_again:
      send prepare from t in retry_timer, o in open, p in members(participant)
        where not! yes_votes[o.xid].contains(p) and not! no_vote(xid: o.xid)
        select {to: p, xid: o.xid};

    rule tally_yes: into yes_votes from v in vote where v.yes select {xid: v.xid, voters: lset(v.$sender)};
    rule tally_no:  into no_vote from v in vote where not v.yes select {xid: v.xid};

    // Commit iff every participant voted yes: ∀ over the static cluster membership of a
    // monotone predicate, hence monotone (no bang).
    let all_yes = from o in open join y in yes_votes on y.xid == o.xid
                  where all p in members(participant): y.voters.contains(p)
                  select {xid: o.xid};

    // Abort on any "no", or on timeout. Commit takes priority in the tick where both would hold.
    let must_abort = from o in open
                     where no_vote(xid: o.xid) or now() - o.started > vote_timeout
                     where not! all_yes(xid: o.xid)
                     select {xid: o.xid};

    rule commit: into next outcome_log from a in all_yes select {xid: a.xid, outcome: Commit};
    rule abort:  into next outcome_log from a in must_abort select {xid: a.xid, outcome: Abort};

    rule tell_first:
      send decision from o in outcome_log.inserted!, p in members(participant)
        select {to: p, xid: o.xid, outcome: o.outcome};
    rule tell_again:
      send decision from t in retry_timer, o in outcome_log, p in members(participant)
        where not! acks[o.xid].contains(p)
        select {to: p, xid: o.xid, outcome: o.outcome};

    rule collect_acks: into acks from a in decision_ack select {xid: a.xid, ackers: lset(a.$sender)};
    rule report:       into decided from o in outcome_log.inserted! select o;

    // Forget the transaction once everyone acknowledged; outcome_log is kept (presumed abort).
    rule forget:
      delete! next xact from x in xact join a in acks on a.xid == x.xid
        where outcome_log(xid: x.xid) and all p in members(participant): a.ackers.contains(p)
        select x;
  }

  at participant {
    input  can_commit(xid: u64 => ok: bool);            // the local resource manager's verdict
    output vote_requested(xid: u64);                    // asks the local resource manager
    output applied(xid: u64 => outcome: Outcome);

    table asked(xid: u64 => coord: Node@coordinator);
    durable table my_vote(xid: u64 #1 => coord: Node@coordinator #2, yes: bool #3);
    durable table done(xid: u64 #1 => outcome: Outcome #2);

    rule remember: into asked from p in prepare select {xid: p.xid, coord: p.$sender};
    rule ask_rm:   into vote_requested from p in prepare where not! my_vote(xid: p.xid) select {xid: p.xid};

    // Log the vote; it is sent from the logged row, so it is durable before it leaves.
    rule log_vote:
      into next my_vote from c in can_commit join a in asked on a.xid == c.xid
        where not! my_vote(xid: c.xid)
        select {xid: c.xid, coord: a.coord, yes: c.ok};
    rule vote_first:
      send vote from v in my_vote.inserted! select {to: v.coord, xid: v.xid, yes: v.yes};
    rule vote_again:                                     // answer re-sent prepares with the logged vote
      send vote from v in my_vote join p in prepare on p.xid == v.xid
        select {to: v.coord, xid: v.xid, yes: v.yes};

    // A "no" voter may abort unilaterally (presumed abort); the coordinator will agree.
    rule self_abort:
      into next done from v in my_vote.inserted! where not v.yes select {xid: v.xid, outcome: Abort};
    rule learn:
      into next done from d in decision where not! done(xid: d.xid) select {xid: d.xid, outcome: d.outcome};
    rule ack: send decision_ack to d.$sender from d in decision select {xid: d.xid};
    rule apply: into applied from d in done.inserted! select d;

    fail "commit decided although this participant voted no"
      from d in decision join v in my_vote on v.xid == d.xid
      where not v.yes and d.outcome == Commit select d;
  }
}
```

Projection gives two programs. The inferred ACLs are `prepare`, `decision`: {coordinator}; `vote`,
`decision_ack`: {participant}. `$sender` on `prepare` has type `Node@coordinator`, which is why `asked.coord`
type-checks as a reply address.

### E5. Lattices: vector clocks, a Bloom^L shopping cart, a quorum threshold, a user lattice

**Vector clocks** (`vclock` = `lmap<Node, lmax<u64>>`, LANG-130).

```
module CausalEvents {
  static peers(n: Node);
  input  local_event(id: u64 => note: string);                  // ids are globally unique (host-assigned)
  output happened_before(a: u64, b: u64);
  output concurrent_with(a: u64, b: u64);

  channel gossip(@to: Node, id: u64 => note: string, vc: vclock);
  durable lattice my_vc: vclock;
  table events(id: u64 => origin: Node, note: string, vc: vclock);

  // Stamp this tick's local events in canonical id order. `my_vc.at(self)` is a morphism into lmax,
  // and adding a scalar shifts an lmax monotonically, so the stamp is monotone in my_vc.
  let stamped = from e in local_event enumerate! as i by e.id
                select {id: e.id, note: e.note,
                        vc: merge(my_vc, vclock_single(self, my_vc.at(self) + 1 + i))};

  rule advance:       into next my_vc from s in stamped select s.vc;
  rule absorb:        into next my_vc from g in gossip select g.vc;
  rule record_local:  into events from s in stamped select {id: s.id, origin: self, note: s.note, vc: s.vc};
  rule record_remote: into events from g in gossip select {id: g.id, origin: g.$sender, note: g.note, vc: g.vc};
  rule spread:
    send gossip from s in stamped, p in peers where p.n != self
      select {to: p.n, id: s.id, note: s.note, vc: s.vc};

  // happens_before! and concurrent! compare two clocks exactly: non-monotone, hence the bangs.
  rule hb: into happened_before from a in events, b in events
             where a.vc.happens_before!(b.vc) select {a: a.id, b: b.id};
  rule cc: into concurrent_with from a in events, b in events
             where a.id < b.id and a.vc.concurrent!(b.vc) select {a: a.id, b: b.id};
}
```

**A user-defined lattice** (verified-constructor form), and the monotone cart replica of Bloom^L SoCC §6.

```
type CartOp   = {item: string, delta: i64};
type Checkout = {lbound: u64, ubound: u64, reply_to: Node};

// ops: op-id -> op. `point` makes a second, different op under the same id a conflict (⊤), which is
// Bloom^L lcart's "same id, different value raises". checkout: at most one checkout per session.
lattice type Cart = record { ops: lmap<u64, point<CartOp>>, checkout: point<Checkout> } {
  // Complete once the checkout is known and every op id in [lbound, ubound] is present.
  // `get()` on a point is a guarded read: defined only where `is_set()` holds, and constant above it.
  monotone fn is_complete(self) -> lbool =
    self.checkout.is_set()
      and self.ops.covers(self.checkout.get().lbound, self.checkout.get().ubound);

  // The summary reads the raw ops: exact, so it is a plain method and callers write `summary!()`.
  plain fn! summary(self) -> map<string, i64> =
    map_sum([ (op.item, op.delta) for op in reveal!(self.ops).values() ]);
}

module CartReplica {
  channel action(@server: Node, session: u64, op_id: u64 => item: string, delta: i64);
  channel checkout(@server: Node, session: u64 => op_id: u64, lbound: u64, reply_to: Node);
  channel receipt(@to: Node, session: u64 => items: map<string, i64>);

  table carts(session: u64 => cart: Cart);                     // one Cart cell per session
  table answered(session: u64);

  rule add:
    into carts from a in action
      select {session: a.session,
              cart: Cart{ops: lmap_single(a.op_id, point(CartOp{item: a.item, delta: a.delta}))}};
  rule close:
    into carts from c in checkout
      select {session: c.session,
              cart: Cart{checkout: point(Checkout{lbound: c.lbound, ubound: c.op_id - 1, reply_to: c.reply_to})}};

  // The exact read `summary!()` is gated by the threshold `is_complete()`: once complete, the cart can no
  // longer change without becoming ⊤, so the analyzer reports "confluent, not certified" (ANA-143).
  rule reply:
    send receipt from c in carts
      where c.cart.is_complete() and not! answered(session: c.session)
      select {to: c.cart.checkout.get().reply_to, session: c.session, items: c.cart.summary!()};
  rule once: into next answered from c in carts where c.cart.is_complete() select {session: c.session};
}
```

Omitted fields of a lattice record constructor are ⊥, so `Cart{ops: …}` and `Cart{checkout: …}` merge into one
cell per session under `carts`' key.

**Quorum threshold with `size(lset) >= k`** (Bloom^L QuorumVoteL).

```
module QuorumVote(quorum: u64 = 5, result_addr: Node) {
  channel vote_chn(@addr: Node);                               // voter = $sender
  channel result_chn(@addr: Node);
  lattice votes: lset<Node>;

  rule collect: into votes from v in vote_chn select v.$sender;
  rule decide:  send result_chn to result_addr where votes.size >= quorum select {};
}
```

```
votes(; S) :- vote_chn(Sender), S := lset_single(Sender).
result_chn(D)@async :- votes(; S), Sz := lset_size(S), param_quorum(Q), Sz >= Q, param_result_addr(D).
```

Every occurrence is monotone, the async head is consumed only by a threshold, and ANA-141 certifies the module
confluent. Like Bloom^L, the persistent lattice re-sends the result every tick; ANA-061 (ARM) may suppress
duplicates because the receiver is idempotent (DIST-007).

### E6. Word count: hash-partitioned shuffle, reducers aggregate, seals mark the end of input

```
choreography WordCount(resend: duration = 1s) {
  role mapper: cluster;
  role reducer: cluster;

  // Partitioned channel: `send shuffle` omits the address; the compiler routes by hash (LANG-154).
  channel shuffle(@to: Node@reducer, word: string => n: u64)
    partition by hash64(bytes_of(word)) over members(reducer);
  channel shuffle_done(@to: Node@reducer => digest: u64);     // one punctuation per (mapper, reducer)
  channel shuffle_ack(@to: Node@mapper);                      // reducer = $sender

  at mapper {
    static split(line: u64 => text: string);                  // this mapper's input: closed
    table acked(reducer: Node@reducer);
    periodic resend_timer every resend;
    once go;

    // Map and combine. `split` is static, so this non-monotone aggregate reads a closed input.
    // `pos` is bound so that two occurrences of a word on one line are two valuations.
    let partial = from l in split
                  unnest (pos, w) in enumerate(split_ws(l.text))
                  let word = lower(w)
                  group! by word
                  select {word, n: count()};

    let fire = from g in go select {} union from t in resend_timer select {};

    // Send partial counts until the owning reducer confirms the whole partition.
    rule ship:
      send shuffle from f in fire, p in partial
        where not! acked(reducer: owner(shuffle, p.word))
        select {word: p.word, n: p.n};

    // Punctuation with digest: how many distinct words this mapper sends to reducer r (0 is valid).
    rule punctuate:
      send shuffle_done from f in fire, r in members(reducer)
        where not! acked(reducer: r)
        let k = count!(p in partial where owner(shuffle, p.word) == r)
        select {to: r, digest: k};

    rule confirmed: into acked from a in shuffle_ack select {reducer: a.$sender};
  }

  at reducer {
    table got(mapper: Node@mapper, word: string => n: u64);    // keyed by producer: resends are idempotent
    output final word_count(word: string => n: u64);

    rule receive: into got from s in shuffle select {mapper: s.$sender, word: s.word, n: s.n};

    // The seal takes effect only when `got` holds exactly `digest` words from that mapper,
    // so a punctuation that overtakes its data is harmless.
    seal got on (mapper) counted
      from d in shuffle_done select {mapper: d.$sender, count: d.digest};

    rule confirm:
      send shuffle_ack from m in members(mapper) where is_sealed(got, mapper: m) select {to: m};

    // Reduce once every mapper has sealed. The guard makes this group! a sealed exact read (ANA-142),
    // and ANA-120 classifies word_count as SEALED-final, which `output final` requires.
    rule reduce:
      into word_count from g in got
        where all m in members(mapper): is_sealed(got, mapper: m)
        group! by g.word
        select {word: g.word, n: sum(g.n)};
  }
}
```

IR of the reducer's `reduce` (the seal expansion is the one in §3.13):

```
miss$(M) :- members_mapper(M), notin sealed_got(M).
all$() :- unit, notin miss$(_).
reduce_vars$(W, M, N) :- got(M, W, N), all$().
word_count(W, sum<N>) :- reduce_vars$(W, M, N).        // sum over distinct (M, N) per W: one row per mapper
```

`sum` sums over distinct valuations of the in-scope binders, and `got`'s key includes the mapper, so each mapper's
partial count is added exactly once even if two mappers report the same count for a word.

### E7. Single-node analytics: closure, shortest paths, stratified negation

```
module GraphAnalytics(max_hops: u64 = 64) {
  static vertex(v: string);
  static edge(src: string, dst: string => w: u64);

  output final reach(src: string, dst: string);
  output final shortest(src: string, dst: string => cost: u64);
  output final shortest_l(src: string, dst: string => cost: lmin<u64>);
  output final unreachable(src: string, dst: string);

  // 1. Transitive closure: monotone recursion.
  let tc = from e in edge select {src: e.src, dst: e.dst}
     union from t in tc join e in edge on e.src == t.dst select {src: t.src, dst: e.dst};
  rule r_reach: into reach from t in tc select t;

  // 2a. Shortest paths with min aggregation. Path costs are enumerated up to a hop bound so that the
  //     relation is finite on cyclic graphs; the min is stratified above the recursion.
  let path = from e in edge select {src: e.src, dst: e.dst, cost: e.w, hops: 1u64}
       union from p in path join e in edge on e.src == p.dst where p.hops < max_hops
             select {src: p.src, dst: e.dst, cost: p.cost + e.w, hops: p.hops + 1};
  rule r_short:
    into shortest from p in path group! by (p.src, p.dst)
      select {src: p.src, dst: p.dst, cost: min(p.cost)};

  // 2b. The same with an lmin lattice: recursion through a monotone shift, no bound, no stratum
  //     boundary (Bloom^L ShortestPathsL). Terminates for non-negative weights (ascending chains in
  //     lmin are finite over u64).
  table dist(src: string, dst: string => d: lmin<u64>);
  rule d_base: into dist from e in edge select {src: e.src, dst: e.dst, d: lmin(e.w)};
  rule d_step: into dist from x in dist join e in edge on e.src == x.dst
                 select {src: x.src, dst: e.dst, d: x.d + e.w};
  rule r_short_l: into shortest_l from x in dist select {src: x.src, dst: x.dst, cost: x.d};

  // 3. Stratified negation: pairs of distinct vertices with no path.
  rule r_unreach:
    into unreachable from a in vertex, b in vertex
      where a.v != b.v and not! tc(src: a.v, dst: b.v)
      select {src: a.v, dst: b.v};
}
```

```
tc(S, D) :- edge(S, D, _).
tc(S, D) :- tc(S, M), edge(M, D, _).
path(S, D, C, 1) :- edge(S, D, C).
path(S, D, C2, H2) :- path(S, M, C, H), edge(M, D, W), param_max_hops(X), H < X, C2 := C + W, H2 := H + 1.
short_vars$(S, D, C, H) :- path(S, D, C, H).
shortest(S, D, min<C>) :- short_vars$(S, D, C, H).
dist(S, D; L) :- edge(S, D, W), L := lmin(W).
dist(S, D; L2) :- dist(S, M; L), edge(M, D, W), L2 := lmin_add(L, W).
unreachable(A, B) :- vertex(A), vertex(B), A != B, notin tc(A, B).
```

Strata: {`tc`, `path`, `dist`} → {`shortest`, `unreachable`}. All inputs are `static` (closed), so every output is
final at tick 0 (POS-FINAL for `reach`/`shortest_l`, SEALED for `shortest`/`unreachable`).

### E8. Soft-state heartbeat failure detector

```
module HeartbeatFD(period: duration = 1s, expire: duration = 4s, grace: duration = 5s) {
  static peers(n: Node);
  output alive(peer: Node => last_heard: timestamp);
  output suspected(peer: Node);

  channel heartbeat(@to: Node => sent_at: timestamp);           // sender identity = $sender
  periodic hb_timer every period;
  table started( => at: timestamp);

  // TTL table: a tuple lives while now() - $birth < expire; re-deriving it refreshes $birth.
  soft table heard(peer: Node) ttl expire max 4096;

  bootstrap { into started select {at: now()}; }

  rule beat:
    send heartbeat from t in hb_timer, p in peers where p.n != self select {to: p.n, sent_at: t.at};

  rule note:
    into heard from h in heartbeat where peers(n: h.$sender) select {peer: h.$sender};

  rule up:
    into alive from h in heard select {peer: h.peer, last_heard: reveal!(h.$birth)};

  // Suspect a peer we have not heard from within the TTL, after an initial grace period.
  rule down:
    into suspected from p in peers, s in started
      where p.n != self and now() - s.at > grace and not! heard(peer: p.n)
      select {peer: p.n};
}
```

```
heard(P; B) :- heartbeat(_, Sender), peers(Sender), P := Sender, now(N), B := lmax(N).
heard(P; B)@next :- heard(P; B), notin del_heard(P), now(N), N - reveal(B) < param_expire.
alive(P, T) :- heard(P; B), T := reveal(B).
suspected(P) :- peers(P), started(S), now(N), P != self, N - S > param_grace, notin heard(P; _).
```

(`max 4096` adds the eviction rules of §3.2.1.) `suspected` is NEVER-FINAL (a heard peer can expire and a
suspected peer can be heard again), which the finality report states. That is the right answer for a failure
detector.

### E9. Two instances of E2's broadcast, with interposition on one interface

`ClusterBus` runs a fast control plane and a slower data plane over two independent instances of
`ReliableBroadcast`. It interposes on the data plane's `deliver` output to drop messages from muted senders, record
what it dropped, and meter traffic per sender with a monotone lattice.

```
module ClusterBus {
  static cluster(n: Node);
  table muted(src: Node);                                      // maintained by the operator
  input mute(src: Node);
  input unmute(src: Node);

  input  send_data(ident: u64 => payload: bytes);
  input  send_ctrl(ident: u64 => payload: bytes);
  output data_in(src: Node, ident: u64 => payload: bytes);
  output ctrl_in(src: Node, ident: u64 => payload: bytes);
  output data_meter(src: Node => delivered: u64);

  // The same module imported twice: two disjoint instances with their own buffers, timers and channels.
  import ReliableBroadcast(members: cluster, retry: 200ms) as ctrl: Broadcast;
  import ReliableBroadcast(members: cluster, retry: 2s)    as data: Broadcast;

  table dropped(src: Node, ident: u64);
  table meter(src: Node => idents: lset<u64>);

  // Interposition: data's own rules now write `raw_data`; these rules decide what `data.deliver` is.
  interpose data.deliver as raw_data {
    rule pass:  into data.deliver from m in raw_data where not! muted(src: m.src) select m;
    rule drop:  into dropped from m in raw_data where muted(src: m.src) select {src: m.src, ident: m.ident};
    rule count: into meter from m in raw_data where not! muted(src: m.src)
                  select {src: m.src, idents: lset(m.ident)};
  }

  rule do_mute:   into next muted from m in mute select m;
  rule do_unmute: delete! next muted from u in unmute join m in muted on m.src == u.src select m;

  rule data_out: into data.bcast from s in send_data select s;
  rule ctrl_out: into ctrl.bcast from s in send_ctrl select s;
  rule data_up:  into data_in from m in data.deliver select m;
  rule ctrl_up:  into ctrl_in from m in ctrl.deliver select m;
  rule metering: into data_meter from x in meter select {src: x.src, delivered: reveal!(x.idents.size)};
}
```

Lowering of the interposition (instance prefix shown, only the renamed rule of `ReliableBroadcast` listed):

```
// in the copy of ReliableBroadcast for alias `data`, rule deliver_once originally had head data.deliver:
raw_data(S, I, P) :- data.rd.pipe_out(_, S, I, P), notin data.seen(S, I).
// interposer's rules:
data.deliver(S, I, P) :- raw_data(S, I, P), notin muted(S).
dropped(S, I)         :- raw_data(S, I, _), muted(S).
meter(S; L)           :- raw_data(S, I, _), notin muted(S), L := lset_single(I).
```

The `ctrl` instance is untouched: its `ctrl.deliver` rules are the originals. `data.seen` still records a dropped
message as seen, so a muted sender's retransmissions are not delivered after an unmute. That is intentional here,
and it is visible because interposition works at the interface, not inside the instance.

### E10. LDFI and verification specs

**Simple broadcast** (Molly `simplog.ded` + `deliv_assert.ded`) and its spec.

```
module SimpleBroadcast {
  static neighbors(n: Node);
  input  bcast(payload: string);
  table  log(payload: string);
  channel log_msg(@to: Node, payload: string);

  rule local: into log from b in bcast select b;
  rule fwd:   send log_msg from b in bcast, n in neighbors select {to: n.n, payload: b.payload};
  rule recv:  into log from m in log_msg select {payload: m.payload};
}

spec SimpleBroadcastDelivers for SimpleBroadcast {
  nodes a, b, c;
  failures { eot: 4, eff: 2, crashes: 1 }

  on a { into neighbors values ("b"), ("c"); }
  on b { into neighbors values ("a"), ("c"); }
  on c { into neighbors values ("a"), ("b"); }
  at tick 1 on a { into bcast values ("hello"); }

  // Someone has the payload, but a neighbor does not.
  let missing_log = from l in log, n in neighbors
      where n.$node == l.$node
        and not! exists (m in log where m.$node == n.n and m.payload == l.payload)
      select {node: n.n, payload: l.payload};

  // pre: a node that logged a payload it did not originate, and did not crash.
  pre  from l in log
       where not! exists (b in bcast at tick 1 where b.$node == l.$node and b.payload == l.payload)
         and not! crash(node: l.$node)
       select {node: l.$node, payload: l.payload};

  // post: nobody is missing the payload.
  post from l in log
       where not! exists (m in missing_log where m.payload == l.payload)
       select {node: l.$node, payload: l.payload};

  eventually post within 2 after eff;

  check ldfi;                      // expected: counterexample (one omission a→b at tick 1), as in Molly
  expect log: confluent;           // ANA-025: monotone, no negation in protocol rules
}
```

Lowered spec (global relations get an explicit node column; `crash` and absolute time are oracles):

```
missing_log(N2, P) :- log(N1, P), neighbors(N1, N2), notin log(N2, P).
pre(N, P)  :- log(N, P), notin bcast_at(N, P, 1), notin crash(N, _).
post(N, P) :- log(N, P), notin missing_log(_, P).
```

The failure spec is ⟨EOT = 4, EFF = 2, crashes ≤ 1⟩: omissions only at send ticks 1 ≤ t < 2 (CR-21), at most one
crash (TEST-027). LDFI runs the failure-free execution, builds lineage for every `post` row, and searches for
minimal fault sets that falsify `post` while `pre` still holds (TEST-022..029).

**Raft election safety** for E3: at most one leader per term, over the whole trace.

```
spec ElectionSafety for RaftElection(election_min: 150ms, election_max: 300ms, heartbeat: 50ms) {
  nodes s1, s2, s3;
  failures { eot: 40, eff: 30, crashes: 1, crash_recovery: true }

  on s1 { into peers values ("s1"), ("s2"), ("s3"); into last_log values (0, 0); }
  on s2 { into peers values ("s1"), ("s2"), ("s3"); into last_log values (0, 0); }
  on s3 { into peers values ("s1"), ("s2"), ("s3"); into last_log values (0, 0); }

  // The property: two different nodes are never Leader for the same term, at any ticks.
  invariant one_leader_per_term: never
    from a in trace(my_role), b in trace(my_role)
    where a.role == Leader and b.role == Leader and a.term == b.term and a.$node != b.$node
    select {term: a.term, first: a.$node, second: b.$node};

  // Auxiliary invariants that make the property inductive for `check inductive`.
  invariant one_vote_per_term: never
    from a in trace(voted_for), b in trace(voted_for)
    where a.$node == b.$node and a.term == b.term and a.cand != b.cand
    select {node: a.$node, term: a.term};

  invariant votes_are_real: never
    from v in votes unnest x in v.granted.items()
    where not! exists (w in voted_for where w.$node == x and w.term == v.term and w.cand == v.$node)
    select {candidate: v.$node, voter: x, term: v.term};

  invariant leader_had_quorum: never
    from r in my_role join v in votes on v.$node == r.$node and v.term == r.term
    where r.role == Leader and not! majority(v.granted, of: peers)
    select {node: r.$node, term: r.term};

  check simulate { seeds: 10000, max_delay: 5 };
  check bmc { nodes: 3, ticks: 16, delay: 3, crashes: 1 };
  check inductive { strengthen: [one_vote_per_term, votes_are_real, leader_had_quorum] };
}
```

```
violation("one_leader_per_term", T, N1, N2) :-
    my_role_log(N1, Leader, T, _T1), my_role_log(N2, Leader, T, _T2), N1 != N2.
```

For `check inductive`, `majority(…, of: peers)` becomes the quorum sort with the intersection axiom (VER-008), and
the four invariants form the standard Raft election-safety argument: a leader had a quorum of real votes, each node
votes once per term, and two quorums intersect.

---

## 5. Self-critique

### 5.1 Weaknesses

1. **Bang density in protocol code.** E3 has about twenty bangs. That is accurate, because Raft is locally
   non-monotone almost everywhere, but it means the bang loses its signal inside coordination code. The mitigation
   is that the analyzer groups bangs by component, and that `trusted module` (LANG-205) can silence the warnings,
   though not the bangs. A more radical option, not taken: allow a `coordinated { … }` block inside which bangs are
   implicit. It was rejected because it would reintroduce the invisible points of order this angle exists to remove.
2. **Two ways to aggregate.** `group! by … select {count()}` and `count!(v in r where …)` overlap. They have
   different empty-group semantics (CR-08 vs LANG-106), which is the reason both exist, but a data engineer will
   write the wrong one about as often as SQL users confuse `WHERE` and `HAVING`. The formatter cannot rewrite one
   into the other because they mean different things.
3. **Set semantics under SQL-looking syntax.** `select` always deduplicates, `count()` counts distinct valuations,
   and `sum` sums over distinct valuations. Anyone who reads `from … select` as SQL expects bags. Section 3.8
   defines these rules, but the surprise is real. `bag table` / `zset table` (LANG-138) cover the cases that truly
   need multiplicity.
4. **Target-first sinks versus data-flow order.** The sink comes first and the query then reads top to bottom, so
   the eye jumps from the first line to the `select` to learn what is written. PRQL-style "sink last" reads more
   naturally as a pipeline, but then the rule kind would be the last thing on the line, which defeats principle 1
   and makes error recovery worse.
5. **`choose!`'s chosen columns are inferred from liveness.** `choose! per (x̄)` chooses the variables that are
   live after the clause. Adding an unrelated field to `select` changes Ȳ and therefore the seeded priority, which
   can change which candidate wins. This is deterministic and replayable, but it is spooky action at a distance. An
   explicit `choose! per (x̄) of (ȳ)` form would be more robust. It is a candidate amendment, and the compiler should
   at least print Ȳ in the site report.
6. **Correlated subqueries and `all` hide negation.** `count!(…) default 0`, `exists`, and `all x in r: p` all lower
   to `notin` over auxiliary relations. The bang rules keep them visible where they are non-monotone, but a reader
   who expects `all` to be free may be surprised when `all!` is demanded over an open relation.
7. **`$`-fields are a second namespace.** `$sender`, `$principal`, `$session`, `$birth`, `$node` and `$tick` are
   easy to read but are not real columns: they exist only when read (SEM-091), `$birth` is a lattice, and `$node`
   and `$tick` exist only in specs. The type checker has to explain all of that in errors.
8. **Upsert and resolution are written at the sink, not the relation.** `upsert! next kv resolve! …` puts the
   conflict policy on each rule, while `table … resolve! …` puts it on the relation. Both exist (LANG-117 needs the
   relation form, SEM-051 the sink form), which again gives two places to look.
9. **Relation parameters are an addition.** `module M(members: static(n: Node))` is not in FEATURES.md. It is small
   and lowers by renaming, but it widens the module system and needs a design review with the other proposals.
10. **Spec blocks reuse query syntax with different scoping.** Inside `spec`, relations are global and plain `r`
    means different things in `pre`/`post` (EOT) and in `invariant` (every visited state). The same text therefore
    has two meanings depending on the enclosing item.

### 5.2 Ambiguities and open edges in the grammar

1. **`#` comments versus field numbers.** `#1` is a field number and `# 1` is a comment. LANG-208 asks for `#`
   comments, and this is the least bad lexical rule, but `#TODO` works while `#1TODO` does not. A formatter
   rewrites `#` comments to `//`.
2. **`name(...)` is a function call, a relation atom or an enum variant.** The parser builds one call node and
   name resolution decides. That keeps the grammar LL(1) but moves some "syntax" errors into the resolver, and a
   relation shadowing a function is a hard error to keep it unambiguous.
3. **`in` has three roles**: binder introduction (`from p in r`), membership (`e in r`), and ring intervals
   (`x in (a, b]`). They are syntactically separated (after a pattern in a clause; at comparison level; with an
   interval literal), but a reader has to know which is which.
4. **Where `where` may appear.** Clauses may be written in any order, and `where` before `from` is legal (it filters
   the unit row). That keeps the grammar simple but allows odd-looking queries, which the formatter reorders.
5. **The LL(2) spots** (§2.3) are harmless for a hand-written recursive-descent parser, but they rule out a pure
   LL(1) table-driven parser.
6. **`fold!` has two leading arguments that other correlated aggregates do not.** The parser special-cases the
   name, which is a small wart.
7. **Implicit singleton wrapping into lattices** (`into votes select v.$sender`) is convenient but type-directed. It
   is restricted to six lattice types; everything else needs a constructor.
8. **`point.get()` in user lattice methods** is a guarded partial read (E5). Its monotonicity claim holds only above
   the `is_set()` threshold, which the law harness can test but the type system does not express.

### 5.3 What this angle does well, for comparison with the other proposals

- The rule kind, and whether a rule is monotone, are visible from the first token and from the bangs.
- Tide-style workloads (E6, E7) read like SQL or PRQL: grouping, unnesting, partitioned channels and seals are
  clauses and declarations, not idioms.
- Every construct has one lowering, and the lowering of a clause never depends on distant context. The one
  exception is `choose!`'s inferred Ȳ (weakness 5).
- Raft and 2PC fit without special syntax (E3, E4). What they need, such as per-tick serialization views,
  guarded upserts, durable logs and role projection, is ordinary rules plus `choreography`.
