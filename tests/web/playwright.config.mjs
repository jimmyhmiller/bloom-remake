// Playwright tests of the browser host (docs/design/BROWSER.md). Build first: scripts/build-web.sh.
import { defineConfig } from "@playwright/test";

// The page's server: its own port (BLOSSOM_WEB_PORT, default 8714), so a server left running for a person does not
// stand in for the build under test.
const port = Number(process.env.BLOSSOM_WEB_PORT ?? 8714);

export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.mjs/,
  timeout: 60_000,
  workers: 2,
  use: { baseURL: `http://localhost:${port}`, browserName: "chromium", headless: true },
  webServer: {
    command: `PORT=${port} node ../../scripts/serve-web.mjs`,
    url: `http://localhost:${port}/index.html`,
    reuseExistingServer: false,
  },
});
