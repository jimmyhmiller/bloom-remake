# Apps on the client/server setup

The user (2026-10-09): "make a few different apps beyond Todo MVC that use our whole backend frontend synced setup
... to show just how much easier building all of this in Datalog is ... make sure that they really work, fix any
bugs."

Four apps, each one Blossom program that holds the server and the page (docs/design/CLIENTS.md). The channels
between the two roles are the whole API: no routes, no message classes, no client store, no sync library.

| App | What it does | Run it |
|---|---|---|
| `examples/web/polls.bls` | Ask a question with choices; everyone votes; counts and voter names move live; the asker closes it. | `scripts/run-app.sh polls` |
| `examples/web/tictactoe.bls` | A lobby pairs players; the server referees; anyone can watch a game; rematch, resign. | `scripts/run-app.sh tictactoe` |
| `examples/web/board.bls` | A kanban board: add, rename (others see who is editing), drag within and between columns, delete. | `scripts/run-app.sh board` |
| `examples/web/pixels.bls` | An r/place canvas: paint a square, wait out a cooldown the server enforces, a live leaderboard. | `scripts/run-app.sh pixels` |

`scripts/run-app.sh APP [PORT] [--fresh]` builds what is missing, keeps the store and seed under
`examples/web/.data/`, and serves the page on http://localhost:8080/. Open it in two tabs, or two browsers.

## How big they are

Lines of code (no comments, no blank lines), by part of the program:

| App | Total | Channels, constants | Server (and what both ends share) | Page: events and sync | Page: views | Page: markup |
|---|---|---|---|---|---|---|
| polls | 240 | 17 | 57 | 55 | 55 | 56 |
| tictactoe | 303 | 25 | 86 | 67 | 68 | 57 |
| board | 272 | 10 | 56 | 92 | 58 | 56 |
| pixels | 205 | 22 | 53 | 52 | 29 | 49 |

The server side of each app is 53 to 86 lines (with the sections both ends share), and that includes its storage schema, its rules, and its whole sync
protocol. Each app also has a stylesheet of 60 to 130 lines (`examples/web/APP.css`, sharing `web/apps.css`).

## What the platform does that the apps do not write

None of the four apps has code for any of these; they come from the language and the runtime:

- **Transport and reconnection.** The link (CLIENTS.md §3, §3a) frames messages, numbers them, acknowledges them,
  reconnects with backoff, and resumes without loss inside the replay buffer. Plain HTTP or a WebSocket, chosen by the
  deployment.
- **The offline queue.** A page's sends wait in its storage while the server is unreachable, across reloads, and go
  out in order when it is back. The board test edits a card while the node is killed and checks it arrives.
- **Exactly-once delivery on the link**, so "a vote sent twice" never happens by accident.
- **Identity.** Each tab is a `Node<Browser>` with a token that survives reloads and server restarts.
- **Durability on both ends.** `durable table` on the server is in its write-ahead log and database before any reply
  leaves (Invariant R); on the page it is in `localStorage`. Each Playwright test kills the node with SIGKILL and
  checks nothing was lost.
- **Schema checks across the wire.** Both ends are compiled from one program; the handshake refuses a page built from
  another version and the page reloads itself.
- **Provenance across the wire.** The page's "Why?" button explains any element down to the rules, the clicks and the
  server messages that made it ("received from s in round 18").
- **Simulation.** The same program runs in the deterministic simulator with tabs as nodes: `web_apps.rs` plays a whole
  tic-tac-toe game there, on the oracle and the engine.

## The patterns, and how many lines each takes

**Who is here.** Presence is a table the link events maintain, and a view that joins it with names:

```blossom
join: on Browser.connected(b, _) { emit online(b); }
leave: on Browser.disconnected(b), not Browser.connected(b, _) { delete online(b); }
view presence(b, name, here) {
    names(b, name), online(b), let here = true;
    names(b, name), not online(b), let here = false;
}
```

**Sync.** Every app syncs state to the pages with two rules per table: tell everyone online about each change, and
tell a tab that connects without resuming about everything.

