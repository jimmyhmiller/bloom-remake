//! `blossom trace`: replay and question a node's recorded trace (`blossom run --record`, ARCHITECTURE §6.4).
//!
//! Every subcommand replays the trace with the program of the deployment it was recorded under (`--deploy`), checking
//! each tick against the recording; a divergence stops it with the tick.
//!
//! - `replay`: replay the whole trace and summarize it.
//! - `show`: a relation's rows at a tick (views and events included).
//! - `history`: every tick a relation's rows matching a pattern changed: a table's rows added or removed (for the
//!   next tick), a channel's messages received (`<-`) and sent (`->`), an event's rows (`!`), and when a view's rows
//!   start and stop holding.
//! - `profile`: the rules that did the most join work at a tick, or over the whole trace (`replay --slow` finds the
//!   slow ticks).
//! - `why`: the rule firings that derived the rows matching a pattern at a tick.
//! - `whynot`: for each rule that could derive a tuple matching a pattern, how far its body got at a tick, and the
//!   first literal no valuation passed (ask again about that literal's relation to go deeper).
//!
//! A pattern is `rel(v, …)`: one value per column of the relation in its IR form (a channel's destination is column
//! 0), `_` for any value; values are written as `show` prints them (`3`, `"name"`, `b"bytes"`, `(a, b)`, `None`,
//! `Some(x)`, `true`, a node by name).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{FnId, RelId, RuleId, TypeId};
use blossom_ir::core::{Persistence, Program, RelClass};
use blossom_ir::printer::{literal_text, rule_text, value_text, var_text};
use blossom_ir::tick::{FnWork, Row, RuleWork};
use blossom_sim::replay::{Replay, ReplayError, Replayed};
use blossom_value::time::NodeId;
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use crate::cmd::run::load;
use crate::common::Context;
use crate::exit::Exit;

/// Arguments of `blossom trace`.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: TraceCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum TraceCommand {
    /// Replay a trace, checking every tick against the recording.
    Replay {
        #[command(flatten)]
        common: Common,
        /// Also list the ticks whose replay took at least this many milliseconds, with what they received, and
        /// the ticks the recorded node started more than this long after the one before.
        #[arg(long, value_name = "MS")]
        slow: Option<u64>,
    },
    /// A relation's rows at a tick.
    Show {
        #[command(flatten)]
        common: Common,
        /// The relation (its IR name).
        #[arg(long)]
        rel: String,
        /// The tick (`blossom trace replay` prints a trace's ticks).
        #[arg(long)]
        at: u64,
        /// Only rows matching this pattern (`rel(v, _, …)`).
        #[arg(long = "match")]
        pattern: Option<String>,
    },
    /// Every tick a relation's rows matching a pattern changed (a table's, a view's), arrived or were sent.
    History {
        #[command(flatten)]
        common: Common,
        /// Only ticks at or after this time (seconds since the epoch, as the output prints them).
        #[arg(long)]
        from: Option<f64>,
        /// Only ticks at or before this time.
        #[arg(long)]
        to: Option<f64>,
        /// The pattern (`rel(v, _, …)`).
        pattern: String,
    },
    /// The rule firings that derived the rows matching a pattern at a tick.
    Why {
        #[command(flatten)]
        common: Common,
        /// The tick.
        #[arg(long)]
        at: u64,
        /// The pattern (`rel(v, _, …)`).
        pattern: String,
    },
    /// The rules that did the most join work at a tick (rows examined), with what the tick received.
    Profile {
        #[command(flatten)]
        common: Common,
        /// The tick (without it: the whole trace, summed).
        #[arg(long)]
        at: Option<u64>,
        /// Summing, only ticks at or after this time (seconds since the epoch).
        #[arg(long)]
        from: Option<f64>,
        /// Summing, only ticks at or before this time.
        #[arg(long)]
        to: Option<f64>,
        /// How many rules to list.
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// How far each rule that could derive a tuple matching a pattern got at a tick.
    Whynot {
        #[command(flatten)]
        common: Common,
        /// The tick.
        #[arg(long)]
        at: u64,
        /// The pattern (`rel(v, _, …)`).
        pattern: String,
        /// How many partial valuations to show per rule.
        #[arg(long, default_value_t = 3)]
        samples: usize,
    },
}

