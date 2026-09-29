//! `blossom sim`: simulate a deployment deterministically, or replay a trace.
//!
//! Slice 1 (docs/design/SLICES.md) simulates Molly `.ded` programs in synchronous rounds (TEST-006) under Molly's
//! crash view, optionally with injected faults, and prints every node's relations tick by tick; with `pre` and
//! `post` defined it also judges the run at its last tick. Simulating `.bls` deployments, traces and replay are WP
//! M7.2's (TEST-001).

use std::collections::BTreeSet;
use std::process::ExitCode;

use blossom_artifact::ded::{DedArtifact, DedRelKind};
use blossom_ldfi::report::{DedNames, timeline};
use blossom_sim::ded::DedSim;
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::Tick;

use crate::common::{Context, ded};
use crate::exit::Exit;

/// Arguments of `blossom sim`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The program's `.ded` files (their `include`s are loaded too).
    #[arg(required = true)]
    pub files: Vec<String>,
    /// The deployment's nodes.
    #[arg(long, value_delimiter = ',', required = true)]
    pub nodes: Vec<String>,
    /// Run ticks 0 through this one.
    #[arg(long)]
    pub ticks: u64,
    /// Lose everything `from` sends to `to` at a tick: `from:to:tick` (repeatable).
    #[arg(long = "omit", value_name = "FROM:TO:TICK")]
    pub omissions: Vec<String>,
    /// Crash a node at a tick: `node:tick` (repeatable).
    #[arg(long = "crash", value_name = "NODE:TICK")]
    pub crashes: Vec<String>,
    /// Show only these relations (repeatable; default: every protocol relation).
    #[arg(long = "rel")]
    pub rels: Vec<String>,
    /// Show only this tick.
    #[arg(long)]
    pub tick: Option<u64>,
    /// Also print the messages sent between nodes.
    #[arg(long)]
    pub messages: bool,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    if !ded::all_ded(&args.files) {
        eprintln!("`blossom sim` on `.bls` deployments arrives with WP M7.2 (slice 2, docs/design/SLICES.md)");
        return crate::exit::not_implemented("TEST-001", "M7.2");
    }
    let artifact = match ded::compile(&args.files, &args.nodes) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let faults = match faults(&artifact, &args) {
        Ok(f) => f,
        Err(message) => {
            eprintln!("{message}");
            return Exit::Usage.into();
        }
    };
    let sim = match DedSim::new(&artifact) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Exit::UserError.into();
        }
    };
    let last = Tick(args.ticks);
    let run = match sim.run(last, &faults, false) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Exit::Fault.into();
        }
    };
    let names = DedNames { artifact: &artifact };
    let shown: BTreeSet<&str> = args.rels.iter().map(String::as_str).collect();
    for t in 0..=args.ticks {
        if args.tick.is_some_and(|only| only != t) {
            continue;
        }
        println!("== tick {t}");
        for (n, name) in artifact.nodes.iter().enumerate() {
            let Some(node) = u32::try_from(n).ok().map(blossom_value::time::NodeId) else {
                continue;
            };
            let Some(nt) = run.node_tick(Tick(t), node) else {
                continue;
            };
            let mut lines = Vec::new();
            for rel in &artifact.rels {
                if rel.kind != DedRelKind::Protocol || (!shown.is_empty() && !shown.contains(rel.name.as_str())) {
                    continue;
                }
                let Some(ir) = rel.protocol else { continue };
                for row in nt.instance.rows(ir) {
                    let vals: Vec<String> = row.iter().map(|v| names.value(v)).collect();
                    lines.push(format!("  {}({})", rel.name, vals.join(", ")));
                }
            }
            if !lines.is_empty() {
                println!("{name}");
                for l in lines {
                    println!("{l}");
                }
            }
        }
    }
    if args.messages {
        println!("== messages");
        print!("{}", timeline(&names, &run));
    }
    if artifact.spec.is_some() {
        match sim.outcome(&run, last, false) {
            Ok(outcome) => {
                println!("== outcome at tick {}", last.0);
                for (label, rows) in [("pre", &outcome.pre), ("post", &outcome.post)] {
                    for row in rows {
                        let vals: Vec<String> = row.iter().map(|v| names.value(v)).collect();
                        println!("  {label}({})", vals.join(", "));
                    }
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return Exit::Fault.into();
            }
        }
    }
    Exit::Ok.into()
}

fn faults(artifact: &DedArtifact, args: &Args) -> Result<FaultSchedule, String> {
    let node = |name: &str| {
        artifact
            .node_id(name)
            .ok_or_else(|| format!("`{name}` is not one of the nodes"))
    };
    let mut out = FaultSchedule::default();
    for text in &args.omissions {
        let (names, tick) = ded::parse_fault(text, 3)?;
        let [from, to] = names.as_slice() else {
            return Err(format!("`{text}`: expected FROM:TO:TICK"));
        };
        out.omissions.insert(Omission {
            from: node(from)?,
            to: node(to)?,
            send: Tick(tick),
        });
    }
    for text in &args.crashes {
        let (names, tick) = ded::parse_fault(text, 2)?;
        let [n] = names.as_slice() else {
            return Err(format!("`{text}`: expected NODE:TICK"));
        };
        out.crashes.insert(node(n)?, Tick(tick));
    }
    Ok(out)
}
