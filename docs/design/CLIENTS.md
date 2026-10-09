# Clients: the browser as a node of the program

The user (2026-10-06): "Could we make the browser a participant in this with a server model? So you could have an
'api' but it is all just datalog chatting?", then "Yes, I want to do it." Their choices (question tool):

- **C1.** A new role kind, `client`: a role that holds rules, compiled to the page, whose members come and go (one
  per open tab). One program: the simulator, LDFI and `why` see both sides. (Not an `external` role and a second
  program.)
- **C2.** A tab keeps its identity across reconnects: a token in its storage makes it the same member after a reload,
  a dropped connection or a server restart, and the server resends what it missed.
- **C3.** Demos: TodoMVC with its list on the server, then a chat room.
- **C4.** `blossom run` serves the page and a WebSocket (`--web ADDR`); no separate dev server.

S27 (2026-10-08): "I definitely want to be able to do the HTTP stuff, and make it so you don't have to do web sockets.
I don't know how I want to expose that, but that's the goal." The link also runs over plain requests (§3a); the node
serves both. Asked where the choice belongs, the user chose the deployment, with plain requests by default: the
program means the same over either, so it is where it runs that decides (`[web] link`, §4).

## 1. The language

```blossom
program todos version 1;

role Server;                       // process: one node
role Browser: client;              // one member per open tab, coming and going

channel add(text: String): Browser -> Server;
channel item(id: u64, text: String): Server -> Browser;

at Server {
    table items(id: u64, text: String);
    store: on add(t) from b { … emit items(id, t); }
    // A tab that (re)connects gets the list; one that resumed got what it missed already.
    greet: on Browser.connected(b, false), items(i, t) { send item(i, t) to b; }
    fan: on add(t), items(i, t2), online(b) … { send item(…) to b; }
}
at Browser {
    include "ui.bls";
    table known(id: u64, text: String);
    keep: on item(i, t) { emit known(i, t); }
    ask: on press(_, "Enter"), srv in Server { send add(…) to srv; }
}
```

- **`role R: client;`** (LANGUAGE §6.10). A client role holds rules (`at R`), like a process or cluster role. Its
  members are not part of the deployment: each is a running page, admitted at run time. A member is a `Node<R>`;
  `self` inside `at R` is one, and the server addresses a tab with one (`send c(…) to b`, `b: Node<R>`).
- **No static membership.** `p in R`, `R.size()` and `majority(s, R)` are errors for a client role (BLS0404): the
  members are not known when the program is compiled. A program learns of its clients from their messages and from
  the link events.
- **Link events** (LANGUAGE §7.15). Between a client role and every other role, each side sees the other's link:
  - at a role `S` that a client role `R` talks to, `R.connected(c: Node<R>, resumed: bool)` when a tab connects to
    this node, and `R.disconnected(c: Node<R>)` when its connection ends;
  - at the client role `R`, `S.connected(s: Node<S>, resumed: bool)` and `S.disconnected(s: Node<S>)` for its link to
    a node of `S`.
  `resumed` says whether the link took up where the last one left off (§3): nothing sent on it was lost. A program
  resends state on `resumed == false`.
  A tab that drops and reconnects quickly, or reconnects while its old connection is still open (the node then closes
  the old one), can bring a `disconnected` and a `connected` of the same member into one tick. Since `delete` takes
  effect at the next tick and `emit` now, a rule that forgets a member on `disconnected` says so:
  `leave: on Browser.disconnected(b), not Browser.connected(b, _) { delete online(b); }`.
- **Channels.** A channel between a client role and another role is an ordinary channel (`Browser -> Server`,
  `Server -> Browser`). Client roles do not talk to each other directly (BLS0404): tabs talk through a server.