/// What every subcommand takes: the trace, and the program it was recorded with, from the deployment spec or (a
/// simulated run, which has none) as the program's source, nodes and parameters.
#[derive(Debug, clap::Args)]
pub struct Common {
    /// The trace (`<node>-<incarnation>.blstrace`).
    pub trace: PathBuf,
    /// The deployment spec it was recorded under (`deploy.toml`): its program is compiled and replayed.
    #[arg(
        long = "deploy",
        value_name = "FILE",
        required_unless_present = "program",
        conflicts_with = "program"
    )]
    pub deploy: Option<PathBuf>,
    /// Instead of a deployment: the program's source, compiled for `--node`s with `--param`s.
    #[arg(long, value_name = "FILE")]
    pub program: Option<PathBuf>,
    /// A node of the deployment, in order (`NAME:ROLE`, or `NAME` in a role-free program).
    #[arg(long = "node", value_name = "NAME[:ROLE]", requires = "program")]
    pub nodes: Vec<String>,
    /// A deploy-time parameter (`NAME=VALUE`: `true`/`false`, an integer, or text such as `500ms`).
    #[arg(long = "param", value_name = "NAME=VALUE", requires = "program")]
    pub params: Vec<String>,
}

/// Runs the command.
pub fn run(args: Args, cx: &Context) -> ExitCode {
    let _ = cx;
    match drive(args.command) {
        Ok(()) => Exit::Ok.into(),
        Err(e) => {
            eprintln!("blossom trace: {e}");
            Exit::Fault.into()
        }
    }
}

fn open(common: &Common) -> Result<(Arc<BlsArtifact>, Replay<BufReader<File>>), String> {
    let artifact = match (&common.deploy, &common.program) {
        (Some(deploy), _) => load(deploy).map_err(|_| "the deployment did not load".to_owned())?.1,
        (None, Some(program)) => Arc::new(compile_program(program, &common.nodes, &common.params)?),
        (None, None) => return Err("give --deploy, or --program with its --node and --param".into()),
    };
    let file = File::open(&common.trace).map_err(|e| format!("{}: {e}", common.trace.display()))?;
    let externs = crate::common::std_externs().map_err(|e| e.to_string())?;
    let replay = Replay::open(&artifact, BufReader::new(file), externs).map_err(|e| e.to_string())?;
    Ok((artifact, replay))
}

/// Compiles `program` for `nodes` (`NAME[:ROLE]`) with `params` (`NAME=VALUE`).
fn compile_program(program: &std::path::Path, nodes: &[String], params: &[String]) -> Result<BlsArtifact, String> {
    use blossom_front::api::{NodeSpec, ParamBinding};
    let nodes: Vec<NodeSpec> = nodes
        .iter()
        .map(|n| match n.split_once(':') {
            Some((name, role)) => NodeSpec {
                name: name.to_owned(),
                role: Some(role.to_owned()),
            },
            None => NodeSpec {
                name: n.clone(),
                role: None,
            },
        })
        .collect();
    let mut bindings = BTreeMap::new();
    for p in params {
        let (name, value) = p.split_once('=').ok_or_else(|| format!("`{p}` is not NAME=VALUE"))?;
        let binding = match value {
            "true" => ParamBinding::Bool(true),
            "false" => ParamBinding::Bool(false),
            v => match v.parse::<i128>() {
                Ok(n) => ParamBinding::Int(n),
                Err(_) => ParamBinding::Text(v.to_owned()),
            },
        };
        bindings.insert(name.to_owned(), binding);
    }
    let source = program
        .to_str()
        .ok_or_else(|| format!("the program path {} is not UTF-8", program.display()))?;
    crate::common::bls::compile_with(source, &nodes, &bindings).map_err(|_| "the program did not compile".to_owned())
}

