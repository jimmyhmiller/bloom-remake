//! What a function refuses when it starts, by name (docs/design/DEPLOY.md §1, §3): a store that does not outlive a
//! request, a program whose nodes' timers fire as time passes, and pages linked over WebSockets. A client role's
//! timers run in its pages, so they are no reason to refuse.

use std::path::{Path, PathBuf};

use blossom_value::Seed;
use blossom_vercel::{App, Config};

#[cfg(test)]
fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web")
}

#[cfg(test)]
fn config(deploy: PathBuf, store: &str) -> Config {
    Config {
        deploy,
        store: store.into(),
        seed: Seed([3; 16]),
        tokens_per_minute: 60,
    }
}

/// The keyed chat in a directory of its own, its program and spec edited by `edit_program` and `edit_spec`.
#[cfg(test)]
fn edited_chat(name: &str, edit_program: impl Fn(String) -> String, edit_spec: impl Fn(String) -> String) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blossom-vercel-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = std::fs::read_to_string(examples().join("keyed_chat.bls")).unwrap();
    std::fs::write(dir.join("keyed_chat.bls"), edit_program(program)).unwrap();
    for f in ["keyed_chat.css", "ui.bls", "events.bls"] {
        std::fs::copy(examples().join(f), dir.join(f)).unwrap();
    }
    let spec = std::fs::read_to_string(examples().join("keyed_chat.deploy.toml")).unwrap();
    let path = dir.join("keyed_chat.deploy.toml");
    std::fs::write(&path, edit_spec(spec)).unwrap();
    path
}

#[cfg(test)]
fn refusal(cfg: &Config) -> String {
    match App::load(cfg) {
        Ok(_) => panic!("{} loaded", cfg.deploy.display()),
        Err(e) => e,
    }
}

#[test]
fn a_store_that_does_not_outlive_a_request_is_refused() {
    for store in ["sqlite:/tmp/state.db", "memory:"] {
        let why = refusal(&config(examples().join("keyed_chat.deploy.toml"), store));
        assert!(why.contains("postgres://… or s3://…"), "{store}: {why}");
    }
}

#[test]
fn a_program_whose_nodes_have_clocked_timers_is_refused_by_name() {
    let deploy = edited_chat(
        "timer",
        |p| p.replacen("at Room {\n", "at Room {\n    timer heartbeat every 5s;\n", 1),
        |s| s,
    );
    let why = refusal(&config(deploy, "sqlite:/tmp/state.db"));
    assert!(why.contains("timers heartbeat fire as time passes"), "{why}");
}

#[test]
fn a_page_role_s_timer_is_no_reason_to_refuse() {
    // pixels' only timer is its pages' (`at Browser`): the refusal is the store's, after every check of the program.
    let why = refusal(&config(examples().join("pixels.deploy.toml"), "sqlite:/tmp/state.db"));
    assert!(why.contains("postgres://… or s3://…"), "{why}");
}

#[test]
fn pages_over_websockets_are_refused() {
    let deploy = edited_chat("ws", |p| p, |s| s.replace("[web]\n", "[web]\nlink = \"websocket\"\n"));
    let why = refusal(&config(deploy, "sqlite:/tmp/state.db"));
    assert!(why.contains("a function holds no socket"), "{why}");
}
