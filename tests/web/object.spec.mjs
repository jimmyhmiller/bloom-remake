// The polls app's server as a Cloudflare Durable Object (docs/design/DURABLE-OBJECTS.md), run locally by workerd
// (`wrangler dev`, nothing deployed): two tabs vote through the object; workerd is killed and started again over the
// same storage, and the open tabs reconnect to what the object kept. Needs scripts/build-do.sh polls and `npm ci` in
// do/ (scripts/test-tiers.sh web does both); BLOSSOM_DO=1 runs it.
import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { freePort, repo } from "./node.mjs";

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);

/** `wrangler dev` over the storage in `dir`; `kill` stops workerd outright. */
async function worker(dir, port) {
  const child = spawn("npx", ["wrangler", "dev", "--port", String(port), "--ip", "127.0.0.1", "--persist-to", dir], {
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
  let w = await worker(dir, port);
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
    w = await worker(dir, port);
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
