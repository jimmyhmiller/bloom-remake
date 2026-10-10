// The keyed chat as a Vercel function (docs/design/DEPLOY.md §3): every page request answered by the function
// (crates/blossom-vercel), nothing running between requests, the page's files static. Two ways to run it:
//
// - locally (BLOSSOM_VERCEL_BIN, the built function, and BLOSSOM_TEST_POSTGRES from scripts/test-services.sh): three
//   instances of the function as Vercel's runtime runs it outside Vercel (each a server on VERCEL_DEV_PORT), behind a
//   front that does what Vercel's does: the page's files from web/, each /blossom/ request to the next instance, and
//   x-real-ip set to the client's address. Instances are killed mid-chat;
// - deployed (BLOSSOM_VERCEL_URL, from scripts/deploy-vercel.sh).
//
// Tabs in two rooms of their own (named for the run, since a deployment keeps every room) chat; every tab keeps every
// line of its room exactly once and none of the other's; each room lists the other; a tab opened later reads the
// room's history; a tab that goes quiet leaves its room's count at the next request there after its lease, with
// nothing else running.
import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { readFileSync, existsSync, statSync } from "node:fs";
import http from "node:http";
import { extname, join, normalize } from "node:path";
import { randomBytes } from "node:crypto";
import { freePort, repo } from "./node.mjs";

const deployed = process.env.BLOSSOM_VERCEL_URL;
const fnBin = process.env.BLOSSOM_VERCEL_BIN;
const SEED = "4e1f2a3b5c6d7e8f9a0b1c2d3e4f5a6b";

const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".wasm": "application/wasm",
  ".css": "text/css",
  ".json": "application/json",
  ".bls": "text/plain",
};

/** Three function instances over a Postgres schema of their own, and the front before them; `restart(i)` kills
 * instance `i` (SIGKILL) and starts another on its port. */
async function local() {
  const pg = process.env.BLOSSOM_TEST_POSTGRES;
  if (!pg) throw new Error("BLOSSOM_TEST_POSTGRES is not set: scripts/test-services.sh start, then its env");
  const store = `${pg}&schema=v_${randomBytes(6).toString("hex")}`;
  const ports = [await freePort(), await freePort(), await freePort()];
  const children = [];
  const start = async (i) => {
    const child = spawn(fnBin, [], {
      env: {
        ...process.env,
        VERCEL_DEV_PORT: String(ports[i]),
        BLOSSOM_DEPLOY: join(repo, "examples", "web", "keyed_chat.deploy.toml"),
        BLOSSOM_STORE: store,
        BLOSSOM_SEED: SEED,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let err = "";
    child.stderr.on("data", (b) => (err += b));
    await new Promise((ready, fail) => {
      let out = "";
      child.stdout.on("data", (b) => {
        out += b;
        if (out.includes("Dev server listening")) ready();
      });
      child.on("exit", (code) => fail(new Error(`the function exited ${code}: ${err}`)));
    });
    child.removeAllListeners("exit");
    children[i] = child;
  };
  await Promise.all([0, 1, 2].map(start));
  let next = 0;
  const counts = [0, 0, 0];
  const web = join(repo, "web");
  const front = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const url = new URL(req.url, "http://front");
      if (url.pathname.startsWith("/blossom/")) {
        const forward = (tries) => {
          const i = next++ % ports.length;
          counts[i] += 1;
          const headers = { ...req.headers, "x-real-ip": "127.0.0.1" };
          const up = http.request(
            { host: "127.0.0.1", port: ports[i], method: req.method, path: req.url, headers, agent: false },
            (r) => {
              res.writeHead(r.statusCode ?? 502, r.headers);
              r.pipe(res);
            },
          );
          up.on("error", (e) => {
            if (!res.headersSent && tries > 1 && e.code === "ECONNREFUSED") forward(tries - 1);
            else if (!res.headersSent) {
              res.writeHead(502, { "content-type": "text/plain" });
              res.end("no instance answered");
            } else res.destroy();
          });
          up.end(Buffer.concat(chunks));
        };
        forward(ports.length);
        return;
      }
      const rel = normalize(decodeURIComponent(url.pathname)).replace(/^\/+/, "") || "index.html";
      const file = join(web, rel);
      if (!file.startsWith(web) || !existsSync(file) || !statSync(file).isFile()) {
        res.writeHead(404, { "content-type": "text/plain" });
        res.end("not found");
        return;
      }
      res.writeHead(200, { "content-type": types[extname(file)] ?? "application/octet-stream" });
      res.end(readFileSync(file));
    });
  });
  const sockets = new Set();
  front.on("connection", (s) => {
    sockets.add(s);
    s.on("close", () => sockets.delete(s));
  });
  const frontPort = await new Promise((ready) => front.listen(0, "127.0.0.1", () => ready(front.address().port)));
  return {
    base: `http://localhost:${frontPort}`,
    counts,
    async restart(i) {
      children[i].kill("SIGKILL");
      await new Promise((done) => children[i].once("exit", done));
      await start(i);
    },
    async remove() {
      for (const s of sockets) s.destroy();
      await new Promise((done) => front.close(done));
      for (const c of children) c.kill("SIGKILL");
    },
  };
}

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);
const lines = (page) => page.locator(".lines .text");

async function open(page, base, room) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto(`${base}/?member=${room}&link=http`);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready", { timeout: 30_000 });
}

