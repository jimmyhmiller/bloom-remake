// Blossom apps as Cloudflare Durable Objects (docs/design/DURABLE-OBJECTS.md), run locally by workerd (`wrangler dev`,
// nothing deployed). The polls app's server is one object: two tabs vote through it; workerd is killed and started
// again over the same storage, and the open tabs reconnect to what the object kept. The rooms app's rooms are keyed
// members (docs/design/KEYED.md), each an object of its own that pages reach by `?member=`, with tokens a registry
// object minted. Needs scripts/build-do.sh polls, scripts/build-do.sh rooms Room and `npm ci` in do/
// (scripts/test-tiers.sh web does all three); BLOSSOM_DO=1 runs them.
import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { freePort, repo } from "./node.mjs";

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);

/** `wrangler dev` of the app built in do/build/APP, over the storage in `dir`; `kill` stops workerd outright. */
async function worker(app, dir, port) {
  const args = ["wrangler", "dev", "--config", `build/${app}/wrangler.toml`, "--port", String(port), "--ip", "127.0.0.1"];
  const child = spawn("npx", [...args, "--persist-to", dir], {
    cwd: join(repo, "do"),
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  let log = "";
  await new Promise((ready, fail) => {
    const seen = (b) => {
      log += b;
      if (log.includes("Ready on")) ready();
    };
    child.stdout.on("data", seen);
    child.stderr.on("data", seen);
    child.on("exit", (code) => fail(new Error(`wrangler dev exited (${code}): ${log}`)));
  });
  return {
    /** Kills wrangler and its workerd (their process group). */
    async kill() {
      await new Promise((gone) => {
        child.on("exit", gone);
        process.kill(-child.pid, "SIGKILL");
      });
    },
  };
}

async function member(context, url, name) {
  const p = await context.newPage();
  p.on("pageerror", (e) => console.log("page error:", e.message));
  await p.goto(url);
  await expect(p.locator("body")).toHaveAttribute("data-blossom", "ready");
  await bid(p, "name").fill(name);
  await bid(p, "name").press("Enter");
  await expect(bid(p, "who")).toContainText(`You are ${name}.`);
  return p;
}

test("polls on a Durable Object: votes go through it, and it keeps them across a restart", async ({ context }) => {
  test.skip(!process.env.BLOSSOM_DO, "BLOSSOM_DO=1, after scripts/build-do.sh polls and npm ci in do/");
  test.setTimeout(120_000);
  const dir = mkdtempSync(join(tmpdir(), "blossom-object-"));
  const port = await freePort();
  const url = `http://127.0.0.1:${port}/`;
  let w = await worker("polls", dir, port);
  try {
    const a = await member(context, url, "Ada");
    const b = await member(context, url, "Bob");
    await expect(b.locator(".here .person")).toHaveText(["Ada"]);
    await bid(a, "question").fill("Lunch?");
    await bid(a, "opt-0").fill("Tacos");
    await bid(a, "opt-1").fill("Ramen");
    await bid(a, "ask").click();
    const counts = (p) => p.locator(".poll .choice .count");
    await b.locator(".choice .text", { hasText: "Ramen" }).click();
    await a.locator(".choice .text", { hasText: "Ramen" }).click();
    await expect(counts(a)).toHaveText(["0", "2"]);
    await expect(counts(b)).toHaveText(["0", "2"]);
    // workerd dies; the tabs go offline and keep working on their own copies.
    await w.kill();
    await expect(bid(a, "status")).toHaveText(/offline/);
    w = await worker("polls", dir, port);
    // The object starts again from its storage; the tabs reconnect as the same members.
    await expect(bid(a, "status")).toHaveText("live", { timeout: 30_000 });
    await expect(bid(b, "status")).toHaveText("live", { timeout: 30_000 });
    await b.locator(".choice .text", { hasText: "Tacos" }).click();
    await expect(counts(a)).toHaveText(["1", "1"]);
    // A new tab gets everything from the object.
    const c = await member(context, url, "Cy");
    await expect(c.locator(".poll h2")).toHaveText(["Lunch?"]);
    await expect(counts(c)).toHaveText(["1", "1"]);
    await expect(c.locator(".poll .voters").nth(1)).toHaveText("Ada");
  } finally {
    await w.kill();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("rooms on Durable Objects: a game per room, each its own object, kept across a restart", async ({ context }) => {
  test.skip(!process.env.BLOSSOM_DO, "BLOSSOM_DO=1, after scripts/build-do.sh rooms Room and npm ci in do/");
  test.setTimeout(120_000);
  const dir = mkdtempSync(join(tmpdir(), "blossom-rooms-object-"));
  const port = await freePort();
  const url = `http://127.0.0.1:${port}/`;
  let w = await worker("rooms", dir, port);
  const at = async (room, name) => {
    const p = await context.newPage();
    p.on("pageerror", (e) => console.log("page error:", e.message));
    await p.goto(`${url}?member=${room}`);
    await expect(p.locator("body")).toHaveAttribute("data-blossom", "ready");
    await expect(bid(p, "title")).toHaveText(`Room ${room}`);
    await bid(p, "name").fill(name);
    await bid(p, "name").press("Enter");
    await expect(bid(p, "who")).toContainText(`You are ${name}.`);
    return p;
  };
  try {
    const a = await at("lunch", "Ada");
    const b = await at("lunch", "Bob");
    const e = await at("dinner", "Eve");
    await expect(a.locator(".here .person")).toHaveText(["Bob"]);
    await expect(e.locator(".here .person")).toHaveCount(0);
    await bid(a, "sit").click();
    await expect(bid(b, "headline")).toHaveText("Ada plays X: sit down to play O.");
    await bid(b, "sit").click();
    await expect(bid(a, "headline")).toHaveText("Your move (X).");
    await expect(bid(e, "headline")).toHaveText("Nobody is playing yet: sit down to play X.");
    const sq = (p, i) => p.locator(".grid button").nth(i);
    for (const [p, cell, next] of [
      [a, 0, b],
      [b, 4, a],
      [a, 1, b],
      [b, 8, a],
    ]) {
      await sq(p, cell).click();
      await expect(bid(next, "headline")).toHaveText(/^Your move/);
    }
    await sq(a, 2).click();
    await expect(bid(a, "headline")).toHaveText("You win!");
    await expect(bid(b, "headline")).toHaveText("You lose.");
    // workerd dies and comes back: each room from its own object's storage.
    await w.kill();
    await expect(bid(a, "status")).toHaveText("offline");
    w = await worker("rooms", dir, port);
    await expect(bid(a, "status")).toHaveText("live", { timeout: 30_000 });
    const late = await at("lunch", "Gus");
    await expect(bid(late, "headline")).toHaveText("Ada wins.");
    await expect(late.locator(".grid button")).toHaveText(["X", "X", "X", "", "O", "", "", "", "O"]);
    const other = await at("dinner", "Hal");
    await expect(other.locator(".here .person")).toHaveText(["Eve"]);
  } finally {
    await w.kill();
    rmSync(dir, { recursive: true, force: true });
  }
});
