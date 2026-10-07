// The browser host (docs/design/BROWSER.md): loads a Blossom program's source, compiles and runs it in WebAssembly,
// applies each round's page patches to the DOM, reports the DOM events the program reads, and keeps its durable
// tables in localStorage. `?app=NAME` picks examples/web/NAME.bls (default: todomvc).
//
// `?bench` runs the app alone for a benchmark (scripts/bench-todomvc.sh): from an empty state and the app's own
// source, without the inspector and editor bar, saving nothing; `?bench=persist` saves the durable tables as usual.
//
// A program with physical timers runs on the page's clock: every animation frame moves it, and the timers due by then
// fire (LANGUAGE §15.2).
//
// Beside the app: the inspector (click an element, see why it is there; it holds the clock while it is on) and the
// editor (the app's source, edited and re-run in place, the durable state kept when its schema stays).
//
// Served by a node (`blossom run --web`, docs/design/CLIENTS.md §5), the page is a member of the program's client
// role: it compiles the program the node's `/blossom/app.json` gives, connects to the node over a WebSocket, and
// runs as the member the node admits. It keeps its link (identity, what it took, what it sent and the node has not
// acknowledged) in localStorage beside its durable tables, so a reload is the same member and a line typed while the
// node is unreachable goes out when it is back; it reconnects with backoff. Tabs of one browser share localStorage
// but are different members: each tab holds a numbered slot (a Web Lock, released when the tab goes) and keeps its
// state under it, so a reload takes its slot back and a second tab takes the next. The program is the node's: the
// editor is off.
// The wasm module: with the compiler for a page on its own (pkg/), the engine only for a member page (pkg-member/).
let wasm = null;

const search = new URLSearchParams(location.search);
let appName = search.get("app") ?? "todomvc";
/** Benchmark mode: `null` (off), `""` (saves nothing) or `"persist"`. */
const bench = search.get("bench");
const saving = bench === null || bench === "persist";
let root = `${appName}.bls`;
const mount = document.getElementById("app");
const statusLine = document.getElementById("blossom-status");
let storageKey = `blossom:${appName}`;
const sourceKey = `blossom-source:${appName}`;
/** The stylesheet of each example app that has one. */
const STYLES = { todomvc: "todomvc.css", todos_shared: "todomvc.css", chat: "chat.css" };

/** The elements the program made, by its ids. */
let nodes = new Map();
const SVG = "http://www.w3.org/2000/svg";
/** Attributes the DOM keeps as live state: set as properties. */
const PROPS = new Set(["value", "checked", "disabled"]);

function element(id) {
  return id === "" ? mount : nodes.get(id);
}

/** An element's text node, before its children. */
function textOf(node) {
  if (!node.blossomText) {
    node.blossomText = document.createTextNode("");
    node.insertBefore(node.blossomText, node.firstChild);
  }
  return node.blossomText;
}

function setProp(node, name, value) {
  node[name] = name === "value" ? value : value !== "false" && value !== "";
}

function apply(patches) {
  let focus = null;
  for (const p of patches) {
    switch (p.op) {
      case "create": {
        const n = p.svg ? document.createElementNS(SVG, p.tag) : document.createElement(p.tag);
        n.dataset.bid = p.id;
        nodes.set(p.id, n);
        break;
      }
      case "remove": {
        const n = nodes.get(p.id);
        if (n) {
          nodes.delete(p.id);
          n.remove();
        }
        break;
      }
      case "attr": {
        const n = nodes.get(p.id);
        if (PROPS.has(p.name)) setProp(n, p.name, p.value);
        else n.setAttribute(p.name, p.value);
        break;
      }
      case "unattr": {
        const n = nodes.get(p.id);
        if (PROPS.has(p.name)) setProp(n, p.name, "");
        else n.removeAttribute(p.name);
        break;
      }
      case "text":
        textOf(nodes.get(p.id)).data = p.text;
        break;
      case "place":
        // Last sibling first: `before` is already in place (null: the element goes last).
        element(p.parent).insertBefore(nodes.get(p.id), p.before === null ? null : nodes.get(p.before));
        break;
      case "focus":
        focus = nodes.get(p.id);
        break;
      default:
        throw new Error(`unknown patch ${JSON.stringify(p)}`);
    }
  }
  if (focus && !keepFocus) {
    focus.focus();
    if (typeof focus.value === "string" && focus.setSelectionRange) {
      const end = focus.value.length;
      focus.setSelectionRange(end, end);
    }
  }
}

