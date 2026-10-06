// Runs Speedometer 3's TodoMVC suites, the Blossom suites among them (scripts/bench-todomvc.sh prepares the tree), in
// headless Chromium and prints each suite's mean time per iteration and per step.
//
//   node bench/todomvc.mjs SPEEDOMETER_DIR [--iterations N] [--suites A,B,…] [--json FILE]
import { createServer } from "node:http";
import { readFile, writeFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { chromium } from "playwright";

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".bls": "text/plain; charset=utf-8",
};

/** Speedometer's TodoMVC suites without the complex-DOM variants, then the Blossom ones. */
const DEFAULT_SUITES = [
  "TodoMVC-JavaScript-ES5",
  "TodoMVC-JavaScript-ES6-Webpack",
  "TodoMVC-WebComponents",
  "TodoMVC-React",
  "TodoMVC-React-Redux",
  "TodoMVC-Backbone",
  "TodoMVC-Angular",
  "TodoMVC-Vue",
  "TodoMVC-jQuery",
  "TodoMVC-Preact",
  "TodoMVC-Svelte",
  "TodoMVC-Lit",
  "TodoMVC-Blossom",
  "TodoMVC-Blossom-Persist",
];

function args() {
  const argv = process.argv.slice(2);
  const out = { root: null, iterations: 10, suites: DEFAULT_SUITES, json: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--iterations") out.iterations = Number(argv[++i]);
    else if (a === "--suites") out.suites = argv[++i].split(",");
    else if (a === "--json") out.json = argv[++i];
    else if (out.root === null) out.root = a;
    else throw new Error(`unexpected argument ${a}`);
  }
  if (out.root === null || !(out.iterations >= 1)) {
    throw new Error("usage: node bench/todomvc.mjs SPEEDOMETER_DIR [--iterations N] [--suites A,B] [--json FILE]");
  }
  return out;
}

function serve(root) {
  const server = createServer(async (req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^(\.\.[/\\])+/, "");
    const file = join(root, path.endsWith("/") ? path + "index.html" : path);
    try {
      const body = await readFile(file);
      res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404);
      res.end("not found");
    }
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

const { root, iterations, suites, json } = args();
const server = await serve(root);
const port = server.address().port;
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  page.on("pageerror", (e) => console.error(`page error: ${e.message}`));
  const url = `http://127.0.0.1:${port}/index.html?suites=${suites.join(",")}&iterationCount=${iterations}`;
  await page.goto(url);
  await page.waitForFunction(() => globalThis.benchmarkClient !== undefined, null, { timeout: 60_000 });
  const metrics = await page.evaluate(
    () =>
      new Promise((resolve, reject) => {
        globalThis.addEventListener(
          "SpeedometerDone",
          () => resolve(JSON.parse(JSON.stringify(globalThis.benchmarkClient.metrics))),
          { once: true },
        );
        globalThis.addEventListener("error", (e) => reject(new Error(String(e.message))));
        globalThis.addEventListener("unhandledrejection", (e) => reject(new Error(String(e.reason))));
        globalThis.benchmarkClient.start();
      }),
  );
  const version = browser.version();
  if (json) await writeFile(json, JSON.stringify({ chromium: version, iterations, metrics }, null, 2));
  const ms = (name) => metrics[name]?.mean;
  const fmt = (x) => (x === undefined ? "—" : x.toFixed(1).padStart(8));
  const rows = suites.map((s) => ({ s, total: ms(s) })).sort((a, b) => a.total - b.total);
  const steps = ["Adding100Items", "CompletingAllItems", "DeletingAllItems"];
  console.log(`Speedometer 3 TodoMVC, headless Chromium ${version}, ${iterations} iterations: mean ms per iteration`);
  console.log(`${"suite".padEnd(32)}${"total".padStart(8)}${steps.map((x) => x.padStart(20)).join("")}`);
  for (const { s, total } of rows) {
    console.log(`${s.padEnd(32)}${fmt(total)}${steps.map((x) => fmt(ms(`${s}/${x}`)).padStart(20)).join("")}`);
  }
} finally {
  await browser.close();
  server.close();
}
