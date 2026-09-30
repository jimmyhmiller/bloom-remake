//! Rendering LDFI results (ARCHITECTURE §8.7): the verdict, each counterexample's fault set, the lineage of the
//! `post` tuple it violated, and its message timeline (a textual space-time diagram).

use std::fmt::Write;

use blossom_artifact::sim::SimArtifact;
use blossom_prov::{Firing, GoalKey, Names, ProvGraph, Space};
use blossom_sim::{Fate, FaultSchedule, SyncRun};
use blossom_value::{Value, time::NodeId};

use crate::driver::{Counterexample, LdfiReport, Method, Verdict};
use crate::faults::labels;

/// Names for a `.ded` program's goals.
pub struct DedNames<'a> {
    pub artifact: &'a SimArtifact,
}

impl DedNames<'_> {
    /// A value in source syntax, without its type (enum variants by number).
    pub fn value(&self, v: &Value) -> String {
        self.typed(v, None, None)
    }

    /// A value of type `ty` in `program`'s type table, in source syntax: enum variants and struct fields by name,
    /// lattice values by their elements (`⊥` for bottom), durations and instants in seconds.
    pub fn typed(
        &self,
        v: &Value,
        ty: Option<blossom_base::TypeId>,
        program: Option<&blossom_ir::core::Program>,
    ) -> String {
        use blossom_value::TypeDef;
        let def = ty.and_then(|t| program.and_then(|p| p.types.get(t)));
        let list = |vs: &mut dyn Iterator<Item = String>| vs.collect::<Vec<_>>().join(", ");
        match (v, def) {
            (Value::Node(n), _) => self.node(*n),
            (Value::Str(s), _) => format!("{s:?}"),
            (Value::Bool(b), _) => b.to_string(),
            (Value::Unit, _) => "()".to_owned(),
            (Value::Int(i), _) => format!("{i:?}")
                .split_once('(')
                .and_then(|(_, rest)| rest.strip_suffix(')'))
                .map_or_else(|| format!("{i:?}"), str::to_owned),
            (Value::Duration(d), _) => seconds(d.as_nanos()),
            (Value::Instant(t), _) => format!("@{}", seconds(t.0)),
            (Value::Session(s), _) => format!("session {}", s.0),
            (Value::Conn(c), _) => format!("conn#{}", c.0),
            // Bytes as a byte string: printable ASCII as is, the rest escaped.
            (Value::Bytes(b), _) => format!("b\"{}\"", b.escape_ascii()),
            (Value::Principal(p), _) => format!("principal {p:?}"),
            (Value::Option(None), _) => "None".to_owned(),
            (Value::Option(Some(x)), Some(TypeDef::Option(t))) => format!("Some({})", self.typed(x, Some(*t), program)),
            (Value::Option(Some(x)), _) => format!("Some({})", self.typed(x, None, program)),
            (Value::Tuple(xs), Some(TypeDef::Tuple(ts))) => format!(
                "({})",
                list(&mut xs.iter().zip(ts).map(|(x, t)| self.typed(x, Some(*t), program)))
            ),
            (Value::Tuple(xs), _) => format!("({})", list(&mut xs.iter().map(|x| self.typed(x, None, program)))),
            (Value::Enum { variant, fields }, Some(TypeDef::Enum(e))) => {
                let var = e.variants.iter().find(|x| x.number == *variant);
                let name = var.map_or_else(|| format!("#{variant}"), |x| x.name.to_string());
                if fields.is_empty() {
                    name
                } else {
                    let tys: Vec<Option<blossom_base::TypeId>> = var
                        .map(|x| x.payload.iter().map(|f| Some(f.ty)).collect())
                        .unwrap_or_default();
                    format!(
                        "{name}({})",
                        list(&mut fields.iter().enumerate().map(|(i, x)| self.typed(
                            x,
                            tys.get(i).copied().flatten(),
                            program
                        )))
                    )
                }
            }
            (Value::Struct(xs), Some(TypeDef::Struct(s))) => format!(
                "{} {{ {} }}",
                s.name,
                list(&mut xs.iter().zip(&s.fields).map(|(x, f)| format!(
                    "{}: {}",
                    f.name,
                    self.typed(x, Some(f.ty), program)
                )))
            ),
            (Value::Vec(xs), Some(TypeDef::Vec(t))) => {
                format!("[{}]", list(&mut xs.iter().map(|x| self.typed(x, Some(*t), program))))
            }
            (Value::Set(xs), Some(TypeDef::Set(t))) => {
                format!(
                    "set[{}]",
                    list(&mut xs.iter().map(|x| self.typed(x, Some(*t), program)))
                )
            }
            (Value::Map(m), Some(TypeDef::Map(k, t))) => format!(
                "map[{}]",
                list(&mut m.iter().map(|(a, b)| format!(
                    "{} => {}",
                    self.typed(a, Some(*k), program),
                    self.typed(b, Some(*t), program)
                )))
            ),
            (Value::Lattice(l), _) => {
                let ctor = match def {
                    Some(TypeDef::Lattice(id)) => program.and_then(|p| p.lattices.get(*id)).map(|d| d.ctor.clone()),
                    _ => None,
                };
                self.lattice(l, ctor.as_ref(), program)
            }
            (other, _) => format!("{other:?}"),
        }
    }

    fn lattice(
        &self,
        l: &blossom_value::value::LatValue,
        ctor: Option<&blossom_ir::core::LatticeCtor>,
        program: Option<&blossom_ir::core::Program>,
    ) -> String {
        use blossom_ir::core::LatticeCtor as C;
        use blossom_value::value::LatValue as L;
        let elem = match ctor {
            Some(C::Max(t) | C::Min(t) | C::Point(t) | C::Set(t) | C::PSet(t)) => Some(*t),
            _ => None,
        };
        match l {
            L::Bottom => "⊥".to_owned(),
            L::Top => "⊤".to_owned(),
            L::Bool(b) => b.to_string(),
            L::Elem(x) => self.typed(x, elem, program),
            L::Set(xs) => format!(
                "{{{}}}",
                xs.iter()
                    .map(|x| self.typed(x, elem, program))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            L::Map(m) => {
                let (key, inner) = match ctor {
                    Some(C::Map(k, inner)) => (
                        Some(*k),
                        program.and_then(|p| p.lattices.get(*inner)).map(|d| d.ctor.clone()),
                    ),
                    _ => (None, None),
                };
                format!(
                    "{{{}}}",
                    m.iter()
                        .map(|(k, v)| format!(
                            "{}: {}",
                            self.typed(k, key, program),
                            self.lattice(v, inner.as_ref(), program)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            other => format!("{other:?}"),
        }
    }

    /// A row of protocol relation `rel`, column by column.
    pub fn row(&self, rel: blossom_base::RelId, row: &[Value]) -> Vec<String> {
        let program = self.artifact.protocol.get();
        let cols = program.rels.get(rel).map(|r| &r.schema.cols);
        row.iter()
            .enumerate()
            .map(|(i, v)| self.typed(v, cols.and_then(|c| c.get(i)).map(|c| c.ty), Some(program)))
            .collect()
    }

    fn rel_name(&self, space: Space, rel: blossom_base::RelId) -> String {
        let program = match space {
            Space::Protocol => Some(self.artifact.protocol.get()),
            Space::Spec => self.artifact.spec.as_ref().map(|s| s.program.get()),
        };
        program
            .and_then(|p| p.rels.get(rel))
            .map_or_else(|| format!("{rel:?}"), |r| r.name.to_string())
    }

    /// A Molly-style tuple: `rel(node, v…)@t`; spec tuples keep their own first column.
    pub fn tuple(&self, key: &GoalKey) -> String {
        let mut cols: Vec<String> = Vec::with_capacity(key.row.len() + 1);
        let name = self.rel_name(key.space, key.rel);
        let channel = name.ends_with("$async");
        if let (Some(n), false) = (key.node, channel) {
            cols.push(self.node(n));
        }
        match key.space {
            Space::Protocol => cols.extend(self.row(key.rel, &key.row)),
            Space::Spec => {
                let program = self.artifact.spec.as_ref().map(|s| s.program.get());
                let tys = program.and_then(|p| p.rels.get(key.rel)).map(|r| &r.schema.cols);
                cols.extend(
                    key.row
                        .iter()
                        .enumerate()
                        .map(|(i, v)| self.typed(v, tys.and_then(|c| c.get(i)).map(|c| c.ty), program)),
                );
            }
        }
        match name.strip_suffix("$async") {
            Some(base) => format!("{base}({})@{} arrives", cols.join(", "), key.tick.0),
            None => format!("{name}({})@{}", cols.join(", "), key.tick.0),
        }
    }
}

impl Names for DedNames<'_> {
    fn goal(&self, key: &GoalKey) -> String {
        self.tuple(key)
    }

    fn rule(&self, firing: &Firing) -> String {
        let program = match firing.space {
            Space::Protocol => Some(self.artifact.protocol.get()),
            Space::Spec => self.artifact.spec.as_ref().map(|s| s.program.get()),
        };
        let label = program
            .and_then(|p| p.rules.get(firing.rule))
            .map_or_else(|| format!("{:?}", firing.rule), |r| r.label.text.to_string());
        match firing.node {
            Some(n) => format!("rule {label} at {} tick {}", self.node(n), firing.tick.0),
            None => format!("spec rule {label}"),
        }
    }

    fn node(&self, node: NodeId) -> String {
        self.artifact
            .node_name(node)
            .map_or_else(|| format!("node#{}", node.0), |s| s.to_string())
    }

    fn logical(&self, logical: u32) -> String {
        self.artifact
            .rels
            .get(logical as usize)
            .map_or_else(|| format!("#{logical}"), |r| r.name.to_string())
    }
}

/// The fault set in the corpus's notation: `{C(a,2), O(a,b,1)}`.
pub fn fault_labels(artifact: &SimArtifact, faults: &FaultSchedule) -> Vec<String> {
    let names = DedNames { artifact };
    labels(faults, &|n| names.node(n))
}

/// A human-readable report.
pub fn render(artifact: &SimArtifact, report: &LdfiReport) -> String {
    let names = DedNames { artifact };
    let mut out = String::new();
    match (report.verdict, report.method) {
        (Verdict::NoCounterexample, Method::Lineage) => {
            let _ = writeln!(
                out,
                "no counterexample: every fault set the lineage suggested left the outcome correct ({} run(s))",
                report.runs
            );
        }
        (Verdict::Counterexample, Method::Lineage) => {
            let _ = writeln!(out, "counterexample found after {} run(s)", report.runs);
        }
        (
            verdict,
            Method::Exhaustive {
                states,
                schedules,
                after,
            },
        ) => {
            let what = match verdict {
                Verdict::NoCounterexample => "no counterexample",
                Verdict::Counterexample => "counterexample found",
            };
            let why = match after {
                crate::driver::Fallback::RunBudget => {
                    format!("after the lineage-driven search spent its {} run(s)", report.runs)
                }
                crate::driver::Fallback::IncompleteLineage => {
                    "because the lineage-driven search's lineage was incomplete".to_owned()
                }
            };
            let _ = writeln!(
                out,
                "{what} by exhaustive certification: {states} distinct state(s) over {schedules} crash schedule(s), {why}"
            );
        }
    }
    let _ = writeln!(
        out,
        "failure-free run: {} post tuple(s), {} pre tuple(s)",
        report.failure_free.post.len(),
        report.failure_free.pre.len()
    );
    for ce in &report.counterexamples {
        render_counterexample(&mut out, artifact, &names, report, ce);
    }
    out
}

fn render_counterexample(
    out: &mut String,
    artifact: &SimArtifact,
    names: &DedNames<'_>,
    report: &LdfiReport,
    ce: &Counterexample,
) {
    let _ = writeln!(out, "\nfaults: {{{}}}", fault_labels(artifact, &ce.faults).join(", "));
    let Some(spec) = &artifact.spec else { return };
    for row in &ce.violated {
        let key = GoalKey {
            space: Space::Spec,
            rel: spec.post,
            node: None,
            tick: report.failure_free.eot,
            row: row.clone(),
        };
        let _ = writeln!(
            out,
            "violated: {} holds in `pre` but is missing from `post`",
            names.tuple(&key)
        );
        if let Some(goal) = report.failure_free_graph.find(&key) {
            let _ = writeln!(out, "its lineage in the failure-free run:");
            for line in report.failure_free_graph.render(goal, names).lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
    let _ = writeln!(out, "the faulty run's messages:");
    out.push_str(&timeline(names, &ce.run));
}

/// The run's messages and crashes, tick by tick.
pub fn timeline(names: &DedNames<'_>, run: &SyncRun) -> String {
    let mut out = String::new();
    for (node, at) in &run.faults.crashes {
        let _ = writeln!(out, "  {} crashes at tick {}", names.node(*node), at.0);
    }
    let program = names.artifact.protocol.get();
    let mut tick = None;
    for m in &run.messages {
        if m.from == m.to {
            continue;
        }
        if tick != Some(m.send) {
            let _ = writeln!(out, "  tick {}:", m.send.0);
            tick = Some(m.send);
        }
        let rel = program
            .rels
            .get(m.rel)
            .map_or_else(|| format!("{:?}", m.rel), |r| r.name.to_string());
        let rel = rel.strip_suffix("$async").unwrap_or(&rel).to_owned();
        // Column 0 is the destination, shown by the arrow.
        let args: Vec<String> = names.row(m.rel, &m.row).into_iter().skip(1).collect();
        let fate = match m.fate {
            Fate::Delivered(t) => format!("delivered at {}", t.0),
            Fate::Lost => "LOST".to_owned(),
            Fate::AfterEnd => "after EOT".to_owned(),
        };
        let _ = writeln!(
            out,
            "    {} -> {}  {rel}({})  {fate}",
            names.node(m.from),
            names.node(m.to),
            args.join(", ")
        );
    }
    out
}

/// The lineage of every `post` tuple of a run, for `blossom ldfi --lineage`.
pub fn post_lineage(artifact: &SimArtifact, graph: &ProvGraph, report: &LdfiReport) -> String {
    let names = DedNames { artifact };
    let mut out = String::new();
    let Some(spec) = &artifact.spec else { return out };
    for row in &report.failure_free.post {
        let key = GoalKey {
            space: Space::Spec,
            rel: spec.post,
            node: None,
            tick: report.failure_free.eot,
            row: row.clone(),
        };
        if let Some(goal) = graph.find(&key) {
            out.push_str(&graph.render(goal, &names));
        }
    }
    out
}

/// Nanoseconds as seconds: `1s`, `1.5s`, `-2s`.
fn seconds(nanos: i64) -> String {
    let whole = nanos / 1_000_000_000;
    let frac = (nanos % 1_000_000_000).unsigned_abs();
    if frac == 0 {
        format!("{whole}s")
    } else {
        let digits = format!("{frac:09}");
        format!("{whole}.{}s", digits.trim_end_matches('0'))
    }
}
