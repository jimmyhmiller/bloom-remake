# Syntactic sugar for writing trees and strings (S16)

**Goal (user, 2026-10-05):** make HTML (and SVG) pleasant to write in Blossom, with sugar that is general rather than
HTML-specific. Flappy Bird's drawing took 151 lines against Eve's 42 (S15): the id written on every row, the parent
threaded by hand, positions numbered by hand, and attribute strings built with `++ x.to_string() ++`. The user chose
all four features below (2026-10-05).

Every feature here is sugar: it lowers to statements, blocks and expressions the language already has, before name
resolution's checks, so analyses, provenance, both evaluators and the inspector see ordinary rules. Nothing new
reaches the IR.

## 1. String interpolation

```blossom
f"translate({X} {y}) rotate({tilt(v)})"
f"{n} item{if n == 1u64 { "" } else { "s" }} left"
f"{x:.2}"
```

- `f"…"` is a string literal whose `{expr}` holes hold any expression; `{{` and `}}` are literal braces; the escapes
  are a string's (§2.4).
- A hole's value is converted with `to_string`: a `String` as it is, integers and `f64` as their `to_string`
  (Appendix B), `bool` as `true`/`false`. Another type is a type error at the hole.
- `{x:.N}` (an `f64` only) writes `x` with exactly `N` digits after the point, rounding half to even on the exact
  binary value (as Rust's `{:.N}`; deterministic on every platform).
- Lowering: the literal parts and the converted holes, joined with `++`.

Elsewhere: error messages (`error(f"offset {o} out of range")`), keys and log lines in Kafka, ids
(`f"toggle-{n}"` instead of `id_of("toggle", n)`).

## 2. Nested heads

```blossom
table order(id: u64, customer: String) key(id);
table line(id: u64, sku: String, qty: u64) key(id, sku);

emit order(id: o, customer: c) {
    line(sku: "apple", qty: 2);       // line(id: o, sku: "apple", qty: 2)
    line(sku: "pear", qty: 1);
}
```

- A statement's head may be followed by a block of **child heads** (no verb: they take the parent's), each with named
  arguments, optionally with blocks of their own, and with `if`/`for` blocks among them.
- **Inheritance by name.** A child head inherits, from its enclosing heads (nearest first), every column it does not
  give whose name and type match a column of that head. A column given in the child wins over an inherited one; an
  inherited one wins over the column's default. A column neither given, inherited nor defaulted is BLS0303, as
  today.
- Lowering: one statement per head, each in the enclosing block (a child inside an `if`/`for` block is in that
  block), its inherited arguments copied from the parent's argument expressions.

Elsewhere: any one-to-many output with shared names flowing down: a Kafka produce response's topics, partitions and
records; an order and its lines; a plan and its steps.

## 3. Tree literals

A **tree** declaration names a node relation and the relations that hold a node's properties and content. The host's
`ui.bls` declares the page's:

```blossom
tree html {
    node elem(id, parent, pos, tag);
    props attr(id, name, value);
    content text(id, s);
}
```

`node` names the node relation's columns in the roles *id*, *parent*, *position* and *kind*; `props` a relation of
(node id, name, value); `content` one of (node id, value). A statement then writes a tree:

```blossom
page: while screen(s), bird(y, v), score(n) {
    emit html svg[id: "game"](viewBox: "10 0 80 100", width: 480) {
        rect[id: "sky"](x: 0, y: 0, width: 100, height: 95, fill: SKY);
        g[id: "bird"](transform: f"translate({X} {y}) rotate({tilt(v)})") {
            ellipse(rx: 5, ry: 4.2, fill: YELLOW);
            circle(cx: 2.2, cy: -1.5, r: 1.5, fill: "white");
        }
        if s == "game" {
            text[id: "score"](x: 50, y: 14) { n }
        }
        for obstacle_at(k, x, h) {
            g[key: k](transform: f"translate({x} 0)") { rect(width: 10, height: h); }
        }
    }
}
```

- An element is `KIND` (an identifier; `-` is allowed inside it, as in `font-face`), then optionally `[…]` with
  `id: e`, `key: e` and `pos: e`, then optionally `(name: value, …)` properties, then `{ … }` children or `;`. (Not
  `#id`: `#` starts a comment, LANGUAGE §2.2, kept for pasted Dedalus.)
