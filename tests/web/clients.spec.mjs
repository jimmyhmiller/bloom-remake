// Pages as members of a node's program (docs/design/CLIENTS.md): a real `blossom run --web` serves the shared TodoMVC
// (examples/web/todos_shared.bls) and the chat (examples/web/chat.bls), and Chromium tabs run as members of their
// `Browser` role. Two tabs stay in sync, a reload keeps a tab's identity and state, a tab cut off from the node keeps
// working and catches up, and the node's restart is survived. Every test runs twice, with the deployment's `[web]
// link` (CLIENTS.md §3a) set to a WebSocket and to plain requests, where the page opens no WebSocket at all. Needs the
// CLI: BLOSSOM_BIN (scripts/test-tiers.sh web builds it and sets it).
import { test, expect } from "@playwright/test";
import { bin, deployment } from "./node.mjs";

/** The WebSockets each page opened (none over the HTTP link). */
const sockets = new WeakMap();

async function open(page, url) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  if (!sockets.has(page)) {
    sockets.set(page, []);
    page.on("websocket", (ws) => sockets.get(page).push(ws.url()));
  }
  await page.goto(url);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
}

const labels = (page) => page.locator(".todo-list li label");
/** The element the program calls `id`. */
const bid = (page, id) => page.locator(`[data-bid="${id}"]`);
const sync = (page) => bid(page, "sync");

async function add(page, title) {
  await page.locator(".new-todo").fill(title);
  await page.locator(".new-todo").press("Enter");
}

for (const link of ["websocket", "http"]) test.describe(`members of a node's program (${link} link)`, () => {
  test.skip(!bin, "BLOSSOM_BIN names the blossom CLI (scripts/test-tiers.sh web sets it)");

  /** Over the HTTP link, no page opened a WebSocket; over the WebSocket one, each did. */
  function checkTransport(...pages) {
    for (const p of pages) {
      const opened = sockets.get(p) ?? [];
      if (link === "http") expect(opened).toEqual([]);
      else expect(opened.length).toBeGreaterThan(0);
    }
  }

  test("two tabs share one todo list; a reload keeps a tab's identity", async ({ context }) => {
    const d = await deployment("todos_shared", link);
    try {
      await d.start(true);
      const a = await context.newPage();
      const b = await context.newPage();
      const fetched = [];
      a.on("request", (r) => fetched.push(new URL(r.url()).pathname));
      await open(a, d.url);
      await open(b, d.url);
      await expect(sync(a)).toHaveText("synced with the server");
      await expect(sync(b)).toHaveText("synced with the server");
      // The page got its role's part of the program and the engine-only module: no source, no compiler.
      expect(fetched).toContain("/blossom/client/Browser");
      expect(fetched).toContain("/pkg-member/blossom_web_bg.wasm");
      expect(fetched.filter((p) => p.endsWith(".bls") || p.startsWith("/pkg/"))).toEqual([]);
      await add(a, "buy milk");
      await expect(labels(b)).toHaveText(["buy milk"]);
      await add(b, "walk the dog");
      await expect(labels(a)).toHaveText(["buy milk", "walk the dog"]);
      // The two tabs are different members: each made one todo, under its own identity.
      const ids = await a.locator(".todo-list li").evaluateAll((els) => els.map((e) => e.dataset.bid));
      expect(new Set(ids.map((id) => id.replace(/^todo-\d+-/, ""))).size).toBe(2);
      // b ticks a's todo; a sees it.
      await b.locator(".todo-list li").filter({ hasText: "buy milk" }).locator(".toggle").check();
      await expect(a.locator(".todo-list li.completed label")).toHaveText(["buy milk"]);
      // A reload is the same member: its next todo is numbered after its first.
      await b.reload();
      await expect(b.locator("body")).toHaveAttribute("data-blossom", "ready");
      await expect(labels(b)).toHaveText(["buy milk", "walk the dog"]);
      await add(b, "feed the cat");
      await expect(labels(a)).toHaveText(["buy milk", "walk the dog", "feed the cat"]);
      const bids = await a.locator(".todo-list li").evaluateAll((els) => els.map((e) => e.dataset.bid));
      const bOwner = bids[1].replace(/^todo-\d+-/, "");
      expect(bids[2]).toBe(`todo-1-${bOwner}`);
      checkTransport(a, b);
    } finally {
      await d.remove();
    }
  });

  test("a tab keeps working while the node is down and catches up after its restart", async ({ context }) => {
    const d = await deployment("todos_shared", link);
    try {
      await d.start(true);
      const a = await context.newPage();
      const b = await context.newPage();
      await open(a, d.url);
      await open(b, d.url);
      await add(a, "before the crash");
      await expect(labels(b)).toHaveText(["before the crash"]);
      await d.kill();
      await expect(sync(a)).toHaveText("offline: changes wait until the server is back");
      await expect(sync(b)).toHaveText("offline: changes wait until the server is back");
      // Offline, a tab still works on its own copy, and the change waits in its link.
      await add(a, "while it was down");
      await expect(labels(a)).toHaveText(["before the crash", "while it was down"]);
      await expect(labels(b)).toHaveText(["before the crash"]);
      // The node comes back (its store survived the crash); both tabs reconnect, and the waiting change goes out.
      await d.start(false);
      await expect(sync(a)).toHaveText("synced with the server", { timeout: 15_000 });
      await expect(sync(b)).toHaveText("synced with the server", { timeout: 15_000 });
      await expect(labels(b)).toHaveText(["before the crash", "while it was down"]);
      await add(b, "after");
      await expect(labels(a)).toHaveText(["before the crash", "while it was down", "after"]);
      checkTransport(a, b);
    } finally {
      await d.remove();
    }
  });

  test("a page built from an older program loads the new one when its link is refused", async ({ context }) => {
    const d = await deployment("todos_shared", link);
    try {
      await d.start(true);
      const a = await context.newPage();
      await open(a, d.url);
      await add(a, "survives the upgrade");
      await expect(sync(a)).toHaveText("synced with the server");
      await expect(a.locator(".info")).toContainText("shared by every tab");
      // The node comes back running a changed tab part: the open page's link is refused, and it loads again.
      await d.kill();
      d.edit((src) => src.replace("Written in Blossom, shared by every tab", "Written in Blossom, now upgraded"));
      await d.start(false);
      await expect(a.locator(".info")).toContainText("now upgraded", { timeout: 15_000 });
      await expect(sync(a)).toHaveText("synced with the server");
      await expect(labels(a)).toHaveText(["survives the upgrade"]);
    } finally {
      await d.remove();
    }
  });

  test("a chat between two tabs", async ({ context }) => {
    const d = await deployment("chat", link);
    try {
      await d.start(true);
      const a = await context.newPage();
      const b = await context.newPage();
      await open(a, d.url);
      await open(b, d.url);
      await expect(bid(a, "status")).toHaveText("online, 2 here");
      await bid(a, "say").fill("hello");
      await bid(a, "say").press("Enter");
      await expect(b.locator(".lines .text")).toHaveText(["hello"]);
      await expect(a.locator(".lines li[data-mine=true] .text")).toHaveText(["hello"]);
      await expect(b.locator(".lines li[data-mine=true]")).toHaveCount(0);
      checkTransport(a, b);
      await b.close();
      await expect(bid(a, "status")).toHaveText("online, 1 here");
    } finally {
      await d.remove();
    }
  });
});
