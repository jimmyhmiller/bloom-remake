// The host's tools beside an app (docs/design/BROWSER.md): the `why` inspector and the live source editor, on
// TodoMVC in the browser.
import { test, expect } from "@playwright/test";

const newTodo = (page) => page.locator(".new-todo");
const items = (page) => page.locator(".todo-list li");
const labels = (page) => page.locator(".todo-list li label");
const code = (page) => page.locator("#blossom-code");
const diags = (page) => page.locator("#blossom-diags li");

async function open(page) {
  page.on("pageerror", (e) => console.log("page error:", e.message));
  await page.goto("/index.html");
  await expect(page.locator("body")).toHaveAttribute("data-blossom", "ready");
}

async function add(page, ...titles) {
  for (const t of titles) {
    await newTodo(page).fill(t);
    await newTodo(page).press("Enter");
  }
}

/** A reason in the inspector's tree: its fact and what made it. */
function reason(page, fact) {
  return page.locator(".blossom-why-line", { has: page.locator(".blossom-fact", { hasText: fact }) });
}

test.beforeEach(async ({ page }) => {
  await open(page);
  await page.evaluate(() => localStorage.clear());
  await open(page);
});

test("the inspector explains a todo down to the events that made it, and holds the app's input", async ({ page }) => {
  await add(page, "Buy milk", "Walk the dog");
  await items(page).nth(1).locator(".toggle").check();
  await page.locator("#blossom-inspect").click();
  await expect(page.locator("#blossom-why")).toBeVisible();
  await labels(page).nth(1).hover();
  await expect(page.locator("#blossom-highlight")).toHaveAttribute("data-id", "label-1");
  await labels(page).nth(1).click();
  await expect(page.locator("#blossom-why .blossom-subject")).toHaveText("label-1");
  await expect(reason(page, 'elem("label-1"').first()).toContainText("rule `item`");
  await expect(reason(page, 'todos(1, "Walk the dog", true)').first()).toContainText("by rule `toggle`");
  await expect(reason(page, 'change("toggle-1", true)').first()).toContainText("the event of round");
  await expect(reason(page, 'todos(1, "Walk the dog", false)').first()).toContainText("by rule `add`");
  await expect(reason(page, 'keydown("new-todo", "Enter", "Walk the dog")').first()).toContainText("the event of");
  // In inspect mode a click explains and does nothing else.
  await items(page).nth(0).locator(".toggle").click();
  await expect(page.locator("#blossom-why .blossom-subject")).toHaveText("toggle-0");
  await expect(items(page).nth(0)).not.toHaveClass("completed");
  await expect(page.locator(".todo-count")).toHaveText("1 item left");
  // Escape leaves inspect mode; the app has its input back, and the explanation follows the page.
  await page.keyboard.press("Escape");
  await expect(page.locator("#blossom-inspect")).toHaveAttribute("aria-pressed", "false");
  await items(page).nth(0).locator(".toggle").check();
  await expect(page.locator(".todo-count")).toHaveText("0 items left");
  await expect(reason(page, 'todos(0, "Buy milk", true)').first()).toContainText("by rule `toggle`");
});

test("the editor re-runs an edited program in place, keeping the todos", async ({ page }) => {
  await add(page, "Buy milk", "Walk the dog");
  await page.locator("#blossom-edit").click();
  await expect(code(page)).toHaveValue(/program todomvc version 1;/);
  const source = await code(page).inputValue();
  await code(page).fill(source.replace("What needs to be done?", "What next?"));
  await code(page).press("Control+Enter");
  await expect(newTodo(page)).toHaveAttribute("placeholder", "What next?");
  await expect(labels(page)).toHaveText(["Buy milk", "Walk the dog"]);
  await expect(diags(page)).toHaveText(["Running."]);
  await expect(code(page)).toBeFocused();
  // The running program is the edited one: it takes events.
  await add(page, "Write Blossom");
  await expect(labels(page)).toHaveText(["Buy milk", "Walk the dog", "Write Blossom"]);
  // The edit survives a reload, and Revert goes back to the app's own source.
  await open(page);
  await expect(newTodo(page)).toHaveAttribute("placeholder", "What next?");
  await expect(page.locator("#blossom-revert")).toBeEnabled();
  await page.locator("#blossom-edit").click();
  await page.locator("#blossom-revert").click();
  await expect(newTodo(page)).toHaveAttribute("placeholder", "What needs to be done?");
  await expect(labels(page)).toHaveText(["Buy milk", "Walk the dog", "Write Blossom"]);
  await expect(page.locator("#blossom-revert")).toBeDisabled();
});

test("a program that does not compile is not run: its diagnostics point into the source", async ({ page }) => {
  await add(page, "Buy milk");
  await page.locator("#blossom-edit").click();
  const source = await code(page).inputValue();
  await code(page).fill(source.replace("upsert todos(n, value.trim(), false);", "upsert todos(n, value.trim(), 7);"));
  await page.locator("#blossom-run").click();
  await expect(diags(page).first()).toHaveText("Not run: the program does not compile.");
  const pos = page.locator("#blossom-diags .blossom-pos").first();
  await expect(pos).toHaveText(/^todomvc\.bls:\d+:\d+$/);
  // The old program still runs.
  await add(page, "Walk the dog");
  await expect(labels(page)).toHaveText(["Buy milk", "Walk the dog"]);
  // A diagnostic's position selects its place in the source.
  await pos.click();
  const selected = await code(page).evaluate((a) => a.value.slice(a.selectionStart, a.selectionEnd));
  expect(selected.length).toBeGreaterThan(0);
  expect("upsert todos(n, value.trim(), 7);").toContain(selected);
});

test("a durable table whose schema changed starts empty, and the editor says so", async ({ page }) => {
  await add(page, "Buy milk");
  await page.locator("#blossom-edit").click();
  const source = await code(page).inputValue();
  // `next_n` gains a column (its rules follow), so its saved rows no longer fit; `todos` keeps its schema.
  const edited = source
    .replace("durable table next_n(n: u64) key();", "durable table next_n(n: u64, spare: u64) key();")
    .replace("view next_id(n = max!(x default 0u64)) = next_n(x);", "view next_id(n = max!(x default 0u64)) = next_n(x, _);")
    .replace("upsert next_n(n + 1u64);", "upsert next_n(n + 1u64, 0u64);");
  await code(page).fill(edited);
  await page.locator("#blossom-run").click();
  await expect(page.locator("#blossom-diags .blossom-note")).toContainText("next_n");
  await expect(labels(page)).toHaveText(["Buy milk"]);
});
