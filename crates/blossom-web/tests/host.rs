//! The browser host, natively (docs/design/BROWSER.md): programs of `examples/web` compiled in memory, run round by
//! round, their pages diffed into patches and their durable tables saved and restored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use blossom_value::time::Instant;
use blossom_web::{App, Event, Patch, compile};

/// The clock of the tests that need none (programs without timers).
const T0: Instant = Instant(0);

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
    App::new(compiled, blossom_value::Seed::from_u64(0)).unwrap()
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
    let started = a.start(None, "", T0).unwrap();
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
    assert_eq!(a.dispatch(&click("plus"), T0).unwrap(), [text("value", "1")]);
    assert_eq!(a.dispatch(&click("plus"), T0).unwrap(), [text("value", "2")]);
    assert_eq!(a.dispatch(&click("minus"), T0).unwrap(), [text("value", "1")]);
    // A click elsewhere runs a round that changes nothing; an event it does not listen to runs none.
    assert_eq!(a.dispatch(&click("value"), T0).unwrap(), []);
    assert_eq!(a.dispatch(&Event::Route { hash: "#/x".to_owned() }, T0).unwrap(), []);
}

#[test]
fn the_counter_continues_from_its_saved_state() {
    let mut a = app("counter.bls");
    a.start(None, "", T0).unwrap();
    a.dispatch(&click("plus"), T0).unwrap();
    a.dispatch(&click("plus"), T0).unwrap();
    let saved = a.saved().unwrap();
    let mut b = app("counter.bls");
    let started = b.start(Some(&saved), "", T0).unwrap();
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
    let mut c = App::new(
        compile("counter.bls", &files).unwrap_or_else(|d| panic!("{:?}", d)),
        blossom_value::Seed::from_u64(0),
    )
    .unwrap();
    let started = c.start(Some(&saved), "", T0).unwrap();
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
    let mut a = App::new(compile("bad.bls", &files).unwrap(), blossom_value::Seed::from_u64(0)).unwrap();
    let err = a.start(None, "", T0).unwrap_err().to_string();
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
    // Its range is an editor's (UTF-16 offsets): past text that is not ASCII, it still marks the wrong expression.
    let source = "program mixed version 1;\n/// Ünïcödé, and 🌸 (two UTF-16 units).\ntable t(x: u64) key(x);\non boot() { upsert t(\"seven\"); }\n";
    files.insert("mixed.bls".to_owned(), source.to_owned());
    let diags = compile("mixed.bls", &files).err().unwrap();
    let (start, end) = diags[0].range.unwrap_or_else(|| panic!("{diags:?}"));
    let units: Vec<u16> = source.encode_utf16().collect();
    let marked = String::from_utf16(&units[start as usize..end as usize]).unwrap();
    assert!(marked.contains("\"seven\""), "{marked:?} {diags:?}");
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
                Patch::Create { id, tag, .. } => {
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
    let go = |a: &mut App, dom: &mut Dom, e: Event| dom.apply(&a.dispatch(&e, T0).unwrap());
    dom.apply(&a.start(None, "", T0).unwrap().patches);
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
        a.dispatch(
            &Event::Blur {
                id: "edit-1".to_owned(),
                value: "Walk the cat".to_owned()
            },
            T0
        )
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
    dom.apply(&b.start(Some(&saved), "#/active", T0).unwrap().patches);
    assert_eq!(dom.todos(), ["Feed the cat"]);
    assert_eq!(dom.attr("link-active", "class").as_deref(), Some("selected"));
    dom.apply(&b.dispatch(&ev_key("new-todo", "Enter", "Next"), T0).unwrap());
    assert!(
        dom.shown("todo-3"),
        "numbering continues: {:?}",
        dom.children.get("list")
    );
}

/// The first explanation of `fact`, anywhere in the tree.
#[cfg(test)]
fn find<'w>(ws: &'w [blossom_web::why::Why], fact: &str) -> Option<&'w blossom_web::why::Why> {
    ws.iter().find_map(|w| {
        if w.fact == fact && w.how != "(explained above)" {
            Some(w)
        } else {
            find(&w.because, fact)
        }
    })
}

#[cfg(test)]
fn render(ws: &[blossom_web::why::Why], depth: usize, out: &mut String) {
    for w in ws {
        out.push_str(&format!("{}{} <- {}\n", "  ".repeat(depth), w.fact, w.how));
        render(&w.because, depth + 1, out);
    }
}

