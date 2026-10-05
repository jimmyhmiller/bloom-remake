// The browser host (docs/design/BROWSER.md): loads a Blossom program's source, compiles and runs it in WebAssembly,
// applies each round's page patches to the DOM, reports the DOM events the program reads, and keeps its durable
// tables in localStorage. `?app=NAME` picks examples/web/NAME.bls (default: todomvc).
import init, { compile } from "./pkg/blossom_web.js";

const appName = new URLSearchParams(location.search).get("app") ?? "todomvc";
const mount = document.getElementById("app");
const statusLine = document.getElementById("blossom-status");
const storageKey = `blossom:${appName}`;

/** The elements the program made, by its ids. */
const nodes = new Map();
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
        const n = document.createElement(p.tag);
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
      case "children": {
        const parent = element(p.parent);
        const want = p.ids.map((id) => nodes.get(id));
        for (const child of want) parent.appendChild(child);
        for (const child of [...parent.children]) {
          if (!want.includes(child)) parent.removeChild(child);
        }
        break;
      }
      case "focus":
        focus = nodes.get(p.id);
        break;
      default:
        throw new Error(`unknown patch ${JSON.stringify(p)}`);
    }
  }
  if (focus) {
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

let app = null;
let busy = false;
const queue = [];

/** Runs events one at a time: an event fired while patches apply (a removed field's blur) waits for its turn. */
function send(event) {
  queue.push(event);
  if (busy) return;
  busy = true;
  try {
    while (queue.length > 0) {
      const next = queue.shift();
      try {
        apply(JSON.parse(app.dispatch(JSON.stringify(next))));
        localStorage.setItem(storageKey, app.saved());
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
  typed: ["input", (e, id) => ({ kind: "input", id, value: e.target.value ?? "" })],
  keydown: ["keydown", (e, id) => ({ kind: "keydown", id, key: e.key, value: e.target.value ?? "" })],
  blur: ["focusout", (e, id) => ({ kind: "blur", id, value: e.target.value ?? "" })],
  change: ["change", (e, id) => ({ kind: "change", id, checked: Boolean(e.target.checked) })],
};

function listen(inputs) {
  for (const name of inputs) {
    if (name === "route") {
      addEventListener("hashchange", () => send({ kind: "route", hash: location.hash }));
      continue;
    }
    const [dom, make] = LISTENERS[name];
    mount.addEventListener(dom, (e) => {
      const target = e.target.closest?.("[data-bid]");
      if (target) send(make(e, target.dataset.bid));
    });
  }
}

async function source(file) {
  const res = await fetch(`pkg/apps/${file}`);
  if (!res.ok) throw new Error(`cannot load ${file}: ${res.status}`);
  return res.text();
}

async function main() {
  await init();
  const css = document.getElementById("app-css");
  if (appName === "todomvc") css.href = "todomvc.css";
  const files = { "ui.bls": await source("ui.bls"), [`${appName}.bls`]: await source(`${appName}.bls`) };
  try {
    app = compile(`${appName}.bls`, JSON.stringify(files));
  } catch (err) {
    const diags = JSON.parse(String(err));
    report(diags.map((d) => d.rendered).join("\n"));
    return;
  }
  listen(JSON.parse(app.listens()));
  const started = JSON.parse(app.start(localStorage.getItem(storageKey) ?? "", location.hash));
  apply(started.patches);
  localStorage.setItem(storageKey, app.saved());
  if (started.notes.length > 0) report(started.notes.join("\n"));
  document.body.dataset.blossom = "ready";
}

main().catch((err) => report(`error: ${err}`));
