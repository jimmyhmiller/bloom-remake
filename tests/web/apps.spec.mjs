// The shared example apps (examples/web/{polls,tictactoe,board,pixels}.bls): each runs on a real `blossom run --web`
// node, and Chromium tabs run as members of its `Browser` role. Every app is one Blossom program for both ends, with
// channels as its only API; these tests check what a person would: tabs agree, the server's rules hold, a reload
// keeps a tab's identity, and the node's crash and restart lose nothing. Needs the CLI: BLOSSOM_BIN.
import { test, expect } from "@playwright/test";
import { bin, deployment } from "./node.mjs";

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);

async function open(page, url) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto(url);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
}

/** A tab of the app, signed in as `name`. */
async function member(context, d, name) {
  const p = await context.newPage();
  await open(p, d.url);
  await bid(p, "name").fill(name);
  await bid(p, "name").press("Enter");
  await expect(bid(p, "who")).toContainText(`You are ${name}.`);
  return p;
}

/** Runs `body` against a fresh node of `program` (playing `role`), removed afterwards. */
async function withApp(program, body, role = "Server", link = "http") {
  const d = await deployment(program, link, role);
  try {
    await d.start(true);
    await body(d);
  } finally {
    await d.remove();
  }
}

test.describe("example apps on a node", () => {
  test.skip(!bin, "BLOSSOM_BIN names the blossom CLI (scripts/test-tiers.sh web sets it)");

  test("polls: live counts, one vote each, closed by the asker only, durable", async ({ context }) => {
    await withApp("polls", async (d) => {
      const a = await member(context, d, "Ada");
      const b = await member(context, d, "Bob");
      // The stylesheet the deployment names is the page's.
      await expect(a.locator("#app-css")).toHaveAttribute("href", "/blossom/style.css");
      await expect(b.locator(".here .person")).toHaveText(["Ada"]);
      await bid(a, "question").fill("Lunch?");
      await bid(a, "opt-0").fill("Tacos");
      await bid(a, "opt-1").fill("Ramen");
      await bid(a, "opt-2").fill("Salad");
      await bid(a, "ask").click();
      const poll = (p) => p.locator(".poll", { hasText: "Lunch?" });
      const counts = (p) => poll(p).locator(".choice .count");
      await expect(poll(b).locator(".choice .text")).toHaveText(["Tacos", "Ramen", "Salad"]);
      // A click on a choice's text (an element inside the button) is a click on the choice.
      await poll(b).locator(".choice .text", { hasText: "Ramen" }).click();
      await poll(a).locator(".choice .count").nth(1).click();
      await expect(counts(a)).toHaveText(["0", "2", "0"]);
      await expect(counts(b)).toHaveText(["0", "2", "0"]);
      await expect(poll(b).locator(".voters").nth(1)).toHaveText("Ada, Bob");
      // Changing a vote quickly, three times: the last one counts, everywhere.
      for (const c of ["Tacos", "Salad", "Tacos"]) await poll(b).locator(".choice", { hasText: c }).click();
      await expect(counts(a)).toHaveText(["1", "1", "0"]);
      await expect(counts(b)).toHaveText(["1", "1", "0"]);
      // Only the asker can close it; then votes are refused.
      await expect(poll(b).getByText("Close the poll")).toHaveCount(0);
      await poll(a).getByText("Close the poll").click();
      await expect(poll(b)).toHaveClass(/closed/);
      await poll(b).locator(".choice", { hasText: "Salad" }).click();
      await expect(counts(b)).toHaveText(["1", "1", "0"]);
      // A crash of the node loses nothing; a reload keeps the tab's name.
      await d.kill();
      await expect(bid(a, "status")).toHaveText(/offline/);
      await d.start(false);
      await expect(bid(a, "status")).toHaveText("live", { timeout: 15_000 });
      await b.reload();
      await expect(b.locator("body")).toHaveAttribute("data-blossom", "ready");
      await expect(bid(b, "who")).toContainText("You are Bob.");
      await expect(counts(b)).toHaveText(["1", "1", "0"]);
      await expect(poll(b)).toHaveClass(/closed/);
    });
  });

  test("tic-tac-toe: pairing, the referee, a spectator, a rematch", async ({ context }) => {
    await withApp("tictactoe", async (d) => {
      const a = await member(context, d, "Ada");
      const b = await member(context, d, "Bob");
      const c = await member(context, d, "Cy");
      await bid(a, "play").click();
      await expect(bid(a, "seeking")).toBeVisible();
      await bid(b, "play").click();
      // The one who waited longer plays X.
      await expect(bid(a, "headline")).toHaveText("Your move (X).");
      await expect(bid(b, "headline")).toHaveText("Waiting for Ada…");
      await expect(c.locator(".games .vs")).toHaveText(["Ada (X) vs Bob (O)"]);
      await c.locator(".games button").click();
      await expect(bid(c, "headline")).toHaveText("Ada to move (X).");
      const sq = (p, i) => p.locator(".grid button").nth(i);
      const grid = (p) => p.locator(".grid button");
      // Out of turn: nothing happens.
      await sq(b, 4).click();
      await sq(a, 0).click();
      await expect(grid(b)).toHaveText(["X", "", "", "", "", "", "", "", ""]);
      await sq(b, 4).click();
      await expect(bid(a, "headline")).toHaveText("Your move (X).");
      await sq(a, 1).click();
      await expect(bid(b, "headline")).toHaveText("Your move (O).");
      // A taken square: nothing happens.
      await sq(b, 0).click();
      await sq(b, 8).click();
      await expect(bid(a, "headline")).toHaveText("Your move (X).");
      await sq(a, 2).click();
      await expect(bid(a, "headline")).toHaveText("You win!");
      await expect(bid(b, "headline")).toHaveText("You lose.");
      await expect(bid(c, "headline")).toHaveText("Ada wins.");
      await expect(grid(c)).toHaveText(["X", "X", "X", "", "O", "", "", "", "O"]);
      await expect(c.locator(".grid .win")).toHaveCount(3);
      // A rematch: Bob asked first, so Bob plays X; Ada resigns.
      await bid(b, "again").click();
      await bid(a, "again").click();
      await expect(bid(b, "headline")).toHaveText("Your move (X).");
      await bid(a, "resign").click();
      await expect(bid(b, "headline")).toHaveText("You win!");
      await bid(a, "lobby").click();
      await expect(bid(a, "play")).toBeVisible();
    });
  });

  test("rooms: a game per room, each a keyed member on the host, kept across the host's restart", async ({
    context,
  }) => {
    await withApp(
      "rooms",
      async (d) => {
        const at = async (room, name) => {
          const p = await context.newPage();
          await open(p, `${d.url}?member=${room}`);
          await expect(bid(p, "title")).toHaveText(`Room ${room}`);
          await bid(p, "name").fill(name);
          await bid(p, "name").press("Enter");
          await expect(bid(p, "who")).toContainText(`You are ${name}.`);
          return p;
        };
        const a = await at("lunch", "Ada");
        const b = await at("lunch", "Bob");
        const c = await at("lunch", "Cy");
        // Another room is another game, with nobody in it.
        const e = await at("dinner", "Eve");
        await expect(c.locator(".here .person")).toHaveText(["Ada", "Bob"]);
        await expect(e.locator(".here .person")).toHaveCount(0);
        await bid(a, "sit").click();
        await expect(bid(b, "headline")).toHaveText("Ada plays X: sit down to play O.");
        await bid(b, "sit").click();
        await expect(bid(a, "headline")).toHaveText("Your move (X).");
        await expect(bid(c, "headline")).toHaveText("Ada to move (X).");
        await expect(bid(c, "sit")).toHaveCount(0);
        await expect(bid(e, "headline")).toHaveText("Nobody is playing yet: sit down to play X.");
        const sq = (p, i) => p.locator(".grid button").nth(i);
        const grid = (p) => p.locator(".grid button");
        // Out of turn: nothing happens.
        await sq(b, 4).click();
        await sq(a, 0).click();
        await expect(grid(b)).toHaveText(["X", "", "", "", "", "", "", "", ""]);
        await sq(b, 4).click();
        await expect(bid(a, "headline")).toHaveText("Your move (X).");
        await sq(a, 1).click();
        await expect(bid(b, "headline")).toHaveText("Your move (O).");
        await sq(b, 8).click();
        await expect(bid(a, "headline")).toHaveText("Your move (X).");
        await sq(a, 2).click();
        await expect(bid(a, "headline")).toHaveText("You win!");
        await expect(bid(b, "headline")).toHaveText("You lose.");
        await expect(bid(c, "headline")).toHaveText("Ada wins.");
        await expect(c.locator(".grid .win")).toHaveCount(3);
        // The dinner room plays on its own.
        await bid(e, "sit").click();
        await expect(bid(e, "headline")).toHaveText("Eve plays X: sit down to play O.");
        await expect(grid(e)).toHaveText(["", "", "", "", "", "", "", "", ""]);
        // The host crashes and comes back: each room from its own store; the tabs reconnect to theirs.
        await d.kill();
        await expect(bid(a, "status")).toHaveText("offline");
        await d.start(false);
        await expect(bid(a, "status")).toHaveText("live", { timeout: 30_000 });
        await expect(bid(e, "status")).toHaveText("live", { timeout: 30_000 });
        const late = await at("lunch", "Gus");
        await expect(bid(late, "headline")).toHaveText("Ada wins.");
        await expect(grid(late)).toHaveText(["X", "X", "X", "", "O", "", "", "", "O"]);
        const later = await at("dinner", "Hal");
        await expect(bid(later, "headline")).toHaveText("Eve plays X: sit down to play O.");
      },
      "Room",
      "websocket",
    );
  });

  test("board: cards, moves, drag and drop, who edits, offline edits", async ({ context }) => {
    await withApp("board", async (d) => {
      const a = await member(context, d, "Ada");
      const b = await member(context, d, "Bob");
      const add = async (p, c, t) => {
        await bid(p, `add-${c}`).fill(t);
        await bid(p, `add-${c}`).press("Enter");
        await expect(p.locator(`[data-bid="list-${c}"] .title`, { hasText: t })).toHaveCount(1);
      };
      const titles = (p, c) => p.locator(`[data-bid="list-${c}"] .title`);
      const card = (p, t) => p.locator(".card", { hasText: t });
      await add(a, 0, "One");
      await add(a, 0, "Two");
      await add(a, 0, "Three");
      await expect(titles(b, 0)).toHaveText(["One", "Two", "Three"]);
      // A card dropped on another goes before it; one dropped on a column goes last.
      await card(a, "Three").dragTo(card(a, "One"));
      await expect(titles(b, 0)).toHaveText(["Three", "One", "Two"]);
      await card(b, "One").dragTo(bid(b, "list-2"));
      await expect(titles(a, 0)).toHaveText(["Three", "Two"]);
      await expect(titles(a, 2)).toHaveText(["One"]);
      await card(a, "Two").getByTitle("Move right").click();
      await expect(titles(b, 1)).toHaveText(["Two"]);
      await expect(b.locator(".column h2 .count")).toHaveText(["1", "1", "1"]);
      // Renaming: the other tab sees who is at it.
      await card(b, "Three").locator(".title").dblclick();
      await expect(card(a, "Three").locator(".by")).toHaveText("✎ Bob");
      await b.locator(".card .edit").fill("Three, renamed");
      await b.locator(".card .edit").press("Enter");
      await expect(titles(a, 0)).toHaveText(["Three, renamed"]);
      await expect(a.locator(".card .by")).toHaveCount(0);
      await card(a, "Two").getByTitle("Delete").click();
      await expect(titles(b, 1)).toHaveText([]);
      // While the node is down, a tab keeps working; its changes go out when it is back.
      await d.kill();
      await expect(bid(a, "status")).toHaveText(/offline/);
      await add(a, 1, "Offline card");
      await d.start(false);
      await expect(bid(a, "status")).toHaveText("live", { timeout: 15_000 });
      await expect(titles(b, 1)).toHaveText(["Offline card"], { timeout: 15_000 });
    });
  });

  test("pixels: squares reach every tab, painters wait, the canvas survives", async ({ context }) => {
    await withApp("pixels", async (d) => {
      const a = await member(context, d, "Ada");
      const b = await member(context, d, "Bob");
      const px = (p, x, y) => bid(p, `px-${x}-${y}`);
      await bid(a, "swatch-2").click();
      await px(a, 3, 4).dispatchEvent("pointerdown");
      await expect(px(a, 3, 4)).toHaveAttribute("fill", "#e4572e");
      await expect(bid(a, "wait")).toHaveText(/^Wait/);
      await expect(px(b, 3, 4)).toHaveAttribute("fill", "#e4572e");
      // During the wait a paint does nothing.
      await px(a, 5, 5).dispatchEvent("pointerdown");
      await expect(px(a, 5, 5)).toHaveAttribute("fill", "#ffffff");
      // Bob paints over Ada's square.
      await bid(b, "swatch-6").click();
      await px(b, 3, 4).dispatchEvent("pointerdown");
      await expect(px(a, 3, 4)).toHaveAttribute("fill", "#00a5cf");
      await expect(bid(a, "wait")).toHaveText("Click a square to paint it", { timeout: 5_000 });
      await px(a, 5, 5).dispatchEvent("pointerdown");
      await expect(px(b, 5, 5)).toHaveAttribute("fill", "#e4572e");
      await expect(b.locator(".leaders li")).toHaveText(["Ada1", "Bob1"]);
      await d.kill();
      await d.start(false);
      const c = await member(context, d, "Cy");
      await expect(px(c, 3, 4)).toHaveAttribute("fill", "#00a5cf");
      await expect(px(c, 5, 5)).toHaveAttribute("fill", "#e4572e");
    });
  });
});
