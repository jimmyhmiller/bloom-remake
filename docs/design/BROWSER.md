# Blossom in the browser

**Goal (user, 2026-10-05):** show that Blossom is a general-purpose language, not only a distributed-systems one, the
way Kafka showed the systems claim: a complete TodoMVC, written in Blossom, running in the browser, in the style of
Eve (UI as relations). "If you can make a to-do MVC style UI in the browser, that'd be amazing."

## Decisions (user, 2026-10-05)

| | Decision |
|---|---|
| B1 | **Everything runs in the browser.** The compiler and the engine are compiled to WebAssembly; the page loads the app's `.bls` source, compiles it and runs it client-side. A thin JavaScript layer connects the engine to the DOM. No server beyond static files. (Every workspace crate on the path checks clean for `wasm32-unknown-unknown`.) |
| B2 | **UI as relations** (Eve's model). The program derives what the page shows as relations; the host diffs them into the DOM every round. DOM events arrive as input relations. |
| B3 | **The full TodoMVC spec**: add, toggle, toggle all, edit by double-click (Enter and blur save, Escape cancels, an empty title deletes), delete, the item count, clear completed, routing by URL hash (`#/`, `#/active`, `#/completed`), persistence in `localStorage`, the official TodoMVC CSS. |
| B4 | **A live `why` inspector**: point at any element and see why it is there: the rules, the events and the state it comes from (Blossom's provenance). |
| B5 | **A live source editor**: the app's source in the page, edited and re-run in place. |

No app-specific host code: the browser host is a generic layer any Blossom program can use; TodoMVC is one program.

## The model

### A program and the page

A browser app is an ordinary single-node Blossom program (role-free). Nothing new in the language: the host reads the
program's `output` relations (tick-local, recomputed every round, LANGUAGE §7.6) and writes its `input` relations, as
LANGUAGE §16.5 describes for any host.

**The page (outputs).** A program declares the outputs it uses, by name and schema; the host recognizes:

| Output | Meaning |
|---|---|
| `elem(id: String, parent: String, pos: i64, tag: String)` | An element `tag` with identity `id`, the `pos`-th child (by order of `pos`, then `id`) of `parent` (`""`: the app's mount point). |
| `attr(id: String, name: String, value: String)` | An attribute of an element. `value`, `checked` and `disabled` are set as properties (the DOM's live state), the rest as attributes. |
| `text(id: String, s: String)` | Text inside an element, before its children. |
| `focus(id: String)` | The element to focus after the round's changes (with its text selected at the end). |

**The world (inputs).** The host feeds the events a program declares and reads (an event no rule reads is not
listened to), each into a round of its own:

| Input | When |
|---|---|
| `boot()` | The first round (as everywhere). |
| `route(hash: String)` | At boot and on every `hashchange`: `location.hash`. |
| `click(id: String)`, `dblclick(id: String)` | On an element with an `id`, or its nearest ancestor with one. |
| `typed(id: String, value: String)` | Typing in a text field (the DOM's `input` event; `input` is a keyword). |
| `keydown(id: String, key: String, value: String)` | A key (DOM `key` names: `Enter`, `Escape`, …), with the field's value. |
| `blur(id: String, value: String)` | A field loses focus. |
| `change(id: String, checked: bool)` | A checkbox changes. |

`examples/web/ui.bls` declares these, for a program to `include`.

### A round

An event runs until its effects settle: the host steps the engine (incremental) with the event's row, then steps it
without events while the state changes (a write takes effect in the next round, so an `upsert` in an event's round
shows on the page a round later), at most 1000 rounds. It then diffs the page against the one before the event and
hands the DOM a list of patches (create, move, set or
remove an attribute or text, remove, focus). Elements are keyed by `id`, so an element keeps its DOM node (and its
focus and caret) across rounds while its id stays. Rounds are run one at a time; an event that arrives while a round
runs waits for it.

### Persistence

The program's `durable` tables are its persistent state. After each event the host writes them to `localStorage`
(JSON: each table's rows under its schema hash, the runtime's `schema_hash`), keyed by the program's name; at boot it
restores them, so a reload continues where it was. A table whose schema changed (in the editor) starts empty, and the
host says so.

### The inspector

The engine runs the rounds; provenance comes from the oracle, which re-runs a round with capture on (the pattern of
`blossom trace why`). The host keeps a bounded history of rounds (the last 500: the state each started from, its
events and the rows it wrote). In inspect mode, clicking an element asks for its derivation: the firings that
produced its `elem`, `attr` and `text` rows, recursively through views, down to the round's event and the table rows
it read; a table row leads back to the round that wrote it, that write's firing and that round's event. Generated
relations (a statement's expansion) are provenance-transparent: their reasons stand in for them, and a write by an
expansion's own rule (an `upsert`'s) is credited to the user rules that fed it. A fact explained once is referred to
afterwards ("explained above"). A row older than the history, or restored from storage, says so. The page shows the
tree beside the app; while inspecting, the app gets no input, and the tree follows the page as events arrive.

### The editor

The source is shown beside the app. Running it (a button, or Ctrl-Enter) compiles it in the page; diagnostics are
listed with their positions, and clicking one selects its span in the source. A program that does not compile is not
run (the old one keeps running). A successful compile replaces the running program, keeping each durable table whose
schema is unchanged (one that changed starts empty, and the editor says so). The edited source is kept in
`localStorage` across reloads until it is reverted to the app's own.

## Architecture

- **`crates/blossom-web`**: the host, in Rust. A pure core (compile from source in memory, the round loop with the
  engine, the page diff, persistence, the inspector's replay) that native tests drive; and a thin `wasm-bindgen`
  API over it (strings and JSON in and out), built as a `cdylib` for the browser.
- **`web/`**: the page. `index.html`, `host.js` (DOM events in, patches out, `localStorage`, hash routing, the
  inspector and editor panels), and the vendored TodoMVC CSS (`todomvc-app-css`, MIT).
- **`examples/web/todomvc.bls`**: TodoMVC, in Blossom.

Build: `cargo build -p blossom-web --target wasm32-unknown-unknown --release`, then `wasm-bindgen --target web`
(0.2.114, matching the crate) into `web/pkg/`. The evaluator recurses deeply, so the wasm module gets a large stack
(linker `stack-size`), as native threads get 64 MiB.

## What the language and toolchain need

- **String functions** (general library, every backend): concatenation, `trim`, `is_empty`, `len`, integer to
  string. TodoMVC trims titles and prints "3 items left".
- **The driver's analyses** (stratification, determinism) available to an in-memory compile, so the editor shows the
  same diagnostics as `blossom check`.
- Nothing for the engine: rounds, inputs, outputs, durable tables, aggregates and negation are there.

## Testing

- **Fast tier:** native Rust tests of the host: the diff on a model DOM, persistence round trips, the inspector's
  derivations, and TodoMVC driven by events through the host (its outputs checked round by round).
- **Full tier:** a Playwright test in headless Chromium against the built page, through every TodoMVC behavior
  (the spec's checklist), the reload (persistence), the editor and the inspector.

## Slices

1. **The host core, natively:** compile in memory, rounds through the engine, the page diff, persistence; a small
   app (a counter) end to end in native tests. String functions.
2. **In the browser:** the wasm build, `host.js`, the page; the counter, then TodoMVC with the official CSS; the
   Playwright gate.
3. **The inspector.**
4. **The editor.**

## Out of scope

Server rendering, multiple programs on a page, timers' wall clock beyond what the runtime already maps, CSS-in-Blossom,
and the original TodoMVC Cypress suite (the Playwright test follows the same spec).