/// What a relation's history reports: a carried relation's changes, a channel's messages received and sent, an
/// event's rows, or when a view's rows start and stop holding.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Carried,
    Channel,
    Host,
    Event,
    View,
}

use crate::common::stopwatch::Stopwatch;

/// The `top` rules that did the most work in `work` (a profiled tick's, or a sum): by expression nodes evaluated,
/// rows examined and rows written, each a unit of the engine's work.
fn print_work(names: &Names<'_>, work: &BTreeMap<RuleId, RuleWork>, top: usize) {
    let (rows, steps, writes) = work.values().fold((0u64, 0u64, 0u64), |(r, s, w), x| {
        (r + x.rows, s + x.steps, w + x.writes)
    });
    let mut by: Vec<(RuleId, RuleWork)> = work.iter().map(|(k, v)| (*k, *v)).collect();
    let total = |w: &RuleWork| w.rows + w.steps + w.writes;
    by.sort_by(|a, b| total(&b.1).cmp(&total(&a.1)).then(a.0.cmp(&b.0)));
    println!(
        "  {rows} rows examined, {steps} expression steps, {writes} rows written, by {} rules",
        by.len()
    );
    println!("  {:>12}  {:>12}  {:>12}", "steps", "rows", "writes");
    let program = names.program();
    for (id, w) in by.into_iter().take(top) {
        let Some(rule) = program.rules.get(id) else { continue };
        println!(
            "  {:>12}  {:>12}  {:>12}  {} ({:?})",
            w.steps, w.rows, w.writes, rule.label, rule.kind
        );
        println!(
            "                                            {}",
            rule_text(program, rule)
        );
    }
}

/// The `top` functions whose own bodies took the most steps in `work`, with their calls and their steps including
/// the functions they called.
fn print_fn_work(names: &Names<'_>, work: &BTreeMap<FnId, FnWork>, top: usize) {
    if work.is_empty() {
        return;
    }
    let mut by: Vec<(FnId, FnWork)> = work.iter().map(|(k, v)| (*k, *v)).collect();
    by.sort_by(|a, b| b.1.self_steps.cmp(&a.1.self_steps).then(a.0.cmp(&b.0)));
    println!("  functions, by their own steps:");
    println!("  {:>12}  {:>12}  {:>10}", "own steps", "with calls", "calls");
    let program = names.program();
    for (id, w) in by.into_iter().take(top) {
        let name = program
            .fns
            .get(id)
            .map_or_else(|| format!("{id:?}"), |d| d.name.to_string());
        println!("  {:>12}  {:>12}  {:>10}  {name}", w.self_steps, w.steps, w.calls);
    }
}

/// Seconds since the epoch (as `history --from/--to` take them) in the trace's nanoseconds.
#[allow(clippy::cast_possible_truncation)] // A time within ±292 years of the epoch: nanoseconds fit an i64.
fn seconds_to_nanos(s: f64) -> i64 {
    (s * 1e9).round() as i64
}

/// What a tick received, counted per relation: `name ×n` for events, deliveries and client requests.
fn received(names: &Names<'_>, r: &Replayed) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let rels = r
        .inputs
        .events
        .iter()
        .map(|(rel, _)| *rel)
        .chain(r.inputs.delivered.iter().map(|d| d.rel))
        .chain(r.inputs.ingress.iter().map(|g| g.rel));
    for rel in rels {
        *counts.entry(names.rel_name(rel)).or_default() += 1;
    }
    if counts.is_empty() {
        return "received nothing".into();
    }
    let parts: Vec<String> = counts.iter().map(|(n, c)| format!("{n} ×{c}")).collect();
    format!("received {}", parts.join(", "))
}

/// Names and printing for one trace's program.
struct Names<'a> {
    artifact: &'a BlsArtifact,
}

