// The keyed chat deployed on Vercel (docs/design/DEPLOY.md §3, scripts/deploy-vercel.sh), at the URL
// BLOSSOM_VERCEL_URL names: tabs in two rooms of their own (named for the run, since the deployment keeps every room)
// chat, every tab keeps every line of its room exactly once and none of the other's, each room lists the other, and a
// tab that leaves leaves its room's count. Then the wake route refuses a caller without the secret. Skipped without
// BLOSSOM_VERCEL_URL; it is not part of any tier (it needs a deployment).
import { test, expect } from "@playwright/test";
import { randomBytes } from "node:crypto";

const base = process.env.BLOSSOM_VERCEL_URL;
const link = process.env.BLOSSOM_VERCEL_LINK ?? "http";

const bid = (page, id) => page.locator(`[data-bid="${id}"]`);
const lines = (page) => page.locator(".lines .text");

async function open(page, room) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto(`${base}/?member=${room}&link=${link}`);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready", { timeout: 30_000 });
}

async function say(page, text) {
  await bid(page, "say").fill(text);
  await bid(page, "say").press("Enter");
}

test.describe(`keyed chat on Vercel (${link} link)`, () => {
  test.skip(!base, "BLOSSOM_VERCEL_URL names the deployment");
  test.setTimeout(180_000);

  test("two rooms chat, apart, and a tab that leaves leaves", async ({ context }) => {
    const tag = randomBytes(4).toString("hex");
    const [lunch, dinner] = [`lunch${tag}`, `dinner${tag}`];
    const a = await context.newPage();
    const b = await context.newPage();
    const d = await context.newPage();
    await open(a, lunch);
    await open(b, lunch);
    await open(d, dinner);
    await expect(bid(a, "title")).toHaveText(`Room ${lunch}`);
    await expect(bid(d, "title")).toHaveText(`Room ${dinner}`);
    await expect(bid(a, "status")).toHaveText("online, 2 here", { timeout: 30_000 });
    await expect(bid(d, "status")).toHaveText("online, 1 here", { timeout: 30_000 });

    await say(a, "hello from a");
    await expect(lines(b)).toHaveText(["hello from a"]);
    await expect(bid(a, `go-${dinner}`)).toHaveText(dinner, { timeout: 30_000 });
    await expect(bid(d, `go-${lunch}`)).toHaveText(lunch, { timeout: 30_000 });

    await say(b, "hello from b");
    await expect(lines(a)).toHaveText(["hello from a", "hello from b"]);
    await say(d, "only in dinner");
    await say(a, "third");
    await expect(lines(a)).toHaveText(["hello from a", "hello from b", "third"]);
    await expect(lines(b)).toHaveText(["hello from a", "hello from b", "third"]);
    await expect(lines(d)).toHaveText(["only in dinner"]);

    // A tab opened later reads the room's history from the store.
    const e = await context.newPage();
    await open(e, lunch);
    await expect(lines(e)).toHaveText(["hello from a", "hello from b", "third"]);
    await e.close();

    await b.close();
    await expect(bid(a, "status")).toHaveText("online, 1 here", { timeout: 90_000 });
  });

  test("wake is only for the cron job", async ({ request }) => {
    expect((await request.get(`${base}/blossom/wake`)).status()).toBe(401);
    const wrong = await request.get(`${base}/blossom/wake`, { headers: { Authorization: "Bearer guessing-the-secret" } });
    expect(wrong.status()).toBe(401);
    const health = await request.get(`${base}/blossom/health`);
    expect(health.status()).toBe(200);
    expect(await health.text()).toBe("ok");
  });
});
