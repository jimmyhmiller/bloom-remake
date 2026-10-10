# Deploying

**Status (2026-10-10):** branch `slice-deploy`. The keyed chat as a Vercel function (Vercel's Rust runtime) over Neon
Postgres (§3); `blossom serve` hardened for hosting it yourself (§2).

The user (2026-10-10), after stateless hosting (STATELESS.md) and SQL tables (SQL-TABLES.md): deploy it on Vercel,
serverless, with the Rust runtime: functions that answer requests, nothing running between them. (A first attempt ran
`blossom serve` in a container with a cron job to fire time-driven work; the user rejected it, rightly: §1.)

## 1. Only requests

A function runs while it answers a request. So everything a deployment does has to happen in some request:

- **A page's link** is requests already (CLIENTS.md §3a): open, receive (a long poll), send, close.
- **Messages between objects** are delivered by the request that sent them (STATELESS.md §8); what an instance that
  died left in an outbox goes with the sender's next request.
- **Who is here** is settled when the object is asked (STATELESS.md §6.4): before a page's request on an object, a
  session whose page made no request for its 30 s lease ends. A page that went away without a word leaves at the next
  request in its room; a page waiting in the room is such a request (its long poll looks every 5 s).
- **The store's housekeeping** (history below the floor, unreferenced blobs) runs after a request's answer is sent, at
  most once a minute per instance (Vercel's `waitUntil`).
- **A program's own timers** (`every`, `once after`) fire as time passes, which needs something running at that time.
  A function host refuses such a program when it starts, naming the timers; `blossom serve` (§2), whose sweeper fires
  them, hosts it. `once` fires in a node's first tick, so it is fine.

No scheduler, no cron job, no wake route.

## 2. `blossom serve` on the open internet

For hosting a deployment yourself (a server, a container). It has the routes of §3 and what a server of its own adds:

- **The wake route has a secret.** `/blossom/wake` (GET or POST) runs a sweep only for `Authorization: Bearer SECRET`,
  the secret read from the environment variable `--wake-secret-env` names (default `BLOSSOM_WAKE_SECRET`), compared by
  hash. Without a secret the route answers 403; with one shorter than 16 characters, or none while `--sweep off`
  leaves the route as the only sweeper, the host refuses to start.
- **Connections are capped** at `--max-connections` (1024; a thread each): another is answered 503.
- **Draining.** On SIGTERM or SIGINT the host stops accepting and sweeping, closes idle connections at once, lets
  requests in flight finish, closes WebSockets at their next presence, and exits after at most `--drain` seconds (25).
  Nothing is lost by a drain cut short: every request's effect is committed before it is answered.
- **Configuration from the environment**: the store from `BLOSSOM_STORE`, the address from `PORT` when `--web` is not
  given.
- **`--insecure-dev` is gone from `serve`**: a deployment's `security.mode` secures `blossom run`'s TCP between nodes;
  `serve`'s nodes reach each other only through the store, and pages prove who they are with tokens signed by the
  deployment's seed (STATELESS.md §7).

Shared with the function (`blossom_runtime::stateless::front`, so both answer a page the same way):

- **Health.** `GET /blossom/health` answers `ok` when the store does, 503 with the store's error when not.
- **Page tokens are rationed.** Each `/blossom/token` takes one of a node's 2^20 page serials, forever, so an
  unauthenticated script could use them all. Each client is given 60 a minute (`--tokens-per-minute`;
  `BLOSSOM_TOKENS_PER_MINUTE` for the function), as a bucket that fills again at that rate; a client is its
  connection's address, or the first address in a header its proxy sets (`--client-ip-header`; `x-real-ip` on
  Vercel). At most 65,536 clients are remembered per instance. Refused: 429 and `Retry-After`.
- **Bodies** are at most 4 MiB.
- Session secrets are compared whole, everywhere.

## 3. Vercel

`crates/blossom-vercel` is the function: `api/blossom.rs`, built by Vercel's Rust builder (`@vercel/rust`, which runs
`cargo build -p blossom-vercel --bin blossom-vercel --release` in the workspace). Its `App` loads the deployment when
an instance starts (the program compiled and its lattice laws checked, as `blossom serve` does) and answers each
request under `/blossom/` with the shared front; nothing it holds is needed by the next request. Its configuration is
the environment: `BLOSSOM_DEPLOY` (the spec, bundled with its directory), `BLOSSOM_STORE` (Postgres or S3: a function's
disk does not outlive its request), `BLOSSOM_SEED`. It refuses, by name, what a function cannot do: clocked timers
(§1), and pages linked over WebSockets (a function holds no socket; the deployment links them over plain requests). An
instance that cannot load answers every request with why.

- `deploy/vercel/vercel.json`: the function (with the deployment's directory in its bundle), the page's files
  (`web/`) as static files, `/blossom/*` to the function and the rest to `web/`.
- `scripts/deploy-vercel.sh`: builds the page (`scripts/build-web.sh`), stages the repository at a commit with it,
  links the project (creating it), provisions a Neon database with `--database` (free plan, `iad1`), makes
  `BLOSSOM_SEED` once, sets `BLOSSOM_STORE` from the database's direct URL on every deploy (the store `LISTEN`s, which
  Neon's pooler cannot carry), deploys, and waits for `/blossom/health`.

The state is the store's: the rooms, the lobby, sessions and outboxes, and the durable relations as tables
(SQL-TABLES.md), in the schema named after the deployment.

## 4. Tests

- `tests/integration/tests/stateless.rs` `a_quiet_tab_leaves_at_the_next_request_on_its_room`: two tabs, a 300 ms
  lease, one goes quiet; the other's receive alone (no sweep, no wake) has the room hear it go. Fails without the
  check.
- `tests/integration/tests/serve_http.rs`, over real connections: the wake secret, health over a store that is up and
  one that is down, token allowances by connection and by proxy header, the connection cap, a drain. Each check was
  seen to fail with its fix broken.
- `crates/blossom-cli/tests/it/serve.rs`, the binary: the store and port from the environment, refusals without a wake
  secret when only a scheduler sweeps, SIGTERM draining to exit 0.
- `tests/web/vercel.spec.mjs`, the keyed chat in Chromium: locally, three instances of the built function (Vercel's
  runtime outside Vercel serves on `VERCEL_DEV_PORT`) behind a front doing what Vercel's does, over the services'
  Postgres, with instances killed mid-chat; and against a deployment (`BLOSSOM_VERCEL_URL`). Two rooms chat apart,
  history reads back, a tab whose network is gone leaves its room at the next request there after its lease, and there
  is no wake route.
- The Postgres state store's conformance suite against Neon over TLS from outside its region.

## 5. Open

- Page serials are finite (2^20 per node) and never reclaimed: rationing slows a script that mints tokens, it does not
  stop one with many addresses. Wider client ids, or reclaiming the serials of pages gone for good, is its own work.
- One instance's allowances are its own: with many instances, a client gets up to that many allowances.
- A program with clocked timers cannot be a function (§1).
