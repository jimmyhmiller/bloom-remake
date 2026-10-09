// Real nodes for the Playwright tests of pages served by a node (docs/design/CLIENTS.md): a one-node deployment of an
// example program in a temporary directory, run with `blossom run --web`. Needs the CLI: BLOSSOM_BIN.
import { spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";

export const repo = resolve(fileURLToPath(new URL(".", import.meta.url)), "..", "..");
export const bin = process.env.BLOSSOM_BIN;

/** A free TCP port on localhost. */
export function freePort() {
  return new Promise((done, fail) => {
    const s = createServer();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => done(port));
    });
  });
}

/** A one-node deployment of examples/web/PROGRAM.bls (a copy of it, which `edit` rewrites), serving the page with
 * the program's stylesheet when it has one (examples/web/PROGRAM.css); `start`/`kill` run and stop the node. Its node
 * `s` plays `role` (a keyed role's node is a host of its members, docs/design/KEYED.md). */
export async function deployment(program, link, role = "Server") {
  const dir = mkdtempSync(join(tmpdir(), `blossom-clients-${program}-`));
  for (const f of [`${program}.bls`, "ui.bls", "events.bls"]) copyFileSync(join(repo, "examples", "web", f), join(dir, f));
  const css = join(repo, "examples", "web", `${program}.css`);
  const style = existsSync(css);
  if (style) copyFileSync(css, join(dir, `${program}.css`));
  const peer = await freePort();
  const web = await freePort();
  const deploy = join(dir, "deploy.toml");
  writeFileSync(
    deploy,
    [
      "format = 1",
      "[deployment]",
      `id = "${program}-web"`,
      `program = "${program}"`,
      "version = 1",
      `source = "${program}.bls"`,
      "[[node]]",
      'name = "s"',
      `role = "${role}"`,
      `addr = "127.0.0.1:${peer}"`,
      `principal = "spiffe://test/${program}/${role}/s"`,
      "[security]",
      'mode = "insecure-dev"',
      "[storage]",
      'data_dir = "data"',
      "[web]",
      `link = "${link}"`,
      ...(style ? [`style = "${program}.css"`] : []),
      "",
    ].join("\n"),
  );
  let child = null;
  const d = {
    url: `http://localhost:${web}/`,
    /** Starts the node (a new one the first time) and waits for its readiness line. */
    async start(fresh) {
      const args = ["run", "--deploy", deploy, "--node", "s", "--insecure-dev", "--web", `127.0.0.1:${web}`];
      args.push("--web-root", join(repo, "web"));
      if (fresh) args.push("--init-fresh");
      child = spawn(bin, args, {
        env: { ...process.env, BLOSSOM_SEED: "00112233445566778899aabbccddeeff" },
        stdio: ["ignore", "pipe", "pipe"],
      });
      let err = "";
      child.stderr.on("data", (b) => (err += b));
      await new Promise((ready, fail) => {
        let out = "";
        child.stdout.on("data", (b) => {
          out += b;
          if (out.includes(" ready: ")) ready();
        });
        child.on("exit", (code) => fail(new Error(`blossom run exited (${code}): ${err}`)));
      });
    },
    /** Kills the node outright (no shutdown: a crash). */
    async kill() {
      if (!child) return;
      const c = child;
      child = null;
      await new Promise((gone) => {
        c.on("exit", gone);
        c.kill("SIGKILL");
      });
    },
    /** Rewrites the program's source (the node runs it from its next start). */
    edit(change) {
      const file = join(dir, `${program}.bls`);
      writeFileSync(file, change(readFileSync(file, "utf8")));
    },
    async remove() {
      await d.kill();
      rmSync(dir, { recursive: true, force: true });
    },
  };
  return d;
}