```blossom
tell_vote: on inserted votes(w, n, v, i, s), online(b) { send voted(w, n, v, i, s) to b; }
greet: on Browser.connected(b, false) { for votes(w, n, v, i, s) { send voted(w, n, v, i, s) to b; } }
```

**Conflicts.** A keyed table with a resolution policy is a last-writer-wins register. A vote is the voter's newest
change; the page and the server use the same declaration, so a vote changed three times in one tick, heard twice, or
heard late settles on the last:

```blossom
durable table votes(w: Node<Browser>, n: u64, voter: Node<Browser>, i: u64, s: u64) key(w, n, voter)
    resolve choose_most(s);
```

The board keeps the server's version first and the tab's own edits since: `ver: (u64, u64)` is (server version, own
change), and `resolve choose_most(ver)` keeps the greatest.

**The server decides.** A rule's header is its precondition. Tic-tac-toe takes a move only from the player whose
turn it is, on an empty square, in a game that is not over; there is no separate validation layer:

```blossom
take: on play(g, cell) from b, games(g, x, o, _), made(g, k), not outcome(g, _), not mark(g, cell, _)
        where cell < 9u64 && b == (if k % 2u64 == 0u64 { x } else { o }) {
    next moves(g, k, cell);
}
```

and who won is a view over the moves:

```blossom
view three(g, m) = line(a, b, c), mark(g, a, m), mark(g, b, m), mark(g, c, m);
```

**Matchmaking.** The waiting players, ranked by how long they waited, paired two by two:

```blossom
view ranked(since, b, r = index!()) = waiting(b, since);
view pair(x, o, p) = ranked(_, x, r), ranked(_, o, r2), let p = r / 2u64 where r % 2u64 == 0u64 && r2 == r + 1u64;
```

**Optimistic UI with answers.** Pixels shows a paint at once as pending, and the server answers each paint; the
answer removes it, and the square shows what the server has:

```blossom
draw: on press(id), cell(id, x, y), color(c), next_paint(s), srv in Server, not resting(_) {
    emit pending(s, x, y, c);
    send paint(x, y, c, s) to srv;
    ...
}
hear_done: on done(s, _), pending(s, x, y, c) { delete pending(s, x, y, c); }
```

**Rate limits.** The server refuses a paint that comes too soon after the painter's last one:

```blossom
view rested(b) {
    asked(b, _, _, _, _), not last_paint(b, _);
    asked(b, _, _, _, _), last_paint(b, at) where now() - at >= COOLDOWN;
}
```

## What a conventional stack would need for the same apps

Not measured: no app here was also written in another stack, so there are no line counts to compare. What can be
said is which parts a typical web stack (a database, an HTTP or WebSocket server, a client state library) would
make the programmer write, and where each is in a Blossom app:

| Concern | Conventional stack | Blossom app |
|---|---|---|
| Schema | SQL tables, migrations | `durable table` declarations (server and page) |
| API | routes or message types, both ends, serializers | `channel` declarations |
| Fan-out | a subscription registry, broadcasting code | `on inserted r(…), online(o) { send … }` |
| Catch-up after reconnect | resend logic, cursors | `greet: on Browser.connected(b, false)` |
| Reconnect, resume, dedup | client and server code, idempotency keys | the link (none in the app) |
| Offline queue | IndexedDB queue, replay | the link (none in the app) |
| Conflict resolution | merge code on both ends | `resolve choose_most(…)` |
| Validation | a server layer, often repeated on the client | the rule's header |
| Client state | a store, reducers or signals, cache invalidation | tables and views |
| Rendering | components and their props | `emit html …` from views |
| Debugging "why is this here?" | logs | the inspector, across the wire |

## What building them found

### Fixed

- **A compiler bug.** An `outer` atom over a variable bound by a generator (`x in [a, b], outer names(x, n)`) made
  the generator a membership test and `x` an `Option` (a type error). The resolver declared `outer` atoms' variables
  before deciding which `in` literals are generators; it now declares them after. Test:
  `fixtures/apps/outer_generator.bls`.
