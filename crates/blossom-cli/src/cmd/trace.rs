//! `blossom trace`: replay and question a node's recorded trace (`blossom run --record`, ARCHITECTURE §6.4).
//!
//! Every subcommand replays the trace with the program of the deployment it was recorded under (`--deploy`), checking
//! each tick against the recording; a divergence stops it with the tick.
//!
//! - `replay`: replay the whole trace and summarize it.
//! - `show`: a relation's rows at a tick (views and events included).
//! - `history`: every tick a relation's carried rows matching a pattern were added or removed.
//! - `why`: the rule firings that derived the rows matching a pattern at a tick.
//! - `whynot`: for each rule that could derive a tuple matching a pattern, how far its body got at a tick, and the
//!   first literal no valuation passed (ask again about that literal's relation to go deeper).
//!
//! A pattern is `rel(v, …)`: one value per column of the relation in its IR form (a channel's destination is column
//! 0), `_` for any value; values are written as `show` prints them (`3`, `"name"`, `b"bytes"`, `(a, b)`, `None`,
//! `Some(x)`, `true`, a node by name).

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, TypeId};
use blossom_ir::core::Program;
use blossom_ir::printer::{literal_text, rule_text, value_text, var_text};
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
    /// Every tick the carried rows of a relation matching a pattern were added or removed.
    History {
        #[command(flatten)]
        common: Common,
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

/// What every subcommand takes.
#[derive(Debug, clap::Args)]
pub struct Common {
    /// The trace (`<node>-<incarnation>.blstrace`).
    pub trace: PathBuf,
    /// The deployment spec it was recorded under (`deploy.toml`): its program is compiled and replayed.
    #[arg(long = "deploy", value_name = "FILE")]
    pub deploy: PathBuf,
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
    let (_spec, artifact) = load(&common.deploy).map_err(|_| "the deployment did not load".to_owned())?;
    let file = File::open(&common.trace).map_err(|e| format!("{}: {e}", common.trace.display()))?;
    let externs = crate::common::std_externs().map_err(|e| e.to_string())?;
    let replay = Replay::open(&artifact, BufReader::new(file), externs).map_err(|e| e.to_string())?;
    Ok((artifact, replay))
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
        TraceCommand::Replay { common } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let h = replay.header().clone();
            let (mut ticks, mut first, mut last, mut failed) = (0u64, None, None, None);
            while let Some(r) = replay.next(false).map_err(err)? {
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
        TraceCommand::History { common, pattern } => {
            let (artifact, mut replay) = open(&common)?;
            let names = Names { artifact: &artifact };
            let (rel, pat) = names.pattern(&pattern)?;
            while let Some(r) = replay.next(false).map_err(err)? {
                let Some(changes) = &r.changes else { continue };
                // A tick's changes are what its rules write for the next tick.
                for (sign, side) in [("-", &changes.deleted), ("+", &changes.inserted)] {
                    for row in side.get(&rel).into_iter().flatten() {
                        if matches(&pat, row) {
                            let t = r.inputs.tick.0;
                            println!("tick {t} {sign} {} (from tick {})", names.row(rel, row), t + 1);
                        }
                    }
                }
            }
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