#[test]
fn the_inspector_explains_an_element_down_to_the_events_and_rules() {
    let mut a = app("todomvc.bls");
    a.start(None, "", T0).unwrap();
    a.dispatch(&ev_key("new-todo", "Enter", "Buy milk"), T0).unwrap();
    a.dispatch(&ev_key("new-todo", "Enter", "Walk the dog"), T0).unwrap();
    a.dispatch(
        &Event::Change {
            id: "toggle-1".to_owned(),
            checked: true,
        },
        T0,
    )
    .unwrap();
    let why = a.why("label-1").unwrap();
    let mut tree = String::new();
    render(&why, 0, &mut tree);
    // The label's element and its text, each made by rule `item` this round.
    let elem = find(&why, r#"elem("label-1", "view-1", 1, "label")"#).unwrap_or_else(|| panic!("{tree}"));
    assert!(elem.how.starts_with("rule `item` in round "), "{tree}");
    let text = find(&why, r#"text("label-1", "Walk the dog")"#).unwrap_or_else(|| panic!("{tree}"));
    assert!(text.how.starts_with("rule `item` in round "), "{tree}");
    // From the todo, toggled by its checkbox's change, added by the Enter that typed it.
    let done = find(&why, r#"todos(1, "Walk the dog", true)"#).unwrap_or_else(|| panic!("{tree}"));
    assert!(done.how.ends_with("by rule `toggle`"), "{tree}");
    assert!(
        find(&done.because, r#"change("toggle-1", true)"#).is_some_and(|w| w.how.starts_with("the event of round"))
    );
    let added = find(&done.because, r#"todos(1, "Walk the dog", false)"#).unwrap_or_else(|| panic!("{tree}"));
    assert!(added.how.ends_with("by rule `add`"), "{tree}");
    assert!(
        find(&added.because, r#"keydown("new-todo", "Enter", "Walk the dog")"#)
            .is_some_and(|w| w.how.starts_with("the event of round")),
        "{tree}"
    );
    // Its number, from the first todo's add.
    assert!(
        find(&added.because, "next_n(1)").is_some_and(|w| w.how.ends_with("by rule `add`")),
        "{tree}"
    );
    // The filter it is shown under, from the route at boot.
    assert!(
        find(&why, r#"showing("all")"#).is_some_and(|w| w.how.ends_with("by rule `route`")),
        "{tree}"
    );
    // No expansion's internals as facts, except a negation (which says whose it is).
    for line in tree.lines() {
        assert!(
            !line.contains('$') || line.contains("expansion)"),
            "an internal relation shown: {line}\n{tree}"
        );
    }
    // The edit field, from the double-click that started the edit.
    a.dispatch(
        &Event::Dblclick {
            id: "label-0".to_owned(),
        },
        T0,
    )
    .unwrap();
    let why = a.why("edit-0").unwrap();
    let mut tree = String::new();
    render(&why, 0, &mut tree);
    let editing = find(&why, "editing(0)").unwrap_or_else(|| panic!("{tree}"));
    assert!(editing.how.ends_with("by rule `start_edit`"), "{tree}");
    assert!(find(&editing.because, r#"dblclick("label-0")"#).is_some(), "{tree}");
    // An element not on the page has no explanation.
    assert_eq!(a.why("nope").unwrap(), []);
}

#[test]
fn the_inspector_says_when_a_row_was_restored_from_storage() {
    let mut a = app("todomvc.bls");
    a.start(None, "", T0).unwrap();
    a.dispatch(&ev_key("new-todo", "Enter", "Buy milk"), T0).unwrap();
    let saved = a.saved().unwrap();
    let mut b = app("todomvc.bls");
    b.start(Some(&saved), "", T0).unwrap();
    let why = b.why("label-0").unwrap();
    let mut tree = String::new();
    render(&why, 0, &mut tree);
    let row = find(&why, r#"todos(0, "Buy milk", false)"#).unwrap_or_else(|| panic!("{tree}"));
    assert!(row.how.contains("restored from storage"), "{tree}");
}

#[cfg(test)]
fn ms(n: i64) -> Instant {
    Instant(n * 1_000_000)
}

#[cfg(test)]
fn time_of(patches: &[Patch]) -> Option<String> {
    patches.iter().find_map(|p| match p {
        Patch::Text { id, text } if id == "time" => Some(text.clone()),
        _ => None,
    })
}

#[test]
fn the_clock_fires_timers_while_their_guard_holds_and_counts_late_firings() {
    let mut a = app("stopwatch.bls");
    assert!(a.clocked());
    a.start(None, "", ms(0)).unwrap();
    // Stopped: no timer is active.
    assert_eq!(a.next_deadline().unwrap(), None);
    assert_eq!(a.advance(ms(5_000)).unwrap(), []);
    // Started at 5.2s: the next firing is the first one after, at 6s (counted from the start).
    a.dispatch(&click("toggle"), ms(5_200)).unwrap();
    assert_eq!(a.next_deadline().unwrap(), Some(ms(6_000)));
    assert_eq!(a.advance(ms(5_900)).unwrap(), []);
    assert_eq!(time_of(&a.advance(ms(6_000)).unwrap()).as_deref(), Some("1"));
    // Late by two seconds: one round, with every firing due (each counts).
    assert_eq!(time_of(&a.advance(ms(9_050)).unwrap()).as_deref(), Some("4"));
    // Stopped again: the clock moves, nothing fires.
    a.dispatch(&click("toggle"), ms(9_100)).unwrap();
    assert_eq!(a.next_deadline().unwrap(), None);
    assert_eq!(a.advance(ms(20_000)).unwrap(), []);
    // The seconds are durable; the timer counts from the new start.
    let saved = a.saved().unwrap();
    let mut b = app("stopwatch.bls");
    b.start(Some(&saved), "", ms(100_000)).unwrap();
    b.dispatch(&click("toggle"), ms(100_000)).unwrap();
    assert_eq!(b.next_deadline().unwrap(), Some(ms(101_000)));
    assert_eq!(time_of(&b.advance(ms(101_000)).unwrap()).as_deref(), Some("5"));
    // A clock that goes back is held where it was.
    assert_eq!(b.advance(ms(50)).unwrap(), []);
    assert_eq!(time_of(&b.advance(ms(102_000)).unwrap()).as_deref(), Some("6"));
}

/// The text of `id`, if it is on the page.
#[cfg(test)]
fn shown_text(dom: &Dom, id: &str) -> Option<String> {
    dom.shown(id).then(|| dom.text(id))
}

/// The text of an element, as the patches so far set it.
#[cfg(test)]
fn texts(patches: &[Patch]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for p in patches {
        if let Patch::Text { id, text } = p {
            out.insert(id.clone(), text.clone());
        }
    }
    out
}

#[test]
fn flappy_plays_falls_flaps_scores_and_crashes() {
    let mut a = app("flappy.bls");
    let mut dom = Dom::default();
    dom.apply(&a.start(None, "", ms(0)).unwrap().patches);
    assert_eq!(shown_text(&dom, "message").as_deref(), Some("Click to begin!"));
    // The menu has no clock.
    assert_eq!(a.next_deadline().unwrap(), None);
    dom.apply(&a.dispatch(&Event::Press { id: "sky".to_owned() }, ms(1_000)).unwrap());
    assert_eq!(shown_text(&dom, "score").as_deref(), Some("0"));
    assert!(a.next_deadline().unwrap().is_some());
    // A second of frames: the bird falls (its y grows), the score is 1.
    let y_of = |dom: &Dom| -> f64 {
        let t = dom.attr("bird", "transform").unwrap();
        t.split(' ').nth(1).unwrap().trim_end_matches(')').parse().unwrap()
    };
    let start_y = y_of(&dom);
    let mut t = 1_000;
    for _ in 0..30 {
        t += 17;
        dom.apply(&a.advance(ms(t)).unwrap());
    }
    let fallen = y_of(&dom);
    assert!(fallen > start_y + 5.0, "{start_y} -> {fallen}");
    // A flap sends it up.
    dom.apply(&a.dispatch(&Event::Press { id: "bird".to_owned() }, ms(t)).unwrap());
    for _ in 0..8 {
        t += 17;
        dom.apply(&a.advance(ms(t)).unwrap());
    }
    assert!(y_of(&dom) < fallen, "{fallen} -> {}", y_of(&dom));
    // Left alone it falls to the ground: game over, with the score and the best kept.
    for _ in 0..600 {
        t += 17;
        dom.apply(&a.advance(ms(t)).unwrap());
        if shown_text(&dom, "over").is_some() {
            break;
        }
    }
    assert_eq!(shown_text(&dom, "over").as_deref(), Some("Game Over"));
    assert_eq!(a.next_deadline().unwrap(), None, "the clock stops when the game does");
    let score = shown_text(&dom, "final").unwrap();
    let best = shown_text(&dom, "best").unwrap();
    assert_eq!(score.trim_start_matches("Score "), best.trim_start_matches("Best "));
    // The best score is durable.
    let saved = a.saved().unwrap();
    let mut b = app("flappy.bls");
    b.start(Some(&saved), "", ms(0)).unwrap();
    assert!(texts(&b.dispatch(&Event::Press { id: "sky".to_owned() }, ms(10)).unwrap()).contains_key("score"));
}

#[test]
fn flappy_with_an_autopilot_passes_obstacles_and_hits_one_without_it() {
    let num = |s: &str| -> f64 { s.parse().unwrap() };
    let translate = |dom: &Dom, id: &str, i: usize| -> f64 {
        let t = dom.attr(id, "transform").unwrap();
        let inner = t.trim_start_matches("translate(");
        num(inner.split([' ', ')']).nth(i).unwrap())
    };
    let mut a = app("flappy.bls");
    let mut dom = Dom::default();
    dom.apply(&a.start(None, "", ms(0)).unwrap().patches);
    let mut t = 0;
    dom.apply(&a.dispatch(&Event::Press { id: "sky".to_owned() }, ms(t)).unwrap());
    // Flap whenever the bird sinks below the middle of the next gap (the obstacle nearest ahead of it).
    let mut gaps = BTreeSet::new();
    for _ in 0..60 * 12 {
        t += 17;
        dom.apply(&a.advance(ms(t)).unwrap());
        if shown_text(&dom, "over").is_some() {
            break;
        }
        let y = translate(&dom, "bird", 1);
        let ahead = (0..2)
            .filter(|n| dom.shown(&format!("obstacle-{n}")))
            .map(|n| (translate(&dom, &format!("obstacle-{n}"), 0), n))
            .filter(|(x, _)| *x + 12.0 > 25.0 - 5.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let target = match ahead {
            Some((_, n)) => {
                let h = num(&dom.attr(&format!("top-{n}"), "height").unwrap());
                gaps.insert(h.to_bits());
                h + 35.0 / 2.0 + 4.0
            }
            None => 50.0,
        };
        if y > target {
            dom.apply(&a.dispatch(&Event::Press { id: "bird".to_owned() }, ms(t)).unwrap());
        }
    }
    let score: u64 = shown_text(&dom, "score")
        .or_else(|| shown_text(&dom, "final").map(|s| s.trim_start_matches("Score ").to_owned()))
        .unwrap()
        .parse()
        .unwrap();
    assert!(score >= 8, "the autopilot scored only {score}");
    assert!(gaps.len() >= 3, "the gaps did not change: {gaps:?}");
    // Without it, a bird that flaps only to stay above the ground hits the first obstacle, not the ground.
    let mut b = app("flappy.bls");
    let mut dom = Dom::default();
    dom.apply(&b.start(None, "", ms(0)).unwrap().patches);
    let mut t = 0;
    dom.apply(&b.dispatch(&Event::Press { id: "sky".to_owned() }, ms(t)).unwrap());
    let mut last_y = 0.0;
    for _ in 0..60 * 6 {
        t += 17;
        dom.apply(&b.advance(ms(t)).unwrap());
        if shown_text(&dom, "over").is_some() {
            break;
        }
        last_y = translate(&dom, "bird", 1);
        if last_y > 80.0 {
            dom.apply(&b.dispatch(&Event::Press { id: "bird".to_owned() }, ms(t)).unwrap());
        }
    }
    assert_eq!(shown_text(&dom, "over").as_deref(), Some("Game Over"));
    assert!(last_y < 85.0, "it hit the ground at {last_y}, not an obstacle");
}

/// A program from `body` with the page interface.
#[cfg(test)]
fn app_of(name: &str, body: &str) -> App {
    let mut files = files();
    files.insert(
        format!("{name}.bls"),
        format!("program {name} version 1;\ninclude \"ui.bls\";\n{body}"),
    );
    let compiled = compile(&format!("{name}.bls"), &files).unwrap_or_else(|d| {
        panic!(
            "{name}:\n{}",
            d.iter().map(|d| d.rendered.clone()).collect::<Vec<_>>().join("\n")
        )
    });
    App::new(compiled, blossom_value::Seed::from_u64(0)).unwrap()
}

#[test]
fn the_sugar_draws_the_page_the_plain_rows_draw() {
    // docs/design/SUGAR.md: a tree literal, a child head inheriting its parent's id, and a spread, against the same
    // page written as plain rows (derived ids and slots spelled out).
    let state = "table todo(n: u64, title: String) key(n);\n\
                 view total(k = count!(n default 0u64)) = todo(n, _);\n\
                 init: on boot() { upsert todo(1, \"milk\"); upsert todo(2, \"eggs\"); }\n";
    let sugar = format!(
        "{state}page: while total(k) {{\n\
             emit html section[id: \"app\"](class: \"todoapp\", data-count: k) {{\n\
                 h1 {{ \"todos\" }}\n\
                 ul[id: \"list\"] {{ for todo(n, t) {{ li[key: n, pos: n as i64](class: \"item\") {{ t }} }} }}\n\
                 footer {{ f\"{{k}} left\" }}\n\
             }}\n\
             emit elem(id: \"x\", parent: \"\", pos: 1, tag: \"p\") {{ attr(name: \"title\", value: \"hi\"); text(s: \"x\"); }}\n\
             emit attr(\"x\", ..{{lang: \"en\", tabindex: 3}});\n\
         }}\n"
    );
    let plain = format!(
        "{state}page: while total(k) {{\n\
             emit elem(\"app\", \"\", 0, \"section\");\n\
             emit attr(\"app\", \"class\", \"todoapp\");\n\
             emit attr(\"app\", \"data-count\", k.to_string());\n\
             emit elem(\"app/h1.0\", \"app\", 0, \"h1\");\n\
             emit text(\"app/h1.0\", \"todos\");\n\
             emit elem(\"list\", \"app\", 1, \"ul\");\n\
             emit elem(\"app/footer.2\", \"app\", 2, \"footer\");\n\
             emit text(\"app/footer.2\", k.to_string() ++ \" left\");\n\
             emit elem(\"x\", \"\", 1, \"p\");\n\
             emit attr(\"x\", \"title\", \"hi\");\n\
             emit text(\"x\", \"x\");\n\
             emit attr(\"x\", \"lang\", \"en\");\n\
             emit attr(\"x\", \"tabindex\", \"3\");\n\
         }}\n\
         items: while todo(n, t) {{\n\
             emit elem(\"list/li.0[\" ++ n.to_string() ++ \"]\", \"list\", n as i64, \"li\");\n\
             emit attr(\"list/li.0[\" ++ n.to_string() ++ \"]\", \"class\", \"item\");\n\
             emit text(\"list/li.0[\" ++ n.to_string() ++ \"]\", t);\n\
         }}\n"
    );
    let mut a = app_of("sugar", &sugar);
    let mut b = app_of("plain", &plain);
    let pa = a.start(None, "", T0).unwrap().patches;
    let pb = b.start(None, "", T0).unwrap().patches;
    assert!(!pa.is_empty());
    assert_eq!(a.page(), b.page());
    assert_eq!(pa, pb);
}

#[test]
fn a_map_spread_draws_a_row_per_entry() {
    let state = "table seen(n: u64) key();\ninit: on boot() { upsert seen(5); }\n";
    let mut a = app_of(
        "spread",
        &format!(
            "{state}page: while seen(k), let m = map[\"data-a\" => k, \"data-b\" => 7u64] {{\n\
                 emit elem(\"x\", \"\", 0, \"p\");\n\
                 emit attr(\"x\", ..m);\n\
             }}\n"
        ),
    );
    let mut b = app_of(
        "spread_plain",
        &format!(
            "{state}page: while seen(k) {{\n\
                 emit elem(\"x\", \"\", 0, \"p\");\n\
                 emit attr(\"x\", \"data-a\", k.to_string());\n\
                 emit attr(\"x\", \"data-b\", \"7\");\n\
             }}\n"
        ),
    );
    a.start(None, "", T0).unwrap();
    b.start(None, "", T0).unwrap();
    assert_eq!(a.page(), b.page());
}