- **Constant collections.** `const LINES: Vec<(u64, u64, u64)> = [...]` was "not implemented" (LANG-010). Constants
  may now be tuples, vectors, sets and maps.
- **Drag and drop.** The browser host had no drag events. It now has `drop(id, target)` (BROWSER.md).
- **A page's stylesheet** came from a table of app names in `host.js`. A deployment now names it: `[web] style`.
- **The inspector** listed a rule once per path that credited it ("by rules `hear_poll`, `poll_list`, `hear_poll`,
  …"); it names each once.
- **`collect!(e by k̄)` and `index!(by k̄)`** (LANG-100, LANG-097) are implemented: `collect!` aggregates
  `(k̄, e, valuation)` and keeps `e` (the IR's `CollectVecAt`), `index!` ranks `(k̄, head tuple)`. An `index!` key
  reads only the view's columns (BLS0511), so each tuple has one. Descending keys are still not implemented.
- **Events name the element you meant.** `examples/web/events.bls` (include it after `ui.bls`) has `clicked(id)`,
  `dblclicked(id)`, `pressed(id)` and `dropped(item, id)`: the element an event names and every element it is inside,
  walked up the page's `elem` rows. Polls had a real bug here: a click on a choice's text named the text's `span`
  and the vote was dropped. The Playwright test now clicks the text.
- **One namespace for every role, and no logic shared between roles.** A relation placed at the server and one at
  the page could not share a name (polls kept `polls` and `poll_list`), and a view lived at one role, so tic-tac-toe
  judged wins on the server and repeated the join on the page. A section of several roles, `at Server, Browser { … }`
  (LANGUAGE §6.10), now gives each end its own copy under one name and runs its rules at both: polls shares `polls`
  and `votes`, pixels its `canvas`, and tic-tac-toe its games, moves and the referee's views, so a page shows the
  outcome and the winning line by the server's own rules (the `ended` message is gone). A module written in the file
  can also be imported at each role; it now calls the file's functions, which it could not.
- **A page that silently disappears** is warned about: BLS1011, for a `while` handler that writes an output and needs
  a row of an ungrouped aggregate view with no `default`.
- **`if … else` as content**: `p { if open { "a" } else { "b" } }` was BLS0303 ("one content"); an `if` and its
  `else` now each may give the element its content.

### Open gaps

1. **A resolution cost is one column** (LANG-117). The board puts its two-part version in one tuple column.
2. **Descending `by` keys** (`collect!(e by k desc)`) are not implemented (LANG-118).
3. **`index!` over persistent state re-ranks every tick** (BLS0601), with the quadratic reference lowering. Fine for a
   board; a large list would want the engine's sort.

### Mistakes made while writing the apps

Recorded because a person writing their first Blossom app would make them too:

- Reusing a variable name binds it twice: `dropped_on(t)` and a card's title `t` in one header joined the drop target
  with the title. Nothing matched, and nothing said why.
- Comparing the page's clock with the server's (`at >= since`) to decide whether a game answered the tab's request.
  The fix compares nothing across clocks: a new game of this tab's answers it.
- `emit` into a table that a page view reads through negation is a same-tick cycle (BLS0502); `next` is the fix, and
  the error names the cycle.

## Tests

- `tests/web/apps.spec.mjs` (Playwright, real nodes): per app, two or three tabs, the server's rules, a reload, and a
  SIGKILL and restart of the node. `scripts/test-tiers.sh web` runs it with the other browser tests.
- `tests/integration/tests/web_apps.rs`: every app compiles for a server and tabs; tic-tac-toe plays a game in the
  simulator on the oracle and the engine, which agree every round; the `outer`/generator and constant fixture.
- `crates/blossom-web/tests/host.rs`: `a_drop_names_the_dragged_element_and_the_target`.
- `tests/integration/tests/page_members.rs`: `the_deployment_names_the_pages_stylesheet`.
