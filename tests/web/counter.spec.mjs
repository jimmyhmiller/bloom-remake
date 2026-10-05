// The counter of examples/web/counter.bls in the browser: compiled and run in WebAssembly, its page drawn and
// redrawn by the host, its count kept across a reload.
import { test, expect } from "@playwright/test";

test("the counter counts, and keeps its count across a reload", async ({ page }) => {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto("/index.html?app=counter");
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  const value = page.locator('[data-bid="value"]');
  await expect(value).toHaveText("0");
  await page.locator('[data-bid="plus"]').click();
  await page.locator('[data-bid="plus"]').click();
  await page.locator('[data-bid="plus"]').click();
  await page.locator('[data-bid="minus"]').click();
  await expect(value).toHaveText("2");
  await page.reload();
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  await expect(page.locator('[data-bid="value"]')).toHaveText("2");
  await expect(page.locator("#blossom-status")).toBeEmpty();
});