function report(message) {
  statusLine.textContent = message;
}

/** A fresh root for the program's randomness: 32 hex digits. */
function seed() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** The page's clock, in milliseconds since the epoch. */
function now() {
  return performance.timeOrigin + performance.now();
}

// The durable tables in localStorage: one entry per row, `blossom:NAME:row:TABLE:ROW` (the row as JSON), and the
// tables' schema hashes under `blossom:NAME:tables`, so a round writes only the rows it changed. (Before, the whole
// state was one entry, `blossom:NAME`: it is read once and rewritten in this form.)
let rowPrefix = `${storageKey}:row:`;
let tablesKey = `${storageKey}:tables`;

/** The saved durable tables, as the JSON `start` takes (empty: none). */
function loadSaved() {
  const tables = localStorage.getItem(tablesKey);
  if (tables === null) return localStorage.getItem(storageKey) ?? "";
  const rows = new Map(Object.keys(JSON.parse(tables)).map((name) => [name, []]));
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key.startsWith(rowPrefix)) continue;
    // A table's name may hold `::` (a module's); a row's JSON starts with `[`.
    const rest = key.slice(rowPrefix.length);
    const cut = rest.indexOf(":[");
    const name = rest.slice(0, cut);
    if (cut >= 0 && rows.has(name)) rows.get(name).push(JSON.parse(rest.slice(cut + 1)));
  }
  const schemas = JSON.parse(tables);
  return JSON.stringify({ tables: [...rows].map(([name, r]) => ({ name, schema: schemas[name], rows: r })) });
}

/** Writes what the program's durable tables gained and lost since the last save (everything, the first time). */
function writeSaved(c) {
  if (c.full) {
    const old = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key.startsWith(rowPrefix)) old.push(key);
    }
    for (const key of old) localStorage.removeItem(key);
    localStorage.removeItem(storageKey);
    localStorage.setItem(tablesKey, JSON.stringify(c.tables));
  }
  for (const [table, row] of c.delete) localStorage.removeItem(`${rowPrefix}${table}:${row}`);
  for (const [table, row] of c.put) localStorage.setItem(`${rowPrefix}${table}:${row}`, "");
}

/** Saves the durable tables: at once, or (while the clock runs, which may change them every frame) soon after. */
let saveTimer = null;
function persist(soon) {
  if (soon) {
    if (saveTimer === null) saveTimer = setTimeout(() => persist(false), 500);
    return;
  }
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = null;
  if (!app || !saving) return;
  // The link first: a line the rows say was sent is then in the link's queue too.
  if (member) localStorage.setItem(member.linkKey, app.linkState());
  writeSaved(JSON.parse(app.saveChanges()));
}
addEventListener("pagehide", () => persist(false));

let app = null;
/** The inputs the running program reads. */
let listening = new Set();
let busy = false;
/** While set, a focus patch does not move the focus (the editor keeps it across a run). */
let keepFocus = false;
const queue = [];

/** Runs events one at a time: an event fired while patches apply (a removed field's blur) waits for its turn. */
function send(event) {
  queue.push(event);
  drain();
}

/** Runs the queued events, unless a round is running (it drains them when it ends). */
function drain() {
  if (busy) return;
  busy = true;
  try {
    while (queue.length > 0) {
      const next = queue.shift();
      try {
        apply(JSON.parse(app.dispatch(JSON.stringify(next), now())));
        flushFrames();
        persist(false);
        inspector.refresh();
      } catch (err) {
        report(`error: ${err}`);
      }
    }
  } finally {
    busy = false;
  }
}