impl Names<'_> {
    fn program(&self) -> &Program {
        self.artifact.program.get()
    }

    fn node(&self, n: NodeId) -> String {
        self.artifact
            .nodes
            .get(n.0 as usize)
            .map_or_else(|| format!("node#{}", n.0), |s| s.to_string())
    }

    fn rel(&self, name: &str) -> Result<RelId, String> {
        self.artifact
            .rel_named(name)
            .ok_or_else(|| format!("the program has no relation `{name}`"))
    }

    fn rel_name(&self, rel: RelId) -> String {
        self.program()
            .rels
            .get(rel)
            .map_or_else(|| format!("{rel:?}"), |r| r.name.to_string())
    }

    fn col_types(&self, rel: RelId) -> Vec<TypeId> {
        self.program()
            .rels
            .get(rel)
            .map(|r| r.schema.cols.iter().map(|c| c.ty).collect())
            .unwrap_or_default()
    }

    fn value(&self, v: &Value, ty: Option<TypeId>) -> String {
        value_text(Some(self.program()), v, ty, &|n| self.node(n))
    }

    fn row(&self, rel: RelId, row: &[Value]) -> String {
        let tys = self.col_types(rel);
        let cols: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, v)| self.value(v, tys.get(i).copied()))
            .collect();
        format!("{}({})", self.rel_name(rel), cols.join(", "))
    }

    /// A pattern `rel(v, _, …)`: the relation and one optional value per column.
    fn pattern(&self, text: &str) -> Result<(RelId, Vec<Option<Value>>), String> {
        let text = text.trim();
        let (name, rest) = text
            .split_once('(')
            .ok_or_else(|| format!("`{text}` is not a pattern `rel(v, …)`"))?;
        let args = rest
            .strip_suffix(')')
            .ok_or_else(|| format!("`{text}` does not end with `)`"))?;
        let rel = self.rel(name.trim())?;
        let tys = self.col_types(rel);
        let parts = split_top(args);
        if parts.len() != tys.len() {
            return Err(format!(
                "`{}` has {} columns, the pattern {}",
                name.trim(),
                tys.len(),
                parts.len()
            ));
        }
        let values = parts
            .iter()
            .zip(&tys)
            .map(|(p, t)| {
                if p.trim() == "_" {
                    Ok(None)
                } else {
                    self.parse(p.trim(), *t).map(Some)
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((rel, values))
    }

    /// A value of type `ty` written as `show` prints it.
    fn parse(&self, text: &str, ty: TypeId) -> Result<Value, String> {
        let def = self.program().types.get(ty).cloned();
        let shown = format!("{def:?}");
        let bad = || format!("cannot read `{text}` as a value of type {shown}");
        match def {
            Some(TypeDef::Bool) => text.parse::<bool>().map(Value::Bool).map_err(|_| bad()),
            Some(TypeDef::Int(t)) => {
                let digits = strip_int_suffix(text);
                let n: i128 = digits.parse().map_err(|_| bad())?;
                IntValue::from_i128(t, n).map(Value::Int).ok_or_else(bad)
            }
            // As the printer writes a float (`1.0`, `2.5e-7`, `NaN`, `inf`), or a literal with its `f64` suffix.
            Some(TypeDef::F64) => text
                .strip_suffix("f64")
                .unwrap_or(text)
                .parse::<f64>()
                .map(|f| Value::F64(blossom_value::float::canonical(f)))
                .map_err(|_| bad()),
            Some(TypeDef::Str) => text
                .strip_prefix('"')
                .and_then(|t| t.strip_suffix('"'))
                .map(|t| Value::Str(t.into()))
                .ok_or_else(bad),
            Some(TypeDef::Bytes) => text
                .strip_prefix("b\"")
                .and_then(|t| t.strip_suffix('"'))
                .and_then(unescape)
                .map(|b| Value::Bytes(b.into()))
                .ok_or_else(bad),
            Some(TypeDef::Unit) if text == "()" => Ok(Value::Unit),
            Some(TypeDef::Node(_)) => self
                .artifact
                .node_id(text)
                .map(Value::Node)
                .ok_or_else(|| format!("no node `{text}` in the deployment")),
            Some(TypeDef::Tuple(ts)) => {
                let inner = text
                    .strip_prefix('(')
                    .and_then(|t| t.strip_suffix(')'))
                    .ok_or_else(bad)?;
                let parts = split_top(inner);
                if parts.len() != ts.len() {
                    return Err(bad());
                }
                let vs = parts
                    .iter()
                    .zip(&ts)
                    .map(|(p, t)| self.parse(p.trim(), *t))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Tuple(vs.into()))
            }
            Some(TypeDef::Option(t)) => {
                if text == "None" {
                    Ok(Value::Option(None))
                } else {
                    let inner = text
                        .strip_prefix("Some(")
                        .and_then(|x| x.strip_suffix(')'))
                        .ok_or_else(bad)?;
                    Ok(Value::Option(Some(Arc::new(self.parse(inner.trim(), t)?))))
                }
            }
            _ => Err(format!(
                "cannot read a value of type {def:?} in a pattern yet: write `_` for that column"
            )),
        }
    }
}

fn strip_int_suffix(t: &str) -> &str {
    for s in ["u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16", "u8", "i8"] {
        if let Some(x) = t.strip_suffix(s) {
            return x;
        }
    }
    t
}

/// The comma-separated parts of `text` outside brackets and quotes.
fn split_top(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut escaped = false;
    for ch in text.chars() {
        if quoted {
            cur.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                cur.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

/// The bytes a `b"…"` literal (as `escape_ascii` writes it) denotes.
fn unescape(t: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut it = t.bytes();
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match it.next()? {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'0' => out.push(0),
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'x' => {
                let hi = (it.next()? as char).to_digit(16)?;
                let lo = (it.next()? as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn matches(pattern: &[Option<Value>], row: &[Value]) -> bool {
    pattern.len() == row.len() && pattern.iter().zip(row).all(|(p, v)| p.as_ref().is_none_or(|p| p == v))
}

fn drive(cmd: TraceCommand) -> Result<(), String> {
    match cmd {
        TraceCommand::Replay { common, slow } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let h = replay.header().clone();
            let (mut ticks, mut first, mut last, mut failed) = (0u64, None, None, None);
            let mut before: Option<(u64, i64)> = None;
            if slow.is_some() {
                replay.profile(true).map_err(err)?;
            }
            loop {
                let clock = Stopwatch::start();
                let Some(r) = replay.next(false).map_err(err)? else {
                    break;
                };
                if let Some(ms) = slow {
                    let took = clock.nanos() / 1_000_000;
                    let (t, now) = (r.inputs.tick.0, r.inputs.now.0);
                    // The recorded node's gap: from the tick before's start to this one's.
                    if let Some((pt, pnow)) = before {
                        let gap = (now - pnow) / 1_000_000;
                        if gap >= i64::try_from(ms).unwrap_or(i64::MAX) {
                            println!("tick {pt}: the next tick started {gap} ms after it, on the recorded node");
                        }
                    }
                    before = Some((t, now));
                    if took >= ms {
                        println!("tick {t}: replayed in {took} ms; {}", received(&names, &r));
                        print_work(&names, &r.work, 3);
                    }
                }
                ticks += 1;
                first.get_or_insert(r.inputs.tick.0);
                last = Some(r.inputs.tick.0);
                if let Some(f) = r.failed {
                    failed = Some((r.inputs.tick.0, f));
                }
            }
            println!(
                "{} (node {}, incarnation {}): {ticks} ticks replayed, matching the recording",
                common.trace.display(),
                names.node(h.node),
                h.incarnation
            );
            if let (Some(a), Some(b)) = (first, last) {
                println!("  ticks {a} to {b}");
            }
            if replay.torn() {
                println!("  the trace ends inside a record: the node was killed while recording");
            }
            if let Some((t, e)) = failed {
                println!("  tick {t} failed (as recorded): {e}");
            }
            Ok(())
        }
        TraceCommand::Show {
            common,
            rel,
            at,
            pattern,
        } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let rel = names.rel(&rel)?;
            let pat = match &pattern {
                Some(p) => {
                    let (r, values) = names.pattern(p)?;
                    if r != rel {
                        return Err("the pattern names another relation than --rel".into());
                    }
                    Some(values)
                }
                None => None,
            };
            let r = examine(&mut replay, at)?;
            let Some(out) = &r.examined else {
                return Err("the tick was not examined".into());
            };
            println!(
                "tick {} at {}:",
                r.inputs.tick.0,
                names.value(&Value::Instant(r.inputs.now), None)
            );
            for row in out.instance.rows(rel) {
                if pat.as_ref().is_none_or(|p| matches(p, row)) {
                    println!("  {}", names.row(rel, row));
                }
            }
            Ok(())
        }
        TraceCommand::History {
            common,
            from,
            to,
            pattern,
        } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let (rel, pat) = names.pattern(&pattern)?;
            let decl = names
                .program()
                .rels
                .get(rel)
                .ok_or_else(|| format!("{rel:?} is not declared"))?;
            let kind = match (&decl.class, &decl.persistence) {
                (RelClass::Channel(_), _) => Kind::Channel,
                (RelClass::HostOut(_), _) => Kind::Host,
                (RelClass::Event(_), _) => Kind::Event,
                (_, Persistence::None) => Kind::View,
                _ => Kind::Carried,
            };
            if kind == Kind::View {
                replay.observe(vec![rel]);
            }
            let mut held: BTreeSet<Row> = BTreeSet::new();
            while let Some(r) = replay.next(false).map_err(err)? {
                // The window, compared in nanoseconds (as the trace holds times).
                let now = r.inputs.now.0;
                if to.is_some_and(|t| now > seconds_to_nanos(t)) {
                    break;
                }
                let shown = from.is_none_or(|f| now >= seconds_to_nanos(f));
                if !shown && kind != Kind::View {
                    continue;
                }
                let at = format!(
                    "tick {} {}",
                    r.inputs.tick.0,
                    names.value(&Value::Instant(r.inputs.now), None)
                );
                match kind {
                    // A tick's changes are what its rules write for the next tick.
                    Kind::Carried => {
                        let Some(changes) = &r.changes else { continue };
                        for (sign, side) in [("-", &changes.deleted), ("+", &changes.inserted)] {
                            for row in side.get(&rel).into_iter().flatten() {
                                if matches(&pat, row) {
                                    println!("{at} {sign} {} (from the next tick)", names.row(rel, row));
                                }
                            }
                        }
                    }
                    Kind::Host => {
                        for h in r.host.iter().filter(|h| h.rel == rel && matches(&pat, &h.row)) {
                            println!("{at} -> {}", names.row(rel, &h.row));
                        }
                    }
                    Kind::Channel => {
                        for d in r
                            .inputs
                            .delivered
                            .iter()
                            .filter(|d| d.rel == rel && matches(&pat, &d.row))
                        {
                            println!("{at} <- {} from {}", names.row(rel, &d.row), names.node(d.from));
                        }
                        for s in r.sent.iter().filter(|s| s.rel == rel && matches(&pat, &s.row)) {
                            println!("{at} -> {}", names.row(rel, &s.row));
                        }
                    }
                    Kind::Event => {
                        for (_, row) in r
                            .inputs
                            .events
                            .iter()
                            .filter(|(e, row)| *e == rel && matches(&pat, row))
                        {
                            println!("{at} ! {}", names.row(rel, row));
                        }
                    }
                    // A view holds at a tick: report when its matching rows start and stop holding.
                    Kind::View => {
                        let Some(rows) = r.observed.get(&rel) else { continue };
                        let now: BTreeSet<Row> = rows.iter().filter(|row| matches(&pat, row)).cloned().collect();
                        if shown {
                            for row in held.difference(&now) {
                                println!("{at} - {} (no longer holds)", names.row(rel, row));
                            }
                            for row in now.difference(&held) {
                                println!("{at} + {} (holds)", names.row(rel, row));
                            }
                        }
                        held = now;
                    }
                }
            }
            Ok(())
        }
        TraceCommand::Profile {
            common,
            at: None,
            from,
            to,
            top,
        } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            replay.profile(true).map_err(err)?;
            let mut work: BTreeMap<RuleId, RuleWork> = BTreeMap::new();
            let mut fns: BTreeMap<FnId, FnWork> = BTreeMap::new();
            let mut ticks = 0u64;
            while let Some(r) = replay.next(false).map_err(err)? {
                let now = r.inputs.now.0;
                if to.is_some_and(|t| now > seconds_to_nanos(t)) {
                    break;
                }
                if from.is_some_and(|f| now < seconds_to_nanos(f)) {
                    continue;
                }
                ticks += 1;
                for (rule, n) in r.work {
                    let w = work.entry(rule).or_default();
                    w.rows += n.rows;
                    w.steps += n.steps;
                    w.writes += n.writes;
                }
                for (f, n) in r.fn_work {
                    let w = fns.entry(f).or_default();
                    w.calls += n.calls;
                    w.steps += n.steps;
                    w.self_steps += n.self_steps;
                }
            }
            println!("{ticks} ticks");
            print_work(&names, &work, top);
            print_fn_work(&names, &fns, top);
            Ok(())
        }
        TraceCommand::Profile {
            common,
            at: Some(at),
            top,
            ..
        } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            loop {
                match replay.peek_tick().map_err(err)? {
                    None => return Err(format!("tick {at} is not in the trace")),
                    Some(t) if t.0 > at => {
                        return Err(format!("tick {at} is not in the trace (it continues at {})", t.0));
                    }
                    Some(t) if t.0 == at => break,
                    Some(_) => {
                        replay.next(false).map_err(err)?;
                    }
                }
            }
            replay.profile(true).map_err(err)?;
            let r = replay.next(false).map_err(err)?.ok_or("the trace ended")?;
            println!(
                "tick {at} at {}: {}",
                names.value(&Value::Instant(r.inputs.now), None),
                received(&names, &r)
            );
            print_work(&names, &r.work, top);
            print_fn_work(&names, &r.fn_work, top);
            Ok(())
        }
        TraceCommand::Why { common, at, pattern } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let (rel, pat) = names.pattern(&pattern)?;
            let r = examine(&mut replay, at)?;
            let Some(out) = &r.examined else {
                return Err("the tick was not examined".into());
            };
            let program = names.program();
            let mut any = false;
            for row in out.instance.rows(rel).filter(|row| matches(&pat, row)) {
                any = true;
                println!("{} at tick {at}:", names.row(rel, row));
                let firings: Vec<_> = out
                    .firings
                    .iter()
                    .filter(|f| program.rules.get(f.rule).is_some_and(|x| x.head.rel == rel) && *f.head == **row)
                    .collect();
                if firings.is_empty() {
                    let base = r.inputs.events.iter().any(|(e, x)| *e == rel && x == row)
                        || r.inputs.delivered.iter().any(|d| d.rel == rel && d.row == *row)
                        || r.inputs.ingress.iter().any(|g| g.rel == rel && g.row == *row);
                    if base {
                        println!("  an input of the tick (an event, a delivered message or a client's request)");
                    } else {
                        println!(
                            "  carried from an earlier tick (an inductive head, a table row): `blossom trace history` \
                             finds the tick it was written"
                        );
                    }
                }
                for f in firings {
                    let Some(rule) = program.rules.get(f.rule) else {
                        continue;
                    };
                    println!("  by {} ({:?})", rule.label, rule.kind);
                    println!("    {}", rule_text(program, rule));
                    for read in &f.reads {
                        println!("    read {}", names.row(read.rel, &read.row));
                    }
                    for neg in &f.negations {
                        let tys = names.col_types(neg.rel);
                        let cols: Vec<String> = neg
                            .pattern
                            .iter()
                            .enumerate()
                            .map(|(i, v)| v.as_ref().map_or("_".into(), |v| names.value(v, tys.get(i).copied())))
                            .collect();
                        println!("    absent {}({})", names.rel_name(neg.rel), cols.join(", "));
                    }
                }
            }
            if !any {
                println!(
                    "no row of {} matches at tick {at}: try `blossom trace whynot`",
                    names.rel_name(rel)
                );
            }
            Ok(())
        }
        TraceCommand::Whynot {
            common,
            at,
            pattern,
            samples,
        } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let (rel, pat) = names.pattern(&pattern)?;
            let r = examine(&mut replay, at)?;
            let Some(out) = &r.examined else {
                return Err("the tick was not examined".into());
            };
            let held: Vec<_> = out.instance.rows(rel).filter(|row| matches(&pat, row)).collect();
            if !held.is_empty() {
                println!(
                    "{} rows match at tick {at} (see `blossom trace why`); the rules:",
                    held.len()
                );
            }
            let program = names.program();
            let explained = replay.why_not(&r, rel, &pat, samples).map_err(err)?;
            if explained.is_empty() {
                println!(
                    "no rule of this node derives {}: it is an input (an event, a message, a client's request)",
                    names.rel_name(rel)
                );
            }
            for w in explained {
                let Some(rule) = program.rules.get(w.rule) else {
                    continue;
                };
                println!("{} ({:?}):", rule.label, rule.kind);
                println!("  {}", rule_text(program, rule));
                if let Some(col) = w.head_differs {
                    println!("  cannot derive it: head column {col} is fixed to another value");
                    continue;
                }
                if let Some(e) = &w.error {
                    println!("  an expression failed on the way: {e}");
                }
                if w.complete > 0 {
                    let when = match rule.kind {
                        blossom_ir::core::RuleKind::Deductive => "at this tick",
                        blossom_ir::core::RuleKind::Inductive => "for the next tick",
                        blossom_ir::core::RuleKind::Async => "as a message",
                    };
                    println!(
                        "  its body holds ({} valuations): it derives such a tuple {when}",
                        w.complete
                    );
                } else if let Some(lit) = w.failed.and_then(|i| rule.body.lits.get(i)) {
                    println!(
                        "  no valuation passes `{}` ({} of {} steps passed)",
                        literal_text(program, rule, lit),
                        w.passed,
                        w.steps
                    );
                }
                for p in &w.partial {
                    let binds: Vec<String> = p
                        .iter()
                        .map(|(v, x)| {
                            let ty = rule.body.vars.get(*v).map(|d| d.ty);
                            format!("{} = {}", var_text(rule, *v), names.value(x, ty))
                        })
                        .collect();
                    println!(
                        "    with {}",
                        if binds.is_empty() {
                            "nothing bound".into()
                        } else {
                            binds.join(", ")
                        }
                    );
                }
            }
            Ok(())
        }
    }
}

/// Replays up to tick `at` and examines it (runs it again with the oracle, capturing its firings).
fn examine(replay: &mut Replay<BufReader<File>>, at: u64) -> Result<Replayed, String> {
    loop {
        match replay.peek_tick().map_err(err)? {
            None => return Err(format!("the trace ends before tick {at}")),
            Some(t) if t.0 > at => return Err(format!("tick {at} is not in the trace (it continues at {})", t.0)),
            Some(t) if t.0 == at => {
                return replay
                    .next(true)
                    .map_err(err)?
                    .ok_or_else(|| format!("the trace ends before tick {at}"));
            }
            Some(_) => {
                replay.next(false).map_err(err)?;
            }
        }
    }
}

fn err(e: ReplayError) -> String {
    e.to_string()
}
