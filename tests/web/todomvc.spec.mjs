// TodoMVC (examples/web/todomvc.bls) in the browser, through TodoMVC's behaviors (https://github.com/tastejs/todomvc/
// blob/master/app-spec.md): real DOM events into a Blossom program compiled and run in WebAssembly.
import { test, expect } from "@playwright/test";

const newTodo = (page) => page.locator(".new-todo");
const items = (page) => page.locator(".todo-list li");
const labels = (page) => page.locator(".todo-list li label");

async function open(page, hash = "") {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto(`/index.html${hash}`);
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
}

async function add(page, ...titles) {
  for (const t of titles) {
    await newTodo(page).fill(t);
    await newTodo(page).press("Enter");
  }
}

test.beforeEach(async ({ page }) => {
  await open(page);
  await page.evaluate(() => localStorage.clear());
  await open(page);
});

test("no todos: only the header, the new-todo field focused", async ({ page }) => {
  await expect(page.locator(".todoapp h1")).toHaveText("todos");
  await expect(newTodo(page)).toBeFocused();
  await expect(page.locator(".main")).toHaveCount(0);
  await expect(page.locator(".footer")).toHaveCount(0);
});

test("adding trims, clears the field, and ignores blanks", async ({ page }) => {
  await add(page, "  buy some cheese  ", "feed the cat");
  await expect(labels(page)).toHaveText(["buy some cheese", "feed the cat"]);
  await expect(newTodo(page)).toHaveValue("");
  await add(page, "   ");
  await expect(items(page)).toHaveCount(2);
  await expect(page.locator(".todo-count")).toHaveText("2 items left");
});

test("toggling, the count, clear completed", async ({ page }) => {
  await add(page, "one", "two", "three");
  await items(page).nth(1).locator(".toggle").check();
  await expect(items(page).nth(1)).toHaveClass("completed");
  await expect(page.locator(".todo-count")).toHaveText("2 items left");
  await expect(page.locator(".clear-completed")).toBeVisible();
  await page.locator(".clear-completed").click();
  await expect(labels(page)).toHaveText(["one", "three"]);
  await expect(page.locator(".clear-completed")).toHaveCount(0);
  await items(page).nth(0).locator(".toggle").check();
  await expect(page.locator(".todo-count")).toHaveText("1 item left");
});

test("toggle all, both ways", async ({ page }) => {
  await add(page, "one", "two");
  await page.locator(".toggle-all").check();
  await expect(items(page).nth(0)).toHaveClass("completed");
  await expect(items(page).nth(1)).toHaveClass("completed");
  await expect(page.locator(".todo-count")).toHaveText("0 items left");
  await page.locator(".toggle-all").uncheck();
  await expect(items(page).nth(0)).toHaveClass("");
  await expect(page.locator(".todo-count")).toHaveText("2 items left");
  // Checking every todo checks toggle all.
  await items(page).nth(0).locator(".toggle").check();
  await items(page).nth(1).locator(".toggle").check();
  await expect(page.locator(".toggle-all")).toBeChecked();
});

test("editing: double-click, Enter saves, Escape cancels, blur saves, empty deletes", async ({ page }) => {
  await add(page, "one", "two", "three");
  await labels(page).nth(1).dblclick();
  await expect(items(page).nth(1)).toHaveClass("editing");
  const edit = items(page).nth(1).locator(".edit");
  await expect(edit).toBeFocused();
  await expect(edit).toHaveValue("two");
  await edit.fill("  zwei  ");
  await edit.press("Enter");
  await expect(labels(page)).toHaveText(["one", "zwei", "three"]);
  await expect(page.locator(".edit")).toHaveCount(0);
  // Escape.
  await labels(page).nth(0).dblclick();
  await items(page).nth(0).locator(".edit").fill("uno");
  await items(page).nth(0).locator(".edit").press("Escape");
  await expect(labels(page)).toHaveText(["one", "zwei", "three"]);
  // Blur saves.
  await labels(page).nth(2).dblclick();
  await items(page).nth(2).locator(".edit").fill("drei");
  await page.locator(".todoapp h1").click();
  await expect(labels(page)).toHaveText(["one", "zwei", "drei"]);
  // Empty deletes.
  await labels(page).nth(0).dblclick();
  await items(page).nth(0).locator(".edit").fill("");
  await items(page).nth(0).locator(".edit").press("Enter");
  await expect(labels(page)).toHaveText(["zwei", "drei"]);
});

test("destroy", async ({ page }) => {
  await add(page, "one", "two");
  await items(page).nth(0).hover();
  await items(page).nth(0).locator(".destroy").click();
  await expect(labels(page)).toHaveText(["two"]);
});

test("routing: all, active, completed, and back", async ({ page }) => {
  await add(page, "one", "two", "three");
  await items(page).nth(1).locator(".toggle").check();
  await page.locator(".filters a", { hasText: "Active" }).click();
  await expect(page).toHaveURL(/#\/active$/);
  await expect(labels(page)).toHaveText(["one", "three"]);
  await expect(page.locator(".filters a.selected")).toHaveText("Active");
  await page.locator(".filters a", { hasText: "Completed" }).click();
  await expect(labels(page)).toHaveText(["two"]);
  await page.goBack();
  await expect(labels(page)).toHaveText(["one", "three"]);
  await page.locator(".filters a", { hasText: "All" }).click();
  await expect(labels(page)).toHaveText(["one", "two", "three"]);
});

test("persistence: each durable row is its own entry, and the older one-entry form is read once and rewritten", async ({
  page,
}) => {
  await add(page, "one", "two");
  const rows = await page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("blossom:todomvc:row:todos:")));
  expect(rows.length).toBe(2);
  // The same state in the older form: one entry holding every table.
  await page.evaluate(() => {
    const schemas = JSON.parse(localStorage.getItem("blossom:todomvc:tables"));
    const rows = Object.fromEntries(Object.keys(schemas).map((t) => [t, []]));
    for (const k of Object.keys(localStorage)) {
      if (!k.startsWith("blossom:todomvc:row:")) continue;
      const rest = k.slice("blossom:todomvc:row:".length);
      const cut = rest.indexOf(":[");
      rows[rest.slice(0, cut)].push(JSON.parse(rest.slice(cut + 1)));
      localStorage.removeItem(k);
    }
    localStorage.removeItem("blossom:todomvc:tables");
    const tables = Object.keys(schemas).map((name) => ({ name, schema: schemas[name], rows: rows[name] }));
    localStorage.setItem("blossom:todomvc", JSON.stringify({ tables }));
  });
  await page.reload();
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  await expect(labels(page)).toHaveText(["one", "two"]);
  const after = await page.evaluate(() => ({
    old: localStorage.getItem("blossom:todomvc"),
    rows: Object.keys(localStorage).filter((k) => k.startsWith("blossom:todomvc:row:todos:")).length,
  }));
  expect(after).toEqual({ old: null, rows: 2 });
});

test("persistence: a reload keeps the todos, their state and the route", async ({ page }) => {
  await add(page, "one", "two");
  await items(page).nth(0).locator(".toggle").check();
  await page.locator(".filters a", { hasText: "Active" }).click();
  await page.reload();
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
  await expect(labels(page)).toHaveText(["two"]);
  await page.locator(".filters a", { hasText: "All" }).click();
  await expect(items(page).nth(0)).toHaveClass("completed");
  await expect(page.locator("#blossom-status")).toBeEmpty();
});