/** The DOM event each input listens to, and the event it reports. */
const LISTENERS = {
  click: ["click", (e, id) => ({ kind: "click", id })],
  dblclick: ["dblclick", (e, id) => ({ kind: "dblclick", id })],
  press: ["pointerdown", (e, id) => ({ kind: "press", id })],
  typed: ["input", (e, id) => ({ kind: "input", id, value: e.target.value ?? "" })],
  keydown: ["keydown", (e, id) => ({ kind: "keydown", id, key: e.key, value: e.target.value ?? "" })],
  blur: ["focusout", (e, id) => ({ kind: "blur", id, value: e.target.value ?? "" })],
  change: ["change", (e, id) => ({ kind: "change", id, checked: Boolean(e.target.checked) })],
};

/** Every DOM event the host knows, reported while the running program reads its input. */
function listen() {
  addEventListener("hashchange", () => {
    if (app && listening.has("route")) send({ kind: "route", hash: location.hash });
  });
  for (const [name, [dom, make]] of Object.entries(LISTENERS)) {
    mount.addEventListener(dom, (e) => {
      if (!app || !listening.has(name) || inspector.on) return;
      const target = e.target.closest?.("[data-bid]");
      if (target) send(make(e, target.dataset.bid));
    });
  }
}

/** The clock: every animation frame, the timers due by now fire (while the program has timers, the inspector is
 * off, and no event is running). A failed round stops it until the program is run again. */
let clockFailed = false;
function frame() {
  requestAnimationFrame(frame);
  if (!app || clockFailed || inspector.on || busy || !app.clocked()) return;
  busy = true;
  try {
    const patches = JSON.parse(app.advance(now()));
    flushFrames();
    if (patches.length > 0) {
      apply(patches);
      persist(true);
      inspector.soon();
    }
  } catch (err) {
    clockFailed = true;
    report(`error: ${err}`);
  } finally {
    busy = false;
  }
  drain();
}

async function fetchSource(file) {
  const res = await fetch(`pkg/apps/${file}`);
  if (!res.ok) throw new Error(`cannot load ${file}: ${res.status}`);
  return res.text();
}

/** The diagnostics a failed compile throws (JSON), or the error itself when it is something else. */
function diagnostics(err) {
  try {
    return { diags: JSON.parse(String(err)), ok: false };
  } catch {
    return { diags: [], ok: false, error: String(err) };
  }
}

/** Compiles `files` and, if it compiles and starts, runs it in place of the running program, from its saved state
 * (each durable table whose schema stays is kept). */
function run(files) {
  let next;
  try {
    next = wasm.compile(root, JSON.stringify(files), seed());
  } catch (err) {
    return diagnostics(err);
  }
  const warnings = JSON.parse(next.warnings());
  const saved = app ? app.saved() : bench !== null ? "" : loadSaved();
  let started;
  try {
    started = JSON.parse(next.start(saved, location.hash, now()));
  } catch (err) {
    next.free();
    return { diags: warnings, ok: false, error: String(err) };
  }
  if (app) app.free();
  app = next;
  listening = new Set(JSON.parse(app.listens()));
  nodes = new Map();
  mount.replaceChildren();
  clockFailed = false;
  apply(started.patches);
  persist(false);
  report(started.notes.join("\n"));
  inspector.refresh();
  return { diags: warnings, ok: true, notes: started.notes };
}

// ---------------------------------------------------------------- a member of a node's program

/** The member page's state (`null` for a page on its own): the program, its link before the page runs, the frames
 * that link's handshake left, where it stores its link, and the connection. */
let member = null;

/** Writes the frames the last rounds left for the server (none while the link is down: they wait in the link). */
function flushFrames() {
  if (!member) return;
  for (let f = app.takeFrame(); f !== undefined; f = app.takeFrame()) member.conn.write(f);
}

/** The WebSocket to the node: opened again after a loss, waiting longer each time (up to 5 s). */
class Connection {
  constructor(url, onOpen, onFrame, onClose) {
    Object.assign(this, { url, onOpen, onFrame, onClose, ws: null, failures: 0, timer: null });
  }

  open() {
    this.timer = null;
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => this.onOpen();
    ws.onmessage = (e) => {
      if (this.ws === ws) this.onFrame(new Uint8Array(e.data));
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.onClose();
      this.retry();
    };
  }