- **Placement.** Inside `at R` of a client role go the page's relations: the browser interface (`ui.bls`) is included
  there. What both ends keep, and the rules both apply, go in a section of both roles, `at Server, Browser { … }`
  (LANGUAGE §6.10): each end has its own copy under the one name (examples/web/tictactoe.bls judges a game at the
  server and shows its outcome at the page by the same views). A client role's durable tables persist in the page's storage (BROWSER.md §Persistence).

**Meaning.** The Dedalus meaning is the one of §6.10: one program, every rule guarded by its role, `Node<R>` a sort.
A client member is a node of role `R` whose existence is dynamic; the link events are inputs that the transport
raises. Nothing in the meaning depends on how many members there are, which is why static membership is excluded.

## 2. Deployment and identity

- The deployment names the server nodes only (a client role holds none). The page compiles the same source with the
  same deployment, so both ends agree on relations, types and schema hashes; the handshake checks it.
- A tab's identity is a `Node<R>` the server mints the first time it connects, outside the deployment's node ids:
  `NodeId(CLIENT | server << 20 | serial)` with `CLIENT = 1 << 31`, so ids minted by different server nodes never
  collide. It is printed `R#serial@server`.
- The server keeps a durable **client registry** next to its store: for each minted id, its role and the hash of the
  token it gave out. A token is the id and 128 random bits; a tab keeps it in its storage. Presenting it again (after a
  reload, a dropped connection or a restart of either end) resumes the same identity. An unknown token (another
  server node's, or a registry that was lost) is refused: the tab starts over with a new identity.
- Values of client nodes stored durably on the server keep their ids (the disk codec writes them as their role and
  number).

## 3. The link: framing, resume and the offline queue

The page and the server speak the node protocol (blossom-wire frames: HELLO, HELLO_OK, REJECT, BATCH) over a
WebSocket, one binary WebSocket message per frame, with three additions:

- **`HELLO` from a client** carries `Peer::Member { role, token, received }`: its role, its token (none the first
  time) and the sequence number of the last message it took from the server.
- **`HELLO_OK` to a client** carries its identity (`NodeId`, token) and the last sequence number the server took from
  it, so the tab resends exactly what was not taken.
- **`MSG { seq, batch }`** wraps a batch with its sequence number on that direction of the link, and **`ACK { seq }`**
  says everything up to `seq` was taken (each end acknowledges at least once a second and when its buffer runs high).

Each end keeps what it sent until it is acknowledged:

- the **server** keeps a bounded replay buffer per client identity in memory. A resume within it is lossless
  (`resumed = true`); one past it, or after a server restart, is not (`resumed = false`), and the program resends;
- the **page** keeps its unacknowledged messages in its storage, so messages sent while offline (and across a reload)
  go out when the link is back: the offline queue. Its own sends queue while the link is down; rules that send do not
  wait for the link.

A receiver drops a message whose sequence number it has taken already, so delivery on the link is exactly once and in
order. (Channels promise less, LANGUAGE §8: a program written for lossy channels is unchanged.)

A client's message must be addressed to the server node it is connected to; one addressed elsewhere is dropped and
counted (`dropped_unroutable`). Replies to a client that is not connected go to its replay buffer while its identity
is known, else they are dropped and counted (`dropped_closed_session`).

## 3a. The link over plain requests (S27)

The same link without a held connection: the frames, their numbers and acknowledgements, the replay buffer, the
offline queue and `resumed` are §3's. Requests carry the frames, and a **session** stands for the connection: it is
what a `connected` opens and a `disconnected` closes.

- `POST /blossom/http/open`, the body the page's `HELLO`: answered with the node's `HELLO` and `HELLO_OK` (or a
  `REJECT`) and, when the member is admitted, the session's id in `Blossom-Session` (128 random bits: only the page
  holding it can use the session; the token is presented once, at the open).
- `GET /blossom/http/SESSION/recv`: the node's frames for the page in order (the `WELCOME`, then `MSG`s and `ACK`s),
  answered as soon as there are some or empty after 25 s (a long poll). The page keeps exactly one outstanding, and
  only receives answer with frames, so the frames arrive in order without a numbering of their own.
