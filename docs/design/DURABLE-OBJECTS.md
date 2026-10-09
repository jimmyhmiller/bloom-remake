# Blossom on Cloudflare Durable Objects

The user (2026-10-09): "explore the idea of using something like durable objects from Cloudflare as a way of having
the backend ... Don't try to host it, but ... figure out the model, make some documentation."

This is that exploration: what a Durable Object is, how a Blossom node maps onto one, what has to be built, and what
does not fit. **A prototype runs** (§The prototype): each of the four apps of APPS.md runs unchanged as a Durable
Object on workerd, Cloudflare's runtime, on this machine (`wrangler dev`; nothing is deployed or hosted).
This is not the shelved serverless plan (OBJECT-STORAGE.md): that put object storage under the commit path; a Durable
Object keeps compute and a transactional store together.

## Summary

A Durable Object is close to a Blossom node already. It is single-threaded, it handles one event at a time, its
storage is transactional, and its **output gate** holds every outgoing message until the writes made before it are
durable. That last rule is Blossom's Invariant R (a tick's messages leave only after its durable changes are synced),
enforced by the platform instead of by our driver. So a node runs in an object without changing what a program means.

What Durable Objects add that Blossom does not have is a **namespace of objects named by key, created on demand**:
"the object for poll 17", "the object for room lobby". Blossom's roles are static (`process`, `cluster`) or admitted
from outside (`client`). A role kind whose members are named by key and exist once addressed would let an app shard
itself the way Durable Objects intend: one object per poll, per board, per game. That is the language work this
direction asks for.

What does not fit: programs whose point is their own replication (the Raft KV, the Kafka broker). An object's
storage is already replicated by Cloudflare, so running Raft between objects buys nothing. The fit is the
client/server apps (APPS.md): each object a server node, each tab a client member.

## What a Durable Object is

From Cloudflare's documentation (links at the end):

- **An object per name.** A Worker reaches an object through a stub from its namespace (`env.ROOMS.getByName("x")`);
  the object starts when first called. Calls are RPC (public methods of the class) or `fetch`.
- **Single-threaded, one event at a time.** Events are requests, RPC calls, WebSocket messages and alarms. An
  object's soft limit is 1,000 requests per second.
- **Input gates.** While a storage operation is in progress, no other event is delivered to the object.
- **Output gates.** While a write is in progress, new outgoing messages (responses, `fetch`es) are held until it
  completes. If it fails, the held messages are discarded and the object restarts from storage. Writes made without
  an `await` between them are committed atomically, and writes are coalesced.
- **Storage.** A SQLite database per object, up to 10 GB on the paid plan (1 GB free), with synchronous calls
  (`ctx.storage.sql.exec`, `transactionSync`, a synchronous key-value API) and point-in-time recovery to any moment of
  the last 30 days. A row (key and value) is at most 2 MB.
- **Alarms.** One alarm per object (`setAlarm(time)`), retried on failure.
- **WebSocket hibernation.** An object that accepts sockets with `ctx.acceptWebSocket` can be evicted from memory
  while its sockets stay open (after about 10 seconds idle); a message wakes it and runs its constructor again. Up to
  32,768 sockets per object, messages up to 32 MiB, 2 KB of attachment per socket that survives hibernation.
- **Lifecycle.** All memory is lost on hibernation or eviction. An object may restart at any time (deploys, runtime
  updates, placement), with no shutdown hook; whatever matters must be in storage.
- **Limits.** 128 MB of memory per isolate, 1 s to start, 30 s of CPU per invocation (up to 5 min on the paid plan),
  a Worker of up to 64 MiB uncompressed.
- **Clock.** In production `Date.now()` advances only when I/O happens (a Spectre mitigation).

## The mapping

| Blossom | Durable Object |
|---|---|
| A node of a role | An object |
| A tick | One event handler's synchronous run |
| The tick's durable delta, WAL append, sync | Writes to `ctx.storage.sql` in the same synchronous run |
| Invariant R: the outbox leaves after the sync | The output gate |
| One tick at a time; nothing interleaves | The input gate, and a tick that never `await`s |
| Group commit | Write coalescing |
| A client member's link (CLIENTS.md §3) | A hibernatable WebSocket |
| The client registry, a member's token | A table in the object's SQLite |
| Timers (`timer t every d`) | The object's one alarm, set to the earliest due timer |
| A channel between server nodes | An RPC call between objects |
| Crash and restart (LDFI's crash-restart) | Eviction, restart, redeploy |
| `blossom query --as-of` | Point-in-time recovery bookmarks (and our own versions) |

## A tick in an object

An event arrives (a WebSocket message from a tab, an RPC from another object, the alarm). The object:

1. Turns it into the tick's inputs: a link frame into deliveries, an alarm into due timer firings.
2. Runs the tick on the engine. The engine is synchronous Rust compiled to WebAssembly: the whole tick runs in one
   JavaScript turn, so nothing interleaves with it.
3. Writes the tick's durable delta (and the link's state) through `ctx.storage.sql`, still in the same turn. These
   writes are one atomic commit.