  /** The handshake finished: the next loss starts the backoff over. */
  welcomed() {
    this.failures = 0;
  }

  retry() {
    if (this.timer !== null) return;
    const wait = Math.min(5000, 200 * 2 ** this.failures) * (0.75 + Math.random() / 2);
    this.failures = Math.min(this.failures + 1, 10);
    this.timer = setTimeout(() => this.open(), wait);
  }

  write(bytes) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) this.ws.send(bytes);
  }

  /** Drops the connection (the link's loss follows, as for any loss). */
  drop() {
    if (this.ws) this.ws.close();
  }
}

/** The server refused the page. A page built from another version of the program (or for another deployment or node
 * directory) loads again, which gets the current one, unless it just did; any other refusal is reported, and the
 * connection tries again later. */
function refused(r) {
  member.conn.drop();
  const stale = ["program", "deployment", "directory"].includes(r.reason);
  const key = `${storageKey}:reloaded`;
  let last = 0;
  try {
    last = Number(sessionStorage.getItem(key) ?? 0);
  } catch {}
  if (stale && Date.now() - last > 30_000) {
    try {
      sessionStorage.setItem(key, String(Date.now()));
    } catch {}
    report("the server runs a newer version of this page: loading it");
    location.reload();
    return;
  }
  report(`the server refused this page (${r.reason}): ${r.detail}`);
}

/** A frame from the node: the handshake's, until the page runs; then the app's. */
function memberFrame(bytes) {
  if (!app) {
    let heard;
    try {
      heard = JSON.parse(member.link.recv(bytes));
    } catch (err) {
      report(`error: ${err}`);
      member.conn.drop();
      return;
    }
    if (heard.refused) {
      refused(heard.refused);
      return;
    }
    if (heard.welcome) {
      member.conn.welcomed();
      startMember(wasm.WebApp.member(member.client, member.link));
    }
    return;
  }
  busy = true;
  try {
    const heard = JSON.parse(app.linkRecv(bytes, now()));
    if (heard.refused) {
      refused(heard.refused);
      return;
    }
    if (heard.restart) {
      restartMember("the server no longer knows this page: it starts over as a new member");
      return;
    }
    apply(heard.patches);
    flushFrames();
    persist(false);
    inspector.soon();
  } catch (err) {
    report(`error: ${err}`);
  } finally {
    busy = false;
  }
  drain();
}

/** The connection ended: the program hears its link is down. */
function memberClosed() {
  if (!app) {
    member.link.down();
    return;
  }
  busy = true;
  try {
    apply(JSON.parse(app.linkDown(now())));
    persist(false);
  } catch (err) {
    report(`error: ${err}`);
  } finally {
    busy = false;
  }
  drain();
}

/** The connection opened: the link's first frame. */
function memberOpened() {
  member.conn.write(app ? app.linkHello() : member.link.hello());
}

/** Runs the member page (its identity known): from its saved tables, the link's leftover frames written first. */
function startMember(next) {
  member.client = null;
  member.link = null;
  app = next;
  listening = new Set(JSON.parse(app.listens()));
  nodes = new Map();
  mount.replaceChildren();
  let started;
  try {
    started = JSON.parse(app.start(loadSaved(), location.hash, now()));
  } catch (err) {
    report(`error: ${err}`);
    return;
  }
  apply(started.patches);
  flushFrames();
  persist(false);
  report(started.notes.join("\n"));
  inspector.refresh();
}

/** Forgets the member's stored state and loads the page again. */
function restartMember(why) {
  report(why);
  const old = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key.startsWith(`${storageKey}:`) || key === storageKey) old.push(key);
  }
  for (const key of old) localStorage.removeItem(key);
  location.reload();
}

