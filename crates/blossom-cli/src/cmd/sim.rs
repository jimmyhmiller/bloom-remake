//! `blossom sim`: simulate a deployment deterministically, or replay a trace.
//!
//! Programs run in synchronous rounds (TEST-006), optionally with injected faults, and every node's relations are
//! printed tick by tick; with `pre` and `post` defined the run is also judged at its last tick.
//!
//! - `blossom sim prog.ded --nodes a,b,c --ticks 6`: a Molly program under Molly's crash view;
//! - `blossom sim specs.bls --spec NAME`: a Blossom spec's target in its scenario (nodes, facts, `round`), to EOT
//!   unless `--ticks` says otherwise, under CR-20;
//! - `blossom sim prog.bls --nodes a,b=Role --ticks 6`: a Blossom program root on its own; its byte streams take
//!   scripted connections and chunks (`--open`, `--chunk`, `--close`), and each tick's requests to the host (stream
//!   writes, closes, dials) are printed after its relations.
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
    /// A connection accepted on a `listen` stream: `node:stream:conn:tick` (repeatable).
    #[arg(long = "open", value_name = "NODE:STREAM:CONN:TICK")]
    pub opens: Vec<String>,
    /// A chunk read from a connection: `node:stream:conn:tick:text`, with `\\n`, `\\t`, `\\\\` and `\\xHH`
    /// escapes (repeatable; chunks of one connection are numbered in tick order).
    #[arg(long = "chunk", value_name = "NODE:STREAM:CONN:TICK:TEXT")]
    pub chunks: Vec<String>,
    /// A connection the peer closed: `node:stream:conn:tick` (repeatable).
    #[arg(long = "close", value_name = "NODE:STREAM:CONN:TICK")]
    pub closes: Vec<String>,
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
    let mut artifact = artifact;
    if let Err(message) = scripted_streams(&mut artifact, &args) {
        eprintln!("{message}");
        return Exit::Usage.into();
    }
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
            // What the node asked of the host this tick: stream writes, closes, dials.
            for h in &nt.host {
                let rel = artifact
                    .protocol
                    .get()
                    .rels
                    .get(h.rel)
                    .map(|r| r.name.to_string())
                    .unwrap_or_default();
                lines.push(format!("  => {rel}({})", names.row(h.rel, &h.row).join(", ")));
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

/// One scripted stream event: its node, stream, connection and tick.
struct Scripted {
    node: blossom_value::time::NodeId,
    stream: usize,
    conn: u64,
    tick: u64,
}

/// Parses `node:stream:conn:tick[:text]`.
fn scripted(artifact: &SimArtifact, text: &str, with_text: bool) -> Result<(Scripted, Option<Vec<u8>>), String> {
    let parts: Vec<&str> = text.splitn(if with_text { 5 } else { 4 }, ':').collect();
    let bad = || {
        format!(
            "`{text}`: expected NODE:STREAM:CONN:TICK{}",
            if with_text { ":TEXT" } else { "" }
        )
    };
    let (Some(node), Some(stream), Some(conn), Some(tick)) = (parts.first(), parts.get(1), parts.get(2), parts.get(3))
    else {
        return Err(bad());
    };
    let node = artifact
        .node_id(node)
        .ok_or_else(|| format!("`{text}`: `{node}` is not one of the nodes"))?;
    let stream = artifact
        .protocol
        .get()
        .streams
        .iter()
        .position(|s| s.name.to_string() == *stream)
        .ok_or_else(|| format!("`{text}`: the program has no stream `{stream}`"))?;
    let st = artifact
        .protocol
        .get()
        .streams
        .get(stream)
        .ok_or_else(|| format!("`{text}`: no stream {stream}"))?;
    if let blossom_ir::core::Placement::Role(r) = st.placement
        && artifact.roles.get(node.0 as usize).copied().flatten() != Some(r)
    {
        return Err(format!(
            "`{text}`: node `{}` does not run the stream `{}`",
            parts.first().copied().unwrap_or(""),
            st.name
        ));
    }
    let conn: u64 = conn.parse().map_err(|_| bad())?;
    if conn > u64::from(u32::MAX) {
        return Err(format!("`{text}`: a connection number is at most {}", u32::MAX));
    }
    let tick: u64 = tick.parse().map_err(|_| bad())?;
    let body = match (with_text, parts.get(4)) {
        (true, Some(t)) => Some(unescape(t).ok_or_else(|| format!("`{text}`: a bad escape in the text"))?),
        (true, None) => return Err(bad()),
        (false, _) => None,
    };
    Ok((
        Scripted {
            node,
            stream,
            conn,
            tick,
        },
        body,
    ))
}

/// `\\n`, `\\t`, `\\\\` and `\\xHH`.
fn unescape(t: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut chars = t.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next()? {
            'n' => out.push(b'\n'),
            't' => out.push(b'\t'),
            '\\' => out.push(b'\\'),
            'x' => {
                let hex: String = [chars.next()?, chars.next()?].iter().collect();
                out.push(u8::from_str_radix(&hex, 16).ok()?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Adds the scripted connections and chunks to the run's inputs, checking the order the runtime guarantees
/// (FOREIGN-PROTOCOLS §1.2a): a connection opens in an earlier tick than its first chunk, has at most one chunk per
/// tick, and closes in a later tick than its last chunk.
fn scripted_streams(artifact: &mut SimArtifact, args: &Args) -> Result<(), String> {
    use blossom_artifact::sim::InputFact;
    use blossom_value::Value;
    use blossom_value::value::{ConnId, IntValue};
    use std::collections::BTreeMap;
    if args.opens.is_empty() && args.chunks.is_empty() && args.closes.is_empty() {
        return Ok(());
    }
    type Key = (u32, usize, u64);
    let mut opened: BTreeMap<Key, u64> = BTreeMap::new();
    let mut chunks: BTreeMap<Key, BTreeMap<u64, Vec<u8>>> = BTreeMap::new();
    let mut closed: BTreeMap<Key, u64> = BTreeMap::new();
    for o in &args.opens {
        let (s, _) = scripted(artifact, o, false)?;
        let st = artifact.protocol.get().streams.get(s.stream).ok_or("no such stream")?;
        if st.kind != blossom_ir::core::StreamKind::Listen {
            return Err(format!("`{o}`: `--open` accepts on listen streams only"));
        }
        if opened.insert((s.node.0, s.stream, s.conn), s.tick).is_some() {
            return Err(format!("`{o}`: the connection opens twice"));
        }
    }
    for c in &args.chunks {
        let (s, body) = scripted(artifact, c, true)?;
        let per = chunks.entry((s.node.0, s.stream, s.conn)).or_default();
        if per.insert(s.tick, body.unwrap_or_default()).is_some() {
            return Err(format!("`{c}`: a connection has at most one chunk per tick"));
        }
    }
    for c in &args.closes {
        let (s, _) = scripted(artifact, c, false)?;
        if closed.insert((s.node.0, s.stream, s.conn), s.tick).is_some() {
            return Err(format!("`{c}`: the connection closes twice"));
        }
    }
    let program = artifact.protocol.get().clone();
    let mut facts = Vec::new();
    for (key @ (node, stream, conn), open_tick) in &opened {
        let st = program.streams.get(*stream).ok_or("no such stream")?;
        let node = blossom_value::time::NodeId(*node);
        // Connection numbers are per stream: the stream is the high half of the `Conn`.
        let c = Value::Conn(ConnId((*stream as u64) << 32 | *conn));
        facts.push(InputFact {
            node,
            tick: Tick(*open_tick),
            rel: st.opened,
            row: vec![
                c.clone(),
                Value::Str("script".into()),
                Value::Instant(blossom_value::time::Instant(0)),
            ],
        });
        let per = chunks.remove(key).unwrap_or_default();
        if per.keys().next().is_some_and(|t| t <= open_tick) {
            return Err(format!(
                "connection {conn}: a chunk in or before its opening tick {open_tick}"
            ));
        }
        let last = per.keys().next_back().copied();
        for (seq, (tick, bytes)) in per.into_iter().enumerate() {
            facts.push(InputFact {
                node,
                tick: Tick(tick),
                rel: st.data,
                row: vec![
                    c.clone(),
                    Value::Int(IntValue::U64(seq as u64)),
                    Value::Bytes(bytes.into()),
                ],
            });
        }
        if let Some(close_tick) = closed.remove(key) {
            if close_tick <= last.unwrap_or(*open_tick) {
                return Err(format!("connection {conn}: closes in or before its last chunk's tick"));
            }
            facts.push(InputFact {
                node,
                tick: Tick(close_tick),
                rel: st.closed,
                row: vec![c, Value::Str("closed by the script".into())],
            });
        }
    }
    if let Some(((_, _, conn), _)) = chunks.iter().next().or(None) {
        return Err(format!("connection {conn} has chunks but no `--open`"));
    }
    if let Some(((_, _, conn), _)) = closed.iter().next() {
        return Err(format!("connection {conn} closes but has no `--open`"));
    }
    artifact.inputs.extend(facts);
    Ok(())
}
