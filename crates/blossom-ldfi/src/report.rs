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

    /// A value of type `ty` in `program`'s type table, in source syntax ([`blossom_ir::printer::value_text`]).
    pub fn typed(
        &self,
        v: &Value,
        ty: Option<blossom_base::TypeId>,
        program: Option<&blossom_ir::core::Program>,
    ) -> String {
        blossom_ir::printer::value_text(program, v, ty, &|n| self.node(n))
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
