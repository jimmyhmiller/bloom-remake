//! The browser host, natively (docs/design/BROWSER.md): programs of `examples/web` compiled in memory, run round by
//! round, their pages diffed into patches and their durable tables saved and restored.

use std::collections::BTreeMap;
use std::path::Path;

use blossom_web::{App, Event, Patch, compile};

/// The sources of `examples/web`, by file name.
#[cfg(test)]
fn files() -> BTreeMap<String, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web");
    std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "bls").then(|| {
                let name = p.file_name()?.to_str()?.to_owned();
                Some((name, std::fs::read_to_string(&p).ok()?))
            })?
        })
        .collect()
}

#[cfg(test)]
fn app(root: &str) -> App {
    let compiled = compile(root, &files()).unwrap_or_else(|d| {
        panic!(
            "{root}:\n{}",
            d.iter().map(|d| d.rendered.clone()).collect::<Vec<_>>().join("\n")
        )
    });
    App::new(compiled).unwrap()
}

#[cfg(test)]
fn click(id: &str) -> Event {
    Event::Click { id: id.to_owned() }
}

#[cfg(test)]
fn text(id: &str, t: &str) -> Patch {
    Patch::Text {
        id: id.to_owned(),
        text: t.to_owned(),
    }
}

#[test]
fn the_counter_draws_its_page_and_changes_only_what_a_click_changes() {
    let mut a = app("counter.bls");
    assert_eq!(a.compiled().listens(), ["click"]);
    let started = a.start(None, "").unwrap();
    assert!(started.notes.is_empty());
    let p = &started.patches;
    for id in ["counter", "minus", "value", "plus"] {
        assert!(
            p.iter().any(|x| matches!(x, Patch::Create { id: i, .. } if i == id)),
            "{id}: {p:?}"
        );
    }
    assert!(p.contains(&text("value", "0")), "{p:?}");
    assert!(p.contains(&Patch::Children {
        parent: "counter".to_owned(),
        ids: vec!["minus".to_owned(), "value".to_owned(), "plus".to_owned()],
    }));
    assert!(p.contains(&Patch::Children {
        parent: String::new(),
        ids: vec!["counter".to_owned()],
    }));
    // A click changes the count's text and nothing else.
    assert_eq!(a.dispatch(&click("plus")).unwrap(), [text("value", "1")]);
    assert_eq!(a.dispatch(&click("plus")).unwrap(), [text("value", "2")]);
    assert_eq!(a.dispatch(&click("minus")).unwrap(), [text("value", "1")]);
    // A click elsewhere runs a round that changes nothing; an event it does not listen to runs none.
    assert_eq!(a.dispatch(&click("value")).unwrap(), []);
    assert_eq!(a.dispatch(&Event::Route { hash: "#/x".to_owned() }).unwrap(), []);
}

#[test]
fn the_counter_continues_from_its_saved_state() {
    let mut a = app("counter.bls");
    a.start(None, "").unwrap();
    a.dispatch(&click("plus")).unwrap();
    a.dispatch(&click("plus")).unwrap();
    let saved = a.saved().unwrap();
    let mut b = app("counter.bls");
    let started = b.start(Some(&saved), "").unwrap();
    assert!(started.patches.contains(&text("value", "2")), "{:?}", started.patches);
    // A program whose durable schema changed starts empty, and says so.
    let mut files = files();
    let changed = files["counter.bls"].replace("table count(n: i64) key();", "table count(n: i64, m: i64) key();");
    files.insert(
        "counter.bls".to_owned(),
        changed
            .replace("upsert count(n + 1i64)", "upsert count(n + 1i64, 0)")
            .replace("upsert count(n - 1i64)", "upsert count(n - 1i64, 0)")
            .replace("= count(c)", "= count(c, _)"),
    );
    let mut c = App::new(compile("counter.bls", &files).unwrap_or_else(|d| panic!("{:?}", d))).unwrap();
    let started = c.start(Some(&saved), "").unwrap();
    assert_eq!(started.notes.len(), 1, "{:?}", started.notes);
    assert!(started.patches.contains(&text("value", "0")));
}

#[test]
fn a_program_that_breaks_the_page_or_the_interface_is_told_why() {
    let mut files = files();
    files.insert(
        "bad.bls".to_owned(),
        "program bad version 1;\ninclude \"ui.bls\";\nshow: on boot() { emit elem(\"a\", \"nowhere\", 0, \"div\"); }\n"
            .to_owned(),
    );
    let mut a = App::new(compile("bad.bls", &files).unwrap()).unwrap();
    let err = a.start(None, "").unwrap_err().to_string();
    assert!(err.contains("`nowhere` is not on the page"), "{err}");
    files.insert(
        "wrong.bls".to_owned(),
        "program wrong version 1;\noutput elem(id: String, parent: String, tag: String);\n".to_owned(),
    );
    let diags = compile("wrong.bls", &files).err().unwrap();
    assert!(diags[0].message.contains("`elem` is a page output"), "{diags:?}");
    // A compile error comes with its position.
    files.insert(
        "typo.bls".to_owned(),
        "program typo version 1;\ntable t(x: u64;\n".to_owned(),
    );
    let diags = compile("typo.bls", &files).err().unwrap();
    assert_eq!(diags[0].line, Some(2), "{diags:?}");
}

