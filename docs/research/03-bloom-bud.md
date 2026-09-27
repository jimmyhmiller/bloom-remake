# 03 — Bloom, the Bud runtime, CALM analysis in Bloom, bud-sandbox, BloomUnit

Research cluster report for **bloom-remake**. It is written for the people implementing the Rust engine and the new standalone language. Everything here comes from primary sources: papers I read in full and source code I cloned and read. Where I could not get something, the text says so.

---

## 0. Sources actually read

| Source | What | How accessed |
|---|---|---|
| Alvaro, Conway, Hellerstein, Marczak. *Consistency Analysis in Bloom: a CALM and Collected Approach.* CIDR 2011 | Full paper (12 pp), incl. Appendix A code | PDF: https://www.cidrdb.org/cidr2011/Papers/CIDR11_Paper35.pdf (also http://db.cs.berkeley.edu/papers/cidr11-bloom.pdf) |
| Alvaro, Hutchinson, Conway, Marczak, Hellerstein. *BloomUnit: Declarative Testing for Distributed Programs.* DBTest 2012 | Full paper | PDF: https://dsf.berkeley.edu/papers/dbtest12-bloom.pdf |
| `bloom-lang/bud`, the Bud runtime (Ruby), HEAD `cbcc907` (2020-09-01), version 0.9.8-dev | All of `lib/`, `docs/`, `bin/`, most of `test/` | https://github.com/bloom-lang/bud |
| Bud docs: `docs/cheat.md`, `operational.md`, `modules.md`, `visualizations.md`, `rebl.md`, `ruby_hooks.md`, `bfs.md`, `getstarted.md`, `intro.md`, `History.txt` | Read in full | https://github.com/bloom-lang/bud/tree/master/docs |
| `bloom-lang/bud-sandbox`, HEAD `4f654bf` (2016-11-21) plus **full git history** (599 commits) | Every `.rb` file; deleted `2pc/2pc.rb`, `test/tc_2pc.rb`, `lckmgr/lckmgr.rb` recovered from history | https://github.com/bloom-lang/bud-sandbox |
| `bloom-lang/bloom-compiler` (Josh Rosen's experimental compiler for a standalone, typed Bloom syntax), HEAD `37edbc6` (2014) | Parser, analyzers, example `.bloom` files | https://github.com/bloom-lang/bloom-compiler |
| bloom-lang.net website mirror (`calm`, `features`, `research` pages) | Read | https://github.com/bloom-lang/bloom-lang.github.io |

**Not accessed, or out of scope for this cluster:**
- The Bloom^L paper (SoCC'12, http://db.cs.berkeley.edu/papers/socc12-blooml.pdf), the Dedalus TR, and Blazes (ICDE'14) are only cited here. Other research clusters cover them. §2.7 covers only how Bud's lattice code integrates with the collection runtime, based on the Bud source.
- **BloomUnit has no public source code.** It is not in the `bloom-lang` GitHub org; I checked the org's repo list through the GitHub API. Everything about BloomUnit below comes from the paper.
- **bud-sandbox never contained MapReduce.** I checked the entire git history, and no MapReduce/BOOM-MR directory ever existed. BOOM-MR (Hadoop) was written in Overlog/JOL, which is another cluster. bud-sandbox does contain **BFS**, a GFS/BOOM-FS-style chunked file system, written in Bloom.
- **bud-sandbox's Paxos is incomplete.** Its own README (`paxos/README`) says: *"The paxos implementation is a work in progress. Leader election is (mostly) done, prepare phase is getting there, propose phase is nonexistent. -- palvaro"*. There is **no Raft** in bud-sandbox.

---

## 1. Executive summary for implementers

1. **Bloom is Dedalus with a relational/collection surface syntax.** A node executes a sequence of local **timesteps (ticks)**. In each tick, a set of unordered rules is run to a stratified fixpoint over **collections** (sets of tuples with a key). There are four ways to put facts into a collection: `<=` puts them in *now*, `<+` in the *next tick*, `<-` removes them *at the next tick*, and `<~` delivers them *at some later tick, possibly on another node*. `<+-` is syntax sugar: a deferred upsert by key.
2. **Collection kinds carry persistence semantics.** `table` persists. `scratch`, `interface`, `temp`, `channel`, `periodic` and `stdio` last one tick. `channel` is a scratch whose tuples are routed by a location-specifier column (`@addr`). `periodic` is a scratch fed by wall-clock timers. There are also persistent back-ends (`sync :dbm`, `store :zookeeper`), read-only sources (`readonly`, `file_reader`), and **lattices** (`lmax`, `lset`, `lmap`, …) whose values only grow.
3. **Each tick has three phases** (Bud `docs/operational.md`):
   - (1) *setup*: clear scratches, apply deferred deletes, then deferred inserts, then take in the network messages and timer events that arrived.
   - (2) *logic*: evaluate strata in order, each to fixpoint, semi-naively.
   - (3) *transition*: stage the `<+ <- <+-` results for the next tick, send `<~` tuples, flush durable storage, and run callbacks.
4. **Stratification uses only same-tick (`<=`) edges.** A cycle through a non-monotonic operator that crosses a temporal edge (`<+`, `<-`, `<~`) is legal. The same cycle made only of `<=` edges is a compile error ("unstratifiable"). All temporal rules are evaluated in a final pseudo-stratum.
5. **Keys are enforced at runtime.** Two different tuples with the same key in one collection at the same time raise `KeyConstraintError`. Exact duplicates are ignored. Non-key *lattice-valued* columns are merged instead of conflicting.
6. **The CALM analysis in Bloom (CIDR'11) is a syntactic dataflow analysis.** Rules become edges in a predicate dependency graph, and edges are marked *temporal* (`<+`/`<-`), *async* (`<~`), or *non-monotonic* (aggregation, negation, deletion). Every non-monotonic edge is a **point of order**, and so is every edge incident to a *temporal cluster*: an SCC that contains both a non-monotonic edge and a temporal edge. Later Bud code refined this into a path-label algebra (`Bot`/`M`, `A`, `N`, `D` = diffluent), where **async followed by non-monotonic ⇒ D**. It also added a **guarded-asynchrony** check: joining two channel streams is non-deterministic unless both sides are first persisted.
7. **bud-sandbox is the standard library** of reusable protocols. Each one is written as an abstract *Protocol* module that declares only interfaces, plus one or more implementations. It covers delivery (best-effort, reliable, demonic, dastardly, causal), multicast, membership, heartbeat, timers, voting, 2PC (historical), a lock manager (historical), several KVS variants (basic, persistent, replicated, multi-version, causal/MR/RYW/MW, MVCC), ordering (nonce, serializer, assigners, priority/FIFO queues, counters, Lamport clocks, vector clocks), Chord, shopping carts (destructive, disorderly, lattice-monotone), MI cache coherence, a state machine, chat, a Twitter clone, and BFS.
8. **BloomUnit** is testing by specification:
   - A spec is a Bloom program over automatically generated `X_log` trace tables. It uses no temporal operators and derives into a single `fail` output.
   - Test inputs are generated by the Alloy model finder from exclusion and inclusion constraints.
   - Message schedules are explored stochastically. The search is pruned with CALM: only messages that flow into a non-monotonic operation *after* an async edge need their orderings explored.

---

## 2. The Bloom language as implemented by Bud

### 2.1 Program structure

A Bud program is a Ruby class or module containing three kinds of blocks (cheat sheet, `docs/cheat.md`):

```ruby
require 'bud'
module YourModule
  import SubModule => :sub_m     # scoped instantiation (see §2.8)
  state do ... end               # collection + lattice declarations
  bootstrap do ... end           # statements run once, before the first tick
  bloom :some_stmts do ... end   # named rule block
  bloom :more_stmts do ... end
end
class TopLevelClass
  include Bud                    # include Bud ONCE, in the top-level class only
  include YourModule
end
```

- **Rules are an unordered set.** Statements are separated by newlines or `;`. The order of statements and blocks carries no meaning (`docs/cheat.md`; CIDR §3.3).
- **Named bloom blocks** exist for readability and for **override by name**. If module B includes A and defines a bloom block with the same name as one in A, B's block *replaces* A's rules for that name (`docs/modules.md`, "Hello/HelloTwo" example). Defining two blocks with the same name in one class is a `CompileError` (`test/tc_errors.rb#test_dup_blocks`). A block name must be a symbol; a string raises `CompileError`.
- **State blocks** run in declaration order. Bud tags each state method with an increasing ID so that a module's state can refer to state defined in a parent (`lib/bud.rb#call_state_methods`). State declarations can compute schemas from other collections, for example `scratch :t2, t1.schema` or `table :buf, pipe_in.schema`.
- **Bootstrap blocks** run once, at the start of the first tick (`budtime` 0), before wiring. Facts inserted with `<=` or `<<` appear in tick 0. Facts inserted with `<+` are moved into the tick-0 delta by `collection.bootstrap` (channels and terminals are exceptions), and `<~` sends at the end of tick 0 (`lib/bud.rb#do_bootstrap`, `collections.rb#bootstrap`). Imported modules bootstrap before their importer.
- **Surface grammar of a statement** (CIDR §3.3): `<collection-variable> <op> <collection-expression>`. Bud's AST check (`bud_meta.rb#check_rule_ast`) requires that:
  - the lhs is a (possibly qualified, `a.b.c`) *declared* collection or lattice;
  - the operator is `<=` or `<` followed by a unary `+`, `-` or `~`. Ruby parses `<+`, `<-` and `<~` as a binary `<` plus a unary operator, and Bud rebuilds them as "superators".
  
  `=` is rejected with the error `"illegal operator: '='"`. A statement that is not a rule, such as `t1 << x` inside a bloom block, is a `CompileError`.

### 2.2 Collection kinds

This table combines the cheat sheet, CIDR Fig. 1 and `lib/bud/state.rb` / `collections.rb`. The per-tick behaviour column describes what `#tick` actually does in the code.

| Declaration | Default schema | Persistence / per-tick behaviour | Legal lhs ops | Notes |
|---|---|---|---|---|
| `table :t, [keys] => [vals]` | `[:key] => [:val]` | Persists until deleted. Tick start: apply pending `<-` deletions (exact-tuple match only), then key-deletions from `<+-`, then merge the pending `<+` inserts into delta, checking keys against storage. | `<= <+ <- <+- <-+` | `<-+` is an alias of `<+-`. |
| `scratch :s` | `[:key] => [:val]` | Semantically empty at the start of every tick; `<+` tuples appear in the next tick. The implementation keeps a cache between ticks when nothing upstream was invalidated (§3.6). | `<= <+` | `<~` gives `CompileError`. |
| `interface input, :i` / `interface output, :o` | `[:key] => [:val]` | Scratch. Also records `t_provides(name, input?)`. | `<= <+` | Connection points between modules. `interfaces(:input, [:a, :b])` is an alternative declaration form. |
| `temp :x <= rhs` (inside `bloom`) | Inferred from the rhs; if the rhs has no schema, `[:c0, :c1, …]` from the first tuple's arity | Scratch | only in its defining statement | Defining a temp whose name shadows an existing collection is a `CompileError` (`tc_errors#test_var_shadow_error`). |
| `channel :c, [:@addr, ...] => [...]` | `[:@address, :val] => []` | Receive side is a scratch (cleared each tick, filled from inbound messages). Send side is `pending`, flushed at the end of the tick. | **only `<~`** (`<=` and `<+` give `CompileError`) | Exactly one `@` column is required; zero or more than one is an error. `c.payloads` projects the address away. |
| `loopback :l` | `[:key] => [:val]` | A channel that always delivers to self, **through the network path**, so the tuples arrive in a later tick. | `<~` only | Used to force another tick or as a self-queue. |
| `periodic :p, secs` | `[:key] => [:val]`; key = unique id, val = `Time` | Scratch that the runtime fills about every `secs` seconds on a best-effort basis. No monotonicity or timeliness guarantee. | none (rhs only) | Only fires while running asynchronously. In manual `tick()` mode events are buffered. |
| `stdio` (built-in terminal) | `[:line]` | Rhs: lines read from `:stdin` since the last tick. Lhs `<~`: written to `:stdout` at the end of the tick. | `<~` only (`<=` and `<+` give `CompileError`) | Only one terminal collection per program. |
| `halt` (built-in) | `[:key]` | Scratch. The first insertion halts the instance at the end of the tick; `[:kill]` also kills the OS process. | `<=` | |
| `localtick` (built-in loopback) | `[:col1]` | | `<~` | Lets a program request another tick. |
| `sync :s, :dbm, schema` | `[:key] => [:val]` | Persistent, backed by a DBM file, and **written synchronously at the end of every tick** (`do_flush`). Needs the `:dbm_dir` option and an explicit port. | `<= <+ <-` | Tokyo Cabinet was removed in 0.9.0 (`History.txt`). |
| `store :s, :zookeeper, :path=>..., :addr=>...` | n/a | Persistent, ZooKeeper-backed, with asynchronous write flushing | `<~` (cheat sheet) | Not interesting for us; the sandbox dropped it in 2011. |
| `readonly :r` | `[:key] => [:val]` | Read-only | none | `<=` and `<+` give `CompileError`. |
| `file_reader :f, path` | `[:lineno] => [:text]` | Streams file lines | none | |
| `coll_expr` (internal) | | Evaluates a Ruby lambda each tick | none | Bud rewrites rhs literals (`[[1,2]]`, hashes, integer lattice literals) into these. |
| Lattices: `lmax :m`, `lmin`, `lbool`, `lset`, `lpset`, `lbag`, `lmap`, and user-defined kinds | n/a | A single value that persists and grows by `merge` (§2.7) | `<= <+` (`<~` gives `CompileError`) | |

**Schemas** (`collections.rb#parse_schema`):
- The hash form is `{[key cols] => [val cols]}`. The array form `[a, b]` makes *all* columns key columns.
- An empty key, `[] => [:v]`, means **at most one tuple**: a singleton or "register".
- Column names must be symbols and unique. A name that collides with an existing Ruby method (for example `:map`, `:object_id`) is rejected as "reserved".
- An `@` column is only allowed in channels.
- `c.schema` returns the same form, so other declarations can reuse it.

**Tuples** are Structs with named accessors plus positional `[i]` (`TupleStruct`).
- A tuple shorter than the schema is **padded with nil** (`tc_collections#test_pad_missing_field`).
- A tuple longer than the schema raises `TypeError`.
- A non-Array, non-Struct value raises `TypeError`. For example, `t1 <= ["hello","world"]` fails: an array of strings is not an array of tuples.
- A Hash inserted into a 2-column collection is read as `[k, v]` pairs.
- Tuples support `+` for concatenation (`[t.col..] + [5]`, `x + y` of join halves).

**Reflection/catalog collections** are built-in tables that the compiler fills with the program's own structure (`lib/bud.rb#builtin_state`):
- `t_rules(bud_obj, rule_id => lhs, op, src, orig_src, unsafe_funcs_called)`
- `t_depends(bud_obj, rule_id, lhs, op, body => nm, in_body)`
- `t_provides(interface => input)`
- `t_stratum(predicate => stratum)`
- `t_rule_stratum`, `t_cycle`, `t_table_info`, `t_table_schema`, `t_underspecified`

Bud's own analyses (stratification cross-checks, CALM labeling, budplot) are **Bloom programs over these catalog tables**. That design is worth keeping.

### 2.3 Statements and merge operators

The matrix of operator semantics comes from CIDR Fig. 1, the cheat sheet and `docs/operational.md`:

| Op | Name | Meaning (Bloom) | Dedalus reading |
|---|---|---|---|
| `lhs <= rhs` | instantaneous merge | lhs contains rhs **in the current tick**. Evaluated inside the fixpoint. May be recursive. | deductive rule, `lhs(X)@T :- rhs(X)@T` |
| `lhs <+ rhs` | deferred merge | lhs contains rhs **at the next tick** | inductive rule, `lhs(X)@T+1 :- rhs(X)@T` |
| `lhs <- rhs` | deferred delete | rhs tuples are **absent from lhs at the start of the next tick**. Tables only. Matches the **exact tuple** (the stored tuple under that key must equal the rhs tuple). | removes the tuple from the persistence ("frame") rule `p@T+1 :- p@T, ¬del_p@T` |
| `lhs <+- rhs` (or `<-+`) | deferred update / upsert | Next tick: delete every lhs tuple whose **key** matches an rhs tuple, then insert the rhs tuple. Both happen atomically. | sugar for `<-` by key plus `<+` |
| `lhs <~ rhs` | asynchronous merge | rhs tuples appear in the (usually remote) lhs at a **non-deterministic future tick**. Channels, `stdio`, `store`, `localtick` only. | async rule, `lhs(X)@S :- rhs(X)@T, choose S > T` at the address node |
| `lhs = rhs` (**CIDR'11 only, removed**) | | "rhs defines the contents of the lhs for the current timestep; lhs must not appear in lhs of any other statement." Scratch only. | |

Exact rules taken from the code:

- **Deletion runs before deferred insertion** at the tick boundary (`BudTable#tick`: `@to_delete`, then `@to_delete_by_key`, then `@pending` merged into delta). The standard atomic update pair is therefore `buf <+ [[1,"new"]]` plus `buf <- buf{|b| b if b.key==1}`, which is exactly what `<+-` does (`docs/operational.md` "Atomicity"). A test that shows the exact-match rule: deleting `[5,11]` from a table that holds `[5,10]` does nothing (`tc_collections#test_delete_key`).
- **Using `<=` from outside a tick is illegal.** Since 0.9.7, `<=` called from `sync_do` or a callback raises `CompileError("illegal use of <= outside of bloom block, use <+ instead")`. The reason: anything inserted into a scratch between ticks would be wiped at the next tick start (`History.txt` 0.9.7; `collections.rb#<=`). External code must use `<+` (visible next tick) or `<~`.
- **Rhs evaluation of temporal rules:** every rule whose operator is not `<=` is placed in the **last stratum**, so its rhs sees the final, fixed state of the tick (`bud_meta.rb#meta_rewrite`).
- **Nil results are dropped.** A block that returns `nil` removes the tuple: `t <= bc {|t| t if t.col == 5}` is selection. The runtime also calls `compact`/`uniq` on literal arrays.
- **Side effects in rhs blocks.** Ruby blocks may run **any number of times** per tick, because rescans and the fixpoint re-run them. Side effects in blocks must be idempotent (`docs/ruby_hooks.md`).

### 2.4 Rhs expression language (the BudCollection methods)

A rule's rhs is an expression that yields a collection. It can be a collection name, an Array of tuples (a literal), or a chain of the methods below. They come from the cheat sheet and `collections.rb`, `executor/*.rb`.

**Projection and selection**
- `bc {|t| ...}` / `bc.map{}` / `bc.pro{}`: the implicit map. `map` on a collection is rewritten to `pro` by `MapRewriter`. The block returns the output tuple, or `nil` to drop.
- `bc.flat_map {|t| [...]}`: unnest. The block returns an enumerable of tuples.
- `bc.keys`, `bc.values`: project to the key or non-key columns.
- `chan.payloads{blk?}`: project away the location specifier. Since 0.9.5 it always returns a (k−1)-column tuple.
- `bc.inspected`: one-column tuples holding `t.inspect`. Useful as `stdio <~ bc.inspected`.
- `bc.rename(:newname, [:k] => [:v]) {blk}`: re-schema. It defines a scratch named `newname` at rewrite time (`RenameRewriter`).
- Metadata: `bc.schema`, `bc.cols`, `bc.key_cols`, `bc.val_cols`, `bc.tabname`.

**Joins** (`a * b [* c ...]` builds a `PushSHJoin`, a symmetric hash join; §3.5). Methods on the join expression:
- `pairs(preds){|a,b| ...}`: all matching pairs. With no predicates it is the Cartesian product. `combos` is an alias, meant for n-way joins, where the predicates must be fully qualified.
- Predicate forms:
  - hash `:col1 => :col2`, meaning left.col1 = right.col2 (two-way joins only; an ambiguous bare name is an error);
  - qualified `t1.x => t2.y`;
  - the "general form", an array of equivalence classes: `combos([t1.a, t2.a], [t1.b, t3.b])`.
  
  A predicate on a collection that is not being joined is a `CompileError`. A single-table predicate inside a join (except in a self-join) is a `CompileError`.
- `matches`: natural join on all same-named columns.
- `lefts(preds)` / `rights(preds)`: pairs, projected to the left or right tuple (a semi-join).
- `outer(preds)`: **left** outer join. Unmatched left tuples are emitted with a nil-padded right tuple, and only at the end of the stratum (`PushSHOuterJoin#stratum_end`).
- `flatten`: concatenated tuple with a de-duplicated schema (SQL `SELECT *`). Used for chaining, for example `(r * s).matches.flatten.group([:a], max(:b))`.
- Limitation: **at most two instances of the same collection in one rule** ("only one self-join currently allowed per rule", `join.rb`). Joins are left-deep.

**Negation**
- `bc.notin(bc2, preds...){|l, r| bool}`: an anti-join. For each `bc` tuple, it is output unless some `bc2` tuple matches it on the hash-pair predicates **and** the block (if given) returns true for that pair. With no predicates and no block, tuples are compared whole (`lhs.to_a == rhs.to_a`). Example from the cheat sheet: `foo.notin(bar, :key=>:key) {|f, b| f.val <= b.val}` outputs `foo` items with no matching key in `bar`, *or* whose value is larger than every matching `bar` value.
- `bc.include?(tuple)`, `bc.exists?{|t| ...}`, `bc.empty?`, `bc.has_key?(k)`, `bc[k]` (lookup by key): Ruby-level membership tests used *inside* blocks. They hide a negative dependency, and the rewriter marks such references non-monotonic (§4.2). The docs prefer `notin`.

**Aggregation** (`aggs.rb`, `executor/group.rb`)
- `bc.group([grouping cols], agg1(col), agg2(col), ...){blk}`, SQL `GROUP BY`:
  - A `nil` grouping list means one global group.
  - The output schema is the grouping column names followed by `aggclassname_i` (for example `min_0`, `count_1`).
  - Several aggregates are allowed, and each may take several input columns, as in `accum_pair(x, y)`.
  - If any aggregate is non-exemplary, **input tuples are de-duplicated** before aggregating (set semantics).
- Aggregate functions:
  - *exemplary*: `min(c)`, `max(c)`, `bool_and(c)`, `bool_or(c)`, `choose(c)` (first seen; "arbitrary"), `choose_rand(c)` (Vitter reservoir sampling, size 1);
  - *summary*: `sum(c)`, `count` (argument ignored), `avg(c)` (a float);
  - *structural*: `accum(c)` (since 0.9.3 it returns a Ruby **Set**, not an Array), `accum_pair(x, y)` (a Set of `[x, y]` pairs).
- `bc.argagg(:aggname, [grouping cols], col){blk}`: for each group, outputs **all input tuples** that the exemplary aggregate picks. Ties keep every tied tuple (`Min#trans` returns `:keep` on equality). `argmin(gb, col)` and `argmax(gb, col)` are shorthand. Only `ArgExemplary` aggregates are allowed; anything else raises `"#{aggname} not declared exemplary"`.
- `bc.reduce(init){|memo, t| ...}`: a general fold. The memo is deep-copied from `init` on each invalidation. The result must be Enumerable (a collection of tuples).
- Custom aggregates are classes with `init(v)`, `trans(state, v) -> [state, flag]` and `final(state)`. The exemplary ones return a flag in `:ignore | :keep | :replace | [:delete, tuples...]`.

**Ordering-sensitive operators** (non-deterministic, or deterministic only through sorting)
- `bc.sort{blk}`: produces an array. It is often chained with `.each_with_index`.
- `bc.each_with_index{|t,i| ...}`: the docs warn that "the index assigned to a given collection member is not defined by the language semantics".
- The sandbox shows the right pattern: `SortAssign` makes ID assignment deterministic, while `AggAssign` does not (§6.5).

**Other**
- `budtime`: the local tick counter, starting at 0.
- `bud_clock`: the wall-clock `Time`, fixed for the whole tick. Calling it outside a tick is an error.
- `ip_port`, `ip`, `port`, `int_ip_port`.
- Rhs literals: `t <= [[1,2],[3,4]]`; a Hash for lmap; an integer for lmax, as in `foo <= 2` (0.9.6).

### 2.5 Legacy method names (CIDR'11 syntax, useful for reading the paper)

In CIDR'11:
- `state` was `def state`, and each rule group was a `declare def name ... end` method.
- Schemas were `['k1','k2'], ['v']` (two arrays of strings).
- Joins were `join([a, b], [a.x, b.y])`, `leftjoin`, `outerjoin`, `natjoin`.
- Paper-era tables were `members`/`send_mcast` rather than `member`/`mcast_send`.

The bloom-lang.net CALM page says: *"the Bloom examples in this paper were based on an early prototype, and syntax has changed since then. See the bud sandbox for working implementations."*

### 2.6 Keys, duplicates, errors

This is the full runtime/compile-time error catalogue collected from `errors.rb` and the tests. All of it has to be reproduced (ideally more of it statically):
- `KeyConstraintError`, raised in these cases:
  - Two different tuples in one collection share a key in the same tick. It can come from two rules (`DupKeyBud`: `[2000,'bush']` and `[2000,'gore']`), from a new tuple versus a stored one, or from two `<+` inserts landing in the same next tick.
  - An empty-key collection receives a second, distinct tuple.
  - A **channel's send buffer** conflicts on the channel key (`tc_channel#test_channel_with_key`).
  - The key is preserved when a schema is copied (`scratch :t2, t1.schema`).
  
  **Exception:** if every column that differs holds a lattice value, the lattice columns are **merged** instead (`collections.rb#merge_to_buf`). Lattice values can never be key columns (`TypeError`).
- `CompileError`: duplicate collection names; an lhs that is not a declared collection; an illegal operator; `<=` or `<+` into a channel; `<+` into a terminal; any insertion into a periodic, readonly or file_reader collection; `<~` into a table or lattice; an unstratifiable program; `=` used in a block; predicates on collections not in the join; "inconsistent attribute ref style" in join predicates; an operator-precedence slip such as `foo <= (a and b) or []`; a shadowed temp name.
- `TypeError`: a non-enumerable rhs; a non-tuple element; too many columns; a lattice value used as a key; `reduce` returning a non-Enumerable.
- `Bud::Error`: an invalid schema, reserved column names, a bad location specifier (it must be a `"host:port"` string), and so on.

### 2.7 Lattices inside Bud (integration points only)

Semantics are in the Bloom^L cluster. What matters for the collection runtime (`lattice-core.rb`, `lattice-lib.rb`, cheat sheet):
- **Built-in lattices, with initial values and the functions marked monotone or morphism:**
  - `lbool` starts at `false`. `when_true` is a morphism.
  - `lmax` starts at −∞. `gt`, `gt_eq`, `+`, `min_of` are morphisms.
  - `lmin` starts at +∞. `lt` and `+` are morphisms.
  - `lset` starts at ∅. `intersect`, `contains?`, `pro`, `eqjoin` are morphisms; `size` and `group_count` are monotone.
  - `lpset` is a set of non-negative numbers; `pos_sum` is monotone.
  - `lbag` starts at the empty multiset. `intersect`, `multiplicity`, `+`, `contains?` are morphisms; `size` is monotone.
  - `lmap` starts at `{}`. `at`, `filter`, `apply_morph`, `key?`, `key_set`, `intersect`, `to_collection` are morphisms; `apply` and `size` are monotone.
- A user-defined lattice subclasses `Bud::Lattice`. It declares `wrapper_name :lfoo`, a `merge`, and `monotone :fn do...end` / `morph :fn do...end`. The checks: a method name must be monotone in every lattice that defines it, and cannot be declared both monotone and morph.
- A lattice identifier **persists across ticks** and only grows. `tick` merges pending into storage and never resets it. Semi-naive iteration over lattices pushes `current_delta` after the first iteration.
- `reveal` (reading the raw value) is **non-monotonic**.
- Converting a lattice to a collection: `push_out` converts `v.reveal`, which must be an Enumerable of tuples, whenever the target is a BudCollection.
- Example from the cheat sheet, a quorum vote:
```ruby
QUORUM_SIZE = 5
RESULT_ADDR = "example.org"
class QuorumVote
  include Bud
  state do
    channel :vote_chn, [:@addr, :voter_id]
    channel :result_chn, [:@addr]
    lset    :votes
    lmax    :vote_cnt
    lbool   :vote_done
  end
  bloom do
    votes      <= vote_chn {|v| v.voter_id}
    vote_cnt   <= votes.size
    got_quorum <= vote_cnt.gt_eq(QUORUM_SIZE)
    result_chn <~ got_quorum.when_true { [RESULT_ADDR] }
  end
end
```
(The cheat-sheet code declares `vote_done` but writes `got_quorum`. That is a typo in the source.)

### 2.8 Modules: `include`, `import`, protocols, overriding

These come from `docs/modules.md`, `lib/bud.rb#resolve_imports` and `test/tc_module.rb`.
- **`include M`** (a Ruby mixin) inlines M's state and rules into one flat namespace. Because Ruby is dynamic, M's rules may refer to collections of the includer.
- **`import M => :alias`** creates a *separate instance* of M under a namespace. Within that instance its collections are referred to as `alias.coll`, and nesting works (`c.p.t1`). Importing M twice under two aliases gives independent copies (`import Q => :q1; import Q => :q2`). Two imports under the same alias are an error. Importing something that is not a module is an error. A temp declared inside an imported module gets its own copy per import.
- **The protocol pattern:** an abstract module that declares only `interface input/output` collections (a "contract"), plus concrete implementation modules that `include` the protocol. Clients code against the protocol, and a concrete implementation is mixed in at composition time. Examples: `DeliveryProtocol` → `BestEffortDelivery` / `ReliableDelivery`; `KVSProtocol` → `BasicKVS` / `ReplicatedKVS`.
- **Interposition through import:** redeclare the same interfaces, import the black box, and glue `iin → logic → bb.iin` and `bb.iout → iout`.
- **Underspecification:** an input interface never read, or an output interface never written, is flagged ("Warning: input interface X not used"). budplot draws the gap as a red "??" node (§5.2).
- **CIDR'11 historical feature: interface override by redeclaration.** "If an input interface appears in the lhs of a statement in a module that declared the interface, it is rewritten to reference the interface with the same name in a mixed-in class, because a module cannot insert into its own input interface. The same is the case for output interfaces appearing in the rhs." (CIDR §3.4, used by ReplicatedKVS Fig. 6.) Modern Bud replaced this with `import`, as `kvs/kvs.rb`'s `ReplicatedKVS` shows with `import BasicKVS => :kvs`.

---

## 3. Time and execution semantics

### 3.1 The tick

From `docs/operational.md`, verbatim:

> 1. *setup*: All scratch collections are set to empty. Network messages and periodic timer events are received from the runtime and placed into their designated `channel` and `periodic` scratches, respectively … a batch of multiple messages/events may be received at once.
> 2. *logic*: All Bloom statements for the program are evaluated. In programs with recursion through instantaneous merges (`<=`), the statements are repeatedly evaluated until a *fixpoint* is reached.
> 3. *transition*: Items derived on the lhs of deferred operators (`<+`, `<-`, `<+-`) are placed into/deleted from their corresponding collections, and items derived on the lhs of asynchronous merge (`<~`) are handed off to external code … When multiple items are on the rhs of an async merge, they may "appear" independently spread across multiple different future local timesteps.

The actual order of operations in `Bud#tick_internal` (`lib/bud.rb`, lines ~1100–1160) is:

```
if first tick: do_bootstrap; do_wiring (compile to push dataflow)
else: for each collection t: t.tick           # scratch clear / table deletes+pending / lattice pending merge
      do_invalidate_rescan                     # cross-tick incremental cache invalidation (§3.6)
receive_inbound                                # inbound channel/periodic/stdin buffers -> collection storage
for stratum in 0..n:
    repeat:
        scanners.scan(first_iter)             # push full contents or deltas into the dataflow
        push elements flush                    # stateful ops emit (group, notin, sort, reduce...)
        joins.tick_deltas / merge_targets.tick_deltas   # new_delta -> delta -> storage
    until no join and no target saw a new delta
    elements.stratum_end; targets.flush_deltas
do_flush                                       # channels send pending tuples; dbm/zk flush
invoke_callbacks                               # register_callback(name) for every non-empty collection
budtime += 1; inbound.clear
```

Consequences you can observe:
- **Atomicity.** State is immutable inside a tick: `<=` can only add. `<+`, `<-` and `<+-` take effect together at the next tick boundary. "Any reasoning about atomicity in Bloom programs is built on this simple foundation" (`operational.md`). BFS relies on this: two `kvput <=` inserts in the same fixpoint "will occur together in the same fixpoint computation or not at all" (`docs/bfs.md`).
- **Ticks are event-driven.** A running instance ticks when a network message arrives, a periodic fires, or `sync_do` / `async_do` is called (`server.rb#receive_data` calls `tick_internal if running_async`). No event means no tick. Programs that need a "next tick" without any external event use `localtick <~` or a `loopback`.
- **Channel batching.** Every message that arrives between two ticks is delivered in one tick. Because channel contents are sets, identical messages that arrive in the same tick collapse.
- **Callbacks** run at the end of every tick in which the named collection is **non-empty**. For a table that means every tick, as long as it holds data.

### 3.2 Channels (the network model)

- **Semantics** (CIDR §3.2): "a scratch collection with one attribute designated as the location specifier. Tuples 'appear' at the network address stored in their location specifier." Failure is modelled as "the repeated 'non-appearance' of a fact at every timestep."
- **Send path.** `<~` puts tuples into `pending`, checked against the channel's key. At `do_flush`, each tuple is msgpack-encoded as `[qualified_tabname, tuple_fields, marshal_indexes]` and sent as one **UDP datagram** to the `host:port` in the `@` column. Loopback sends to its own address. Delivery is unreliable, unordered and unacknowledged; reliability is a library concern (§6.1).
- **Receive path.** The `EM::Connection` unpacks the datagram, checks that the table exists, and buffers the tuple per channel. An optional **`:channel_filter`** lambda `(chan_name, tuples) -> [accepted, postponed]` lets tests drop, delay or reorder messages. Postponed messages are offered again on the next receive; anything in neither list is dropped (`tc_channel#test_filter_drop`, `#test_filter_batch`; `tc_causal_delivery` uses it to reorder). **This is Bud's built-in fault-injection hook, and our runtime needs an equivalent.**
- `payloads` strips the address column. The recipient usually needs the sender's address inside the payload (the `src` field) in order to reply.

### 3.3 Timers and time

- **`periodic :p, secs`** is an EventMachine `PeriodicTimer` that enqueues `[gen_id, Time.now]` into inbound and ticks. The cheat sheet: "periodics execute in a best-effort manner … the system clock value stored in the `val` field may not be monotonically increasing."
- `budtime` is the local logical time. There is no global time. "Timesteps and timestamps are not coordinated across nodes; any such coordination has to be programmed in the Bloom language itself" (`operational.md`).

### 3.4 Ruby/host interaction API (execution modes)

From `lib/bud.rb` and `docs/ruby_hooks.md`:
- `tick()`: run one tick synchronously ("single-stepping"). Messages and timers are buffered until the next call.
- `run_bg()`: run in a background thread, driven by events. `run_fg()`: block the caller. `pause()`: return to manual ticking. `stop()`.
- `sync_do { ... }`: runs the block **between ticks**, with the runtime blocked, and **then runs a tick**. `async_do { ... }` does the same without blocking; these calls are FIFO.
- `register_callback(:coll){|c| ...}`, `unregister_callback(id)`, `sync_callback(in_coll, tuples, out_coll)` (insert, then block until `out_coll` is non-empty), `delta(:coll)`, `on_shutdown`, `post_shutdown`.
- Options:
  - network: `:ip`, `:port`, `:ext_ip`, `:ext_port`;
  - I/O: `:stdin`, `:stdout`, `:signal_handling`;
  - tracing and output: `:quiet`, `:trace`, `:rtrace`, `:tag`, `:dump_rewrite`, `:dump_ast`, `:print_wiring`, `:metrics`;
  - testing: `:channel_filter`;
  - storage: `:dbm_dir`, `:dbm_truncate`;
  - evaluation: `:disable_lattice_semi_naive`, `:no_attr_rewrite`;
  - the `BUD_SAFE` environment variable disables incremental caching.

### 3.5 Relationship to Dedalus

Bloom "is based on a formal temporal logic called Dedalus" (CIDR §3). Each fact is timestamped by local tick; `table` persistence is the Dedalus frame rule; `<-` negates persistence. A Bloom program is "side-effect free … if a fact is defined at a given timestep, its existence at that timestep cannot be refuted" (CIDR §3.1). The Bud docs on side effects: "The temporal logic of Dedalus is a lot like a versioning system, where old versions of items are never removed."

### 3.6 Evaluation strategy inside Bud (≥0.9.0, "push-based runtime")

This part is worth studying for performance design, although our Rust engine should do better. Sources: `lib/bud/executor/*.rb`, `executor/README.rescan`, `History.txt` 0.9.0.

- **Wiring.** At the first tick, each rule's rhs is `instance_eval`'d once to *build* a push-based dataflow: `ScannerElement` for each (collection, stratum), then `PushElement` (map), `PushSHJoin`, `PushNotIn`, `PushGroup`, `PushArgAgg`, `PushReduce`, `PushSort`, `PushEachWithIndex`, and lattice `PushApplyMethod` nodes. Elements are sorted topologically per stratum.
- **Semi-naive evaluation.** Every collection has four buffers: `storage`, `delta`, `new_delta` and `pending`, plus `tick_delta`.
  - Scanners push the **full contents** on the first iteration when the collection is in "rescan" mode; otherwise they push only `tick_delta`, the growth from earlier strata.
  - On each iteration they then push `delta`. Derived tuples go into `new_delta`.
  - `tick_deltas` moves `delta → storage` and `new_delta → delta`, merging under key and lattice rules, and reports whether anything new appeared.
  - Joins are **symmetric hash joins** that keep both sides' hash tables for the whole tick, and across ticks when not invalidated. An inserted tuple is added to its own side's table and probes the other side. `found_delta` drives the fixpoint loop.
- **Non-monotonic operators are safe because of stratification.**
  - `PushNotIn` waits for the first `flush` (by then the negated input is complete, since it lives in a lower stratum) and then emits the anti-join.
  - `PushGroup` / `PushArgAgg` accumulate and emit only at `flush`, and only when in rescan mode. Group is **always** put into the rescan set: it re-emits its whole result every tick.
- **Cross-tick incrementality ("invalidate / rescan").** Semantically, every scratch and every operator cache starts from nothing at each tick. Bud avoids recomputation by solving, at wiring time, which elements must be invalidated or rescanned (`prepare_invalidation_scheme`):
  - (1) a source scratch (one never on any lhs) is invalidated every tick;
  - (2) a table invalidates its dependents only if a pending **deletion actually removed** a tuple at the tick start;
  - (3) invalidation implies rescan; rescan implies downstream invalidation; invalidating an element with several inputs requires every one of those inputs to rescan;
  - (4) stateful elements answer rescan requests from their own caches instead of passing them upstream.
  
  The result is two default sets plus a per-scanner `rescan_set` / `invalidate_set` used when that scanner's collection is invalidated at runtime.
- **"Unsafe" functions.** A block that calls a function with an implicit receiver (for example `budtime`, `rand`, `Time.now`, a user helper), other than `ip_port` / `ip` / `port` / `int_ip_port` or a collection name, is flagged `unsafe_funcs_called` (`UnsafeFuncRewriter`). Its dataflow is forced to rescan every tick so that caches do not hold stale values. **Lesson for us:** any impure or non-deterministic scalar function must defeat incremental caching, or be banned from rule bodies.
- **Attribute-name rewriting.** Block-variable field accesses like `t.cost` are rewritten into positional `t[2]` (`AttrNameRewriter`) for speed. Defining two block variables with the same name in one rule is a `CompileError`.
- **Bud is slow and says so.** "Bud alpha is not intended to excel in single-node performance" (`docs/intro.md`). The CIDR-era Bud was under 2,400 lines of Ruby (CIDR §3.5).

---

## 4. Stratification and monotonicity marking in Bud

### 4.1 Dependency extraction (`rewrite.rb#RuleRewriter`)

For every rule, the rewriter records `t_depends(bud_obj, rule_id, lhs, op, body, nm, in_body)`. That is one row per collection or lattice referenced in the rhs, where:
- `op` is `"<="`, `"<+"`, `"<-"`, or `"<~"`. `<+-` shows up as `<+` plus a delete-by-key.
- `nm` is **true** when the collection is referenced in a non-monotonic position.
- `in_body` is **true** when the reference occurs *inside a Ruby block*. For example, `bad_people` in `add_member.map{|m| m unless bad_people.include? [m.name]}` is an implicit dependency that behaves like a join or anti-join.

### 4.2 What counts as non-monotonic (`nm`)

`MONOTONE_WHITELIST` (verbatim):
```ruby
[:==, :+, :<=, :-, :<, :>, :*, :~, :+@,
 :pairs, :matches, :combos, :flatten, :new,
 :lefts, :rights, :map, :flat_map, :pro, :merge,
 :schema, :cols, :key_cols, :val_cols, :payloads, :lambda,
 :tabname, :current_value]
```
Also treated as monotone: every lattice **morphism** or **monotone function** name, column-accessor calls, calls whose receiver is a block-local variable (`lvar`), and names starting with `__`.

A rule is marked non-monotonic when:
- it calls any other method on a non-local receiver, such as `group`, `argagg`, `argmin`, `argmax`, `reduce`, `outer`, `include?`, `exists?`, `empty?`, `length`, `sort`, `each_with_index`, `inspected`, `keys`, or `reveal`;
- or it contains a unary minus, meaning a **deletion rule `<-`**.

For `x.notin(y)`, **y** alone is marked negative, and x stays positive. The flag is "sticky" as the AST is walked, so receivers of non-monotonic calls are recorded with nm = true. This analysis is **purely syntactic and conservative**:
- `inspected` and `keys` are flagged even though they are just projections.
- Arithmetic `-` is whitelisted because it is scalar arithmetic, not set difference.
- `MIN(x) < 100` is *not* recognised as monotone, even though CIDR §2 points out that it is. Later work (Bloom^L morphisms, typed monotonicity in Hydro) fixed this.

### 4.3 Stratification algorithm (`bud_meta.rb`)

1. Build a node for every predicate in `t_depends`. Edges go lhs → body, labelled `(op, neg=nm, temporal = op != "<=")`.
2. Run a DFS from every node. Only `<=` edges are followed:
```
calc_stratum(node, neg, temporal, path):
  if node.status == in_process:
      if neg and not temporal and not node.already_neg: raise "unstratifiable program: path"
  elif node.status == init:
      node.status = in_process
      for edge in node.edges where edge.op == "<=":
          node.already_neg = neg
          s = calc_stratum(edge.to, neg or edge.neg, edge.temporal or temporal, path+[edge.to])
          node.stratum = max(node.stratum, s + (edge.neg ? 1 : 0))
      node.status = done
  return node.stratum
```
3. Renumber the strata densely from 0.
4. **Rule placement.** A `<=` rule goes into stratum `max over body rels r of (stratum(r) + (nm(r) ? 1 : 0))`. This is the "slightly more aggressive" placement from 0.9.8: rules with the same lhs may sit in different strata. A rule with no body relations goes into stratum 0. **Every temporal rule (`<+ <- <~ <+-`) goes into a final stratum** `top+1`.
5. `DepAnalysis` (itself a Bloom program) computes the transitive closure `depends_tc`, and from it:
   - `cycle` (self-reaching predicates, excluding negative cycles that are not temporal);
   - `source` (input interfaces that do not depend on anything);
   - `sink`;
   - `underspecified`.

A canonical unstratifiable example (`operational.md`):
```ruby
glass <= one_item {|t| ['full'] if glass.empty? }
```
"glass.empty? => not glass.empty? … a contradiction. The Bud runtime detects cycles through non-monotonicity for you automatically when you instantiate your class."

**Recommendation for us:** use the textbook SCC formulation rather than Bud's DFS, which needed bug-fixes (0.9.8: "Fix bug in the stratification algorithm"). Build the graph over `<=` edges only and compute SCCs. Reject an SCC that contains any `nm` edge. Stratum = the longest path in the condensation, where `nm` edges weigh 1. The bloom-compiler `Stratifier.scala` expresses this as a declarative circular attribute:
`collectionStratum(c) = max(0, max over non-temporal deps d of stratum(d) + [negated] + [non-monotonic])`.
It also defines "temporally stratifiable": *no negated dependency participates in a deductive cycle*.

---

## 5. CALM analysis in Bloom (CIDR 2011) and its implementation in Bud

### 5.1 The CALM principle as stated in the paper (CIDR §2)

- **The target property is eventual consistency.** "A sufficient condition for eventual consistency is order independence: the independence of program execution from temporal nondeterminism."
- **Monotone programs** ("selection, projection and join (even with recursion)") "can be implemented by streaming algorithms that incrementally produce output elements as they receive input elements… never cause any earlier output to be 'revoked'". **Non-monotonic** programs ("aggregation or negation") "can only be implemented correctly via blocking algorithms". Formal footnote: "in a monotonic logic program, any true statement continues to be true as new axioms—including new facts—are added."
- **Mnemonics:** "counting requires waiting", and "waiting requires counting". Paxos counts a majority; 2PC counts everyone.
- **The CALM principle:** "Monotonic programs guarantee eventual consistency under any interleaving of delivery and computation. By contrast, non-monotonicity … requires coordination schemes that 'wait' until inputs can be guaranteed to be complete."
- **Points of order:** "The loci produced by a non-monotonicity analysis are the program's points of order. A program with non-monotonicity can be made consistent by including coordination logic at its points of order."
- **Refinement:** "the expression 'MIN(x) < 100' is monotonic despite containing an aggregate… once a subset S satisfies this test, any superset of S will also satisfy it". This is a threshold test over a monotone aggregate.
- **Coordinating the coordinator.** Coordination modules add points of order of their own. They "must be verified for order independence … When the verification is done by hand, annotations can inform the analysis tool to skip the module". **We need an annotation for this**, something like "trusted/sealed coordination module".

### 5.2 The dataflow graph and its legend (CIDR §4.4, Fig. 8)

"A Bloom program may be viewed as a dataflow graph with external input interfaces as sources, external output interfaces as sinks, collections as internal nodes, and rules as edges."

- Nodes:
  - **tables** are rectangles;
  - **ephemeral** collections (scratch, periodic, channel) are ovals;
  - **clusters** are octagons;
  - `S` and `T` are the source and sink;
  - a red diamond **`??`** marks an *underspecified* dataflow (an abstract protocol with no implementation).
- Edge A→B means B appears on the lhs of a rule whose rhs refers to A, directly or through a join.
- Edge annotations:
  - `<+` or `<-` gets the label **"+/−"** ("facts traversing the edge 'spend' a timestep");
  - `<~` gets a **dashed** line;
  - **non-monotonic** (aggregation, negation, or deletion via `<-`) gets a **white circle** (Bud: `arrowhead='veeodot'`).
- **Temporal cluster:** "any strongly connected component marked with both a circle and a +/− edge is collapsed into an octagonal 'temporal cluster', which can be viewed abstractly as a single, non-monotonic node."
- **Definition of points of order in the paper:** "Any non-monotonic edge in the graph is a point of order, as are all edges incident to a temporal cluster, including their implicit self-edge."

### 5.3 Case-study conclusions (use them as golden analysis results)

- **BasicKVS** (paper Fig. 5, overwrite via `<+` and `<-`). `kvstate` and `prev` are collapsed into a red octagon. "Any data flowing from kvput to the sink must cross at least one non-monotonic point of order … and any path from kvget to the sink must join state potentially affected by non-monotonicity." Reason: destructive update means "the contents of kvstate may depend on the order of arrival of kvput tuples."
- **Destructive cart** (on the replicated KVS). There are points of order between `action_msg`, `member`, and the temporal cluster. Consistency would need coordination "for every client action or kvput update", in effect eager replication. Informal reasoning also misses a concrete bug: a delete that arrives before its add is ignored, so replicas diverge.
- **Disorderly cart** (actions accumulate in a set and are summarised at checkout). "Communication (via `action_msg`) between client and server—and among server replicas—crosses no points of order". Points of order appear only where `checkout_msg` is joined with the `action_cnt` aggregate, and at the `accum` step between `status` and `response_msg`. Conclusion: "we need to coordinate once per session (at checkout), rather than once per shopping action."
- **Design guidance (§5.5):** use the analysis to "'push back' the points to as late as possible in the dataflow … or to 'localize' points of order by moving them to locations … where the coordination can be implemented on individual nodes without communication."

### 5.4 Tolerating inconsistency (CIDR §6; proposal, never implemented)

This follows Helland & Campbell's *memories, guesses and apologies*:
- Rewrite schemas to add an attribute marking each fact as a "guarantee" or a "guess", and propagate the mark like taint tracking.
- Unresolved points of order turn guarantees into guesses.
- Log the guesses that cross interface boundaries; that log is the "memories".
- A background consistent computation detects bad guesses and issues "apologies".

Nothing in Bud implements this. It is a candidate feature for us: taint labels derived from the point-of-order analysis.

### 5.5 Bud's later analyses (in the code)

**(a) `budplot`** (`bin/budplot`, `lib/bud/graphs.rb`) produces a static graph with the CIDR legend. It collapses negative-and-temporal cycles into octagons and colours nodes by path label (yellow for A/N, red for D). It reports underspecified interfaces. Usage: `budplot FILES MODULES`. For example, `budplot kvs/kvs.rb ReplicatedKVS` shows `??` until `BestEffortMulticast StaticMembership` are added.

**(b) `MetaAlgebra`** (`lib/bud/meta_algebra.rb`), a Bloom program over `t_depends`:
- Rule tags:
  - `<~` with nm gives `:D`;
  - nm alone gives `:N`;
  - `<~` alone gives `:A`;
  - anything else gives `:M`.
- Sequential-composition lattice, from the bootstrap facts `[:M,:A],[:M,:N],[:A,:D],[:N,:D],[:N,:A,directional]`:
  - M < A, M < N, A < D, N < D;
  - "N followed by A" = A (directional);
  - "A followed by N" = D, because the least upper bound of incomparable elements is computed as D.
- `alg_path` enumerates paths from input interfaces and folds the tags along each path.
- `d_begins` finds the first point on a path where it becomes D. `a_preds` finds the last async edge before that point: **"ordering this edge prevents diffluence."**

**(c) `Validate` + `GuardedAsync` labeling** (`lib/bud/labeling/labeling.rb`, CLI `budlabel -r FILE -i MODULE [-C|-P] [-O fmt]`). This is the most refined form.
- Edge labels (`labelof`): `<~` gives `"A"`; nm gives `"N"`; otherwise `"Bot"`.
- SCCs are collapsed into `cluster_IN` / `cluster_OUT` nodes.
- Every path from an input interface to an output interface gets a label by folding `collapse`:
```ruby
def collapse(left, right)
  return [right] if left == 'Bot'
  return [left] if right == 'Bot'
  return [left] if left == right
  return ['D'] if left == 'D' or right == 'D'
  # CALM
  return ['D'] if left == 'A' and right =~ /N/
  # sometimes we cannot reduce
  return [left, right]
end
```
- Each output combines the labels of its paths with `disjunction`:
  - D if any path is D;
  - N together with A gives D;
  - N alone gives N;
  - A alone gives A;
  - otherwise Bot.
- Human-readable meanings (verbatim from `bin/budlabel`):
  - `D` — "Diffluent: Nondeterministic output contents."
  - `A` — "Asynchronous. Nondeterministic output orders."
  - `N` — "Nonmonotonic. Output contents are sensitive to input orders."
  - `Bot` — "Monotonic. Order-insensitive and retraction-free."
- **Guarded asynchrony** (`GuardedAsync`) handles the hidden non-determinism in *monotone* programs, which the source calls "unguarded asynchrony". It considers every pair of distinct **channels** whose dataflows meet at the same collection (`meet`). The race is **guarded** only if *both* paths pass through a persistent `BudTable` or a lattice before the meet. Otherwise `channel_race.guarded = false`, and the meeting collection and everything downstream of it become `divergent_preds`, labelled D.
- Why guarding matters: `result <= (ls * rs).lefts` over two scratches fed by two channels gives an output that depends on whether both messages land *in the same tick*. `(lt * rt)` over tables does not.
- Golden results, from `test/tc_labeling.rb`:
  - `TestNM` (`response <= guard1.notin(guard2)`, both fed by channels): output `{"response"=>"D"}`; paths `{"i1"=>"A","i2"=>"D"}`.
  - `TestGroup` (`guard1.group([:val], count)`): `{"response"=>"D"}`; path `i1 → D`.
  - `TestMono` (`(guard1*guard2).lefts`, tables): `{"response"=>"A"}`; both paths A.
  - `TestDeletion`: paths `dguard→response` and `i2→response` are D.
  - `BugButt` (scratch * scratch) and `HalfGuard` (table * scratch): races are unguarded (`false`). `FullGuard` (table * table): guarded (`true`).

**Limitations that later work fixed:**
- Monotonicity is syntactic. Bloom^L (lattices, morphisms, monotone functions) and Hydro's typed or annotation-based monotonicity replaced it.
- There is no notion of *sealing* or partition-local completeness. Blazes (Alvaro et al., ICDE'14) added `Seal`, `CR/CW/OR/OW` component annotations, and coordination synthesis.
- The analysis gives no way to prove determinism of hand-coordinated code. BloomUnit gives empirical evidence instead (§7).
- Ruby blocks can hide non-monotonic logic, such as `include?` inside a map. The `in_body` catalog flag only partly addresses this.

---

## 6. bud-sandbox: the protocol library (complete catalogue)

Every entry lists the module, its interface (collection signatures) and its behaviour. Code paths are relative to https://github.com/bloom-lang/bud-sandbox. "(hist)" means recovered from git history.

### 6.1 Delivery — `delivery/*.rb`

**`DeliveryProtocol`** (abstract):
```ruby
interface input,  :pipe_in,   [:dst, :src, :ident] => [:payload]
interface output, :pipe_sent, [:dst, :src, :ident] => [:payload]  # sender: delivery "complete"
interface output, :pipe_out,  [:dst, :src, :ident] => [:payload]  # receiver: delivered
```

**`BestEffortDelivery`**:
```ruby
channel :pipe_chan, [:@dst, :src, :ident] => [:payload]
bloom :snd  do pipe_chan <~ pipe_in end
bloom :rcv  do pipe_out <= pipe_chan end
bloom :done do pipe_sent <= pipe_in end   # reports success immediately ("more like 'an effort'")
```

**`ReliableDelivery`**: at-least-once. The comment says: "If you need exactly-once, the receiver-side can record the message IDs".
```ruby
module ReliableDelivery
  include DeliveryProtocol
  import BestEffortDelivery => :bed
  state do
    table :buf, pipe_in.schema
    channel :ack, [:@src, :dst, :ident]
    periodic :clock, 2
  end
  bloom :remember do
    buf <= pipe_in
    bed.pipe_in <= pipe_in
    bed.pipe_in <= (buf * clock).lefts          # retransmit everything unacked every 2s
  end
  bloom :rcv do
    pipe_out <= bed.pipe_out
    ack <~ bed.pipe_out {|p| [p.src, p.dst, p.ident]}
  end
  bloom :done do
    temp :msg_acked <= (buf * ack).lefts(:ident => :ident)
    pipe_sent <= msg_acked
    buf <- msg_acked
  end
end
```
The CIDR'11 Fig. 2 version is equivalent in the old syntax, with `periodic :timer, 10`.

**`DemonicDelivery`** (+ `DemonicDeliveryControl`) is a fault injector:
- Input `set_drop_pct [] => [:pct]` sets the drop rate; `table drop_pct` defaults to 50.
- Each message is sent only when `p.pct <= rand(100)`, yet success is always reported.
- `drop_pct <+- set_drop_pct`.

**`DastardlyDelivery`** (+ `DastardlyDeliveryControl`) is a fault injector that reorders and delays:
- Input `set_max_delay [] => [:delay]`; the default is 5 ticks.
- `buf <+ pipe_in{|m| [m, budtime]}`.
- On every tick, one buffered message is chosen with `argagg(:choose_rand, [], :whenbuf)`. It is sent if the buffer holds more than one message, or once it has waited at least `delay` ticks.
- Success is reported immediately.

**`CausalDelivery`** is point-to-point causal delivery using the Schiper–Eggli–Sandoz (1989) algorithm, **built on lattices**:
- `lmap :my_vc, :next_vc` holds the vector clock as `node → lmax`.
- `lmap :ord_buf` is `node → (node → lmax)`.
- Channel: `chn [:@dst, :src, :ident] => [:payload, :clock, :ord_buf]`.
- A message is delivered when `m.ord_buf.at(ip_port, Bud::MapLattice).lt_eq(my_vc).when_true { m }`.
- The comment notes that it is not live without reliable delivery.

**`MulticastProtocol`**: `interface input :mcast_send, [:ident] => [:payload]`; `interface output :mcast_done, [:ident] => [:payload]`.

**`Multicast`** (depends on Delivery + Membership):
- `pipe_in <= (mcast_send * member)`, for every member other than self.
- It counts members, keeps `unacked_count`, decrements it on `pipe_sent` using `<+-`, and emits `mcast_done` when the count reaches 0. Then it garbage-collects.
- Compositions: `BestEffortMulticast` = BestEffortDelivery + Multicast + StaticMembership. `ReliableMulticast` = ReliableDelivery + Multicast + StaticMembership.

### 6.2 Membership, heartbeat, timers

**`MembershipProtocol`**:
- inputs: `my_id [:ident]`, `add_member [:ident] => [:host]`, `remove_member [:ident]`;
- outputs: `member [:ident] => [:host]`, `added_member [:ident] => [:host]`.

**`StaticMembership`**: `table private_members`; add with `<=`; remove with `<-` joined on ident; `member <= private_members`.

**`HeartbeatProtocol` / `HeartbeatAgent`** (`heartbeat/heartbeat.rb`):
- Inputs: `payld [:pload]`, `return_address [] => [:addy]`. Output: `last_heartbeat [:peer] => [:sender, :time, :pload]`.
- Implementation:
  - `channel heartbeat [:@dst, :src, :sender, :pload]`, `periodic hb_timer 2`;
  - on each timer tick it broadcasts the current payload to every member except itself;
  - receivers log `[peer, sender, time, pload]`;
  - `last_heartbeat` = `argagg(:max, [peer], time)`, then `group(..., choose(pload))`;
  - log entries older than `HB_EXPIRE = 4.0`s are deleted.

**`ProgressTimerProto` / `ProgressTimer`** (`timers/progress_timer.rb`):
- Inputs: `set_alarm [:name, :time_out]`, `del_alarm [:name]`. Output: `alarm [:name, :time_out]`.
- It runs on `periodic 0.2`, fires **once** after `time_out` seconds, and then removes the timer.

### 6.3 Voting, 2PC (hist), lock manager (hist)

**Voting** (`voting/voting.rb`):
- `VoteMasterProto`: input `begin_vote [:ident, :content]`; output `victor [:ident, :content, :response, :resp_content]`.
- `VoteAgentProto`: input `cast_vote [:ident] => [:response, :content]`.
- `VoteInterface`: `channel :ballot, [:@peer, :master, :ident] => [:content]`; `channel :vote, [:@master, :peer, :ident] => [:response, :content]`.
- `VotingMaster` (includes StaticMembership):
  - sends ballots to all members and sets `vote_status` to `'in flight'`;
  - counts with `vote_cnt <= votes_rcvd.group([ident, response], count(peer), accum(content))`;
  - declares a victor only when the vote is **unanimous**: `m.cnt == v.cnt`;
  - updates status with a `<+` / `<-` pair.
- `VotingAgent`: caches ballots in `waiting_ballots`. Its `decide` block (voting 'yes' by default) is meant to be overridden. `vote <~` is sent when `cast_vote` joins a waiting ballot.
- `MajorityVotingMaster` overrides `:summary` and wins when `v.cnt > m.cnt / 2`.

**2PC** (`2pc/2pc.rb`, removed in commit `ec5c011` "Remove two-phase commit code.", 2012-03-13) is a specialisation of voting:
```ruby
module TwoPCVotingMaster
  include VotingMaster
  bloom :summary do
    victor <= (vote_status * member_cnt * vote_cnt).combos(vote_status.ident => vote_cnt.ident) do |s, m, v|
      if v.response == "N"
        [v.ident, s.content, "N"]
      elsif v.cnt == m.cnt
        [v.ident, s.content, v.response]
      end
    end
    vote_status <+ victor {|v| v }
    vote_status <- victor {|v| [v.ident, v.content, 'in flight'] }
  end
end
module TwoPCMaster
  include TwoPCVotingMaster
  state do
    table :xact, [:xid, :data] => [:status]
    scratch :request_commit, [:xid] => [:data]
  end
  bloom :boots do
    xact <= request_commit {|r| [r.xid, r.data, 'prepare'] }
    begin_vote <= request_commit {|r| [r.xid, r.data] }
  end
  bloom :panic_or_rejoice do
    temp :decide <= (xact * vote_status).pairs(:xid => :ident)
    xact <+ decide {|x, s| [x.xid, x.data, "abort"] if s.response == "N" }
    xact <- decide {|x, s| x if s.response == "N" }
    xact <+ decide {|x, s| [x.xid, x.data, "commit"] if s.response == "Y" }
    xact <- decide {|x, s| [x.xid, x.data, "prepare"] if s.response == "Y" }
  end
end
```
- `TwoPCAgent` votes through `can_commit [:xact, :decision]`.
- `Monotonic2PCMaster` replaces status overwrites with an ordinal max: prepare = 0, commit = 1, abort = 2, then `group max(ordinal)`. This is an early attempt to make 2PC state monotone.
- The historical test `test/tc_2pc.rb`: one master and two agents. `request_commit [1,"foobar"]` puts `xact` into `"prepare"`. After both agents vote "Y", `vote_status` becomes "Y" and `xact` becomes `"commit"`.

**Lock manager** (`lckmgr/lckmgr.rb`, removed in `601da69`, 2011-08-22). `LockMgrProtocol` has inputs `request_lock [:xid, :resource]` and `end_xact [:xid]`, and output `lock_status [:xid, :resource] => [:status]`. `TwoPhaseLockMgr`:
- keeps a `pending` table;
- chooses one xid per free key with `group([key], choose(xid))`;
- takes the lock with `lock <+ chosen`;
- releases all of a transaction's locks on `end_xact`.

### 6.4 Key-value stores — `kvs/*.rb`

**`KVSProtocol`**:
```ruby
interface input,  :kvput, [:client, :key] => [:reqid, :value]
interface input,  :kvdel, [:key] => [:reqid]
interface input,  :kvget, [:reqid] => [:key]
interface output, :kvget_response, [:reqid] => [:key, :value]
```
- **`BasicKVS`**: `table kvstate [:key]=>[:value]`; `kvstate <+- kvput{|s| [s.key, s.value]}`; get = join `kvget * kvstate`; delete = `kvstate <- (kvstate*kvdel).lefts`. **Two puts to the same key in one tick raise `KeyConstraintError`.** The sandbox test `ntest_wl5` documents this and is disabled.
- **`PersistentKVS`**: mirrors `kvstate` into `sync :kvstate_backing, :dbm` and reloads in bootstrap.
- **`ReplicatedKVS`**: `import BasicKVS => :kvs`; puts and deletes become `mcast_send` messages carrying `["put", [...]]` or `["del", [...]]`. They are applied locally on `mcast_done`, and at replicas on `pipe_out`.
- `useful_combos.rb`: `SingleSiteKVS`, `SSPKVS` (persistent), `BestEffortReplicatedKVS`, `ReliableReplicatedKVS`. `ReplicatedMeteredGlue` is a sketch of composing two interposers.
- **Multi-version KVS** (`mv_kvs.rb`). `MVKVSProtocol` has `kvput [:client, :key, :version] => [:reqid, :value]`, `kvget [:reqid] => [:client, :key, :version]` and `kvget_response [:reqid, :key, :version] => [:value]`. Implementations:
  - `BasicMVKVS` keeps all versions;
  - `VC_MVKVS` increments a vector clock on put;
  - `Causal_MVKVS` returns only versions that come after the client's clock;
  - `MR_MVKVS` gives monotonic reads (non-strict happens-before);
  - `RYW_MVKVS` gives read-your-writes;
  - `MW_MVKVS` gives monotonic writes.
- **MVCC** (`mvcc.rb`). `MVCCProtocol` = KVSProtocol plus input `new_transaction [:client]`, output `transaction_id_response [:transaction_id] => [:client]`, input `commit [:transaction_id] => [:client]`, and output `aborted_transactions [:transaction_id] => [:client]`.
  - Transaction IDs come from `Counter` + `FIFOQueue`.
  - Snapshot reads work through `snapshot_lookup`.
  - Write-write conflicts abort a transaction: the younger one if it wrote second (the rule compares tx ids).
  - Old versions are garbage-collected when the oldest active transaction commits.
  - Variants: `BasicMVCC`, `ReplicatedMVCC`.

### 6.5 Ordering and identity — `ordering/*.rb`

- **`NonceProto`** has output `nonce [] => [:ident]`: one unique value per tick.
  - `GroupNonce`: `permo.ident + budtime * member_count`, unique among group members.
  - `TimestepNonce`: `(Time.now.to_i << 16) + budtime`.
  - `NNonce`: a counter table.
  - `SNNonce` is annotated as broken ("I thought the below would work").
- **`SerializerProto` / `Serializer`**: inputs `enqueue [:ident] => [:payload]` and `dequeue [] => [:reqid]`; output `dequeue_resp [:reqid] => [:ident, :payload]`. On each dequeue it returns the minimum ident (`group(nil, min(ident))`) and deletes it.
- **`AssignerProto`**: input `id_request [:payload]`; output `id_response [:ident] => [:payload]`.
  - `AggAssign` uses `accum` + `each_with_index`. It is **non-deterministic**: the comment says it is "based on the internal order in which facts appear in collections, which is outside the semantics of Bloom".
  - `SortAssign` uses `id_request.sort.each_with_index` and is **deterministic**.
  - `AggAssignPersist` and `SortAssignPersist` add a persistent high-water mark (`next_id <+- next_id + count`).
  - The deprecated `Assigner` is marked "seems not to work".
- **`PriorityQueueProtocol` / `PriorityQueue`**: inputs `push [:item, :priority, :queue]`, `remove [:item, :queue]`, `pop [:queue]`, `peek [:queue]`; outputs `remove_response`, `pop_response`, `peek_response`. `lowest = items.argmin([:queue], :priority)`, and ties are broken with `argagg(:choose, ...)`.
- **`FIFOQueueProtocol` / `FIFOQueue`**: imports `PriorityQueue` and uses `budtime` as the priority.
- **`SequencesProtocol` / `Counter`**: inputs `increment_count [:ident]`, `clear_ident [:ident]`, `get_count [:ident]`; output `return_count [:ident] => [:tally]`. Implemented with `total_counts <+- ... tally+1`.
- **`LamportInterface` / `LamportClockManager`**: input `to_stamp [] => [:msg]`, output `get_stamped [:msg] => [:lamportmsg]`, input `retrieve_msg [:lamportmsg] => []`, output `msg_return [:lamportmsg] => [:msg]`. The clock advances by `to_stamp.length`; on receive, `clock + max(n, max_recv_clock + 1)`. Messages within a tick are numbered with `each_with_index`.
- **`VectorClock`**: a plain Ruby class used as a column value. It provides `increment`, `merge`, `happens_before`, `happens_before_non_strict` and `<=>` by max component. It is *not* a lattice; compare it with `CausalDelivery`, which uses `lmap`/`lmax`.

### 6.6 Paxos, leader election (incomplete) — `paxos/*.rb`

- **`LeaderMembership`**: every node starts as its own leader. Leader votes and member lists are multicast, and the leader becomes the minimum host seen (`member.group([], min(:host))`, `leader <+- new_leader`). Counters give messages unique IDs.
- **`LeaderElection`**: combines MajorityVotingMaster, VotingAgent, GroupNonce, StaticMembership and ProgressTimer. States are follower, election and leader, with a `view` number. When the progress timer fires, the node starts an election at a higher view. The majority "victor" becomes leader. `channel :proof` carries view proofs, and the timeout doubles on each attempt.
- **`PaxosPrepare` / `PaxosPrepareAgent`**: the prepare phase only, and still leaves debugging `print`s in the rules. Its vocabulary (`aru`, `global_history`, `last_installed`, `datalist`, `view`) appears to follow Amir & Kirsch's "Paxos for System Builders", as the earlier Overlog Paxos did. That attribution is my inference from the identifiers; the repo does not say so. There is **no accept or learn phase**.

### 6.7 Chord DHT — `chord/*.rb`

- `ChordNode`: tables `me [] => [:start, :pred_id, :pred_addr]`, `finger [:index] => [:start, :hi, :succ, :succ_addr]`, `localkeys`; Ruby helpers `in_range`, `at_successor`, `at_local`, `at_finger`.
- `ChordSuccPred`: asks a peer for its successor and predecessor. `sp_req` carries a `hops` count, and requests time out after `periodic 5`.
- `ChordFind`: recursive lookup ("the chord people call this 'recursive' lookup in Section 6.1"). Interfaces: `succ_req [:key]` → `succ_resp [:key] => [:start, :addr]`, and `pred_req` → `pred_resp`. The closest finger is chosen with `argmax([key], index)`.
- `ChordJoin` implements the Section 4 algorithm of the Chord paper and is marked "for reference/experimentation only". `ChordStabilize` implements the Section 5 stabilization protocol: `periodic stable_timer 2`, proxy join, `succ_notify`, key transfer with acknowledgements, and `fix_finger`. `ChordSuccessors` maintains successor lists and swaps in a new `finger[0]` on timeout.

### 6.8 Shopping carts — `cart/*.rb` (the CIDR/BloomUnit running example)

- `CartProtocol` channels:
  - `action_msg [:@server, :client, :session, :reqid] => [:item, :cnt]`
  - `checkout_msg [:@server, :client, :session, :reqid]`
  - `response_msg [:@client, :server, :session] => [:items]`
- `CartClientProtocol`: inputs `client_action`, `client_checkout`; output `client_response`. `CartClient` forwards these over the channels.
- `DestructiveCart`: KVS-based. It merges `{item => cnt}` hashes using an outer join with `kvget_response`.
- `DisorderlyCart` (current version):
```ruby
module DisorderlyCart
  include CartProtocol
  state do
    table :action_log, [:session, :reqid] => [:item, :cnt]
    scratch :item_sum, [:session, :item] => [:num]
    scratch :session_final, [:session] => [:items, :counts]
  end
  bloom :on_action do
    action_log <= action_msg {|c| [c.session, c.reqid, c.item, c.cnt] }
  end
  bloom :on_checkout do
    temp :checkout_log <= (checkout_msg * action_log).rights(:session => :session)
    item_sum <= checkout_log.group([:session, :item], sum(:cnt)) do |s|
      s if s.last > 0
    end
    session_final <= item_sum.group([:session], accum_pair(:item, :num))
    response_msg <~ (session_final * checkout_msg).pairs(:session => :session) do |c,m|
      [m.client, m.server, m.session, c.items.sort]
    end
  end
end
```
  `ReplicatedDisorderlyCart` adds replication: `mcast_send <= action_msg`, and `action_log <=` from both `mcast_done` and `pipe_out`.
- **Monotone cart** (`monotone_cart.rb` + `cart_lattice.rb`), **checkout as a monotone operation**:
  - The custom lattice `lcart` maps `op_id → [ACTION_OP, item, mult]` or `[CHECKOUT_OP, lbound, addr]`.
  - `is_complete` (monotone) holds when a checkout exists and every ID in `lbound..checkout_id` is present.
  - `summary` is the sum of actions per item, keeping only positive counts.
  - The replica is a single `lmap :sessions`. Its rule `response_msg <~ sessions.to_collection{|s, cart| cart.is_complete.when_true{...}}` needs no coordination. This is the design that the BloomUnit "manifest" fix (§7) points to.
  - Illegal inputs raise errors: an action outside the bounds, or a second, different checkout.

### 6.9 Other examples

- **MI cache coherence** (`cache_coherence/mi/*.rb`): a directory-based Modified/Invalid protocol.
  - Channels: `cdq_REX`, `cdq_WBD`, `dcp_REXD`, `dcp_WBAK`, `dcp_NAK`, `dcq_INV`, `cdp_INVD`.
  - The directory keeps one table per state (`dsINV`, `dsEXC`, `dsBEX`) and moves between them "atomically" with `<=` into the new state and `<-` from the old one.
  - This is a good test of state-machine encoding.
- **StateMachine** (`statemachine/statemachine.rb`):
```ruby
module StateMachine
  state do
    table   :states, [:name] => [:accepts]
    table   :xitions, [:from, :to, :event]
    table   :current, [] => [:name]
    interface input, :event, [:name]
    interface output, :result, [:accepted]
  end
  bloom do
    result <= (current*states).pairs(current.name=>states.name) {|c,s| [s.accepts]}
    current <+- (current*event*xitions).combos(current.name=>xitions.from,
                                               event.name=>xitions.event) {|c,e,x| [x.to]}
    current <+- event {|e| ['start'] if e.name == 'reset'}
  end
end
```
- **Chat** (`chat/`, also `bud/examples/chat`): a server that fans out `mcast` to `nodelist`, with clients reading and writing through `stdio`.
- **Twitter clone "Bleet"** (`twitter/`): commands are demultiplexed from a channel into scratches, session cookies are checked with an outer join, and state lives in `sync :dbm` tables.
- **`lattices/vc_scenario.rb`**: a three-node scenario that detects a causal-order violation, `r.clock.lt_eq(my_vc).when_true`, with simulated network delay buffers.

### 6.10 BFS: a GFS/BOOM-FS-style file system in Bloom — `bfs/*.rb`, `bud/docs/bfs.md`

Architecture: one master holds the metadata, and datanodes store replicated chunks. The bulk data path, including datanode-to-datanode pipelining, is **plain Ruby TCP**, not Bloom, as it was in BOOM-FS.
- `FSProtocol`: inputs `fsls [:reqid, :path]`, `fscreate [] => [:reqid, :name, :path, :data]`, `fsmkdir [] => [:reqid, :name, :path]`, `fsrm [] => [:reqid, :name, :path]`; output `fsret [:reqid, :status, :data]`.
- `KVSFS` (FSProtocol + BasicKVS + TimestepNonce) stores the tree in the KVS:
  - keys are full paths;
  - a directory's value is an array of its children;
  - create and mkdir update the parent entry and the new entry **in the same fixpoint**, through two `kvput <=` inserts;
  - `rm` checks that the directory is empty.
- `ChunkedFSProtocol` adds inputs `fschunklist [:reqid, :file]`, `fschunklocations [:reqid, :chunkid]`, `fsaddchunk [:reqid, :file]`. `ChunkedKVSFS` mints chunk IDs from `nonce`, returns `pref_list[0, REP_FACTOR+2]`, and fails if `available` has fewer than `REP_FACTOR` nodes.
- `HBMaster` (includes HeartbeatAgent):
  - `chunk_cache [:node, :chunkid] => [:time]` is built from heartbeat payloads;
  - `chunk_cache_alive` holds entries younger than `OLD`;
  - `available [] => [:pref_list]` is the `accum` of live peers;
  - heartbeats are acknowledged through `hb_ack`.
- `BFSDatanode`: polls its data directory on `hb_timer` and sends newly seen chunk IDs as the heartbeat payload until the master acknowledges them.
- `BFSBackgroundTasks` (`background.rb`) re-replicates:
  - `chunk_cnts_chunk` counts replicas per chunk, and `lowchunks` holds chunks with fewer than `REP_FACTOR` replicas;
  - the destination is the least-full node without the chunk (`argagg(:min, ...)` then `choose`);
  - the source is chosen arbitrarily;
  - the output interface `copy_chunk [:chunkid, :owner, :newreplica]` is consumed by a Ruby callback.
- `BFSClientProtocol` / `BFSMasterGlue`: channels `request_msg [:@master, :source, :reqid, :rtype, :args]` and `response_msg [:@source, :master, :reqid, :status, :response]`. `BFSClient` is Ruby that wraps a Bud instance: `dispatch_command`, `do_append` / `do_read` with retries.
- Config: `REP_FACTOR=2`, `CHUNKSIZE=100000`, `MASTER_DUTY_CYCLE=1`.

### 6.11 Not present in bud-sandbox

MapReduce/BOOM-MR, Raft, a complete Paxos, quorum reads/writes, and a standalone lease or timer service are all absent. What exists is described above: the lattice `QuorumVote` in the cheat sheet, `MajorityVotingMaster`, and `ProgressTimer`. The repo contains no `mapreduce` directory at any commit in its history.

---

## 7. BloomUnit (DBTest 2012)

System (Fig. 1): **Bloom module + input constraints → Alloy solver → inputs 1..n → `BloomUnit::Simulate` → runs → `BloomUnit::Verify` against the test specification → failures (witnesses).**

**Test specification semantics (§3):**
- "A test specification is a Bloom program that does not use any temporal operators (`<~`, `<+` and `<-`) and has a single output interface `fail`. By convention, deriving a tuple into `fail` indicates a violation of the specification."
- "BloomUnit automatically creates a 'log table' for every collection in the tested program. Each log table has the suffix '_log' and contains all the tuples ever inserted into the collection, along with a `time` column that records the (node-local) timestep when the corresponding fact was derived."
- Because a spec is an ordinary Bloom program without temporal operators, it is a "one-shot" query that the unmodified runtime evaluates in a single tick over the trace.

Example, FIFO delivery (Fig. 4, verbatim):
```ruby
module FIFOSpec
  bloom do
    fail <= (pipe_out_log * pipe_out_log).pairs do |p1, p2|
      if p1.src == p2.src and p1.dst == p2.dst and
         p1.ident < p2.ident and p1.time >= p2.time
        ["out-of-order delivery: #{p1.inspect} < #{p2.inspect}"]
      end
    end
  end
end
```

**Input generation (§4):**
- "The set of inputs that a Bloom module receives over time during a test run may be viewed as a database instance". Each input relation gets two extra key columns, **`time` and `location`**.
- BloomUnit derives default Alloy constraints from the collection declarations: arity and key constraints, with string-valued columns.
- Users then add **exclusion constraints**, which rule out impossible inputs. Examples: the FD `src, ident → payload`, and "no spoofing", `p.src = p.location`.
- Users add **inclusion constraints** to force interesting inputs, for example at least two messages from the same source to the same destination.
- Fig. 5:
```
// Exclusion Constraints
all p1 : pipe_in | p1.src = p1.location
all p1, p2 : pipe_in | (p1.src = p2.src and p1.ident = p2.ident) ⇒ p1.payload = p2.payload
// Inclusion Constraints
some p1, p2 : pipe_in | p1 ≠ p2 ⇒ (p1.src = p2.src and p1.dst = p2.dst)
```
- Alloy enumerates models, with symmetry breaking so that the inputs are non-isomorphic.

**Exploring non-determinism (§5):**
- "Because Bloom relegates all nondeterminism to channel reorderings and omissions, the exploration of all possible executions reduces to the exploration of all possible message orderings and losses."
- **CALM pruning:** "Assuming no message omissions, monotonic code will produce the same output for any given input … exploring a single delivery order is sufficient to test a monotonic program fragment." More precisely: "A distributed Bloom program can only produce different outputs for the same inputs when a nonmonotonic operation follows an asynchronous operation in the program's dataflow … we need only explore all delivery orderings for messages that could concurrently be in-flight and destined for an agent at which a nonmonotonic operation will process the message." The worked example is A→B log, C→B counting, B→D: reordering M1∪M2 matters, reordering M3 does not.
- Exhaustive search is intractable even so, so BloomUnit **samples**:
  - Every node is simulated in one process, and channels are buffered.
  - "Each time the system quiesces [all nodes at fixpoint, no messages to send], the runtime delivers a randomly chosen subset of the buffered messages on all channels. At the end of the execution, a specified number of messages are guaranteed to have been dropped."

**Case study (§6, the disorderly cart):** a sequence of bugs, each found by the tool and fixed with a constraint or a design change.
1. Arbitrary strings were fed to `sum` → constrain `action ∈ {−1, 1}`.
2. The `(session, reqid)` key of `action_log` was violated → add a uniqueness constraint.
3. There was no output for sessions without a checkout → "every action has some checkout". This could instead be treated as a bug and fixed with an outer join.
4. Minimum-size inclusion constraints.
5. Correctness spec `CartSpec` (Fig. 8, below) → exposed checkouts that arrive before late actions → constraint `a.time < c.time`.
6. Exploring reordering exposed that actions and checkout race at the server. **Fix: the client sends a "manifest" of required actions with the checkout, and the server waits until the manifest is satisfied.**

The paper's closing observation: after this manual fix, the conservative CALM analysis **still** flags the program. BloomUnit supplies "a concrete 'witness' of the incorrect runs predicted by CALM analysis" and "empirical evidence" once the program is repaired.
```ruby
module CartSpec
  state do
    scratch :adds, [:session, :item, :cnt]
    scratch :dels, [:session, :item, :cnt]
    scratch :itemcnt_final, [:session, :item, :cnt]
  end
  bloom do
    adds <= client_action_log {|l| l if l.action == 1}.group([:session, :item], count(:reqid))
    dels <= client_action_log {|l| l if l.action == -1}.group([:session, :item], count(:reqid))
    itemcnt_final <= (adds * dels).outer.pairs(:session => :session, :item => :item) do |l, r|
      deletes = r.cnt.nil? ? 0 : r.cnt
      [l.session, l.item, l.cnt - deletes] if l.cnt - deletes > 0
    end
    fail <= (itemcnt_final * client_response_log).pairs(:session => :session) do |c, r|
      cnt = r.items.find{|i| i.first == c.item}[1]
      ["#{cnt} vs #{c.cnt}"] if cnt != c.cnt
    end
  end
end
```
**Discussion points from the paper:**
- Specs can check "loose equivalence" of two implementations (the destructive and disorderly carts).
- The authors deliberately write specs *independently* ("multiversion programming") rather than deriving them automatically by removing temporal operators, because a spec derived that way is "either tautological or incomplete".

---

## 8. A standalone-syntax precedent: bloom-compiler (2014)

`bloom-lang/bloom-compiler` is Josh Rosen's work-in-progress compiler. It shows how a standalone, **typed** Bloom looks. Example (`shortest-paths.bloom`, verbatim):
```
input link, [from: string, to: string, cost: int]
table path, [from: string, to: string, nxt: string, cost: int]
output shortest, [from: string, to: string, nxt: string, cost: int]
path <= link {|l| [l.from, l.to, l.to, l.cost]}
path <= (link * path) on (link.to == path.from) { |l, p|
    [l.from, p.to, l.to, l.cost+p.cost]
}
shortest <~ path.argmin([path.from, path.to], path.cost, intOrder)
```
- **Grammar** (`BudParser.scala`):
  - Collection types include `input`/`output`/`table`/`scratch`/…; declarations take the form `type name, [f: type, ...] => [f: type, ...]`.
  - Operators: `<=` `<+` `<~` `<-` `<+-`.
  - Joins: `(c1 * c2 ...) on (pred, ...) {|vars| [exprs]}`.
  - Other forms: `a.notin(b)`, `c.argmin([fields], expr, orderFn)`, `c {|t| [exprs]}`.
  - Comments start with `//`.
- **Phases:** parse → resolve names → assign types (unification) → dependency analysis → stratify → dataflow IR → invalidation/rescan sets → codegen (GraphViz or an RxJS "rxflow" runtime).
- **Function properties as types** (`FunctionProperty.scala`): `Pure`, `Associative`, `Commutative`, `Idempotent` give `SemilatticeMerge`; `Pure`, `Reflexive`, `Antisymmetric`, `Transitive` give `PartialOrder`. `argmin` takes an explicit partial-order function (`intOrder`, `stringOrder`). This is a clean precedent for typing monotone and lattice functions in our language.
- **Execution plan notes (README):** per tick, (1) apply pending deletions and record whether anything was actually deleted, (2) look up the statically computed invalidation sets, (3) clear caches, (4) run each stratum to fixpoint. Termination detection works either through "end-of-round" punctuations in acyclic dataflows, or by buffering at tables in cyclic ones. That is the same design as Bud's executor, cleaned up.

---

## 9. Known limitations of Bloom/Bud, and implications for our design

1. **Ruby embedding.** Arbitrary Ruby inside rule blocks breaks both the analysis and determinism: side effects, `rand`, `Time.now`, hidden collection references. The analysis is conservative but misses what is hidden inside Ruby. → Our language should have a closed, pure scalar-expression sublanguage. Impure built-ins (`now()`, `rand()`, `budtime`) should be explicit effects or input relations.
2. **Syntactic monotonicity.** It gives false positives such as `inspected`, `keys` and `MIN(x) < 100`. → Adopt lattice typing: morphisms, monotone functions, threshold tests (from Bloom^L), plus partial-order and semilattice types.
3. **Runtime key checks.** Key constraints are checked at runtime and conflicts are exceptions. Concurrent same-key `<+-` updates in one tick raise errors (`ntest_wl5`). → Keep the runtime checks, but add static warnings where two rules can produce the same key, and optionally "last-writer" or lattice-merge policies declared per column.
4. **Nondeterministic operators.** `choose`, `choose_rand`, `each_with_index`, `sort`-then-index and `reduce` over a Ruby Hash depend on order. → Treat them as explicit nondeterministic choice (Dedalus `choice`) and label their outputs non-deterministic in the CALM analysis. `sort` with a total order is deterministic.
5. **Unguarded asynchrony.** Channels are scratches, so joining two channel streams depends on both arriving in the same tick. → Have the analysis report this: port `GuardedAsync`.
6. **Channel transport.** Channels are UDP datagrams with no size limit handling, no reliability and no backpressure. → Our channel layer should be pluggable (UDP/TCP/QUIC), always keep "unordered, may drop" semantics, and provide a deterministic simulator with a fault-injecting `channel_filter` equivalent for Molly-style LDFI and BloomUnit-style exploration.
7. **Performance.** Ruby, a push dataflow, and whole-tick group recomputation. → Compile to Rust dataflow with persistent arrangements, delta-driven aggregation, and deletion-aware invalidation (Bud's rescan/invalidate idea done properly).
8. **Self-join limit** of two occurrences per rule, left-deep joins only, no join-order optimisation.
9. **The sandbox's Paxos and 2PC are incomplete or were removed.** Our Raft, Paxos and 2PC must be designed from scratch. The voting and 2PC modules are still good API shapes (Protocol modules with `begin_vote`/`victor`, `request_commit`/`xact` status).

---

## 10. MUST-IMPLEMENT CHECKLIST

Each item: **ID — feature**: one-line precise description *(source)*.

**Program structure and modules**
- **BB-01 — Unordered rule sets**: a program is a set of rules; statement and block order carry no meaning *(CIDR §3.3; cheat.md)*.
- **BB-02 — `state` / `bloom` / `bootstrap` sections**: declarations, named rule blocks, and one-time rules evaluated at tick 0 before wiring, with `<=` visible in tick 0 *(cheat.md; lib/bud.rb#do_bootstrap)*.
- **BB-03 — Named rule blocks with override by name**: an including module's block of the same name replaces the included module's rules; duplicate names in one module are an error *(modules.md; tc_errors#test_dup_blocks)*.
- **BB-04 — `include` (flat mixin)** of modules, including state and rules *(modules.md)*.
- **BB-05 — `import M => :alias`**: independent namespaced instances, repeatable under different aliases, nested qualified access `a.b.c` *(modules.md; tc_module)*.
- **BB-06 — Protocol modules**: interface-only abstract modules, several implementations, composition chosen at instantiation; `??` underspecified detection *(modules.md; CIDR §3.4, §4.5)*.
- **BB-07 — Interface collections**: `input` / `output` interfaces with the direction recorded in the catalog; warnings for unused inputs and never-written outputs *(state.rb; bud_meta.rb#analyze_dependencies)*.

**Collections and schemas**
- **BB-08 — Schemas with keys**: `[k...] => [v...]`; array form means all-key; empty key means a singleton; default `[key]=>[val]`; schemas can be reused from other collections *(collections.rb#parse_schema)*.
- **BB-09 — Set semantics + key constraints**: duplicates ignored; distinct tuples with the same key in one tick raise `KeyConstraintError` (including across `<+` arrivals and channel send buffers) *(collections.rb#merge_to_buf; tc_collections; tc_channel)*.
- **BB-10 — Lattice-valued non-key columns merge** instead of conflicting on key collision; lattice values may not be keys *(collections.rb#merge_to_buf, #prep_tuple)*.
- **BB-11 — `table`**: persistent; tick start applies exact-match deletes, then key-deletes, then pending inserts *(collections.rb#BudTable#tick)*.
- **BB-12 — `scratch`**: semantically empty every tick; `<+` into a scratch appears in the next tick *(BudScratch#tick; tc_collections#test_simple_deduction)*.
- **BB-13 — `temp`**: an inline scratch declared in its defining rule, with the schema inferred from the rhs *(cheat.md; rewrite.rb#TempExpander)*.
- **BB-14 — `channel`**: exactly one `@` location column; lhs only via `<~`; receiver contents last one tick; `payloads` projection *(cheat.md; BudChannel)*.
- **BB-15 — `loopback`**: a channel to self that arrives in a future tick *(state.rb#loopback)*.
- **BB-16 — `periodic`**: a best-effort timer scratch `(id, wallclock)`, rhs only *(cheat.md; make_periodic_timer)*.
- **BB-17 — `stdio` / terminal**: rhs reads stdin lines; `<~` writes to stdout at the end of the tick *(cheat.md; BudTerminal)*.
- **BB-18 — `halt`**: insertion stops the instance at the end of the tick; `[:kill]` stops the process *(cheat.md; do_startup)*.
- **BB-19 — `localtick`**: a built-in loopback for self-driven ticks *(builtin_state)*.
- **BB-20 — Durable `sync` collection**: persistent storage flushed synchronously at every tick end, supporting `<= <+ <-` *(cheat.md; storage/dbm.rb)*.
- **BB-21 — Read-only sources**: `readonly` and `file_reader` (`[:lineno]=>[:text]`) with a lhs-use error *(state.rb; BudReadOnly)*.
- **BB-22 — Catalog relations**: `t_rules`, `t_depends(nm, in_body)`, `t_provides`, `t_stratum`, `t_rule_stratum`, `t_cycle`, `t_table_info`, `t_table_schema`, `t_underspecified`, queryable by programs *(lib/bud.rb#builtin_state)*.

**Operators**
- **BB-23 — `<=` instantaneous merge**, recursive, evaluated to fixpoint *(CIDR Fig. 1)*.
- **BB-24 — `<+` deferred merge** into the next local tick *(CIDR Fig. 1)*.
- **BB-25 — `<-` deferred delete**, exact tuple match, tables and durable collections only *(CIDR Fig. 1; BudTable#tick; tc_collections#test_delete_key)*.
- **BB-26 — `<+-` / `<-+` deferred upsert**: delete by lhs key, then insert, atomically at the next tick *(cheat.md; BudTable superator)*.
- **BB-27 — `<~` async merge**, to channels, stdio or durable async stores only; rhs tuples may arrive at different future ticks *(operational.md)*.
- **BB-28 — Operator/collection legality matrix**, enforced at compile time (§2.2 table) *(collections.rb superators; tc_errors)*.
- **BB-29 — Temporal rules in the final stratum**: `<+ <- <~ <+-` rules are evaluated after all strata reach fixpoint *(bud_meta.rb#meta_rewrite)*.
- **BB-30 — External insertion only via `<+` / `<~`** between ticks; `<=` from outside a tick is an error *(History 0.9.7)*.

**Rhs algebra**
- **BB-31 — Map/select/project** with a per-tuple expression; a null result filters the tuple *(cheat.md)*.
- **BB-32 — `flat_map`** (unnest) *(cheat.md)*.
- **BB-33 — Joins**: n-way `*` with `pairs`/`combos` (hash-pair, qualified and equivalence-class predicates), `matches` (natural join), `lefts`/`rights` (semi-join), `flatten`; no predicates means the Cartesian product *(cheat.md; join.rb)*.
- **BB-34 — Left outer join `outer`**, emitting nil-padded unmatched left tuples at stratum end; treated as non-monotonic *(join.rb#PushSHOuterJoin)*.
- **BB-35 — `notin` anti-join** with optional key pairs and an optional pair-predicate; whole-tuple equality when neither is given; negative dependency on the argument *(cheat.md; join.rb#PushNotIn)*.
- **BB-36 — `group` with aggregates** `min max bool_and bool_or choose choose_rand sum count avg accum accum_pair`; set-semantics input dedup; nil grouping means one global group; several aggregates; multi-column aggregates *(aggs.rb; group.rb)*.
- **BB-37 — `argagg` / `argmin` / `argmax`**: return all exemplar tuples per group; ties kept; only exemplary aggregates allowed *(aggs.rb; group.rb#PushArgAgg)*.
- **BB-38 — `reduce(init)`**: a general fold returning a collection; memo re-initialised on invalidation *(elements.rb#PushReduce)*.
- **BB-39 — `sort` / `each_with_index`**: order-sensitive operators flagged non-deterministic or non-monotonic *(elements.rb; ordering/assigner.rb)*.
- **BB-40 — Membership tests** `include?`, `exists?`, `empty?`, `has_key?`, key lookup, as explicit (anti-)join constructs with non-monotonic marking *(cheat.md; rewrite.rb)*.
- **BB-41 — Utility projections**: `keys`, `values`, `payloads`, `inspected`, `schema`/`cols`/`key_cols`/`val_cols`, `rename` *(cheat.md)*.
- **BB-42 — Tuple semantics**: named and positional access, nil-padding of short tuples, error on long tuples, `+` concatenation *(collections.rb#prep_tuple; tc_collections)*.
- **BB-43 — Literals on the rhs**: tuple arrays, maps for lmap, scalars for lattices *(rewrite.rb#lambda_rewrite; History 0.9.6)*.
- **BB-44 — Built-in scalars**: `budtime` (local tick), `bud_clock` (tick-stable wall clock), `ip_port`; impure functions force rescan *(lib/bud.rb; UnsafeFuncRewriter)*.

**Time and runtime**
- **BB-45 — Three-phase tick**: setup (clear scratches, apply deferred ops, ingest a batch of inbound messages and timers) → stratified fixpoint → transition (stage deferred ops, send async, flush durable storage, callbacks) *(operational.md; tick_internal)*.
- **BB-46 — Event-driven ticking**: a tick runs on message, timer or host request; channel inputs are batched per tick *(server.rb; operational.md)*.
- **BB-47 — Execution modes / host API**: single-step `tick`, `run_bg`, `run_fg`, `stop`, `pause`, `sync_do`/`async_do` (run between ticks, then tick), `register_callback`, `sync_callback`, `delta` *(lib/bud.rb; ruby_hooks.md)*.
- **BB-48 — Channel filter hook**: a per-channel, per-delivery user function that accepts, postpones or drops messages, used for fault injection and test reordering *(lib/bud.rb options; server.rb; tc_channel; tc_causal_delivery)*.
- **BB-49 — Wire format and addressing**: `host:port` location specifiers, a per-tuple message that names the destination collection, and serialisation of lattice values inside tuples *(BudChannel#flush; History 0.9.6)*.

**Analysis and compilation**
- **BB-50 — Dependency extraction**: per rule and per body-collection edges with op, `nm` and `in_body` flags *(rewrite.rb#RuleRewriter)*.
- **BB-51 — Non-monotonicity classification**: aggregation, negation (`notin` argument), deletion, outer join, order-sensitive ops, and membership tests are nm; lattice morphisms and monotone functions are not *(rewrite.rb MONOTONE_WHITELIST; CIDR §2)*.
- **BB-52 — Stratification** over `<=` edges only; unstratifiable = a cycle through an nm edge with no temporal edge; minimal rule placement per body strata *(bud_meta.rb; operational.md "glass" example)*.
- **BB-53 — Semi-naive, delta-driven fixpoint per stratum** with persistent symmetric hash-join state *(executor/*.rb)*.
- **BB-54 — Cross-tick incremental caches with deletion-driven invalidation** (static invalidate/rescan sets) *(executor/README.rescan; prepare_invalidation_scheme)*.
- **BB-55 — Points-of-order analysis**: a dependency graph with temporal, async and nm edges; temporal clusters (SCCs with nm and temporal edges); every nm edge and every edge incident to a cluster is a point of order *(CIDR §4.4)*.
- **BB-56 — Path labels M/Bot, A, N, D**: an A→N sequence yields D; the output label is the join over paths; the report lists the async edge to coordinate *(labeling.rb; meta_algebra.rb; bin/budlabel)*.
- **BB-57 — Guarded-asynchrony check**: two channel streams that meet without both passing through persistent state make the output divergent *(labeling.rb#GuardedAsync; tc_labeling)*.
- **BB-58 — Coordination-module annotation**: a way to mark hand-verified coordination modules so the analysis skips them *(CIDR §2)*.
- **BB-59 — Guess/guarantee taint propagation** (optional, designed but never built in Bud) *(CIDR §6)*.

**Tooling**
- **BB-60 — Dataflow visualiser (budplot equivalent)**: tables as rectangles, ephemeral collections as ovals, clusters as octagons; `+/−`, dashed and circle edges; S/T nodes; `??` for underspecified; nodes coloured by path label *(visualizations.md; graphs.rb; CIDR Fig. 8)*.
- **BB-61 — Execution tracing and replay visualiser (budvis/budtimelines equivalent)**: per-tick snapshots of every collection, plus a cross-node message timeline *(visualizations.md; viz.rb; bin/budtimelines)*.
- **BB-62 — REPL (Rebl equivalent)**: add collections and rules interactively; `/tick [n]`, `/run`, `/stop`, `/lsrules`, `/rmrule n`, `/lscollections`, `/dump c`, `/help`; a `breakpoint` scratch halts `/run` at the end of a tick *(rebl.md; rebl.rb)*.
- **BB-63 — Dump rewritten program / strata / wiring** for debugging *(options :dump_rewrite, :print_wiring)*.

**Testing (BloomUnit)**
- **BB-64 — Automatic `X_log` trace tables** (all tuples ever, plus node-local `time`) for every collection *(BloomUnit §3)*.
- **BB-65 — Spec programs**: no temporal operators, a single `fail` output, evaluated once over traces *(BloomUnit §3)*.
- **BB-66 — Constraint-driven input generation**: inputs as a relational instance with `time`/`location` columns; default constraints from schemas and keys; user exclusion and inclusion constraints; a model finder or SAT back-end (Alloy in the paper) *(BloomUnit §4)*.
- **BB-67 — Deterministic multi-node simulator**: buffered channels; at quiescence deliver a random subset; a guaranteed number of drops; CALM-pruned exploration (reorder only messages feeding nm-after-async) *(BloomUnit §5)*.

**Standard library (bud-sandbox parity)**
- **BB-68 — Delivery family**: `DeliveryProtocol` + best-effort, reliable (retransmit and ack), demonic (drop %), dastardly (reorder and delay), causal (vector-clock lattices) *(bud-sandbox/delivery)*.
- **BB-69 — Multicast** (best-effort and reliable) on top of Delivery + Membership *(delivery/multicast.rb)*.
- **BB-70 — Membership (static), heartbeat with expiry, one-shot progress timer** *(membership/, heartbeat/, timers/)*.
- **BB-71 — Voting**: unanimous and majority masters, overridable agent decision; 2PC on top of voting; lock manager *(voting/; 2pc (hist); lckmgr (hist))*.
- **BB-72 — KVS family**: basic, persistent, replicated, multi-version with causal/MR/RYW/MW reads, MVCC *(kvs/)*.
- **BB-73 — Ordering utilities**: nonces, serializer, deterministic ID assignment (sort-based), priority and FIFO queues, counters, Lamport clocks, vector clocks *(ordering/)*.
- **BB-74 — Shopping carts**: destructive, disorderly, and a lattice-monotone checkout *(cart/)*.
- **BB-75 — Chord DHT** (find, stabilize, successors) and **BFS** (GFS-style file system: FS metadata on the KVS, chunks, heartbeat master, re-replication) *(chord/; bfs/; docs/bfs.md)*.

---

## 11. TEST PROGRAMS

These are end-to-end tests for our engine, taken from the literature and the Bud and bud-sandbox test suites. Each entry gives the source and the expected behaviour. Port each one to the new syntax and keep the expected outputs.

**Core semantics**
1. **Shortest paths** (`bud/examples/basics/paths.rb`). `link = {(a,b,1),(a,b,4),(b,c,1),(c,d,1),(d,e,1)}`, `path` is recursive, `shortest <= path.argmin([from,to], cost)`. After one tick, `shortest` = `(a,b,b,1) (a,c,b,2) (a,d,b,3) (a,e,b,4) (b,c,c,1) (b,d,c,2) (b,e,c,3) (c,d,d,1) (c,e,d,2) (d,e,e,1)`. After adding `(e,f,1)` and ticking again, it also contains `(a,f,b,5) (b,f,c,4) (c,f,d,3) (d,f,e,2) (e,f,f,1)`, and there is no key conflict because no existing minimum changes.
2. **All-paths in REBL** (`docs/rebl.md`). The same links, `path [:from,:to,:next,:cost]`. The first `/tick` prints exactly 14 path tuples, including both `(a,b,b,1)` and `(a,b,b,4)`, and `(a,e,b,7)`.
3. **BabyBud tick semantics** (`tc_collections#test_simple_deduction`).
   - After tick 1, `scrtch2` has 1 tuple.
   - After tick 2: `scrtch == [["c","d",5,6]]` (the `<+` arrived and the bootstrap tuples are gone); `scrtch2` is empty; `tbl` holds exactly `(c,d,5,6)` and `(z,y,9,8)`, because the `<-` of `(a,b,1,2)` and the `<+` of `(c,d,…)` both took effect; `the_keys` = `[[c,d],[z,y]]`.
4. **Key conflict** (`DupKeyBud`): `tab <= [[2000,'bush']]; tab <= [[2000,'gore']]` raises `KeyConstraintError` on the first tick. The empty-key variant (`EmptyPk`): a second distinct tuple raises the error, and re-inserting the same tuple is fine.
5. **Exact-match delete** (`test_delete_key`): `t1 = {[5,10]}`. `del_buf <+ [[5,11]]` never deletes. `[5,10]` is deleted two ticks after it is inserted into `del_buf` (one tick for `<+`, one for `<-`).
6. **Upsert** (`TestUpsert`): `joe={[1,'a']}`, `t1d={[1,'b']}`, `joe <+- t1d`. Tick 1: `joe==[[1,'a']]`. Tick 2: `joe==[[1,'b']]`. `<-+` behaves the same.
7. **Unstratifiable** (`operational.md`): `glass <= one_item {|t| ['full'] if glass.empty?}` must be rejected at compile time. The same program with `<+` instead of `<=` must be accepted: temporal stratification.
8. **Outside-tick `<=` rejected** (`test_instant_merge_outside_bud`), and `<+` / `<-` / `<=` into a periodic rejected (`test_periodic_lhs_error`).
9. **Nil padding and overlong tuples** (`test_pad_missing_field`, `test_too_many_columns`).
10. **Halt** (`tc_halt`): `tbl <+ tbl{key+1}`, `halt <= tbl{key==2}`, so the instance stops at the tick where key 2 appears.

**Networking**
11. **TickleCount** (`tc_channel`): a loopback counter from 0 to 5 and a self-addressed channel. Expect `loopback_done == [[5]]` and `mcast_done == [[5]]`. There must be **exactly 2 strata**, and the dependency `mcast ← loop_chan` must be monotone (`nm == false`).
12. **Ring of 10** (`tc_channel#test_basic_ring`): a token is incremented around a ring until `cnt == 39`. At the end, node *i*'s `last_cnt == 30+i`, and the last node derives `done`.
13. **Channel key constraint at the sender** (`test_channel_with_key`): two `<~` sends with the same key and different values in one tick raise `KeyConstraintError`. `payloads` returns tuples without the address.
14. **Channel filter** (`test_filter_drop`, `test_filter_batch`): with a filter that accepts only `val == 3`, exactly `[[dst,3]]` is delivered. A filter that postpones until 12 messages are buffered delivers all 12 in one tick.

**Protocols (bud-sandbox)**
15. **Reliable delivery** (`tc_reliable_delivery`): 4 messages ("aa".."dd" style) to a live peer are all delivered (`recv_log` equals the sent tuples) and acknowledged, and `buf` is empty after one more tick. Sending to an unreachable `localhost:999` never produces `pipe_out`. Compose it with `DemonicDelivery` (50% drop) to test retransmission.
16. **Best-effort and reliable multicast** (`tc_multicast`): one `mcast_send [1,'foobar']` from a 3-member group gives `mcast_done` at the sender and exactly one `pipe_out` at each other member.
17. **Voting** (`tc_voting`):
    - With default agents, `vote_cnt.first == [1,'yes',2,{nil}]` and `vote_status == [1,'me for king','yes',{nil}]`.
    - With agents that vote manually, the status stays `'in flight'` after 1 of 2 votes. After the second it becomes `[1,'me for king','hell yes',{"madam","sir"}]`.
    - With `MajorityVotingMaster`, the victor is declared as soon as more than half have voted.
18. **2PC** (historical `tc_2pc`): the master plus 2 agents. `request_commit [1,"foobar"]` puts `xact` into `"prepare"`. One "Y" vote leaves it in "prepare"; the second "Y" moves it to `"commit"`. A single "N" moves it to `"abort"`.
19. **KVS** (`tc_kvs`, `kvs_workloads`):
    - Sequential puts to key "foo" of bar, baz, bam, bak (one per tick) leave `kvstate == [["foo","bak"]]`, and a get returns "bak".
    - With `ReliableReplicatedKVS` on 2 nodes, both replicas hold `"bak"`.
    - Deleting leaves `kvstate` empty.
    - The persistent KVS (`SSPKVS`) survives a restart on the same port and `dbm_dir`, with `kvstate == [["foo","bak"]]`. Upstream `test_persistent_kvs` is written this way but disabled with an early `return`, so we must make it pass for real.
    - **Negative test:** 4 puts to the same key delivered in one tick raise `KeyConstraintError` (disabled `ntest_wl5`).
20. **Carts** (`tc_carts`, `cart_workloads`):
    - `simple_workload`: meat +1, books −1, beer +1, diapers +1, meat −1, then 12 × beer +1. Checkout returns `[["beer",13],["diapers",1]]`.
    - Multi-session: session 666 gives `[["blue bottle",1],["cole",1]]`; session 555 gives `[["remedy",1],["sightglass",2]]`.
    - Monotone cart: incomplete until every op ID from `lbound` to the checkout ID has arrived, then `summary == [[5,1],[10,3]]`, and the rule stays in stratum 0.
    - An action after the checkout ID, an action before `lbound`, or a second different checkout is a `TypeError`.
21. **Causal delivery** (`tc_causal_delivery#test_reorder_simple`): src sends a, b, c, d, e. The filter drops c and holds everything until d has been seen. After d, the receiver has seen chn payloads d, then a, b and e. `pipe_out` delivers a, then b, and **never** d or e, because c is missing.
22. **Serializer and assigners** (`tc_ordering`):
    - Enqueue 1 foo, 2 bar, 3 baz. Dequeue 1234 returns `[1234,1,'foo']`, and dequeue 2345 returns `[2345,2,'bar']`.
    - `SortAssign` over 100 shuffled inputs always gives the IDs 0..99 in sorted order.
    - `SortAssignPersist` gives each batch contiguous ranges (0..99, 100..199, …).
    - `GroupNonce` with 3 members gives `t*3 + 1`.
23. **Lamport** (`tc_lamport`): stamping `foo` gives clock 0 and `bar` gives 1. After receiving a message with clock 20, the next stamp is 22.
24. **MVCC** (`tc_mvcc`): the happy path; write-write conflicts abort the correct transaction (`test_abort_me` / `test_abort_you`); snapshot isolation (an uncommitted snapshot is not affected by a later commit); aborted data disappears; GC removes versions that are no longer visible.
25. **MV-KVS** (`tc_mv_kvs`): causal, monotonic-read, read-your-writes and monotonic-write filtering over vector-clock versions.
26. **State machine** (`tc_statemachine`), **heartbeat** (`tc_heartbeat`), **membership** (`tc_member`), **Chord** (`tc_chord`: finger tables and successor/predecessor caches on a small ring, with the exact expected tuples in the test), **leader election** (`tc_leader`, `tc_leader_election`), **MI cache** (`tc_mi`), **BFS** (`tc_bfs`: mkdir/create/ls/rm semantics including rm of a non-empty directory failing; `tc_e2e_bfs`: many datanodes, client append and read, re-replication).
27. **Vector-clock scenario** (`lattices/vc_scenario.rb`): 3 nodes. Node 3 receives message 3 before message 1, and the program must print the causal-violation message.

**Analysis (golden outputs)**
28. **CIDR KVS/cart analyses:**
    - BasicKVS must show the temporal cluster `{kvstate, prev}` with points of order on every path from `kvput`.
    - DisorderlyCart must show *no* point of order on the `action_msg` replication path, and points of order at the `checkout_msg` ⋈ aggregate step and at the `accum` step.
    - The destructive cart must show a point of order on every action.
29. **CALM labels** (`tc_labeling`):
    - `TestNM`: output `{"response"=>"D"}`, paths `{"i1"=>"A","i2"=>"D"}`.
    - `TestGroup`: `{"response"=>"D"}`.
    - `TestMono`: `{"response"=>"A"}`.
    - `TestDeletion`: paths `dguard→response` and `i2→response` are D.
    - Guarded asynchrony: scratch⋈scratch and table⋈scratch are unguarded (D); table⋈table is guarded.
30. **Underspecification** (`docs/visualizations.md`): `ReplicatedKVS` alone warns `my_id`, `add_member`, `send_mcast` (input) and `mcast_done` (output) as underspecified. With `BestEffortMulticast StaticMembership` mixed in, it does not.

**BloomUnit-style specs**
31. **FIFO spec** (BloomUnit Fig. 4) against best-effort delivery under `DastardlyDelivery`: it must eventually derive `fail`. Against an ordered-delivery implementation it must never derive `fail`.
32. **CartSpec** (BloomUnit Fig. 8) against DisorderlyCart:
    - The spec passes when all actions precede the checkout at the server.
    - Exploring reorderings must find a failing schedule, where a checkout overtakes an action.
    - The "manifest" fix, or the lattice-monotone cart, must pass under every explored schedule.