- `POST /blossom/http/SESSION/send`: the page's frames (`MSG`, `ACK`), answered `204`. A page sends a request at a
  time; frames written meanwhile go together in the next.
- `POST /blossom/http/SESSION/close`: the page is going (`navigator.sendBeacon` on a final `pagehide`).

A body is frames, each a 4-byte big-endian length and the frame's bytes. Requests on a connection the client keeps
(HTTP/1.1 keep-alive) are served in turn: a poll does not open a connection each time.

**When a session ends.** The program hears the link go down, as when a socket closes, when:

- the page closes it;
- the connection of its waiting receive closes (a closed tab: the node checks the connection twice a second while the
  receive waits);
- no request reached it for 30 s (a page polls well within that: a receive is answered within 25 s and the next one
  sent at once);
- the page does not take what the node sends (4096 frames or 32 MiB waiting), or a receive's answer cannot be written;
- the member opens another session (the node ends the one it replaces, as it closes a replaced WebSocket).

A request on an ended or unknown session is answered `410 Gone`, and any failed request is the page's loss of the
connection: it opens a new session with backoff, presenting what it took, and the link resumes from the replay
buffer. So a frame lost with a failed answer is resent; the page drops what it took twice, by number.

**What it costs.** A frame from the node reaches the page as fast as over a WebSocket (the receive is waiting); a
frame from the page costs a request. Between receives the page is not connected, and that is fine: the session, not a
socket, is the connection.

## 4. `blossom run --web`

`blossom run --deploy d.toml --node n1 --web 127.0.0.1:8080 [--web-root web]` serves, next to the peer and client
listeners:

- `GET /` and the page's files from `--web-root` (the built host: `index.html`, `host.js`, `host.css`, `pkg/`);
- `GET /blossom/app.json`: the program's source files, the deployment (node names and roles), this node's name, the
  WebSocket path (`link`), the requests' path (`http`), and which of the two a page uses (`transport`: the
  deployment's `[web] link`, `http` unless it says `websocket`; a page's `?link=websocket|http` overrides it, for
  tests);
- `GET /blossom/link` upgraded to a WebSocket: the link of §3;
- `/blossom/http/…`: the link of §3a.

The HTTP server is HTTP/1.1 with keep-alive and the WebSocket handshake (RFC 6455), hand-written over std threads like
the other listeners, with a thread per connection. The page connects back to the node that served it.

## 5. The page

The browser host (BROWSER.md) learns a second mode. Served by a node (it finds `/blossom/app.json`; a page with
`?app=` or served statically, where that is a 404, runs on its own as before) it compiles the program for the given
deployment and runs it as a client member:

- the engine runs as the member's `NodeId` with the client role, so only the rules placed at the client role run;
- each round's sends (`StepOutput.outbox`) go to the link (or the offline queue); the server's messages arrive as the
  next round's deliveries;
- the link events are inputs of the round in which the link comes up or goes down;
- the inspector records deliveries, so `why` explains a row that came from the server down to the message it came in.

What the page keeps, and where:

- **The link state** (`LinkState`: the member's id, token and seed; the last server batch taken; the last own batch
  acknowledged; the own batches not yet acknowledged, which are the offline queue) and the client role's durable
  tables, in `localStorage`. After every round the link state is written first and the tables second, so a line the
  tables say was sent is always in the queue as well.
- **One member per tab.** Tabs of one browser share `localStorage` but must be different members, or two tabs would
  present one token. Each tab holds a numbered slot, a Web Lock (`navigator.locks`) held for the page's life, and keeps
  its state under that slot. A reload releases the slot and takes it back; a second tab takes the next free one. Web
  Locks exist only in secure contexts (https, or localhost); elsewhere the page says so and does not run.