#[test]
fn todomvc_compiles() {
    let a = app("todomvc.bls");
    let mut l = a.compiled().listens();
    l.sort();
    assert_eq!(l, ["blur", "change", "click", "dblclick", "keydown", "route", "typed"]);
}

/// A model of the DOM the patches build: per element its tag, attributes, text and children.
#[cfg(test)]
#[derive(Default)]
struct Dom {
    tags: BTreeMap<String, String>,
    attrs: BTreeMap<String, BTreeMap<String, String>>,
    texts: BTreeMap<String, String>,
    children: BTreeMap<String, Vec<String>>,
    focused: Option<String>,
}

#[cfg(test)]
impl Dom {
    fn apply(&mut self, patches: &[Patch]) {
        for p in patches {
            match p {
                Patch::Create { id, tag } => {
                    self.tags.insert(id.clone(), tag.clone());
                    self.attrs.insert(id.clone(), BTreeMap::new());
                    self.texts.remove(id);
                    self.children.remove(id);
                }
                Patch::Remove { id } => {
                    self.tags.remove(id);
                    self.attrs.remove(id);
                    self.texts.remove(id);
                }
                Patch::Attr { id, name, value } => {
                    self.attrs
                        .entry(id.clone())
                        .or_default()
                        .insert(name.clone(), value.clone());
                }
                Patch::Unattr { id, name } => {
                    self.attrs.entry(id.clone()).or_default().remove(name);
                }
                Patch::Text { id, text } => {
                    self.texts.insert(id.clone(), text.clone());
                }
                Patch::Children { parent, ids } => {
                    self.children.insert(parent.clone(), ids.clone());
                }
                Patch::Focus { id } => self.focused = Some(id.clone()),
            }
        }
    }

    /// Whether `id` is attached under the mount point.
    fn shown(&self, id: &str) -> bool {
        let mut stack = vec![String::new()];
        while let Some(p) = stack.pop() {
            for c in self.children.get(&p).into_iter().flatten() {
                if c == id && self.tags.contains_key(c) {
                    return true;
                }
                stack.push(c.clone());
            }
        }
        false
    }

    /// The text content of `id`: its text, then its children's.
    fn text(&self, id: &str) -> String {
        let mut out = self.texts.get(id).cloned().unwrap_or_default();
        for c in self.children.get(id).into_iter().flatten() {
            if self.tags.contains_key(c) {
                out.push_str(&self.text(c));
            }
        }
        out
    }

    fn attr(&self, id: &str, name: &str) -> Option<String> {
        self.attrs.get(id).and_then(|a| a.get(name)).cloned()
    }

    /// The labels of the todos shown, in order.
    fn todos(&self) -> Vec<String> {
        if !self.shown("list") {
            return Vec::new();
        }
        self.children["list"]
            .iter()
            .map(|li| self.text(&li.replace("todo-", "label-")))
            .collect()
    }
}