4. Sends the outbox: frames on the members' sockets, RPC calls to other objects. The output gate holds them until
   step 3 is durable, and drops them if it fails, restarting the object.
5. Sets the alarm to the earliest due timer, if it changed.

Our driver never syncs anything itself: the commit is the platform's, and Invariant R holds because of the output
gate. The node's pipelining (computing tick t+1 while t syncs) has no counterpart here; the platform overlaps the
commit with the object's next event instead, which is the same effect.

## Storage: two ways

**A. The store's VFS over SQLite.** `blossom_store::Vfs` is the store's only seam to the filesystem (`open`, `pread`,
`append`, `sync_data`, `rename`, `list`, …). An implementation over a table of file chunks in the object's SQLite
runs the existing WAL and LSM database unchanged; `sync_data` is a no-op because the commit happens at the end of the
event. This is the shortest path to a working object, and every durability test of the store (SimFs crash points)
still applies to the code above the VFS. It writes our WAL and SSTables as blobs in SQLite: two logs on top of each
other, and billed per row written.

**B. A database over SQLite.** The node's database (DATABASE.md) behind the engine's `ColdTables` trait, implemented
on SQLite tables: a table per durable relation (its key columns as the primary key), a tick's delta as inserts and
deletes, `probe` as an indexed `SELECT`. No WAL of our own: the object's storage is the log. Durable views
(DATABASE.md §8) are tables too. A program's state becomes queryable with SQL, and point-in-time recovery covers it.
This is the design to aim for; it replaces the LSM's job, not the engine's.

Recommendation: A first, to see a real app run in an object with the least new code, then B.

## Clients in an object

The client role needs nothing new in the language. A page connects to the object's `fetch`, which upgrades to a
WebSocket accepted for hibernation; the link (CLIENTS.md §3) runs over it unchanged.

- **The registry and tokens** live in SQLite (today `blossom_store::ClientRegistry`, over the VFS).
- **Resume across hibernation.** A hibernated object loses its memory, and today the replay buffer is memory: a page
  that reconnects would get `resumed = false` and the program's greeting would resend everything. Correct, but costly
  for a big board. The buffer (bounded, as today) should be in SQLite, and a socket's attachment can hold its
  member id and the last sequence numbers.
- **The HTTP link** (§3a) works too, but a held long poll keeps the object awake and billed for its duration;
  WebSockets with hibernation are the fit here, the reverse of the default `blossom run` chooses.

## Many objects: a keyed role

The apps of APPS.md run on one server node. On Durable Objects the natural shape is an object per thing: per poll,
per board, per game, so load and storage spread and each object stays small. Blossom has no way to say that today.

Built since (docs/design/KEYED.md, where it is called a *keyed* role, `role Game: keyed;`): the language, `blossom
run`'s hosts, pages linked to members, and an object per member here (see §The prototype). The sketch it started from:

```blossom
role Lobby;                    // one object, as today
role Game: object;             // members named by key, created when first addressed
role Browser: client;

channel start(x: Node<Browser>, o: Node<Browser>): Lobby -> Game;
channel play(cell: u64): Browser -> Game;

at Lobby {
    start_game: while pair(x, o, p), next_game(g0) {
        send start(x, o) to Game.named(f"game-{g0 + p}");
    }
}
at Game {
    // `self.key()` is this member's name; its tables are its own game's.
    take: on play(cell) from b, players(x, o), made(k), not outcome(_) where … { next moves(k, cell); }
}
```

- `role R: object;` declares a role whose members are named by a key (a `String`), exist once addressed, and are
  never enumerated: like a client role, `p in R`, `R.size()` and `majority` are errors.
- `R.named(k)` is the member named `k` (a `Node<R>`); `self.key()` is a member's own name.
- A page connects to one member (the URL names it: `/game/17/`), and its `Server.connected` becomes
  `Game.connected(g, …)`.
- **Meaning.** The same as a client role's: `Node<R>` is a sort, membership is dynamic, and the simulator and LDFI
  make each addressed member a node. Nothing in a tick changes.
- **On `blossom run`** (no Cloudflare), a keyed role could run its members in one process, each with its own store
  under the data directory: the same program then runs on a laptop and on Durable Objects. This keeps the platform a
  deployment choice, as `[web] link` is.

