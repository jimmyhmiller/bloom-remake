# Deploying `blossom serve`

**Status (2026-10-10):** branch `slice-deploy`. `blossom serve` hardened for the open internet (§2) and deployed to
Vercel as a container function over Neon Postgres (§3).

The user (2026-10-10), after stateless hosting (STATELESS.md) and SQL tables (SQL-TABLES.md): harden `blossom serve`
for deploying, and deploy it on Vercel.

## 1. What a platform asks of a host

A function or container platform starts copies of a process on demand, routes HTTP to the port it names (`PORT`),
freezes or stops copies it does not need (`SIGTERM`, then a grace period), and offers a scheduler that calls a URL.
It terminates TLS, sets the client's address in a header, and caps request bodies and durations. `blossom serve`
already keeps nothing between requests (STATELESS.md §5): every request loads, runs and commits through the store.
What it lacked was what an open port on the internet needs.

## 2. Hardening

- **The wake route has a secret.** `/blossom/wake` (GET, which Vercel's cron jobs make, or POST) runs a sweep only for
  `Authorization: Bearer SECRET`, the secret read from the environment variable `--wake-secret-env` names (default
  `BLOSSOM_WAKE_SECRET`; `CRON_SECRET` on Vercel, which its cron jobs send). It is compared by hash, so neither how
  much of a guess matched nor its length shows. Without a secret the route answers 403; with one shorter than 16
  characters, or none while `--sweep off` leaves the route as the only sweeper, the host refuses to start.
- **Health.** `GET /blossom/health` answers `ok` when the store does, 503 with the store's error when not.
- **Page tokens are rationed.** Each `/blossom/token` takes one of a node's 2^20 page serials (`NodeId::client`),
  forever, so an unauthenticated script could use them all. Each client is given `--tokens-per-minute` (60) at most,
  as a bucket that fills again at that rate; a client is its connection's address, or the first address in the header
  `--client-ip-header` names (`x-real-ip` behind Vercel's proxy, which overwrites it). A host remembers at most 65,536
  clients; past that a new client is refused until one's allowance is whole again. Refused: 429 and `Retry-After`.
  This slows the drain of the pool; it does not refill it (§5).
- **Connections are capped** at `--max-connections` (1024; a thread each): another is answered 503 and closed.
- **Bodies** are at most 4 MiB (they carry a page's frames; Vercel refuses more than about 4.5 MB itself).
- **Draining.** On SIGTERM or SIGINT the host stops accepting and sweeping, closes idle connections at once (a
  connection between requests looks every 250 ms), lets requests in flight finish (a receive waits at most 25 s),
  closes WebSockets at their next presence (10 s; the page reconnects to another copy), and exits after at most
  `--drain` seconds (25; Vercel's grace is 30). Nothing is lost by a drain cut short: every request's effect is
  committed before it is answered, and pages resume from their cursors (STATELESS.md §6).
- **Configuration from the environment**: the store from `BLOSSOM_STORE` (a URL with a password stays out of the
  process list), the address from `PORT` (every interface) when `--web` is not given.
- **`--insecure-dev` is gone from `serve`.** A deployment's `security.mode` secures `blossom run`'s TCP between nodes;
  `serve`'s nodes reach each other only through the store, and pages prove who they are with tokens signed by the
  deployment's seed (STATELESS.md §7). Asking for the flag said something untrue about the host.
- Session secrets were already compared whole; the one comparison that was not (a send's check inside its request)
  now is.

## 3. Vercel

Vercel runs a container image as a function (`vercel.json`: a `services` entry with `runtime: "container"`), on
x86_64 Linux with one CPU, `PORT=80`, requests up to its maximum duration (a 25 s receive is well within), no bodies
over about 4.5 MB, and production URLs public (deployment URLs sit behind Vercel's login). An instance is frozen when
it serves nothing, so its own sweeper runs only while it serves; a cron job every minute (Pro plans) calls
`/blossom/wake` for the rest. So a timer fires at most about a minute late when nobody is using the app, and on time
while anybody is.

- `deploy/vercel/Dockerfile.vercel`: builds `blossom` (the pinned toolchain) and the page (`scripts/build-web.sh`, with
  the wasm-bindgen release the page pins, checked by its SHA-256), and runs `blossom serve --sweep 1000
  --wake-secret-env CRON_SECRET --client-ip-header x-real-ip` for `BLOSSOM_DEPLOY`.
- `deploy/vercel/vercel.json`: the service, a rewrite of every path to it, and the cron job.
- `scripts/deploy-vercel.sh`: stages the repository at a commit with those two files, links the project (creating
  it), provisions a Neon database with `--database` (free plan, `iad1`, beside the function), makes `BLOSSOM_SEED` and
  `CRON_SECRET` once, sets `BLOSSOM_STORE` from the database's direct URL on every deploy (the store `LISTEN`s, which
  Neon's pooler cannot carry), deploys, and waits for `/blossom/health`.

The state is the store's: the chat's rooms, lobby, sessions and outboxes, and its durable relations as tables
(SQL-TABLES.md), in the schema named after the deployment.

## 4. Tests

- `tests/integration/tests/serve_http.rs`, over real connections: the wake secret (none, wrong, all but its last
  character, right, by GET and POST; a host without one refuses everyone), health over a store that is up and one
  that is down, token allowances by connection and by proxy header, the connection cap, a drain that closes an idle
  connection at once and takes no new ones. Each check was seen to fail with its fix broken.
- `crates/blossom-cli/tests/it/serve.rs`, the binary: the store and port from the environment, refusals without a wake
  secret (or a short one) when only a scheduler sweeps, and SIGTERM draining to exit 0 with an idle connection open.
- The Postgres state store's conformance suite against Neon over TLS from outside its region.
- On Vercel: the keyed chat's Playwright proof against the deployed URL, the wake route from outside (401, 403) and
  Vercel's cron calling it, and the chat's lines in Neon by SQL.

## 5. Open

- Page serials are finite (2^20 per node) and never reclaimed: rationing slows a script that mints tokens, it does not
  stop one with many addresses. Wider client ids, or reclaiming the serials of pages gone for good, is its own work.
- One instance's allowances are its own: on a platform with many instances, a client gets up to that many allowances.
