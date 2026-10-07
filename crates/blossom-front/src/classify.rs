//! Event/standing classification and handler rules (ARCHITECTURE §13.7, LANGUAGE §8.5, §8.6).
//!
//! A relation is an *event* relation if it holds only in ticks caused by an event: channels, loopbacks, a program's
//! inputs, timers and `boot()`; a view whose every alternative has a positive event literal; a scratch, output or
//! instance input that has writers, every one of them event-driven. Everything else is *standing*. The
//! classification is the greatest fixpoint: every candidate starts as an event and loses the status when a
//! definition makes it standing.
//!
//! Then: an `on` handler needs a positive event literal in its header (BLS0504), a `while` handler with one is
//! warned about (BLS0505), and a handler that `emit`s a relation it tests negatively in its header or a block
//! condition is rejected (BLS0506) unless the statement carries `#[allow(self_negation)]`.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{Diagnostic, Diagnostics, InternalError, code};

use crate::ast::{Trigger, Verb};
use crate::hir::*;

/// Classifies the program's relations and checks the handler rules.
pub fn check(hir: &Hir, diags: &mut Diagnostics) -> Result<(), InternalError> {
    let events = classify(hir)?;
    for h in &hir.handlers {
        if h.kind != HandlerKind::Plain {
            continue;
        }
        let has_event = has_event_literal(&h.header, &events);
        match (h.trigger, has_event) {
            (Trigger::On, false) => {
                let mut standing = Vec::new();
                for r in positive_rels(&h.header) {
                    standing.push(format!(
                        "`{}` is standing ({})",
                        hir.rel(r)?.name,
                        why_standing(hir, r)?
                    ));
                }
                let mut d = Diagnostic::new(
                    code!("BLS0504"),
                    "this `on` handler has no event in its header, so it would fire at every tick its header holds; \
                     write `while` if that is meant",
                )
                .with_primary(h.span);
                for s in standing {
                    d = d.with_note(s);
                }
                diags.push(d);
            }
            (Trigger::While, true) => diags.push(
                Diagnostic::new(
                    code!("BLS0505"),
                    "this `while` handler's header has an event literal, so it fires only in event ticks; write `on`",
                )
                .with_primary(h.span),
            ),
            _ => {}
        }
        self_negation(hir, h, diags)?;
    }
    Ok(())
}

/// The event relations.
pub(crate) fn classify(hir: &Hir) -> Result<BTreeSet<HRelId>, InternalError> {
    // Writers of scratch-like relations: whether each writing statement is event-driven, filled per iteration.
    let mut events: BTreeSet<HRelId> = BTreeSet::new();
    let mut candidates: BTreeSet<HRelId> = BTreeSet::new();
    for (i, r) in hir.rels.iter().enumerate() {
        let id = HRelId(i as u32);
        match r.kind {
            HRelKind::Channel(_)
            | HRelKind::Input { root: true }
            | HRelKind::Timer { .. }
            | HRelKind::Boot
            | HRelKind::Recovered
            | HRelKind::Link { .. }
            | HRelKind::Stream(crate::hir::HStreamRel::Event(_)) => {
                events.insert(id);
            }
            HRelKind::View | HRelKind::Scratch | HRelKind::Output { .. } | HRelKind::Input { root: false } => {
                candidates.insert(id);
            }
            _ => {}
        }
    }
    let mut assumed: BTreeSet<HRelId> = events.union(&candidates).copied().collect();
    loop {
        let mut next: BTreeSet<HRelId> = events.clone();
        // Writers per relation: (event-driven?) for each writing statement.
        let mut writers: BTreeMap<HRelId, Vec<bool>> = BTreeMap::new();
        for h in &hir.handlers {
            let driven = h.kind != HandlerKind::Plain || has_event_literal(&h.header, &assumed);
            collect_writers(&h.stmts, driven, &mut writers);
        }
        for c in &candidates {
            let r = hir.rel(*c)?;
            let is_event = match r.kind {
                HRelKind::View => hir
                    .views
                    .iter()
                    .find(|v| v.rel == *c)
                    .is_some_and(|v| v.alternatives.iter().all(|(_, b)| has_event_literal(b, &assumed))),
                _ => writers.get(c).is_some_and(|w| !w.is_empty() && w.iter().all(|x| *x)),
            };
            if is_event {
                next.insert(*c);
            }
        }
        if next == assumed {
            return Ok(next);
        }
        assumed = next;
    }
}

