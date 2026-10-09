# Keyed roles: members named by key, created on demand

The user (2026-10-09) asked for "a role whose members are created on demand by key (one per poll/board/game), on
`blossom run` first; it's the language half of Durable Objects" (DURABLE-OBJECTS.md §Many objects, which sketched it as `object`). This document
is the design; nothing here is built yet.

Name: the role kind is `keyed` (`role Game: keyed;`), not `object`, so it does not collide with the runtime's
`object` module (a node a Durable Object hosts). A keyed role's members are **keyed members**.

**Status (2026-10-09):** sub-slices 1 to 3 are built (§4): the value, the language, both evaluators, the codecs, the
simulator, `blossom run`'s hosts, and pages linked to members (examples/web/rooms.bls). A Durable Object refuses a
send to a member until sub-slice 4.

## 1. What a program says

```blossom
role Lobby;
role Game: keyed;
role Browser: client;

channel start(x: Node<Browser>, o: Node<Browser>): Lobby -> Game;
channel play(cell: u64): Browser -> Game;

at Lobby {
    begin: while pair(x, o, p), next_game(g0) {
        send start(x, o) to Game.named(f"game-{g0 + p}");
    }
}
at Game {
    table players(x: Node<Browser>, o: Node<Browser>) key();
    setup: on start(x, o) { upsert players(x, o); }
    take: on play(cell) from b, players(x, o), … { … }
    view me(k) = let k = self.key();
}
at Browser {
    /// The page knows its game from its link: the page's URL named it.
    up: on Game.connected(g, _) { emit here(g); }
    tap: on click(id), here(g), … { send play(cell) to g; }
}
```

- `role R: keyed;` declares a role whose members are named by a key (a `String`). A member exists from the first
  message to it (or the first page that connects to it); it has its own tables, timers and durable store, like any
  node.
- `R.named(k)` (`k: String`) is the member named `k`, a `Node<R>`. It is pure: every node computes the same value.
- `self.key()` at `R` is the member's own key. (`n.key()` for another member is not offered: a node need not know the
  key of every member it heard from.)
- Membership is dynamic, as a client role's: `p in R`, `R.size()` and `majority(s, R)` are BLS0404.
- Channels to and from a keyed role are ordinary; replies go to `from` as everywhere.
- A client role's page connects to one member, named in the page's URL (`/?member=game-17`); at the page, the link
  events are `R.connected(g, resumed)` and `R.disconnected(g)`.

**Meaning.** As for client roles (CLIENTS.md §1): one program, every rule guarded by its role, `Node<R>` a sort. A
keyed member is a node of role `R` whose existence is dynamic and whose identity is its key. Nothing in a tick changes.

## 2. Identity: the decision

A member's identity must be computable by any node from `(R, k)`, travel in messages and durable rows, order the same
way everywhere (canonical order is observable: `min!`, `choose!` priorities, `index!`), and never confuse two keys.
The options:

| | How | Exact? | Cost |
|---|---|---|---|
| A | `NodeId` stays `u32`; a keyed member is a 30-bit hash of `(R, k)` | No: two keys collide with 50% odds by ~40,000 members | Small |
| B | `NodeId` widens to `u64`; a keyed member is a 62-bit hash of `(R, k)`; hosts keep `id → key` and refuse a collision when a message carries a different key | In practice (50% odds at ~2.5 billion members, and detected) | Every encoding of a node id (frames, keycode, codec) changes width; keys must travel with object-addressed batches |
| C | A value kind of its own, `Value::Member { role, key }`, typed `Node<R>`; node ids for keyed members are assigned inside a process only for its routing tables | Exact | A new value kind through the value model, codecs, printer, canonical order, both evaluators (`send … to`, `from`), the simulator and the runtime |

Durable Objects themselves take B's road (an object's id is a 256-bit hash of its name). **C is the recommendation**:
the identity is the key itself, so nothing can collide, the canonical order is the key's (deterministic without any
registry), and a durable row holds the key, so a restart or a migration never needs a side table to read it back.
Its cost is breadth, not depth: each layer gains one case.

## 3. Under C, layer by layer

- **Values** (built; blossom-value): `Value::Member(MemberRef { role, role_name, key })`, typed `Node<R>` with `R`
  keyed. A member's identity is its role's **name** and its key: it compares, orders (after every node id, by role
  name, then key), hashes and fingerprints by those, never by the role's id, which is the program's own (a page's
  projection numbers roles otherwise, and so may a new version; `hash64` must agree on every node and in every
  version). `to_string` writes `Game:"game-17"`.
- **Front end** (built, but link events): the `keyed` role kind (HIR, IR `RoleKind::Keyed`), `R.named(e)`, `self.key()`
  at a keyed role (BLS0404 elsewhere), the static-membership errors (BLS0404); link events between a keyed role and a
  client role come with sub-slice 3.
- **Evaluators** (built): a host routes by `NodeId`s, so it gives each member it runs or sends to one and tells both
  evaluators which (`blossom_ir::members::Members`, `Oracle::with_members`, `EngineConfig::members`). `self` and a
  delivery's sender become the member's value; a send to a `Value::Member` goes to its id, and one with no id is
  `EvalError::NoMember` ("a send to `Game:"game-9"`, a member no node runs"). A member's role comes from the table, and
  its seed σn derives from its member name (`Game:"game-17"`), the same on every host. The ids never reach a value.
  `R.named(k)` and `self.key()` are the IR builtins `Named { role }` and `MemberKey`.
