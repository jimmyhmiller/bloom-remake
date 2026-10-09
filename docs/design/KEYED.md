# Keyed roles: members named by key, created on demand

The user (2026-10-09) asked for "a role whose members are created on demand by key (one per poll/board/game), on
`blossom run` first; it's the language half of Durable Objects" (DURABLE-OBJECTS.md §Many objects, which sketched it as `object`). This document
is the design; nothing here is built yet.

Name: the role kind is `keyed` (`role Game: keyed;`), not `object`, so it does not collide with the runtime's
`object` module (a node a Durable Object hosts). A keyed role's members are **keyed members**.

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

- **Values** (blossom-value): `Value::Member(MemberRef { role: RoleId, key: Arc<str> })`, typed `Node<R>` with `R`
  keyed. Canonical order: after dense and client node ids, by `(role, key)`. Fingerprints and `to_string`
  (`Game:"game-17"`) follow.
- **Front end**: the `keyed` role kind (HIR, IR `RoleKind::Keyed`), `R.named(e)` (a builtin), `self.key()` at a keyed
  role (BLS error elsewhere), the static-membership errors, link events between a keyed role and a client role.
- **Evaluators**: a node's own identity becomes a `NodeRef` (a deployment node, a client member, or a keyed member);
  `self` evaluates to it; `send … to d` accepts `Value::Member`; a delivery's `from` is a `Value` (today a `NodeId`
  turned into one). The engine's and oracle's role lookup take the member's role from the value.
- **Codecs**: the wire codec and the durable codec write a member as its role and key; the order-preserving key codec
  (keycode.rs) gets a tag for it, ordered by role then key.
- **Simulator and LDFI**: a deployment names keyed members by key (`--nodes lobby=Lobby,game-17=Game:game-17`); they
  are dense nodes there, as client members are, and `R.named(k)` of a key the deployment does not name is a hard error
  ("the simulation has no member `game-9` of `Game`"), so a schedule never silently loses a node.
- **`blossom run`**: the deployment's nodes of a keyed role are its **hosts**. A member lives on the host that
  rendezvous hashing over the hosts picks for `(R, k)`; senders route by the same hash. A host runs its members as
  `ObjectNode`s (the threadless node of blossom-runtime::object), each with its own store under
  `<data_dir>/<host>/members/<hex of hash>/` and the key recorded in it; it creates one on the first message. Messages
  between a member and the rest go over the host's peer links, with the member as `from`; a receiver accepts a member
  as a sender only from the host the hash picks for it.
- **Pages**: a host's web listener serves `/?member=K`; the page's `HELLO` names the member; the member's client
  registry admits it.
- **Durable Objects**: each member is an object (`idFromName("R/" + k)`), and a send to a member is an RPC to that
  object; the prototype's single object becomes the hosts' role.

## 4. Sub-slices

1. **Values and the language**: `Value::Member`, the role kind, `named`, `self.key()`, errors; the codecs; both
   evaluators; the simulator with members named in the deployment. Gate: a program with a lobby and keyed games
   runs in the simulator on the oracle and the engine, which agree every round.
2. **`blossom run`**: hosts, rendezvous routing, members as `ObjectNode`s with their own stores, restart. Gate: a
   lobby and games over TCP, a host killed and restarted.
3. **Pages to members**: the `member` parameter, the `HELLO` field, link events. Gate: tic-tac-toe with a member per
   game, in Playwright.
4. **Durable Objects**: a member per object, RPC between objects. Gate: the same tic-tac-toe on a local workerd.

## 5. Out of scope

Migrating a member between hosts when the host set changes (rendezvous moves about `1/n` of them; their stores would
have to move with them), garbage collection of members nobody will address again, and LDFI over schedules that
create members.