fn collect_writers(stmts: &[HStmt], driven: bool, out: &mut BTreeMap<HRelId, Vec<bool>>) {
    for s in stmts {
        match s {
            HStmt::Verb(v) => out.entry(v.target).or_default().push(driven),
            HStmt::Block { stmts, .. } => collect_writers(stmts, driven, out),
        }
    }
}

/// Whether a body has a positive event literal: a positive atom of an event relation, a delta literal, or an `any`
/// whose every alternative has one.
pub(crate) fn has_event_literal(body: &HBody, events: &BTreeSet<HRelId>) -> bool {
    body.lits.iter().any(|l| match l {
        HLit::Atom(a) | HLit::Per(a) => events.contains(&a.rel),
        HLit::Delta { .. } => true,
        HLit::Any(alts, _) => !alts.is_empty() && alts.iter().all(|b| has_event_literal(b, events)),
        _ => false,
    })
}

fn positive_rels(body: &HBody) -> Vec<HRelId> {
    let mut out = Vec::new();
    for l in &body.lits {
        if let HLit::Atom(a) = l
            && !out.contains(&a.rel)
        {
            out.push(a.rel);
        }
    }
    out
}

fn why_standing(hir: &Hir, r: HRelId) -> Result<&'static str, InternalError> {
    Ok(match hir.rel(r)?.kind {
        HRelKind::Table => "a table",
        HRelKind::Static | HRelKind::Members(_) | HRelKind::NodeDir => "a static relation",
        HRelKind::View => "a view with an alternative that has no event",
        HRelKind::Scratch | HRelKind::Output { .. } | HRelKind::Input { root: false } => {
            "written by a statement that is not event-driven, or by none"
        }
        _ => "standing",
    })
}

/// BLS0506: `emit r` in a handler whose header or block conditions test `r` negatively.
fn self_negation(hir: &Hir, h: &HHandler, diags: &mut Diagnostics) -> Result<(), InternalError> {
    let mut negated = BTreeSet::new();
    negated_rels(&h.header, &mut negated);
    // Block conditions anywhere in the handler count (LANGUAGE §8.6).
    collect_block_negations(&h.stmts, &mut negated);
    emit_checks(hir, &h.stmts, &negated, diags)
}

fn collect_block_negations(stmts: &[HStmt], out: &mut BTreeSet<HRelId>) {
    for s in stmts {
        if let HStmt::Block { cond, stmts, .. } = s {
            negated_rels(cond, out);
            collect_block_negations(stmts, out);
        }
    }
}

fn emit_checks(
    hir: &Hir,
    stmts: &[HStmt],
    negated: &BTreeSet<HRelId>,
    diags: &mut Diagnostics,
) -> Result<(), InternalError> {
    for s in stmts {
        match s {
            HStmt::Verb(v) => {
                if v.verb == Verb::Emit && negated.contains(&v.target) && !v.allow_self_negation {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0506"),
                            format!(
                                "this handler tests `{}` negatively and `emit`s it in the same tick, so the test sees \
                                 its own write; write `next` (or `#[allow(self_negation)]` if it is meant)",
                                hir.rel(v.target)?.name
                            ),
                        )
                        .with_primary(v.span),
                    );
                }
            }
            HStmt::Block { stmts, .. } => emit_checks(hir, stmts, negated, diags)?,
        }
    }
    Ok(())
}

fn negated_rels(body: &HBody, out: &mut BTreeSet<HRelId>) {
    for l in &body.lits {
        match l {
            HLit::Not(a) => {
                out.insert(a.rel);
            }
            HLit::NotBody(b, _) => all_rels(b, out),
            HLit::Any(alts, _) => alts.iter().for_each(|b| negated_rels(b, out)),
            _ => {}
        }
    }
}

fn all_rels(body: &HBody, out: &mut BTreeSet<HRelId>) {
    for l in &body.lits {
        match l {
            HLit::Atom(a) | HLit::Not(a) | HLit::Outer(a) | HLit::Per(a) | HLit::Delta { atom: a, .. } => {
                out.insert(a.rel);
            }
            HLit::NotBody(b, _) => all_rels(b, out),
            HLit::Any(alts, _) => alts.iter().for_each(|b| all_rels(b, out)),
            HLit::Forall { body, .. } => all_rels(body, out),
            _ => {}
        }
    }
}