**Channels between objects** are RPC calls. Cloudflare's documentation states no ordering or exactly-once guarantee
for them, and a call can fail with an exception. Blossom's channels promise little (LANGUAGE §8: lossy, reordered
unless declared otherwise), so a plain call per batch is a correct channel; for `#[fault(reliable)]` channels the
link's numbering and acknowledgements (§3) carry over.

## Timers and the clock

- An object has one alarm. The driver sets it to the earliest due firing in the node's timer table and, when it
  fires, delivers every firing due by then (as the page's clock does, BROWSER.md). Guarded timers that are dormant
  set no alarm, so an idle object sleeps.
- Alarms are for seconds, not milliseconds: the documentation says one may be delayed, and each one is an
  invocation. A program with a 10 ms timer (the Kafka broker's polling) does not suit an object; the apps here have
  no server timers at all, and pixels' cooldown is a comparison of `now()` with a stored instant.
- `now()` is read once per tick from `Date.now()`. That it does not move during a run is what a tick wants anyway;
  the node keeps it monotonic across ticks as it does today.

## What stays the same

The program, its meaning, the simulator, LDFI, the inspector and the tests. An object's eviction is a crash and
restart from durable state, which LDFI already explores (S11). The deployment says where a program runs; the program
does not.

## Limits and risks

- **Memory: 128 MB per isolate.** The engine keeps non-tiered state in memory. Tiered durable tables (DATABASE.md §7)
  matter more here than on a server; a large board must stay tiered.
- **Startup: 1 s to instantiate, and every wake is a cold start.** The engine-only WebAssembly is 4.1 MB (the page's
  `pkg-member`); recovery must not read every table on wake (S26 already made a restart's boot tick cheap with
  durable views). This has to be measured early.
- **Rows written are billed.** Way A writes WAL and SSTable chunks; way B writes one row per changed tuple. B is the
  cheaper one.
- **Not for the broker.** A Raft group of objects replicates what Cloudflare already replicates, at the cost of RPC
  hops and alarms for heartbeats. Blossom's Kafka and Raft programs stay on `blossom run`.
- **Two runtimes to keep equal.** The object driver must be held to the native node by the same tests: the
  differential (oracle and engine) runs and the crash-point store tests (for way A, the store's conformance suite over
  the SQLite VFS).

## The prototype

```sh
scripts/build-do.sh polls                     # or board, tictactoe, pixels; `rooms Room` for a keyed role
cd do && npm ci && npx wrangler dev --config build/polls/wrangler.toml --port 8787
```

then open http://127.0.0.1:8787/ in two tabs (rooms: `/?member=lunch`). The page is the same browser host. Each app
builds into `do/build/APP` (its WebAssembly, page, sources, deployment, an entry module over `do/src/worker.js`, and
its wrangler config), so apps build and run side by side.

**Objects.** Every node of the deployment is an object, `node/NAME`; every member of a keyed role is one too,
`member/ROLE/KEY`, created by the first request for it; and `registry` mints the pages' tokens. The Worker answers
`/blossom/app.json`, the client parts and the stylesheet itself (`DoSite`: the program compiled once per isolate,
no object), sends a page's link to its node's object, or with `?member=KEY` to that member's, and a token request to
the registry. An object learns its name from its first request (a header the Worker sets) and keeps it in its
storage. The deployment's seed is the Worker's secret `BLOSSOM_SEED` (`do/APP.dev.vars` locally), shared by every
object: members' seeds, choices and tokens agree.

**Page tokens.** A member's object cannot mint page ids alone: two rooms would give out the same one. So the registry
object hands out serials, one per new page, and signs each token with a key derived from the seed (`signed_token`: a
keyed BLAKE3 over the role and serial); any member's object checks a token without asking anyone, and a page's id is
`#serial@s` wherever it links. A page fetches its token (`POST /blossom/token`, announced by `app.json`'s `tokens`)
before it links; a token the deployment did not sign is refused (`REJECT token`), and the page gets a new one.

**Messages between objects** are requests: a released tick's sends to other nodes and members leave its object as
`BATCH` frames (from a node, whose name the request carries) or `FROM_MEMBER` frames (from a member), one request per
frame to the destination object's `/blossom/deliver`, which the Worker never routes from outside. The output gate
holds them until the tick's writes are durable; a failed one is a lost message, as Blossom's channels allow.

- **`blossom-runtime::object::ObjectNode`**: one node of a deployment over any `Vfs`, with its client members'
  links, driven by calls: `frame` (a WebSocket message), `closed`, `wake` (the alarm). Each call runs the node's ticks
  until it is quiescent, every tick durable before the next (`ManualDriver`), and queues the frames to write. It shares
  the runtime's member-link code (`MemberLinks`, admission, the client registry): a connection is a `LinkConn` trait,
  a thread's queue in `blossom run`, a socket in an object.
- **`blossom-store::KvFs`**: the store's files over keys and values (inodes and 64 KiB chunks; a rename moves a name).
  It passes the store's VFS and WAL conformance suites, and the KVS of node_kvs.rs recovers over it after a restart
  that keeps only the keys and values.
- **`blossom-store::JournalKv` and `crates/blossom-do`**: the WebAssembly the object loads. The workspace denies
  `unsafe`, and storage calls back into JavaScript from Rust would need it (a JavaScript object is not `Send`), so the
  object's store is held in memory and journaled: the Worker loads every key when the object starts, and after each
  call applies the journal to `ctx.storage.kv` and only then writes the frames, all in one synchronous run of the event
  (one commit; the output gate holds the frames). The program is compiled from its sources inside the object at start.
- **`do/src/worker.js`**: the object class. WebSockets are accepted for hibernation; an object woken from hibernation
  starts its node again from storage and closes the sockets of its last start (their pages reconnect: `resumed` is
  false, and the program's greeting resends). A node fault closes every link and discards the call's writes; the next
  event starts the node from storage, as a crash and restart would.
- **Tests**: tests/integration/tests/object_node.rs (polls on an `ObjectNode` over `KvFs`, two page engines exchanging
  frames in memory, a restart from only the keys); crates/blossom-do/tests/rpc.rs (a lobby node and keyed games as
  objects over storages in memory, their messages routed by name: replies, member to member, a restart of every
  object from its storage); tests/web/object.spec.mjs (polls on workerd with Chromium tabs: votes through the object,
  workerd killed and started again over the same storage, the open tabs reconnect as the same members, a new tab gets
  everything; rooms: two rooms, each its own object, tokens from the registry, a game played, workerd killed and both
  rooms back). `scripts/test-tiers.sh web` runs them.

Measured on this machine: the first request to a new object (compiling polls and opening its store) takes about 140
ms, later ones about 3 ms; a pixel painted in one tab reaches the other in under 20 ms.

What the prototype does not do yet:

- **Way B**: the store is all in memory (128 MB per isolate), loaded at each start. Lazy reads need storage calls from
  the engine, which needs either store traits that accept non-`Send` handles in WebAssembly or a SQLite-native
  database behind `ColdTables` with the Worker answering its queries.
- **Resume across hibernation**: the replay buffer is memory, so every wake is a non-resumed reconnect.
- **Precompiled programs**: an encoding of the whole artifact would remove the compile from every start.
- **Messages between objects on workerd**: the requests are tested at the object API (rpc.rs) but no app on workerd
  sends one yet (rooms talks only to its pages).
- **A front for several hosts' pages**: on workerd every member is reachable from the one Worker; on `blossom run` a
  page must reach the host that runs its member.

## The pieces, in order

Everything below can be built and tested on a laptop with `workerd`, the open-source runtime behind Workers
(`wrangler dev` runs it locally); none of it needs an account or a deployment.

1. **The node in WebAssembly.** Done: `blossom-node`, `blossom-store` and `blossom-wire` check for
   `wasm32-unknown-unknown` (the store's real-filesystem VFS gained a non-Unix read; it was the only thing in the
   way). `ManualDriver` is the threadless driver an object needs.
2. **A key-value VFS** (way A): done, `KvFs`, over a journaled store (see §The prototype for why not SQLite calls).
3. **An object driver**: done, `ObjectNode` and do/src/worker.js (a thin JavaScript class over the wasm module).
4. **The server side of the client link without threads**: done, shared with the runtime (`LinkConn`).
5. **One app end to end**: done, polls on a local workerd with Playwright; the other three apps run too.
6. **The keyed role**: done (KEYED.md): an object per member, page tokens from a registry object, requests between
   objects; rooms on a local workerd with Playwright.
7. **Way B**.

## Sources

- Durable Objects: https://developers.cloudflare.com/durable-objects/
- Input and output gates: https://blog.cloudflare.com/durable-objects-easy-fast-correct-choose-three/
- SQLite storage API: https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/
- Limits: https://developers.cloudflare.com/durable-objects/platform/limits/ and
  https://developers.cloudflare.com/workers/platform/limits/
- Lifecycle and hibernation: https://developers.cloudflare.com/durable-objects/concepts/durable-object-lifecycle/
- WebSockets: https://developers.cloudflare.com/durable-objects/best-practices/websockets/
- Calling objects: https://developers.cloudflare.com/durable-objects/best-practices/create-durable-object-stubs-and-send-requests/
- Timers in Workers: https://developers.cloudflare.com/workers/runtime-apis/performance/
