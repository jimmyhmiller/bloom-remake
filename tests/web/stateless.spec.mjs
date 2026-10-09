// The proof of stateless hosting (docs/design/STATELESS.md §10): the keyed chat (examples/web/keyed_chat.bls) on
// three `blossom serve` instances that keep nothing between requests, behind a round-robin proxy that sends each
// request to the next instance (scripts/rr-proxy.mjs), for each state store (SQLite, Postgres, S3) and each link
// (plain requests, WebSockets). Chromium tabs in two rooms chat; an instance is killed mid-chat and another started
// in its place; every tab keeps every line of its room exactly once and none of the other's, lists the other room,
// and a tab that leaves leaves its room's count.
//
// Needs the CLI (BLOSSOM_BIN) and, for Postgres and S3, the services of scripts/test-services.sh with the variables
// its `env` prints (scripts/test-tiers.sh web starts them and sets them). A store whose variables are missing fails.
import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { bin, freePort, repo } from "./node.mjs";
import { startProxy } from "../../scripts/rr-proxy.mjs";

const SEED = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";

function need(name) {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is not set: run scripts/test-services.sh start, then eval "$(scripts/test-services.sh env)"`);
  return v;
}

/** Each store: its URL for a test of its own (a file, a schema, a prefix), and the environment its instances need. */
const stores = {
  sqlite: (dir) => ({ url: `sqlite:${join(dir, "state.db")}`, env: {} }),
  postgres: (_, tag) => ({ url: `${need("BLOSSOM_TEST_POSTGRES")}&schema=t_${tag}`, env: {} }),
  s3: (_, tag) => {
    const [path, query] = need("BLOSSOM_TEST_S3").split("?");
    return {
      url: `${path}/${tag}?${query}`,
      env: { AWS_ACCESS_KEY_ID: need("BLOSSOM_TEST_S3_KEY"), AWS_SECRET_ACCESS_KEY: need("BLOSSOM_TEST_S3_SECRET") },
    };
  },
};

/** Three instances over one store, and the proxy before them; `restart(i)` kills instance `i` (SIGKILL) and starts
 * another on its port. */
async function cluster(store) {
  const dir = mkdtempSync(join(tmpdir(), "blossom-stateless-"));
  const tag = randomBytes(6).toString("hex");
  const { url, env } = stores[store](dir, tag);
  const ports = [await freePort(), await freePort(), await freePort()];
  const children = [];
  const start = async (i) => {
    const child = spawn(
      bin,
      [
        "serve",
        "--deploy",
        join(repo, "examples", "web", "keyed_chat.deploy.toml"),
        "--store",
        url,
        "--web",
        `127.0.0.1:${ports[i]}`,
        "--web-root",
        join(repo, "web"),
        "--insecure-dev",
      ],
      { env: { ...process.env, ...env, BLOSSOM_SEED: SEED }, stdio: ["ignore", "pipe", "pipe"] },
    );
    let err = "";
    child.stderr.on("data", (b) => (err += b));
    await new Promise((ready, fail) => {
      let out = "";
      child.stdout.on("data", (b) => {
        out += b;
        if (out.includes("serving the page")) ready();
      });
      child.on("exit", (code) => fail(new Error(`blossom serve exited ${code}: ${err}`)));
    });
    child.removeAllListeners("exit");
    children[i] = child;
  };
  await Promise.all([0, 1, 2].map(start));
  const proxy = await startProxy(0, ports.map((p) => `127.0.0.1:${p}`));
  return {
    url: `http://localhost:${proxy.port}/`,
    counts: proxy.counts,
    async restart(i) {
      children[i].kill("SIGKILL");
      await new Promise((done) => children[i].once("exit", done));
      await start(i);
    },
    async remove() {
      await proxy.close();
      for (const c of children) c.kill("SIGKILL");
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);
const lines = (page) => page.locator(".lines .text");

async function open(page, base, room, link) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto(`${base}?member=${room}&link=${link}`);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
}

async function say(page, text) {
  await bid(page, "say").fill(text);
  await bid(page, "say").press("Enter");
}

for (const store of ["sqlite", "postgres", "s3"]) {
  for (const link of ["http", "websocket"]) {
    test.describe(`keyed chat on stateless hosts (${store}, ${link} link)`, () => {
      test.skip(!bin, "BLOSSOM_BIN names the blossom CLI (scripts/test-tiers.sh web sets it)");
      test.setTimeout(120_000);

      test("rooms chat across instances, and survive an instance's death", async ({ context }) => {
        const c = await cluster(store);
        try {
          const sockets = [];
          context.on("page", (p) => p.on("websocket", (ws) => sockets.push(ws.url())));
          const a = await context.newPage();
          const b = await context.newPage();
          const d = await context.newPage();
          await open(a, c.url, "lunch", link);
          await open(b, c.url, "lunch", link);
          await open(d, c.url, "dinner", link);
          await expect(bid(a, "title")).toHaveText("Room lunch");
          await expect(bid(d, "title")).toHaveText("Room dinner");
          await expect(bid(a, "status")).toHaveText("online, 2 here");
          await expect(bid(d, "status")).toHaveText("online, 1 here");

          await say(a, "hello from a");
          await expect(lines(b)).toHaveText(["hello from a"]);
          // Each room lists the other: the lobby, another object, told them.
          await expect(bid(a, "go-dinner")).toHaveText("dinner");
          await expect(bid(d, "go-lunch")).toHaveText("lunch");

          // An instance dies mid-chat (whatever request it held fails, and the page tries again), and another takes
          // its place.
          await c.restart(0);
          await say(b, "hello from b");
          // Lines show in the order their room took them: each is waited for before the next tab speaks.
          await expect(lines(a)).toHaveText(["hello from a", "hello from b"]);
          await say(d, "only in dinner");
          await say(a, "after the restart");
          await expect(lines(a)).toHaveText(["hello from a", "hello from b", "after the restart"]);
          await expect(lines(b)).toHaveText(["hello from a", "hello from b", "after the restart"]);
          await expect(lines(d)).toHaveText(["only in dinner"]);

          // Every instance dies, one after another: nothing is lost, since none held anything.
          await c.restart(1);
          await c.restart(2);
          await c.restart(0);
          await say(d, "still here");
          await expect(lines(d)).toHaveText(["only in dinner", "still here"]);
          await say(b, "and here");
          await expect(lines(a)).toHaveText(["hello from a", "hello from b", "after the restart", "and here"]);

          // A tab that leaves leaves its room's count.
          await b.close();
          await expect(bid(a, "status")).toHaveText("online, 1 here", { timeout: 20_000 });

          // Every instance served requests: the tabs' links really moved between them.
          for (const [instance, n] of Object.entries(c.counts)) expect(n, `requests to ${instance}`).toBeGreaterThan(2);
          // Over plain requests no page opened a WebSocket; over WebSockets they did.
          if (link === "http") expect(sockets).toEqual([]);
          else expect(sockets.length).toBeGreaterThan(0);
        } finally {
          await c.remove();
        }
      });
    });
  }
}
