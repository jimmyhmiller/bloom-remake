//! The TodoMVC benchmark's workload (docs/dev/todomvc-bench.md), natively: add 100 todos, toggle each, delete each,
//! through the browser host's `App` with the events the page sends. Prints the time per phase, the rounds per event,
//! and the rules that did the most work, so a change to the host or the engine can be measured without a browser.
//!
//!   cargo run --release -p blossom-web --example todomvc_work [-- N [REPEAT]]
//!
//! With `REPEAT`, the workload runs that many more times on fresh apps first, quietly (for a profiler to sample).
// A measuring tool: it prints, stops at the first failure, and reads the wall clock to time the phases.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant as Clock;

use blossom_value::time::Instant;
use blossom_web::{App, Event, compile};

fn main() {
    let n: usize = std::env::args().nth(1).map_or(100, |a| a.parse().expect("N: a number"));
    let repeat: usize = std::env::args()
        .nth(2)
        .map_or(0, |a| a.parse().expect("REPEAT: a number"));
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web");
    let mut files = BTreeMap::new();
    for f in ["ui.bls", "todomvc.bls"] {
        files.insert(f.to_owned(), std::fs::read_to_string(dir.join(f)).unwrap());
    }
    let compiled = compile("todomvc.bls", &files).expect("todomvc.bls compiles");
    let t0 = Instant(0);
    for _ in 0..repeat {
        let mut app = App::new(compiled.clone(), blossom_value::Seed::from_u64(0)).unwrap();
        app.start(None, "", t0).unwrap();
        for e in workload(n) {
            app.dispatch(&e, t0).unwrap();
        }
    }
    let mut app = App::new(compiled, blossom_value::Seed::from_u64(0)).unwrap();
    app.start(None, "", t0).unwrap();
    let before_rounds = app.rounds();
    let before: BTreeMap<_, _> = app.work_by_rule().into_iter().collect();

    let phase = |name: &str, events: Vec<Event>, app: &mut App| {
        let r0 = app.rounds();
        let c = Clock::now();
        let mut patches = 0;
        for e in &events {
            patches += app.dispatch(e, t0).unwrap().len();
        }
        let ms = c.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{name:<10} {ms:>9.1} ms  {:>7.3} ms/event  {:>4.1} rounds/event  {:>6.1} patches/event",
            ms / events.len() as f64,
            (app.rounds() - r0) as f64 / events.len() as f64,
            patches as f64 / events.len() as f64
        );
        ms
    };
    let mut adds = Vec::new();
    for i in 0..n {
        let value = format!("todo number {i}");
        adds.push(Event::Input {
            id: "new-todo".into(),
            value: value.clone(),
        });
        adds.push(Event::Keydown {
            id: "new-todo".into(),
            key: "Enter".into(),
            value,
        });
    }
    let total = phase("add", adds, &mut app)
        + phase(
            "toggle",
            (0..n)
                .map(|i| Event::Change {
                    id: format!("toggle-{i}"),
                    checked: true,
                })
                .collect(),
            &mut app,
        )
        + phase(
            "delete",
            (0..n)
                .rev()
                .map(|i| Event::Click {
                    id: format!("destroy-{i}"),
                })
                .collect(),
            &mut app,
        );
    println!(
        "total      {total:>9.1} ms over {} rounds",
        app.rounds() - before_rounds
    );

    let mut evals = 0;
    let mut work: Vec<(String, u64, u64, u64)> = app
        .work_by_rule()
        .into_iter()
        .map(|(label, w)| {
            let b = before.get(&label).copied().unwrap_or_default();
            evals += w.evals - b.evals;
            (
                label.to_string(),
                w.rows - b.rows,
                w.steps - b.steps,
                w.writes - b.writes,
            )
        })
        .collect();
    work.sort_by_key(|(_, rows, steps, writes)| std::cmp::Reverse(rows + steps + writes));
    let sum = |f: fn(&(String, u64, u64, u64)) -> u64| work.iter().map(f).sum::<u64>();
    println!(
        "\nwork: {} rows, {} steps, {} writes, {evals} rule evaluations; the rules that did most:",
        sum(|w| w.1),
        sum(|w| w.2),
        sum(|w| w.3)
    );
    for (label, rows, steps, writes) in work.iter().take(25) {
        println!("{rows:>9} rows {steps:>9} steps {writes:>8} writes  {label}");
    }
    if std::env::var_os("BY_EVALS").is_some() {
        let mut by: Vec<(u64, String)> = app
            .work_by_rule()
            .into_iter()
            .map(|(label, w)| {
                (
                    w.evals - before.get(&label).copied().unwrap_or_default().evals,
                    label.to_string(),
                )
            })
            .collect();
        by.sort_by_key(|(e, _)| std::cmp::Reverse(*e));
        println!("\nthe rules evaluated most:");
        for (e, label) in by.iter().take(40) {
            println!("{e:>7}  {label}");
        }
    }
}

/// Every event of the workload, in order.
fn workload(n: usize) -> Vec<Event> {
    let mut out = Vec::new();
    for i in 0..n {
        let value = format!("todo number {i}");
        out.push(Event::Input {
            id: "new-todo".into(),
            value: value.clone(),
        });
        out.push(Event::Keydown {
            id: "new-todo".into(),
            key: "Enter".into(),
            value,
        });
    }
    out.extend((0..n).map(|i| Event::Change {
        id: format!("toggle-{i}"),
        checked: true,
    }));
    out.extend((0..n).rev().map(|i| Event::Click {
        id: format!("destroy-{i}"),
    }));
    out
}
