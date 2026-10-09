#!/usr/bin/env node
// A round-robin HTTP proxy in front of several `blossom serve` instances (docs/design/STATELESS.md §10): each request,
// and each WebSocket upgrade, goes to the next instance in turn, so a page's requests land on different instances
// one after another. An instance that does not answer gets the next one tried; when none does, the page gets a 502
// (and reconnects, as after any failed request). For tests (tests/web/stateless.spec.mjs) and for trying a deployment
// by hand:
//
//   node scripts/rr-proxy.mjs 8080 127.0.0.1:8081 127.0.0.1:8082 127.0.0.1:8083
import http from "node:http";
import net from "node:net";
import { fileURLToPath } from "node:url";

/** Starts a proxy on `port` (0: any free one) in front of `backends` (`host:port` each); resolves to
 * `{port, counts, close()}`. */
export function startProxy(port, backends) {
  let next = 0;
  /** Requests and upgrades sent to each backend, by `host:port`. */
  const counts = Object.fromEntries(backends.map((b) => [b, 0]));
  const pick = () => {
    const b = backends[next % backends.length];
    next += 1;
    counts[b] += 1;
    const [host, p] = b.split(":");
    return { host, port: Number(p) };
  };
  const forward = (req, res, tries) => {
    const b = pick();
    const up = http.request(
      { host: b.host, port: b.port, method: req.method, path: req.url, headers: req.headers, agent: false },
      (r) => {
        res.writeHead(r.statusCode ?? 502, r.headers);
        r.pipe(res);
      },
    );
    up.on("error", (e) => {
      // Refused: nothing reached the instance, so the next may take the request (its body was buffered). Any other
      // failure may have reached it, and the page decides (its link's requests can be sent again safely).
      if (!res.headersSent && tries > 1 && e.code === "ECONNREFUSED") forward(req, res, tries - 1);
      else if (!res.headersSent) {
        res.writeHead(502, { "content-type": "text/plain" });
        res.end("no instance answered");
      } else res.destroy();
    });
    up.end(req.bodyBytes);
  };
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      req.bodyBytes = Buffer.concat(chunks);
      forward(req, res, backends.length);
    });
  });
  server.on("upgrade", (req, socket, head) => {
    const b = pick();
    const up = net.connect(b.port, b.host, () => {
      let line = `${req.method} ${req.url} HTTP/1.1\r\n`;
      for (let i = 0; i < req.rawHeaders.length; i += 2) line += `${req.rawHeaders[i]}: ${req.rawHeaders[i + 1]}\r\n`;
      up.write(line + "\r\n");
      if (head.length) up.write(head);
      up.pipe(socket);
      socket.pipe(up);
    });
    const end = () => {
      up.destroy();
      socket.destroy();
    };
    up.on("error", end);
    socket.on("error", end);
    up.on("close", end);
    socket.on("close", end);
  });
  const sockets = new Set();
  server.on("connection", (s) => {
    sockets.add(s);
    s.on("close", () => sockets.delete(s));
  });
  return new Promise((ready) => {
    server.listen(port, "127.0.0.1", () =>
      ready({
        port: server.address().port,
        counts,
        close: () =>
          new Promise((done) => {
            for (const s of sockets) s.destroy();
            server.close(() => done());
          }),
      }),
    );
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [port, ...backends] = process.argv.slice(2);
  if (!port || backends.length === 0) {
    console.error("usage: node scripts/rr-proxy.mjs PORT HOST:PORT...");
    process.exit(2);
  }
  const p = await startProxy(Number(port), backends);
  console.log(`round-robin proxy on http://127.0.0.1:${p.port}/ in front of ${backends.join(", ")}`);
}