- **Children** are elements, `if`/`for` blocks, fragment calls (§4) and content: a bare expression (`{ n }`, `{ "Hi" }`,
  `{ f"Score {n}" }`), which becomes the content row (at most one per element).
- **Parent and position.** A child's parent is the enclosing element's id (the root's is `""`, the mount point). Its
  position is its slot: the index of the child (or of the block or call it is in) among its siblings, counting from
  0, unless `[pos: e]` gives it. Siblings with one position order by id, so the children of a `for` that need an
  order give `[pos: …]`.
- **Ids.** `[id: e]` gives an id. Otherwise the id is derived: the parent's id, `/`, the kind, `.`, the
  slot, and `[k]` with `[key: k]` (`game/g.4[0]/rect.0`). An element without an id or a key inside a `for` block is an
  error (BLS0430: its rows would repeat one id), as is a key on an element outside one that has an id (BLS0431).
- **Properties** are rows of the props relation: one per `name: value`, the value converted with `to_string` (as an
  interpolation hole) when the relation's value column is a `String`. A property's name may hold `-`
  (`stroke-width: 0.5`) or be a string (`"aria-label": "Close"`).
- Lowering: per element, one node row, one props row per property, a content row for content, each a statement of
  the statement's verb in the enclosing block; `if`/`for` children are nested blocks as today.

Elsewhere: any tree written to relations with a declared shape: a document, an AST or plan a compiler in Blossom
emits, a scene graph, a menu, a file tree, Graphviz nodes and edges (a tree of clusters).

## 4. Fragments

```blossom
fragment pillar(k: u64, x: f64, h: f64) {
    g[key: k](transform: f"translate({x} 0)") {
        rect(width: 10, height: h);
        rect(x: -1, y: h - 5.0, width: 12, height: 5);
    }
}
```

- A `fragment` is a named, parameterized group of block items: statements, `if`/`for` blocks, nested heads, tree
  elements and calls of other fragments. A call `pillar(k, x, h);` stands where a statement (or, inside a tree, an
  element) can.
- **Meaning.** A call is the fragment's items in a block of their own whose body binds each parameter to its argument
  (`let k = …`): exactly what writing that block out would mean. Variables of the fragment are its own (they never
  capture the caller's). A fragment's tree elements are children of the element the call is in; called outside a tree
  they are an error (BLS0432).
- Fragments may not call themselves, directly or not (BLS0433): a call is expanded where it is written.
- Lowering: the call site's block (`H$frag#…` style, a nested block relation like `if`/`for`).

Elsewhere: reusable groups of statements: Kafka's "reply with the standard error fields", Raft's "send AppendEntries
to every peer", "log, then reject".

## 5. Record spread

```blossom
emit attr(id, ..{rx: 5, fill: YELLOW});        // attr(id, "rx", "5"), attr(id, "fill", YELLOW)
emit header(req, ..m);                           // m: Map<String, String>: one row per entry
```

- `..{name: value, …}` as a head's last argument stands for its relation's last two columns (a name, a value): one
  row per field, the name as a `String`, the value converted with `to_string` when the column is a `String`.
- `..e` with `e` a `Map<String, T>` is one row per entry, in the map's canonical order.
- Lowering: a statement per field; for a map, a `for (k, v) in e` block around one statement.

Elsewhere: entity-attribute-value relations: Kafka record headers, metric labels, configuration properties, flags.

## Diagnostics

| Code | Meaning |
|---|---|
| BLS0430 | a tree element inside a `for` block with neither an id nor a key |
| BLS0431 | a key on a tree element that has an id |
| BLS0432 | a fragment's tree elements where no tree encloses the call |
| BLS0433 | a fragment that calls itself (directly or through others) |
| BLS0434 | a tree declaration whose relations do not have the roles' shapes |
| BLS0435 | an interpolation hole's format spec other than `.N` (a hole of a type with no `to_string`, or `.N` on a non-`f64`, is the type error of the method it lowers to) |

## Slices

1. **Interpolation**, with `to_string` for `bool`.
2. **Nested heads** and **record spread** (both expand heads into heads).
3. **Tree literals** and the `tree` declaration; `ui.bls` declares `html`.
4. **Fragments.**
5. **Flappy and TodoMVC rewritten** with all of it; measured again against Eve; the comparison page updated.

Each slice: the grammar (LANGUAGE.md), the formatter, frontend tests, an integration test where it runs, and the
examples.
