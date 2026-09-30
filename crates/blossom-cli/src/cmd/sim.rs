//! `blossom sim`: simulate a deployment deterministically, or replay a trace.
//!
//! Programs run in synchronous rounds (TEST-006), optionally with injected faults, and every node's relations are
//! printed tick by tick; with `pre` and `post` defined the run is also judged at its last tick.
//!
//! - `blossom sim prog.ded --nodes a,b,c --ticks 6`: a Molly program under Molly's crash view;
//! - `blossom sim specs.bls --spec NAME`: a Blossom spec's target in its scenario (nodes, facts, `round`), to EOT
//!   unless `--ticks` says otherwise, under CR-20;
//! - `blossom sim prog.bls --nodes a,b=Role --ticks 6`: a Blossom program root on its own.
//!
//! Traces, replay and the seeded asynchronous simulator are WP M7.2's (TEST-001).

use std::collections::BTreeSet;
use std::process::ExitCode;

use blossom_artifact::sim::{LogicalKind, SimArtifact};
use blossom_ldfi::report::{DedNames, timeline};
use blossom_sim::spec::SpecSim;
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::Tick;

use crate::common::{Context, ded};
use crate::exit::Exit;

/// Arguments of `blossom sim`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The program's `.ded` files (their `include`s are loaded too), or one `.bls` file.
    #[arg(required = true)]
    pub files: Vec<String>,
    /// A spec of the `.bls` file whose target and scenario to run.
    #[arg(long)]
    pub spec: Option<String>,
    /// The deployment's nodes (`name`, or `name=Role` for a Blossom program with roles).
    #[arg(long, value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// Run ticks 0 through this one (a spec's default: its EOT).
    #[arg(long)]
    pub ticks: Option<u64>,
    /// The clock advance per tick of a Blossom program (default: the spec's `round`, else 1s).
    #[arg(long)]
    pub round: Option<String>,
    /// Lose everything `from` sends to `to` at a tick: `from:to:tick` (repeatable).
    #[arg(long = "omit", value_name = "FROM:TO:TICK")]
    pub omissions: Vec<String>,
    /// Crash a node at a tick: `node:tick` (repeatable).
    #[arg(long = "crash", value_name = "NODE:TICK")]
    pub crashes: Vec<String>,
    /// The run seed (seeded choices and resolution policies draw from it; default 0).
    #[arg(long)]
    pub seed: Option<u64>,
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
    let (artifact, default_ticks) = match load(&args) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let Some(ticks) = args.ticks.or(default_ticks) else {
        eprintln!("give `--ticks`");
        return Exit::Usage.into();
    };
    let faults = match faults(&artifact, &args) {
        Ok(f) => f,
        Err(message) => {
            eprintln!("{message}");
            return Exit::Usage.into();
        }
    };
    let externs = match crate::common::std_externs() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return Exit::Internal.into();
        }
    };
    let sim = match SpecSim::with_externs(&artifact, externs) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Exit::UserError.into();
        }
    };
    let last = Tick(ticks);
    let run = match sim.run(last, &faults, false) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Exit::Fault.into();
        }
    };
    let names = DedNames { artifact: &artifact };
    let shown: BTreeSet<&str> = args.rels.iter().map(String::as_str).collect();
    for t in 0..=ticks {
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
                if rel.kind != LogicalKind::Protocol {
                    continue;
                }
                // Generated relations (`$` in their names) are shown only when asked for.
                let wanted = if shown.is_empty() {
                    !rel.name.as_str().contains('$')
                } else {
                    shown.contains(rel.name.as_str())
                };
                if !wanted {
                    continue;
                }
                let Some(ir) = rel.protocol else { continue };
                for row in nt.instance.rows(ir) {
                    let vals = names.row(ir, row);
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
                        let program = artifact.spec.as_ref().map(|s| s.program.get());
                        let rel = artifact
                            .spec
                            .as_ref()
                            .map(|s| if label == "pre" { s.pre } else { s.post });
                        let cols = program
                            .zip(rel)
                            .and_then(|(p, r)| p.rels.get(r))
                            .map(|r| &r.schema.cols);
                        let vals: Vec<String> = row
                            .iter()
                            .enumerate()
                            .map(|(i, v)| names.typed(v, cols.and_then(|c| c.get(i)).map(|c| c.ty), program))
                            .collect();
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

/// The artifact to run and the default last tick.
fn load(args: &Args) -> Result<(SimArtifact, Option<u64>), ExitCode> {
    if ded::all_ded(&args.files) {
        if args.spec.is_some() || args.round.is_some() || args.seed.is_some() {
            eprintln!("`--spec`, `--round` and `--seed` apply to `.bls` files");
            return Err(Exit::Usage.into());
        }
        if args.nodes.is_empty() {
            eprintln!("a `.ded` program needs `--nodes`");
            return Err(Exit::Usage.into());
        }
        return Ok((ded::compile(&args.files, &args.nodes)?, None));
    }
    let [file] = args.files.as_slice() else {
        eprintln!("a Blossom program is simulated from one `.bls` file");
        return Err(Exit::Usage.into());
    };
    let round = match &args.round {
        None => None,
        Some(text) => match crate::common::bls::parse_duration(text) {
            Some(d) => Some(d),
            None => {
                eprintln!("`--round {text}`: expected a duration such as `1s` or `100ms`");
                return Err(Exit::Usage.into());
            }
        },
    };
    if let Some(name) = &args.spec {
        if !args.nodes.is_empty() {
            eprintln!("a spec names its own nodes; drop `--nodes`");
            return Err(Exit::Usage.into());
        }
        let spec = crate::common::bls::compile_spec(file, name)?;
        let mut artifact = spec.artifact;
        if let Some(r) = round {
            artifact.profile = blossom_artifact::sim::Profile::Blossom { round: r };
        }
        if let Some(s) = args.seed {
            artifact.seed = blossom_value::Seed::from_u64(s);
        }
        return Ok((artifact, spec.faults.map(|f| f.eot)));
    }
    if args.nodes.is_empty() {
        eprintln!("a Blossom program needs `--nodes` (or `--spec`)");
        return Err(Exit::Usage.into());
    }
    let nodes: Vec<blossom_front::api::NodeSpec> = args
        .nodes
        .iter()
        .map(|n| match n.split_once('=') {
            Some((name, role)) => blossom_front::api::NodeSpec {
                name: name.to_owned(),
                role: Some(role.to_owned()),
            },
            None => blossom_front::api::NodeSpec {
                name: n.clone(),
                role: None,
            },
        })
        .collect();
    let bls = crate::common::bls::compile(file, &nodes)?;
    let round = round.unwrap_or(blossom_value::time::Duration::from_nanos(1_000_000_000));
    let seed = blossom_value::Seed::from_u64(args.seed.unwrap_or(0));
    Ok((blossom_front::spec::sim_artifact(bls, round, seed), None))
}

fn faults(artifact: &SimArtifact, args: &Args) -> Result<FaultSchedule, String> {
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