/** The first free slot of `base` (a Web Lock this page holds until it goes): its number. */
async function claimSlot(base) {
  if (!navigator.locks) {
    throw new Error("a member page needs the Web Locks API, which browsers give pages over https or on localhost");
  }
  for (let i = 0; ; i++) {
    const got = await new Promise((resolve) => {
      navigator.locks.request(`${base}:slot:${i}`, { ifAvailable: true }, (lock) => {
        resolve(lock !== null);
        // Held for the page's life.
        return lock === null ? undefined : new Promise(() => {});
      });
    });
    if (got) return i;
  }
}

/** The page as a member of the node's program, from the node's app.json: loads its role's part of the program (no
 * source, no compiler), and runs it at once when the page knows its identity from an earlier visit (its link connects
 * meanwhile), or after the first handshake. */
async function runMember(appText) {
  const desc = JSON.parse(appText);
  appName = desc.program;
  const roles = Object.keys(desc.clients);
  if (roles.length !== 1) {
    report(`the program has ${roles.length} client roles; a page plays one, and this host does not choose`);
    return;
  }
  const role = roles[0];
  const res = await fetch(new URL(desc.clients[role], location.href));
  if (!res.ok) {
    report(`cannot load ${role}'s part of the program: ${res.status}`);
    return;
  }
  const base = `blossom-member:${desc.deployment}:${desc.node}:${role}`;
  storageKey = `${base}:${await claimSlot(base)}`;
  rowPrefix = `${storageKey}:row:`;
  tablesKey = `${storageKey}:tables`;
  let client;
  try {
    client = wasm.loadClient(new Uint8Array(await res.arrayBuffer()), appText);
  } catch (err) {
    report(`error: ${err}`);
    return;
  }
  const linkKey = `${storageKey}:link`;
  const link = client.link(localStorage.getItem(linkKey) ?? "");
  const url = new URL(desc.link, location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  member = { client, link, linkKey, conn: new Connection(url, memberOpened, memberFrame, memberClosed) };
  if (link.hasMember()) startMember(wasm.WebApp.member(client, link));
  else report("connecting to the server…");
  member.conn.open();
}

/** The node's app.json when a node serves the page (`null` when the page is on its own). */
async function served() {
  if (search.has("app")) return null;
  const res = await fetch("blossom/app.json");
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`cannot load blossom/app.json: ${res.status}`);
  return res.text();
}

// ---------------------------------------------------------------- the panel

const panel = document.getElementById("blossom-panel");
const tabs = { why: document.getElementById("blossom-why"), source: document.getElementById("blossom-source") };
const buttons = {
  inspect: document.getElementById("blossom-inspect"),
  source: document.getElementById("blossom-edit"),
};

/** Shows the panel's `tab`, or hides the panel (`null`). */
function show(tab) {
  for (const [name, el] of Object.entries(tabs)) el.hidden = name !== tab;
  panel.hidden = tab === null;
  document.documentElement.classList.toggle("blossom-panel-open", tab !== null);
  buttons.source.setAttribute("aria-pressed", String(tab === "source"));
}

// ---------------------------------------------------------------- the inspector

const highlight = document.getElementById("blossom-highlight");

const inspector = {
  on: false,
  /** The element explained, by id. */
  id: null,

  toggle(on) {
    this.on = on;
    buttons.inspect.setAttribute("aria-pressed", String(on));
    document.documentElement.classList.toggle("blossom-inspecting", on);
    tabs.why.querySelector(".blossom-paused").hidden = !(on && app && app.clocked());
    if (on) show("why");
    else this.mark(null);
  },

  /** Outlines `node` (or nothing). */
  mark(node) {
    if (!node) {
      highlight.hidden = true;
      return;
    }
    const r = node.getBoundingClientRect();
    Object.assign(highlight.style, {
      left: `${r.left}px`,
      top: `${r.top}px`,
      width: `${r.width}px`,
      height: `${r.height}px`,
    });
    highlight.dataset.id = node.dataset.bid;
    highlight.hidden = false;
  },

  explain(id) {
    this.id = id;
    this.refresh();
  },

  /** Refreshes soon (the clock may change the page every frame; an explanation re-runs rounds). */
  pending: null,
  soon() {
    if (this.id === null || this.pending !== null) return;
    this.pending = setTimeout(() => {
      this.pending = null;
      this.refresh();
    }, 400);
  },

  /** Shows why the element is on the page, as of the last round. */
  refresh() {
    if (this.id === null || !app) return;
    const out = tabs.why.querySelector(".blossom-tree");
    tabs.why.querySelector(".blossom-subject").textContent = this.id;
    let whys;
    try {
      whys = JSON.parse(app.why(this.id));
    } catch (err) {
      out.replaceChildren(text("p", `error: ${err}`, "blossom-error"));
      return;
    }
    if (whys.length === 0) {
      out.replaceChildren(text("p", "Not on the page.", "blossom-muted"));
      return;
    }
    out.replaceChildren(...whys.map((w) => tree(w, 0)));
  },
};

