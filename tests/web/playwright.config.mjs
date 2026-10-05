// Playwright tests of the browser host (docs/design/BROWSER.md). Build first: scripts/build-web.sh.
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.mjs/,
  timeout: 60_000,
  workers: 2,
  use: { baseURL: "http://localhost:8714", browserName: "chromium", headless: true },
  webServer: {
    command: "node ../../scripts/serve-web.mjs",
    url: "http://localhost:8714/index.html",
    reuseExistingServer: false,
  },
});
