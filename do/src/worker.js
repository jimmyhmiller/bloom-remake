// A Blossom app's server as a Cloudflare Durable Object (docs/design/DURABLE-OBJECTS.md). The object runs the
// deployment's server node (crates/blossom-do over blossom-runtime's ObjectNode): its store is the object's storage,
// its pages' links are WebSockets the object accepts for hibernation, and its timers are the object's alarm.
//
// Every event runs to completion synchronously: the node's ticks, then the storage writes, then the frames. The
// storage commits the event's writes together, and the output gate holds the frames until they are durable: a reply
// never leaves before the state it depends on (Invariant R).
import { DurableObject } from "cloudflare:workers";
import { DoNode, initSync } from "../build/pkg/blossom_do.js";
import wasm from "../build/pkg/blossom_do_bg.wasm";
import app from "../build/app.js";

initSync({ module: wasm });

/** The storage key of the deployment's seed (the node's own keys are KvFs's: `n/`, `d/`, `i/`, `c/`, `next`). */
const SEED = "$seed";

/** Encodes the storage's entries as blossom-do reads them: per entry 0, the key's length and bytes, the value's. */
function encodeEntries(kv) {
  const enc = new TextEncoder();
  const parts = [];
  let size = 0;
  for (const [key, value] of kv.list()) {
    if (key === SEED) continue;
    const k = enc.encode(key);
    const v = new Uint8Array(value);
    const head = new DataView(new ArrayBuffer(5));
    head.setUint8(0, 0);
    head.setUint32(1, k.length, true);
    const vlen = new DataView(new ArrayBuffer(4));
    vlen.setUint32(0, v.length, true);
    parts.push(new Uint8Array(head.buffer), k, new Uint8Array(vlen.buffer), v);
    size += 9 + k.length + v.length;
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** Calls `f(op, key, value)` for each storage write blossom-do reports (op 0 a value, 1 a deletion). */
function eachWrite(bytes, f) {
  const dec = new TextDecoder();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  while (at < bytes.length) {
    const op = view.getUint8(at);
    const klen = view.getUint32(at + 1, true);
    const key = dec.decode(bytes.subarray(at + 5, at + 5 + klen));
    at += 5 + klen;
    let value = null;
    if (op === 0) {
      const vlen = view.getUint32(at, true);
      value = bytes.slice(at + 4, at + 4 + vlen);
      at += 4 + vlen;
    }
    f(op, key, value);
  }
}

/** Calls `f(kind, conn, bytes)` for each output item (kind 0 a frame, 1 a close). */
function eachOutput(bytes, f) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  while (at < bytes.length) {
    const kind = view.getUint8(at);
    const conn = Number(view.getBigUint64(at + 1, true));
    at += 9;
    let frame = null;
    if (kind === 0) {
      const len = view.getUint32(at, true);
      frame = bytes.slice(at + 4, at + 4 + len);
      at += 4 + len;
    }
    f(kind, conn, frame);
  }
}

export class BlossomObject extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    /** This start of the object: sockets accepted by another (before a hibernation) carry another. */
    this.incarnation = crypto.randomUUID();
    this.node = null;
    /** The open sockets by their connection's id. */
    this.sockets = new Map();
    // A wake from hibernation starts the node again: the links on sockets of the last start are over; their pages
    // reconnect and resume.
    for (const ws of ctx.getWebSockets()) ws.close(1012, "the object restarted");
  }

  /** The node, started from the object's storage on the first event of this start. */
  start() {
    if (this.node) return this.node;
    const kv = this.ctx.storage.kv;
    let seed = kv.get(SEED);
    if (seed === undefined) {
      seed = crypto.getRandomValues(new Uint8Array(16));
      kv.put(SEED, seed);
    }
    const nonce = crypto.getRandomValues(new Uint32Array(1))[0];
    this.node = new DoNode(
      JSON.stringify(app.files),
      app.deploy,
      app.node,
      new Uint8Array(seed),
      encodeEntries(kv),
      Date.now(),
      nonce,
    );
    this.flush();
    return this.node;
  }

  /** Runs `f` on the node, then commits its writes and writes its frames. A failure is a node fault: nothing of
   * the call is written, the links close, and the node starts again from its storage on the next event. */
  run(f) {
    try {
      f(this.start());
    } catch (err) {
      console.error(`blossom: ${err}`);
      this.node = null;
      for (const ws of this.sockets.values()) ws.close(1011, "the node faulted");
      this.sockets.clear();
      return;
    }
    this.flush();
  }

  flush() {
    const kv = this.ctx.storage.kv;
    eachWrite(this.node.takeWrites(), (op, key, value) => {
      if (op === 0) kv.put(key, value);
      else kv.delete(key);
    });
    eachOutput(this.node.takeOutput(), (kind, conn, frame) => {
      const ws = this.sockets.get(conn);
      if (!ws) return;
      if (kind === 0) ws.send(frame);
      else {
        this.sockets.delete(conn);
        ws.close(1000, "the link ended");
      }
    });
    const at = this.node.nextWake();
    if (at !== undefined && at !== null) this.ctx.storage.setAlarm(at);
    else this.ctx.storage.deleteAlarm();
  }

  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    if (path === "/blossom/link") {
      if (request.headers.get("Upgrade") !== "websocket") {
        return new Response("a WebSocket", { status: 426 });
      }
      const { 0: client, 1: server } = new WebSocketPair();
      this.ctx.acceptWebSocket(server);
      let conn = null;
      this.run((node) => (conn = node.connect()));
      if (conn === null) return new Response("the node faulted", { status: 503 });
      server.serializeAttachment({ conn, incarnation: this.incarnation });
      this.sockets.set(conn, server);
      return new Response(null, { status: 101, webSocket: client });
    }
    const node = this.start();
    if (path === "/blossom/app.json") {
      const desc = JSON.parse(node.appJson());
      if (app.style) desc.style = "/blossom/style.css";
      return Response.json(desc);
    }
    if (path === "/blossom/style.css" && app.style) {
      return new Response(app.style, { headers: { "content-type": "text/css; charset=utf-8" } });
    }
    if (path.startsWith("/blossom/client/")) {
      const part = node.clientPart(decodeURIComponent(path.slice("/blossom/client/".length)));
      if (!part) return new Response("no such client role", { status: 404 });
      return new Response(part, { headers: { "content-type": "application/octet-stream" } });
    }
    return new Response("not found", { status: 404 });
  }

  /** The socket's connection, if it is this start's. */
  connOf(ws) {
    const a = ws.deserializeAttachment();
    return a && a.incarnation === this.incarnation ? a.conn : null;
  }

  webSocketMessage(ws, message) {
    const conn = this.connOf(ws);
    if (conn === null) return ws.close(1012, "the object restarted");
    if (typeof message === "string") return ws.close(1003, "a link carries binary frames");
    const entropy = crypto.getRandomValues(new Uint8Array(64));
    this.run((node) => node.frame(conn, new Uint8Array(message), Date.now(), entropy));
  }

  webSocketClose(ws) {
    const conn = this.connOf(ws);
    if (conn === null) return;
    this.sockets.delete(conn);
    this.run((node) => node.closed(conn, Date.now()));
  }

  webSocketError(ws) {
    this.webSocketClose(ws);
  }

  alarm() {
    this.run((node) => node.wake(Date.now()));
  }
}

export default {
  /** The page from the assets; everything under /blossom/ from the app's one object. */
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/blossom/")) {
      return env.OBJECTS.get(env.OBJECTS.idFromName("main")).fetch(request);
    }
    return env.ASSETS.fetch(request);
  },
};