function text(tag, s, cls) {
  const el = document.createElement(tag);
  el.textContent = s;
  if (cls) el.className = cls;
  return el;
}

/** One reason, and (collapsible) the reasons for it. */
function tree(w, depth) {
  const line = document.createElement("div");
  line.className = "blossom-why-line";
  line.append(text("code", w.fact, "blossom-fact"), text("span", w.how, "blossom-how"));
  if (w.how === "(explained above)") line.classList.add("blossom-muted");
  if (w.because.length === 0) {
    const leaf = document.createElement("div");
    leaf.className = "blossom-why blossom-leaf";
    leaf.append(line);
    return leaf;
  }
  const node = document.createElement("details");
  node.className = "blossom-why";
  node.open = depth < 4;
  const summary = document.createElement("summary");
  summary.append(line);
  node.append(summary, ...w.because.map((b) => tree(b, depth + 1)));
  return node;
}

function target(e) {
  return e.target.closest?.("[data-bid]") ?? null;
}

/** In inspect mode the app gets no input: a click explains, the rest is held back. */
function inspectEvents() {
  for (const kind of ["pointerdown", "mousedown", "mouseup", "click", "dblclick", "change", "keydown", "input"]) {
    mount.addEventListener(
      kind,
      (e) => {
        if (!inspector.on) return;
        e.preventDefault();
        e.stopPropagation();
        const t = target(e);
        if (kind === "click" && t) inspector.explain(t.dataset.bid);
      },
      true,
    );
  }
  mount.addEventListener("mouseover", (e) => {
    if (inspector.on) inspector.mark(target(e));
  });
  mount.addEventListener("mouseleave", () => inspector.mark(null));
  addEventListener("keydown", (e) => {
    if (e.key === "Escape" && inspector.on) inspector.toggle(false);
  });
}

// ---------------------------------------------------------------- the editor

const editor = {
  files: {},
  original: {},
  file: root,
  area: document.getElementById("blossom-code"),
  picker: document.getElementById("blossom-file"),
  diags: document.getElementById("blossom-diags"),

  load(files, original) {
    this.files = files;
    this.original = original;
    this.picker.replaceChildren(...Object.keys(files).map((f) => text("option", f)));
    this.picker.value = root;
    this.file = root;
    this.area.value = files[root];
    this.marks();
  },

  pick(file) {
    this.files[this.file] = this.area.value;
    this.file = file;
    this.area.value = this.files[file];
  },

  /** Whether the source differs from the app's files. */
  marks() {
    const edited = Object.keys(this.files).some((f) => this.files[f] !== this.original[f]);
    document.getElementById("blossom-revert").disabled = !edited;
  },

  run() {
    this.files[this.file] = this.area.value;
    keepFocus = true;
    let result;
    try {
      result = run(this.files);
    } catch (err) {
      result = { diags: [], ok: false, error: String(err) };
    } finally {
      keepFocus = false;
    }
    this.show(result);
    if (result.ok) {
      const edited = Object.keys(this.files).filter((f) => this.files[f] !== this.original[f]);
      if (edited.length > 0) localStorage.setItem(sourceKey, JSON.stringify(this.files));
      else localStorage.removeItem(sourceKey);
    }
    this.marks();
  },

  revert() {
    localStorage.removeItem(sourceKey);
    this.load({ ...this.original }, this.original);
    this.run();
  },

  show(result) {
    const items = [];
    if (result.error) items.push(text("li", `error: ${result.error}`, "blossom-error"));
    if (!result.ok && !result.error) items.push(text("li", "Not run: the program does not compile.", "blossom-error"));
    for (const n of result.notes ?? []) items.push(text("li", n, "blossom-note"));
    for (const d of result.diags) {
      const li = document.createElement("li");
      li.className = d.severity === "error" ? "blossom-error" : "blossom-warning";
      const where = d.file ? `${d.file}:${d.line}:${d.column}` : "";
      const pos = text("button", where || d.code, "blossom-pos");
      pos.type = "button";
      pos.addEventListener("click", () => this.reveal(d));
      li.append(pos, text("span", ` ${d.code}: ${d.message}`));
      li.title = d.rendered;
      items.push(li);
    }
    if (result.ok && items.length === 0) items.push(text("li", "Running.", "blossom-ok"));
    this.diags.replaceChildren(...items);
  },

  /** Selects a diagnostic's place in the editor. */
  reveal(d) {
    if (!d.file || !d.range || !(d.file in this.files)) return;
    if (d.file !== this.file) {
      this.picker.value = d.file;
      this.pick(d.file);
    }
    const [start, end] = d.range;
    this.area.focus();
    this.area.setSelectionRange(start, Math.max(start, end));
    const lines = this.area.value.split("\n").length;
    const lineHeight = this.area.scrollHeight / Math.max(1, lines);
    this.area.scrollTop = Math.max(0, (d.line - 5) * lineHeight);
  },
};

