// Flappy Bird in Blossom (examples/web/flappy.bls) in the browser: SVG drawn from relations, a physical timer as
// the game's clock (Playwright drives it), presses as input, the best score kept, and the inspector on the bird.
import { test, expect } from "@playwright/test";

const at = (page, id) => page.locator(`[data-bid="${id}"]`);
const birdY = async (page) => parseFloat((await at(page, "bird").getAttribute("transform")).split(" ")[1]);

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.clock.install();
  await page.goto("/index.html?app=flappy");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/index.html?app=flappy");
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
});

test("a press starts the game; the bird falls to the ground; the best score is kept", async ({ page }) => {
  await expect(at(page, "message")).toHaveText("Click to begin!");
  await expect(at(page, "game")).toHaveJSProperty("namespaceURI", "http://www.w3.org/2000/svg");
  await at(page, "sky").dispatchEvent("pointerdown");
  await expect(at(page, "score")).toHaveText("0");
  const y0 = await birdY(page);
  await page.clock.runFor(300);
  expect(await birdY(page)).toBeGreaterThan(y0);
  // A flap: up it goes.
  const y1 = await birdY(page);
  await at(page, "bird").dispatchEvent("pointerdown");
  await page.clock.runFor(150);
  expect(await birdY(page)).toBeLessThan(y1);
  await page.clock.runFor(3000);
  await expect(at(page, "over")).toHaveText("Game Over");
  await expect(at(page, "best")).toHaveText(/^Best \d+$/);
  const best = await at(page, "best").textContent();
  await page.reload();
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  await at(page, "sky").dispatchEvent("pointerdown");
  await page.clock.runFor(3000);
  await expect(at(page, "best")).toHaveText(best);
});

test("the space bar plays too", async ({ page }) => {
  await at(page, "game").focus();
  await page.keyboard.press(" ");
  await expect(at(page, "score")).toHaveText("0");
});

test("the inspector holds the game and explains the bird", async ({ page }) => {
  await at(page, "sky").dispatchEvent("pointerdown");
  await page.clock.runFor(200);
  await page.locator("#blossom-inspect").click();
  const y = await birdY(page);
  await page.clock.runFor(1000);
  expect(await birdY(page)).toBe(y);
  // The wing's row comes from rule `page`, which reads the bird's row that `step` wrote.
  await at(page, "bird-wing").click();
  await expect(page.locator("#blossom-why .blossom-subject")).toHaveText("bird-wing");
  const line = page.locator(".blossom-why-line", { has: page.locator(".blossom-fact", { hasText: "bird(" }) });
  await expect(line.first()).toContainText("by rule `step`");
});
