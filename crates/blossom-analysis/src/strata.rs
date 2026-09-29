//! Temporal stratification (SEM-020, SEM-022, ANA-002; LANGUAGE §13.3): the deductive rules of a program must
//! stratify. A relation depends on every relation a deductive rule deriving it reads; the dependency is a point of
//! order when the read is negated, the rule aggregates (CR-09), or a lattice read reaches a use non-monotonically
//! (SEM-102, `blossom_ir::polarity`). A point of order inside a strongly connected
//! component is a negation or aggregation through same-tick recursion: the program is rejected with BLS0502, whose
//! witness is the cycle.
//!
//! Generated relations are provenance-transparent (LANGUAGE §4.1), so the witness names the surface constructs that
//! produced them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use blossom_base::graph::{AdjacencyList, tarjan_scc};
use blossom_base::{Diagnostic, Diagnostics, InternalError, RelId, Span, code, internal_error};
use blossom_ir::core::{HeadArg, Literal, Origin, Program, RuleKind};

/// One dependency: `head` reads `body` in a deductive rule, at `span`; `strict` for a point of order.
struct Edge {
    head: usize,
    body: usize,
    strict: bool,
    span: Span,
}

/// BLS0502 for every same-tick cycle through a point of order (one diagnostic per strongly connected component).
pub fn check(p: &Program) -> Result<Diagnostics, InternalError> {
    let n = p.rels.len();
    let mut graph = AdjacencyList::new(n);
    let mut edges = Vec::new();
    for rule in p.rules.iter() {
        if rule.kind != RuleKind::Deductive {
            continue;
        }
        let head = rule.head.rel.index();
        let agg = rule.head.args.iter().any(|a| matches!(a, HeadArg::Agg(_)));
        let exact = blossom_ir::polarity::non_monotone_reads(p, rule);
        for (i, lit) in rule.body.lits.iter().enumerate() {
            let (rel, negated, span) = match lit {
                Literal::Pos(a) => (a.rel, false, rule.span),
                Literal::Neg(a) => (a.rel, true, a.span),
                Literal::Lookup { rel, .. } => (*rel, false, rule.span),
                _ => continue,
            };
            let body = rel.index();
            graph
                .add_edge(head, body)
                .map_err(|e| internal_error!("dependency graph: {e}"))?;
            edges.push(Edge {
                head,
                body,
                strict: negated || agg || exact.contains(&i),
                span,
            });
        }
    }
    let sccs = tarjan_scc(&graph).map_err(|e| internal_error!("dependency graph: {e}"))?;
    let mut diags = Diagnostics::new();
    let mut reported = BTreeSet::new();
    for e in edges.iter().filter(|e| e.strict) {
        let (Some(ch), Some(cb)) = (sccs.component_of.get(e.head), sccs.component_of.get(e.body)) else {
            continue;
        };
        if ch != cb || !reported.insert(*ch) {
            continue;
        }
        // The cycle: head → body → … → head, following dependencies inside the component.
        let path = path_in(&edges, &sccs.component_of, *ch, e.body, e.head);
        let mut names = vec![surface_name(p, e.head)];
        names.extend(path.iter().map(|&r| surface_name(p, r)));
        let mut d = Diagnostic::new(
            code!("BLS0502"),
            format!(
                "`{}` depends through negation or aggregation on itself within one tick: {}",
                surface_name(p, e.head),
                names.join(" -> ")
            ),
        )
        .with_primary(e.span);
        for w in path.windows(2) {
            if let (Some(&a), Some(&b)) = (w.first(), w.get(1))
                && let Some(edge) = edges.iter().find(|x| x.head == a && x.body == b)
            {
                d = d.with_label(
                    edge.span,
                    format!("`{}` reads `{}`", surface_name(p, a), surface_name(p, b)),
                );
            }
        }
        diags.push(d.with_note("a negated read of a relation on the same tick must see its complete contents; write `next` to read it at t+1"));
    }
    Ok(diags)
}

/// A shortest dependency path from `from` to `to` inside component `comp` (both ends included, `from` first).
fn path_in(edges: &[Edge], component_of: &[usize], comp: usize, from: usize, to: usize) -> Vec<usize> {
    let mut prev: BTreeMap<usize, usize> = BTreeMap::new();
    let mut queue = VecDeque::from([from]);
    let mut seen = BTreeSet::from([from]);
    while let Some(r) = queue.pop_front() {
        if r == to {
            break;
        }
        for e in edges.iter().filter(|e| e.head == r) {
            if component_of.get(e.body) == Some(&comp) && seen.insert(e.body) {
                prev.insert(e.body, r);
                queue.push_back(e.body);
            }
        }
    }
    let mut path = vec![to];
    let mut cur = to;
    while cur != from {
        match prev.get(&cur) {
            Some(&p) => {
                path.push(p);
                cur = p;
            }
            None => break,
        }
    }
    path.reverse();
    path
}

/// A relation's name for users: its own name, or for a generated relation the surface construct's label.
fn surface_name(p: &Program, rel: usize) -> String {
    let Some(r) = u32::try_from(rel).ok().and_then(|i| p.rels.get(RelId::from_raw(i))) else {
        return format!("#{rel}");
    };
    match r.origin {
        Origin::User(_) => r.name.to_string(),
        Origin::Generated { construct } => p
            .constructs
            .get(construct)
            .and_then(|c| c.surface.label)
            .map_or_else(|| r.name.to_string(), |l| l.to_string()),
    }
}