- **Codecs** (built): the wire and durable codecs write `Node<R>` of a keyed `R` as the key alone (the role is the
  type's), and an untyped `Node` of a program with keyed roles tagged (0 and the node, or 1, the role's name and the
  key); a program without keyed roles encodes every node as before. The order-preserving key codec gives members
  sub-tag 2 after the nodes, then the escaped role name and the escaped key, so its bytes order as members do. A
  column that may hold a member says so in its relation's schema hash (`Node<keyed Game>`, `Node|keyed`), by name.
  The word store interns `Node` columns (a member is no fixed-width word).
- **Simulator and LDFI** (built): a node of a keyed role is the member its name keys (`--nodes lobby=Lobby,game-17=Game`
  is `Game.named("game-17")`); members are dense nodes there, as client members are, and a send to a key the
  deployment does not name is the hard error above, so a schedule never silently loses a node. LDFI decides programs
  with keyed roles by enumeration; its lineage refuses them (TEST-020) until it reads members.
- **`blossom run`** (built): the deployment's nodes of a keyed role are its **hosts** (`blossom_runtime::hosting`;
  `blossom run` starts one for such a node, and `Server::start` refuses it). A member lives on the host that
  rendezvous hashing over the hosts picks for `(R, k)` (`keyed::Routing`: the highest BLAKE3 score of role, key and
  host name); senders route by the same hash. A host runs no rules as itself: it runs its members as `ObjectNode`s
  (the threadless node of blossom-runtime::object), each with its own store under
  `<store>/members/<first 16 bytes of BLAKE3 of the member's name, hex>/store`, the member named in the `member` file
  beside it; it creates one on the first message and opens every one its store holds when it restarts, so their
  timers run again. Its own store holds only its restart count (`<store>/host`), which its peers' incarnation check
  needs. A node routes a send to a member to the member's host, the row naming the member (column 0); a host sends
  what its members send as `FROM_MEMBER` frames (`0x13 := role:u32 key:str BATCH-body`), and a receiver accepts one
  only from the host the hash picks for that member, admitting it by the member's role and the host's principal. A
  member's message to a member of its own host stays on the host. Each member runs its ticks durably before its
  messages leave (Invariant R per member); a message to a host that is down may be lost, as any message may.
  Not yet: pages at a host (sub-slice 3), streams, external sessions, queries and traces at a host, and traces of a
  node of a program with keyed roles (a trace would name members by one incarnation's ids).
- **Pages** (built): a host's web listener (`blossom run --web`) serves the program's pages; its `app.json` names
  the keyed role it hosts (`"keyed"`), and a page names the member it links to by its URL (`/?member=K`), in its
  `HELLO` (peer kind 3: the role's name and the key) and in where it keeps its link and tables (per member). The host
  admits the page with one client registry for all its members (a page's id is the host's, `#serial@host`, so pages
  of different members never share one), and hands the link to the member, which raises `Browser.connected` and
  answers the page; the page sees its server as the member (`Room.connected(r, _)`, `r` a `Value::Member`), and
  its sends to `r` go over the link. A page naming a member another host runs is refused, naming that host; a node
  that runs no keyed members refuses a page naming one. In simulation a page is linked to every keyed member, as to
  every server node. Not yet: sending a page to the right host (a front that routes by member, or a redirect).
- **Durable Objects**: each member is an object (`idFromName("R/" + k)`), and a send to a member is an RPC to that
  object; the prototype's single object becomes the hosts' role.

## 4. Sub-slices

1. **Values and the language**: `Value::Member`, the role kind, `named`, `self.key()`, errors; the codecs; both
   evaluators; the simulator with members named in the deployment. Gate: a program with a lobby and keyed games
   runs in the simulator on the oracle and the engine, which agree every round. **Done**
   (tests/integration/tests/bls_keyed.rs: the lobby fixture on both evaluators, an unnamed member's error, the codecs,
   and crashes on the cluster simulator's durable path).
2. **`blossom run`**: hosts, rendezvous routing, members as `ObjectNode`s with their own stores, restart. Gate: a
   lobby and games over TCP, a host killed and restarted. **Done** (tests/integration/tests/run_keyed.rs: games on
   two hosts, replies by sender, member-to-member on one host and across hosts, a host stopped mid-game and restarted
   from its store, and a member's timer running again after the restart).
3. **Pages to members**: the `member` parameter, the `HELLO` field, link events. Gate: tic-tac-toe with a member per
   game, in Playwright. **Done** (examples/web/rooms.bls; tests/web/apps.spec.mjs `rooms`: two rooms on a host,
   played, then the host killed and restarted, both rooms back from their own stores; web_apps.rs plays a room on
   both evaluators).
4. **Durable Objects**: a member per object, RPC between objects. Gate: the same tic-tac-toe on a local workerd.

## 5. Out of scope

Migrating a member between hosts when the host set changes (rendezvous moves about `1/n` of them; their stores would
have to move with them), garbage collection of members nobody will address again, and LDFI over schedules that
create members.
