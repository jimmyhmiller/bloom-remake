// A program with a physical timer in the browser (examples/web/stopwatch.bls): the page's clock fires it every
// animation frame it is due, while its guard holds; the inspector holds the clock. Playwright's clock drives time.
import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.clock.install();
  await page.goto("/index.html?app=stopwatch");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/index.html?app=stopwatch");
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
});

test("the stopwatch counts seconds while it runs, and keeps them across a reload", async ({ page }) => {
  const time = page.locator('[data-bid="time"]');
  await expect(time).toHaveText("0");
  await page.clock.runFor(3000);
  await expect(time).toHaveText("0");
  await page.locator('[data-bid="toggle"]').click();
  await expect(page.locator('[data-bid="toggle"]')).toHaveText("Stop");
  await page.clock.runFor(3100);
  await expect(time).toHaveText("3");
  await page.locator('[data-bid="toggle"]').click();
  await page.clock.runFor(5000);
  await expect(time).toHaveText("3");
  await page.reload();
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  await expect(time).toHaveText("3");
});

test("inspecting holds the clock; the late seconds count when it lets go", async ({ page }) => {
  const time = page.locator('[data-bid="time"]');
  await page.locator('[data-bid="toggle"]').click();
  await page.clock.runFor(2100);
  await expect(time).toHaveText("2");
  await page.locator("#blossom-inspect").click();
  await expect(page.locator(".blossom-paused")).toBeVisible();
  await page.clock.runFor(3000);
  await expect(time).toHaveText("2");
  await time.click();
  await expect(page.locator(".blossom-why-line").first()).toContainText("rule `page`");
  await page.keyboard.press("Escape");
  await page.clock.runFor(100);
  await expect(time).toHaveText("5");
});