function editorEvents() {
  editor.picker.addEventListener("change", () => editor.pick(editor.picker.value));
  document.getElementById("blossom-run").addEventListener("click", () => editor.run());
  document.getElementById("blossom-revert").addEventListener("click", () => editor.revert());
  editor.area.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      editor.run();
    } else if (e.key === "Tab" && !e.shiftKey) {
      e.preventDefault();
      editor.area.setRangeText("    ", editor.area.selectionStart, editor.area.selectionEnd, "end");
    }
  });
  editor.area.addEventListener("input", () => {
    editor.files[editor.file] = editor.area.value;
    editor.marks();
  });
}

// ---------------------------------------------------------------- start

async function main() {
  const css = document.getElementById("app-css");
  const appText = await served();
  wasm = await import(appText !== null ? "./pkg-member/blossom_web.js" : "./pkg/blossom_web.js");
  await wasm.default();
  if (appText !== null) {
    listen();
    inspectEvents();
    buttons.source.hidden = true;
    buttons.inspect.addEventListener("click", () => inspector.toggle(!inspector.on));
    document.getElementById("blossom-close").addEventListener("click", () => {
      inspector.toggle(false);
      show(null);
    });
    await runMember(appText);
    if (STYLES[appName]) css.href = STYLES[appName];
    requestAnimationFrame(frame);
    document.body.dataset.blossom = "ready";
    return;
  }
  if (STYLES[appName]) css.href = STYLES[appName];
  const original = { "ui.bls": await fetchSource("ui.bls"), [root]: await fetchSource(root) };
  let files = { ...original };
  const edited = bench === null ? localStorage.getItem(sourceKey) : null;
  if (bench !== null) document.getElementById("blossom-bar").hidden = true;
  if (edited) files = { ...original, ...JSON.parse(edited) };
  listen();
  inspectEvents();
  editorEvents();
  buttons.inspect.addEventListener("click", () => inspector.toggle(!inspector.on));
  buttons.source.addEventListener("click", () => {
    if (inspector.on) inspector.toggle(false);
    show(tabs.source.hidden || panel.hidden ? "source" : null);
  });
  document.getElementById("blossom-close").addEventListener("click", () => {
    inspector.toggle(false);
    show(null);
  });
  editor.load({ ...files }, original);
  requestAnimationFrame(frame);
  const result = run(files);
  editor.show(result);
  if (!result.ok) {
    report(result.error ?? result.diags.map((d) => d.rendered).join("\n"));
    show("source");
  }
  document.body.dataset.blossom = "ready";
}

main().catch((err) => report(`error: ${err}`));
