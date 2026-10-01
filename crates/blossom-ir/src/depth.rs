//! The evaluation depth bound (LANGUAGE §16.1, BLS0217). Both evaluators interpret expressions recursively, so an
//! expression's nesting, the functions it calls and the closures a combinator applies all use stack. Blossom has no
//! recursion (BLS0213), so the deepest evaluation a program can make is a static fact: it is computed here, over the
//! call graph, and a program deeper than [`MAX_EVAL_DEPTH`] is refused, so no evaluation can overflow the stack a
//! thread that runs ticks is given ([`EVAL_STACK_BYTES`], sized for this bound).
//!
//! A unit of depth is one expression node on the evaluation path: a node is one level deeper than its deepest
//! operand; a call of a declared function is one level deeper than the function's body; a closure is one level
//! deeper than its body, and a combinator one level deeper than its arguments, closures included, so a closure a
//! combinator applies counts under it. A rule's evaluation is one level per body literal, plus its head, above the
//! deepest of its literals' expressions.

use blossom_base::{FnId, RelId, RuleId};

use crate::core::{Expr, FnBody, FnRef, GenSource, Literal, Program};

/// The deepest evaluation a program may make, in the units above. The Kafka broker's deepest evaluation is 89.
pub const MAX_EVAL_DEPTH: u32 = 1024;

/// The stack every thread that evaluates a program is given: the runtime's engine thread, the CLI's command thread,
/// LDFI's workers and (through `.cargo/config.toml`) test threads. A debug build's evaluator frames are large (the
/// engine's `eval` is about 15 KB); a test evaluates a program at `MAX_EVAL_DEPTH` on both evaluators within it.
/// Only the pages an evaluation touches are committed.
pub const EVAL_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Where a program's deepest evaluation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthSite {
    Fn(FnId),
    Rule(RuleId),
    /// A relation's `partition by` key.
    Partition(RelId),
}

/// The deepest evaluation of `p`: its depth and where, or `None` for a program with no expressions.
pub fn deepest(p: &Program) -> Option<(u32, DepthSite)> {
    let mut memo: Vec<Option<u32>> = vec![None; p.fns.len()];
    let mut best: Option<(u32, DepthSite)> = None;
    let consider = |d: u32, site: DepthSite, best: &mut Option<(u32, DepthSite)>| {
        if best.is_none_or(|b| d > b.0) {
            *best = Some((d, site));
        }
    };
    for (id, _) in p.fns.iter_enumerated() {
        let d = fn_depth(p, id, &mut memo);
        consider(d, DepthSite::Fn(id), &mut best);
    }
    for (id, r) in p.rules.iter_enumerated() {
        // Both evaluators recurse once per body literal (the oracle's search, the engine's join), in an order the
        // planner picks: any literal's expression may be evaluated below all the others, and the head below them.
        let lits = u32::try_from(r.body.lits.len()).unwrap_or(u32::MAX);
        let mut d = 0;
        for l in &r.body.lits {
            let here = match l {
                Literal::Bind { expr, .. } | Literal::Guard(expr) => depth(p, expr, &mut memo),
                Literal::Gen { src, .. } => match src {
                    GenSource::Value(e) | GenSource::Lattice(e) => depth(p, e, &mut memo),
                    GenSource::Range { lo, hi, .. } => depth(p, lo, &mut memo).max(depth(p, hi, &mut memo)),
                    GenSource::TableFn { .. } => 1,
                },
                Literal::Pos(_) | Literal::Neg(_) | Literal::Lookup { .. } => 0,
            };
            d = d.max(here);
        }
        consider(lits.saturating_add(1).saturating_add(d), DepthSite::Rule(id), &mut best);
    }
    for (id, r) in p.rels.iter_enumerated() {
        let channel = match &r.class {
            crate::core::RelClass::Channel(c) => c.partition.as_ref(),
            _ => None,
        };
        for ps in [r.attrs.partition.as_ref(), channel].into_iter().flatten() {
            let d = depth(p, &ps.key, &mut memo);
            consider(d, DepthSite::Partition(id), &mut best);
        }
    }
    best
}

/// The depth of a call of function `id`: one more than its body's (a host function's is 1).
fn fn_depth(p: &Program, id: FnId, memo: &mut Vec<Option<u32>>) -> u32 {
    if let Some(Some(d)) = memo.get(id.index()) {
        return *d;
    }
    let d = match p.fns.get(id).map(|f| &f.body) {
        Some(FnBody::Ir(body)) => 1 + depth(p, body, memo),
        Some(FnBody::Extern { .. } | FnBody::TableFn { .. } | FnBody::Builtin(_)) | None => 1,
    };
    if let Some(slot) = memo.get_mut(id.index()) {
        *slot = Some(d);
    }
    d
}

fn depth(p: &Program, e: &Expr, memo: &mut Vec<Option<u32>>) -> u32 {
    let most = |xs: &mut dyn Iterator<Item = u32>| xs.max().unwrap_or(0);
    1 + match e {
        Expr::Term(_) | Expr::Param(_) | Expr::Scalar(_) => 0,
        Expr::Unary { arg, .. } => depth(p, arg, memo),
        Expr::Binary { lhs, rhs, .. } => depth(p, lhs, memo).max(depth(p, rhs, memo)),
        Expr::Call { f, args } => {
            let callee = match f {
                FnRef::Fn(id) => fn_depth(p, *id, memo),
                FnRef::Builtin(_) => 0,
            };
            let a: Vec<u32> = args.iter().map(|x| depth(p, x, memo)).collect();
            callee.max(most(&mut a.into_iter()))
        }
        Expr::Construct { fields: xs, .. } | Expr::Collection { elems: xs, .. } | Expr::Lattice { args: xs, .. } => {
            let a: Vec<u32> = xs.iter().map(|x| depth(p, x, memo)).collect();
            most(&mut a.into_iter())
        }
        Expr::Field { base, .. } => depth(p, base, memo),
        Expr::If { cond, then, els } => depth(p, cond, memo).max(depth(p, then, memo)).max(depth(p, els, memo)),
        Expr::Match { scrut, arms } => {
            let mut d = depth(p, scrut, memo);
            for (_, guard, body) in arms {
                if let Some(g) = guard {
                    d = d.max(depth(p, g, memo));
                }
                d = d.max(depth(p, body, memo));
            }
            d
        }
        Expr::Let { value, body, .. } => depth(p, value, memo).max(depth(p, body, memo)),
        Expr::Closure { body, .. } => depth(p, body, memo),
        Expr::Typed { expr, .. } => depth(p, expr, memo),
    }
}
