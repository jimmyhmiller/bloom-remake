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
