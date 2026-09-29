//! `monotone` assertions (ANA-020, LANGUAGE §13.5): a region declared monotone contains no point of order. A point
//! of order is a negative edge of §13.2: a negated read, an aggregate, or a lattice read that reaches a use
//! antitone or exact (SEM-102, `blossom_ir::polarity`). A failure is BLS0702, which lists every point of order.
//!
//! The region of a `monotone view` is its deductive rules and, transitively, the rules of the generated relations
//! they read (the helper relations of its own lowering: `not { … }`, `forall`, blocks).

use std::collections::BTreeSet;

use blossom_base::{Diagnostic, Diagnostics, RelId, Span, code};
use blossom_ir::core::{HeadArg, Literal, Origin, Program, RuleKind};

/// BLS0702 for every `monotone` view whose region has a point of order.
pub fn check(p: &Program) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for (id, rel) in p.rels.iter_enumerated() {
        if !rel.attrs.monotone {
            continue;
        }
        let points = points_of_order(p, id);
        if points.is_empty() {
            continue;
        }
        let mut d = Diagnostic::new(
            code!("BLS0702"),
            format!(
                "`{}` is declared `monotone`, but its rules contain {} point(s) of order",
                rel.name,
                points.len()
            ),
        )
        .with_primary(rel.span);
        for (span, what) in points {
            d = d.with_label(span, what);
        }
        diags.push(d);
    }
    diags
}

/// The points of order of the region of `root`: each with its span and a description.
fn points_of_order(p: &Program, root: RelId) -> Vec<(Span, String)> {
    let mut region: BTreeSet<RelId> = BTreeSet::from([root]);
    let mut todo = vec![root];
    while let Some(r) = todo.pop() {
        for rule in p
            .rules
            .iter()
            .filter(|x| x.kind == RuleKind::Deductive && x.head.rel == r)
        {
            for lit in &rule.body.lits {
                let read = match lit {
                    Literal::Pos(a) | Literal::Neg(a) => Some(a.rel),
                    Literal::Lookup { rel, .. } => Some(*rel),
                    _ => None,
                };
                if let Some(read) = read
                    && p.rels
                        .get(read)
                        .is_some_and(|x| matches!(x.origin, Origin::Generated { .. }))
                    && region.insert(read)
                {
                    todo.push(read);
                }
            }
        }
    }
    let name = |r: RelId| {
        p.rels
            .get(r)
            .map_or_else(|| format!("#{}", r.index()), |x| x.name.to_string())
    };
    let mut out = Vec::new();
    for rule in p.rules.iter() {
        if rule.kind != RuleKind::Deductive || !region.contains(&rule.head.rel) {
            continue;
        }
        if rule.head.args.iter().any(|a| matches!(a, HeadArg::Agg(_))) {
            out.push((rule.span, format!("`{}` aggregates", rule.label.text)));
        }
        let exact = blossom_ir::polarity::non_monotone_reads(p, rule);
        for (i, lit) in rule.body.lits.iter().enumerate() {
            match lit {
                Literal::Neg(a) => out.push((
                    a.span,
                    format!("`{}` reads `{}` negatively", rule.label.text, name(a.rel)),
                )),
                Literal::Pos(a) if exact.contains(&i) => out.push((
                    a.span,
                    format!("`{}` reads the lattice `{}` exactly", rule.label.text, name(a.rel)),
                )),
                Literal::Lookup { rel, .. } if exact.contains(&i) => out.push((
                    rule.span,
                    format!("`{}` reads the lattice `{}` exactly", rule.label.text, name(*rel)),
                )),
                _ => {}
            }
        }
    }
    out
}