async function say(page, text) {
  await bid(page, "say").fill(text);
  await bid(page, "say").press("Enter");
}

test.describe("keyed chat as a Vercel function", () => {
  test.skip(!deployed && !fnBin, "BLOSSOM_VERCEL_URL (a deployment) or BLOSSOM_VERCEL_BIN (the built function)");
  test.setTimeout(180_000);

  test("two rooms chat, apart, and a tab that goes quiet leaves", async ({ browser, context }) => {
    const c = deployed ? null : await local();
    const base = deployed ?? c.base;
    try {
      const tag = randomBytes(4).toString("hex");
      const [lunch, dinner] = [`lunch${tag}`, `dinner${tag}`];
      const a = await context.newPage();
      const b = await context.newPage();
      const d = await context.newPage();
      await open(a, base, lunch);
      await open(b, base, lunch);
      await open(d, base, dinner);
      await expect(bid(a, "title")).toHaveText(`Room ${lunch}`);
      await expect(bid(d, "title")).toHaveText(`Room ${dinner}`);
      await expect(bid(a, "status")).toHaveText("online, 2 here", { timeout: 30_000 });
      await expect(bid(d, "status")).toHaveText("online, 1 here", { timeout: 30_000 });

      await say(a, "hello from a");
      await expect(lines(b)).toHaveText(["hello from a"]);
      await expect(bid(a, `go-${dinner}`)).toHaveText(dinner, { timeout: 30_000 });
      await expect(bid(d, `go-${lunch}`)).toHaveText(lunch, { timeout: 30_000 });

      if (c) await c.restart(0);
      await say(b, "hello from b");
      await expect(lines(a)).toHaveText(["hello from a", "hello from b"]);
      await say(d, "only in dinner");
      if (c) {
        await c.restart(1);
        await c.restart(2);
      }
      await say(a, "third");
      await expect(lines(a)).toHaveText(["hello from a", "hello from b", "third"]);
      await expect(lines(b)).toHaveText(["hello from a", "hello from b", "third"]);
      await expect(lines(d)).toHaveText(["only in dinner"]);

      // A tab opened later reads the room's history from the store. It has a browser of its own, so that it can go
      // away without a word below.
      const quiet = await browser.newContext();
      const e = await quiet.newPage();
      await open(e, base, lunch);
      await expect(lines(e)).toHaveText(["hello from a", "hello from b", "third"]);
      await expect(bid(a, "status")).toHaveText("online, 3 here", { timeout: 30_000 });

      // A tab that goes away without a word (its network is gone: not even the close a page sends as it unloads gets
      // out) leaves at the next request in its room after its 30 s lease: a's own polling is that request.
      await quiet.setOffline(true);
      await quiet.close();
      await expect(bid(a, "status")).toHaveText("online, 2 here", { timeout: 90_000 });

      if (c) for (const [i, n] of c.counts.entries()) expect(n, `requests to instance ${i}`).toBeGreaterThan(2);
    } finally {
      if (c) await c.remove();
    }
  });

  test("the app is never drawn before its stylesheet", async ({ page }) => {
    const c = deployed ? null : await local();
    const base = deployed ?? c.base;
    try {
      // Whether the app's stylesheet had loaded when the app first drew anything.
      await page.addInitScript(() => {
        new MutationObserver((_, seen) => {
          const app = document.getElementById("app");
          if (app && app.childElementCount > 0) {
            window.styledAtFirstDraw = document.getElementById("app-css").sheet !== null;
            seen.disconnect();
          }
        }).observe(document, { childList: true, subtree: true });
      });
      // A slow stylesheet (a cold function serves it) makes a flash plain, if there is one.
      await page.route("**/blossom/style.css", async (route) => {
        await new Promise((done) => setTimeout(done, 1500));
        await route.continue();
      });
      await open(page, base, `styled${randomBytes(4).toString("hex")}`);
      await expect(bid(page, "title")).toBeVisible();
      expect(await page.evaluate(() => window.styledAtFirstDraw)).toBe(true);
    } finally {
      if (c) await c.remove();
    }
  });

  test("a line shows at once, as pending, until the room confirms it", async ({ page }) => {
    const c = deployed ? null : await local();
    const base = deployed ?? c.base;
    try {
      await open(page, base, `quick${randomBytes(4).toString("hex")}`);
      await expect(bid(page, "status")).toHaveText("online, 1 here", { timeout: 30_000 });
      // The room is slow to hear this tab: its sends take 2 s.
      await page.route("**/blossom/http/**/send", async (route) => {
        await new Promise((done) => setTimeout(done, 2000));
        await route.continue();
      });
      await say(page, "right away");
      await expect(page.locator(".line.pending .text")).toHaveText(["right away"], { timeout: 1000 });
      await expect(page.locator(".line.pending")).toHaveCount(0, { timeout: 15_000 });
      await expect(lines(page)).toHaveText(["right away"]);
    } finally {
      if (c) await c.remove();
    }
  });

  test("the function answers only page requests", async ({ request }) => {
    const c = deployed ? null : await local();
    const base = deployed ?? c.base;
    try {
      const health = await request.get(`${base}/blossom/health`);
      expect(health.status()).toBe(200);
      expect(await health.text()).toBe("ok");
      // No scheduler calls in: there is no wake route.
      expect((await request.get(`${base}/blossom/wake`)).status()).toBe(404);
    } finally {
      if (c) await c.remove();
    }
  });
});
