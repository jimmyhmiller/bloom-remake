//! Whether a rule's expressions are functions of its rows alone (docs/design/DATABASE.md §8): a durable view's rows
//! are kept in the database and are the same after a restart, so its rules may read no time (`$now`, `$tick`,
//! `$incarnation`, `$host`), draw no random value, and make no blob (`Blob::of`: a view's row may hold a blob only
//! when a row it reads holds it, durable with that row). User functions are followed into their bodies.

use std::collections::BTreeMap;

use blossom_base::FnId;
use blossom_ir::core::{
    BuiltinFn, BuiltinScalar, Expr, FnBody, FnRef, GenSource, HeadArg, LibFn, Literal, Program, Rule,
};

/// Whether `rule`'s expressions are functions of its rows alone. `memo` keeps each user function's answer.
pub(crate) fn rule_is_pure(p: &Program, rule: &Rule, memo: &mut BTreeMap<FnId, bool>) -> bool {
    let lits = rule.body.lits.iter().all(|l| match l {
        Literal::Pos(_) | Literal::Neg(_) | Literal::Lookup { .. } => true,
        Literal::Bind { expr, .. } | Literal::Guard(expr) => expr_is_pure(p, expr, memo),
        Literal::Gen { src, .. } => match src {
            GenSource::Value(e) | GenSource::Lattice(e) => expr_is_pure(p, e, memo),
            GenSource::Range { lo, hi, .. } => expr_is_pure(p, lo, memo) && expr_is_pure(p, hi, memo),
            GenSource::TableFn { f, .. } => fn_is_pure(p, *f, memo),
        },
    });
    // Head arguments are terms (an aggregate's tuple too).
    lits && rule.head.args.iter().all(|a| matches!(a, HeadArg::Term(_) | HeadArg::Agg(_)))
}

fn fn_is_pure(p: &Program, f: FnId, memo: &mut BTreeMap<FnId, bool>) -> bool {
    if let Some(known) = memo.get(&f) {
        return *known;
    }
    // A function's body calls no function that calls it back (LANGUAGE §16.1); the entry guards a cycle anyway.
    memo.insert(f, false);
    let pure = match p.fns.get(f).map(|d| &d.body) {
        Some(FnBody::Ir(e)) => expr_is_pure(p, e, memo),
        // A host function is pure by declaration (LANG-181).
        Some(FnBody::Extern { .. } | FnBody::TableFn { .. }) => true,
        Some(FnBody::Builtin(b)) => builtin_is_pure(b),
        None => false,
    };
    memo.insert(f, pure);
    pure
}

fn expr_is_pure(p: &Program, e: &Expr, memo: &mut BTreeMap<FnId, bool>) -> bool {
    let all = |es: &[Expr], memo: &mut BTreeMap<FnId, bool>| es.iter().all(|x| expr_is_pure(p, x, memo));
    match e {
        Expr::Term(_) | Expr::Param(_) => true,
        Expr::Scalar(s) => matches!(s, BuiltinScalar::SelfNode),
        Expr::Unary { arg, .. } => expr_is_pure(p, arg, memo),
        Expr::Binary { lhs, rhs, .. } => expr_is_pure(p, lhs, memo) && expr_is_pure(p, rhs, memo),
        Expr::Call { f, args } => {
            let callee = match f {
                FnRef::Fn(id) => fn_is_pure(p, *id, memo),
                FnRef::Builtin(b) => builtin_is_pure(b),
            };
            callee && all(args, memo)
        }
        Expr::Construct { fields, .. } => all(fields, memo),
        Expr::Field { base, .. } => expr_is_pure(p, base, memo),
        Expr::If { cond, then, els } => {
            expr_is_pure(p, cond, memo) && expr_is_pure(p, then, memo) && expr_is_pure(p, els, memo)
        }
        Expr::Match { scrut, arms } => {
            expr_is_pure(p, scrut, memo)
                && arms
                    .iter()
                    .all(|(_, guard, body)| guard.as_ref().is_none_or(|g| expr_is_pure(p, g, memo)) && expr_is_pure(p, body, memo))
        }
        Expr::Collection { elems, .. } => all(elems, memo),
        Expr::Lattice { args, .. } => all(args, memo),
        Expr::Let { value, body, .. } => expr_is_pure(p, value, memo) && expr_is_pure(p, body, memo),
        Expr::Closure { body, .. } => expr_is_pure(p, body, memo),
        Expr::Typed { expr, .. } => expr_is_pure(p, expr, memo),
    }
}

/// A built-in that draws no random value and makes no blob.
fn builtin_is_pure(b: &BuiltinFn) -> bool {
    !matches!(
        b,
        BuiltinFn::Rand
            | BuiltinFn::RandFloat
            | BuiltinFn::RandRange
            | BuiltinFn::RandPrio { .. }
            | BuiltinFn::Lib(LibFn::BlobOf)
    )
}
