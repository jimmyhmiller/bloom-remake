// A Blossom deployment as Cloudflare Durable Objects (docs/design/DURABLE-OBJECTS.md, docs/design/KEYED.md §4). Each
// of its nodes is an object (`node/NAME`, crates/blossom-do over blossom-runtime's ObjectNode), and so is each member
// of a keyed role (`member/ROLE/KEY`), created by the first request for it; one more object, `registry`, mints the
// pages' tokens. An object's store is its storage, its pages' links are WebSockets it accepts for hibernation, its
// timers are its alarm, and its messages to other objects are requests to them.
//
// Every event runs to completion synchronously: the node's ticks, then the storage writes, then the frames and the
// requests to other objects. The storage commits the event's writes together, and the output gate holds what leaves
// until they are durable: a reply never leaves before the state it depends on (Invariant R).
//
// scripts/build-do.sh APP writes do/build/APP: the app (its sources, deployment and stylesheet), the node's
// WebAssembly, the page, and an entry module that makes the object class and the Worker from this file.
import { DurableObject } from "cloudflare:workers";

/** Encodes the storage's entries as blossom-do reads them: per entry 0, the key's length and bytes, the value's. */
function encodeEntries(kv) {
  const enc = new TextEncoder();
  const parts = [];
  let size = 0;
  for (const [key, value] of kv.list()) {
    if (key.startsWith("$")) continue;
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

/** Calls `f(item)` for each output item blossom-do reports: `{frame, conn}`, `{close, conn}`, or `{to, from, frame}`
 * (a message to another object). */
function eachOutput(bytes, f) {
  const dec = new TextDecoder();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  const chunk = () => {
    const len = view.getUint32(at, true);
    const b = bytes.slice(at + 4, at + 4 + len);
    at += 4 + len;
    return b;
  };
  while (at < bytes.length) {
    const kind = view.getUint8(at);
    at += 1;
    if (kind === 2) {
      const to = dec.decode(chunk());
      const from = dec.decode(chunk());
      f({ to, from, frame: chunk() });
      continue;
    }
    const conn = Number(view.getBigUint64(at, true));
    at += 8;
    if (kind === 0) f({ conn, frame: chunk() });
    else f({ conn, close: true });
  }
}

/** The deployment's seed (16 bytes), from the Worker's secret `BLOSSOM_SEED` (hex): every object shares it. */
function seedOf(env) {
  const hex = env.BLOSSOM_SEED ?? "";
  if (!/^[0-9a-f]{32}$/i.test(hex)) throw new Error("the Worker's BLOSSOM_SEED is not 32 hex digits");
  return new Uint8Array(hex.match(/../g).map((h) => parseInt(h, 16)));
}

/** The header naming the object a request is for (objects do not know their names otherwise). */
const OBJECT = "x-blossom-object";
/** The header naming the deployment node that sent a message to another object. */
const FROM = "x-blossom-from";

/** The object class and the Worker of the app `app` (`{files, deploy, node, style}`), over blossom-do's bindings. */
export function blossom(app, { DoNode, DoSite, mintToken }) {
  let site = null;
  /** The deployment itself (app.json, client parts), compiled once per isolate. */
  const siteOf = () => (site ??= new DoSite(JSON.stringify(app.files), app.deploy, app.node));

  class BlossomObject extends DurableObject {
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

    /** The object's name: given with its first request, kept in its storage. */
    nameOf(request) {
      const kv = this.ctx.storage.kv;
      const kept = kv.get("$name");
      const given = request?.headers.get(OBJECT);
      if (kept === undefined) {
        if (!given) throw new Error("a request that names no object");
        kv.put("$name", given);
        return given;
      }
      if (given && given !== kept) throw new Error(`object ${kept} was sent a request for ${given}`);
      return kept;
    }

    /** The node, started from the object's storage on the first event of this start. */
    start(request) {
      if (this.node) return this.node;
      const kv = this.ctx.storage.kv;
      const nonce = crypto.getRandomValues(new Uint32Array(1))[0];
      this.node = new DoNode(
        JSON.stringify(app.files),
        app.deploy,
        this.nameOf(request),
        seedOf(this.env),
        encodeEntries(kv),
        Date.now(),
        nonce,
      );
      this.flush();
      return this.node;
    }

    /** Runs `f` on the node, then commits its writes and sends what it sent. A failure is a node fault: nothing of
     * the call is written, the links close, and the node starts again from its storage on the next event. */
    run(f, request) {
      try {
        f(this.start(request));
      } catch (err) {
        console.error(`blossom: ${err}`);
        this.node = null;
        for (const ws of this.sockets.values()) ws.close(1011, "the node faulted");
        this.sockets.clear();
        return false;
      }
      this.flush();
      return true;
    }

    flush() {
      const kv = this.ctx.storage.kv;
      eachWrite(this.node.takeWrites(), (op, key, value) => {
        if (op === 0) kv.put(key, value);
        else kv.delete(key);
      });
      eachOutput(this.node.takeOutput(), (o) => {
        if (o.to !== undefined) {
          // Another object's message: a request to it, which the output gate holds until this event's writes are
          // durable. A lost one is a lost message, as on any link.
          const stub = this.env.OBJECTS.get(this.env.OBJECTS.idFromName(o.to));
          const headers = { [OBJECT]: o.to, [FROM]: o.from };
          stub
            .fetch("https://blossom/blossom/deliver", { method: "POST", headers, body: o.frame })
            .catch((err) => console.error(`blossom: a message to ${o.to}: ${err}`));
          return;
        }
        const ws = this.sockets.get(o.conn);
        if (!ws) return;
        if (o.close) {
          this.sockets.delete(o.conn);
          ws.close(1000, "the link ended");
        } else ws.send(o.frame);
      });
      const at = this.node.nextWake();
      if (at !== undefined && at !== null) this.ctx.storage.setAlarm(at);
      else this.ctx.storage.deleteAlarm();
    }

    async fetch(request) {
      const path = new URL(request.url).pathname;
      if (path === "/blossom/token") return this.token(request);
      if (path === "/blossom/deliver") {
        const frame = new Uint8Array(await request.arrayBuffer());
        const from = request.headers.get(FROM) || null;
        const ok = this.run((node) => node.rpc(from, frame, Date.now()), request);
        return new Response(null, { status: ok ? 204 : 503 });
      }
      if (path === "/blossom/link") {
        if (request.headers.get("Upgrade") !== "websocket") {
          return new Response("a WebSocket", { status: 426 });
        }
        const { 0: client, 1: server } = new WebSocketPair();
        this.ctx.acceptWebSocket(server);
        let conn = null;
        this.run((node) => (conn = node.connect()), request);
        if (conn === null) return new Response("the node faulted", { status: 503 });
        server.serializeAttachment({ conn, incarnation: this.incarnation });
        this.sockets.set(conn, server);
        return new Response(null, { status: 101, webSocket: client });
      }
      return new Response("not found", { status: 404 });
    }

    /** The registry's mint: the next serial, and a token for it signed with the deployment's seed. */
    token(request) {
      if (this.nameOf(request) !== "registry") return new Response("not the registry", { status: 404 });
      const role = new URL(request.url).searchParams.get("role");
      if (!role) return new Response("a token is for a client role (`?role=`)", { status: 400 });
      const kv = this.ctx.storage.kv;
      const serial = kv.get("$next") ?? 0;
      kv.put("$next", serial + 1);
      const token = mintToken(seedOf(this.env), role, serial);
      return new Response(token, { headers: { "content-type": "application/octet-stream" } });
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

  /** A request for the object named `name`. */
  const toObject = (env, name, request) => {
    const headers = new Headers(request.headers);
    headers.set(OBJECT, name);
    headers.delete(FROM);
    return env.OBJECTS.get(env.OBJECTS.idFromName(name)).fetch(new Request(request, { headers }));
  };

  const worker = {
    /** The page from the assets; the deployment's own requests from its site; a link to its node's object, or, with
     * `?member=KEY`, to that member's; a token from the registry. Messages between objects never come through here. */
    async fetch(request, env) {
      const url = new URL(request.url);
      const path = url.pathname;
      if (!path.startsWith("/blossom/")) return env.ASSETS.fetch(request);
      const s = siteOf();
      if (path === "/blossom/app.json") {
        const desc = JSON.parse(s.appJson());
        if (app.style) desc.style = "/blossom/style.css";
        return Response.json(desc);
      }
      if (path === "/blossom/style.css" && app.style) {
        return new Response(app.style, { headers: { "content-type": "text/css; charset=utf-8" } });
      }
      if (path.startsWith("/blossom/client/")) {
        const part = s.clientPart(decodeURIComponent(path.slice("/blossom/client/".length)));
        if (!part) return new Response("no such client role", { status: 404 });
        return new Response(part, { headers: { "content-type": "application/octet-stream" } });
      }
      if (path === "/blossom/token" && request.method === "POST") return toObject(env, "registry", request);
      if (path === "/blossom/link") {
        const role = s.keyedRole();
        const key = url.searchParams.get("member");
        if (role && !key) return new Response(`a link names the member of ${role} it goes to (?member=)`, { status: 400 });
        return toObject(env, role ? `member/${role}/${key}` : `node/${app.node}`, request);
      }
      return new Response("not found", { status: 404 });
    },
  };

  return { BlossomObject, worker };
}