#[cfg(test)]
fn ev_key(id: &str, key: &str, value: &str) -> Event {
    Event::Keydown {
        id: id.to_owned(),
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

#[test]
fn todomvc_follows_the_spec() {
    let mut a = app("todomvc.bls");
    let mut dom = Dom::default();
    let go = |a: &mut App, dom: &mut Dom, e: Event| dom.apply(&a.dispatch(&e).unwrap());
    dom.apply(&a.start(None, "").unwrap().patches);
    // An empty app: the header only; the new-todo field focused.
    assert!(dom.shown("new-todo") && !dom.shown("main") && !dom.shown("footer"));
    assert_eq!(dom.focused.as_deref(), Some("new-todo"));
    // Adding trims, and clears the field; a blank title adds nothing.
    for title in ["  Buy milk ", "Walk the dog"] {
        go(
            &mut a,
            &mut dom,
            Event::Input {
                id: "new-todo".to_owned(),
                value: title.to_owned(),
            },
        );
        go(&mut a, &mut dom, ev_key("new-todo", "Enter", title));
        assert_eq!(dom.attr("new-todo", "value").as_deref(), Some(""));
    }
    go(&mut a, &mut dom, ev_key("new-todo", "Enter", "   "));
    assert_eq!(dom.todos(), ["Buy milk", "Walk the dog"]);
    assert_eq!(dom.text("count"), "2 items left");
    assert!(!dom.shown("clear-completed"));
    // Toggling one completes it.
    go(
        &mut a,
        &mut dom,
        Event::Change {
            id: "toggle-0".to_owned(),
            checked: true,
        },
    );
    assert_eq!(dom.attr("todo-0", "class").as_deref(), Some("completed"));
    assert_eq!(dom.attr("toggle-0", "checked").as_deref(), Some("true"));
    assert_eq!(dom.text("count"), "1 item left");
    assert!(dom.shown("clear-completed"));
    // The filters.
    go(
        &mut a,
        &mut dom,
        Event::Route {
            hash: "#/active".to_owned(),
        },
    );
    assert_eq!(dom.todos(), ["Walk the dog"]);
    assert_eq!(dom.attr("link-active", "class").as_deref(), Some("selected"));
    go(
        &mut a,
        &mut dom,
        Event::Route {
            hash: "#/completed".to_owned(),
        },
    );
    assert_eq!(dom.todos(), ["Buy milk"]);
    go(&mut a, &mut dom, Event::Route { hash: "#/".to_owned() });
    assert_eq!(dom.todos(), ["Buy milk", "Walk the dog"]);
    // Editing: double-click, Enter saves (trimmed); the blur of the field's removal changes nothing.
    go(
        &mut a,
        &mut dom,
        Event::Dblclick {
            id: "label-1".to_owned(),
        },
    );
    assert!(dom.shown("edit-1"));
    assert_eq!(dom.attr("edit-1", "value").as_deref(), Some("Walk the dog"));
    assert_eq!(dom.attr("todo-1", "class").as_deref(), Some("editing"));
    assert_eq!(dom.focused.as_deref(), Some("edit-1"));
    go(&mut a, &mut dom, ev_key("edit-1", "Enter", " Walk the cat "));
    assert!(!dom.shown("edit-1"));
    assert_eq!(dom.todos(), ["Buy milk", "Walk the cat"]);
    assert_eq!(
        a.dispatch(&Event::Blur {
            id: "edit-1".to_owned(),
            value: "Walk the cat".to_owned()
        })
        .unwrap(),
        []
    );
    // Escape drops an edit; leaving the field saves one; an empty title deletes the todo.
    go(
        &mut a,
        &mut dom,
        Event::Dblclick {
            id: "label-1".to_owned(),
        },
    );
    go(&mut a, &mut dom, ev_key("edit-1", "Escape", "something else"));
    assert_eq!(dom.todos(), ["Buy milk", "Walk the cat"]);
    go(
        &mut a,
        &mut dom,
        Event::Dblclick {
            id: "label-1".to_owned(),
        },
    );
    go(
        &mut a,
        &mut dom,
        Event::Blur {
            id: "edit-1".to_owned(),
            value: "Feed the cat".to_owned(),
        },
    );
    assert_eq!(dom.todos(), ["Buy milk", "Feed the cat"]);
    go(&mut a, &mut dom, ev_key("new-todo", "Enter", "Call mom"));
    go(
        &mut a,
        &mut dom,
        Event::Dblclick {
            id: "label-2".to_owned(),
        },
    );
    go(&mut a, &mut dom, ev_key("edit-2", "Enter", "  "));
    assert_eq!(dom.todos(), ["Buy milk", "Feed the cat"]);
    // Toggle all, both ways; it is checked when every todo is done.
    go(
        &mut a,
        &mut dom,
        Event::Change {
            id: "toggle-all".to_owned(),
            checked: true,
        },
    );
    assert_eq!(dom.text("count"), "0 items left");
    assert_eq!(dom.attr("toggle-all", "checked").as_deref(), Some("true"));
    go(
        &mut a,
        &mut dom,
        Event::Change {
            id: "toggle-all".to_owned(),
            checked: false,
        },
    );
    assert_eq!(dom.text("count"), "2 items left");
    assert_eq!(dom.attr("toggle-all", "checked").as_deref(), Some("false"));
    // Clear completed; destroy.
    go(
        &mut a,
        &mut dom,
        Event::Change {
            id: "toggle-0".to_owned(),
            checked: true,
        },
    );
    go(&mut a, &mut dom, click("clear-completed"));
    assert_eq!(dom.todos(), ["Feed the cat"]);
    let saved = a.saved().unwrap();
    go(&mut a, &mut dom, click("destroy-1"));
    assert!(dom.todos().is_empty() && !dom.shown("main") && !dom.shown("footer"));
    // A reload continues from the saved todos (numbering included).
    let mut b = app("todomvc.bls");
    let mut dom = Dom::default();
    dom.apply(&b.start(Some(&saved), "#/active").unwrap().patches);
    assert_eq!(dom.todos(), ["Feed the cat"]);
    assert_eq!(dom.attr("link-active", "class").as_deref(), Some("selected"));
    dom.apply(&b.dispatch(&ev_key("new-todo", "Enter", "Next")).unwrap());
    assert!(
        dom.shown("todo-3"),
        "numbering continues: {:?}",
        dom.children.get("list")
    );
}