- **A page that knows its member runs at once** from what it stored, and its link connects meanwhile, so the program
  works while the node is unreachable. The first visit waits for the first `WELCOME`. The page itself comes from the
  node, so a page cannot be loaded while the node is down (no service worker).
- **A lost identity.** When the node no longer knows the token (a fresh store) and admits the page as a new member,
  the stored state belongs to the old identity: the page clears it and starts over.
- The WebSocket (or the HTTP session, §3a) reconnects after a loss with backoff (200 ms doubling to 5 s, jittered); a
  finished handshake resets it. The program is the node's, so the source editor is off in this mode; the inspector works.

## 6. The simulator, LDFI and tests

- In a simulation, a client member is a node of the client role named in the deployment (`--nodes s=Server,b1=Browser,
  b2=Browser`). Its link to each server node is up from the start, raising `connected(…, false)` in the first round; a
  crash of a client member takes its links down (`disconnected`), and its restart brings them back as a new link
  (`resumed` is false: a simulated link loses what it carried while down). The
  omission and delay faults of channels apply to client links like any other.
- LDFI searches the omissions of client channels as of any other: "if the add is lost, does the tab ever show it?"
- Tests: frontend (the kind, placement, the membership errors, link events); oracle and engine agree on a program with
  a server and two clients; the link protocol (resume within and past the buffer, the offline queue, duplicates);
  the HTTP and WebSocket server; Playwright with two tabs on one server (sync, reload, offline then online, server
  restart).

## 7. Sub-slices

1. **Language and semantics**: the `client` kind, placement, typing, membership errors, link events, client node ids
   in the engine and oracle, the simulator's client members.
2. **The link and the server**: frames, the client registry, the WebSocket server in `blossom-runtime`, replay
   buffers, `blossom run --web`.
3. **The page**: the host's server mode, sends and deliveries in `blossom-web`, the token and the offline queue, the
   inspector with deliveries.
4. **Demos**: shared TodoMVC and a chat room, with Playwright tests.

## 8. What a page is given (S22)

A page never gets the program's source, nor any rule or relation placed at another role. The node projects the
program onto each client role when it starts (`ClientArtifact::project`, over the IR's role projection):

- **What is kept:** the role's rules (without their role guard); the relations they read and write; every channel the
  role is an end of, as a schema (its other end's rules stay home); the link events at the role and the member
  relations and node directory of the roles it deals with (the deployment, which the page is given anyway); the
  types, functions, constants, lattices and sites those reach; and the anonymous types over them (the validator
  derives expression types structurally). Named types stay only where something kept names them.
- **The leak check** (`ClientArtifact::leaks`): every relation of the projection is the role's, shared, one of its
  channels, its link events, or a member relation; every rule is placed at the role. A node refuses to serve a
  projection that fails it (`blossom run --web` does not start), and so does `blossom build`.
- **The encoding:** `BLSC`, a format number, then the program as data (postcard) with its digest, the role, the
  deployment's nodes and their roles in the projection's numbering. Decoding validates the program again and checks
  the digest. Served at `/blossom/client/ROLE`; `/blossom/app.json` names the program, the node, the connection
  identity and where each role's artifact is, and nothing else. `blossom build --deploy D --role R --out F` writes
  the same bytes.
- **The page holds no compiler.** `web/pkg-member/` is `blossom-web` built without its `compiler` feature: the engine,
  the oracle (for the inspector) and the link (3.8 MB of wasm against 6.2 MB with the compiler). The host loads it
  when a node serves the page and the full build only for a page on its own.
- **A stale page.** A member's `HELLO` carries the first 16 bytes of its projection's digest (`part`); the node
  refuses a link whose part is not the one it serves (`REJECT program`). The page then loads again, which gets the
  current program, unless it did so in the last 30 seconds (then it says why and keeps trying).

## Out of scope

- Client-to-client links (WebRTC); clients relaying to other server nodes; load balancing a tab across server nodes.
- Authentication beyond the token (principals for clients come with LANG-240's security modes).
