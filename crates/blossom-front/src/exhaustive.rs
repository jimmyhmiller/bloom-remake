//! Exhaustiveness of `match` (BLS0314): every value of the scrutinee's type must reach an arm, so a `match` never
//! runs out of arms at run time. A `match` over client input in a protocol decoder is where this matters.
//!
//! The check is the usefulness algorithm of Maranget ("Warnings for pattern matching", 2007) over the patterns the
//! HIR has: variables and `_`, tuples, enum and `Option` variants, and constant equality tests. A `bool` column is
//! covered by `true` and `false`; any other constant (an integer, a string, `[]`) covers only itself, so a column
//! of such constants needs a catch-all. An arm with a guard covers nothing, since its guard may be false.

use blossom_base::TypeId;
use blossom_value::{TypeDef, TypeTable, Value};

use crate::hir::{HExpr, HPat, Hir, TypeRef};

/// A head constructor of a pattern column.
#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    /// A tuple of this arity (the only constructor of its type).
    Tuple(usize),
    /// A variant, by number, of an enum or of `Option` (`None` is 0, `Some` is 1).
    Variant(u32),
    Bool(bool),
    /// Any other constant, or an equality test against a variable: one value of a domain too large to list.
    Other,
}

/// A pattern reduced to what coverage needs.
#[derive(Clone, Debug)]
struct Pat {
    /// `None` for a variable or `_`.
    ctor: Option<Ctor>,
    /// For a variant: its enum type (`None` for `Option`).
    enum_ty: Option<TypeId>,
    subs: Vec<Pat>,
}

const WILD: Pat = Pat {
    ctor: None,
    enum_ty: None,
    subs: Vec::new(),
};

fn ctor(c: Ctor, enum_ty: Option<TypeId>, subs: Vec<Pat>) -> Pat {
    Pat {
        ctor: Some(c),
        enum_ty,
        subs,
    }
}

/// An enum type, or `None` for anything else (`Option`).
fn as_enum(types: &TypeTable, t: TypeId) -> Option<TypeId> {
    matches!(types.get(t), Some(TypeDef::Enum(_))).then_some(t)
}

fn reduce(hir: &Hir, p: &HPat) -> Pat {
    match p {
        HPat::Var(..) | HPat::Wild(_) => WILD,
        HPat::Tuple(ps, _) => ctor(Ctor::Tuple(ps.len()), None, ps.iter().map(|x| reduce(hir, x)).collect()),
        HPat::Variant {
            ty, variant, fields, ..
        } => {
            let enum_ty = match ty {
                TypeRef::Known(t) => as_enum(&hir.types, *t),
                TypeRef::Option => None,
            };
            ctor(
                Ctor::Variant(*variant),
                enum_ty,
                fields.iter().map(|x| reduce(hir, x)).collect(),
            )
        }
        HPat::Expr(e) => match crate::lower::try_const(hir, e) {
            Some(v) => of_value(&hir.types, &v, e.ty),
            None => ctor(Ctor::Other, None, Vec::new()),
        },
    }
}

/// A constant pattern, as the constructors it is made of.
fn of_value(types: &TypeTable, v: &Value, ty: Option<TypeId>) -> Pat {
    let elem = |i: usize| -> Option<TypeId> {
        match ty.and_then(|t| types.get(t)) {
            Some(TypeDef::Option(t)) => Some(*t),
            Some(TypeDef::Tuple(ts)) => ts.get(i).copied(),
            _ => None,
        }
    };
    match v {
        Value::Bool(b) => ctor(Ctor::Bool(*b), None, Vec::new()),
        Value::Option(None) => ctor(Ctor::Variant(0), None, Vec::new()),
        Value::Option(Some(x)) => ctor(Ctor::Variant(1), None, vec![of_value(types, x, elem(0))]),
        Value::Tuple(xs) => ctor(
            Ctor::Tuple(xs.len()),
            None,
            xs.iter()
                .enumerate()
                .map(|(i, x)| of_value(types, x, elem(i)))
                .collect(),
        ),
        Value::Enum { variant, fields } => {
            let enum_ty = ty.and_then(|t| as_enum(types, t));
            let field_ty = |i: usize| -> Option<TypeId> {
                match enum_ty.and_then(|t| types.get(t)) {
                    Some(TypeDef::Enum(def)) => def
                        .variants
                        .iter()
                        .find(|x| x.number == *variant)
                        .and_then(|x| x.payload.get(i))
                        .map(|f| f.ty),
                    _ => None,
                }
            };
            ctor(
                Ctor::Variant(*variant),
                enum_ty,
                fields
                    .iter()
                    .enumerate()
                    .map(|(i, x)| of_value(types, x, field_ty(i)))
                    .collect(),
            )
        }
        _ => ctor(Ctor::Other, None, Vec::new()),
    }
}

/// Every constructor of a column's type, with its arity and name, when the type's constructors can be listed: from
/// the first constructor the column has.
fn all_ctors(types: &TypeTable, heads: &[&Pat]) -> Option<Vec<(Ctor, usize, String)>> {
    let first = heads.iter().find(|p| p.ctor.is_some())?;
    match first.ctor.as_ref()? {
        Ctor::Tuple(k) => Some(vec![(Ctor::Tuple(*k), *k, "(…)".into())]),
        Ctor::Bool(_) => Some(vec![
            (Ctor::Bool(false), 0, "false".into()),
            (Ctor::Bool(true), 0, "true".into()),
        ]),
        Ctor::Variant(_) => match first.enum_ty.and_then(|t| types.get(t)) {
            Some(TypeDef::Enum(def)) => Some(
                def.variants
                    .iter()
                    .map(|v| (Ctor::Variant(v.number), v.payload.len(), v.name.as_str().to_owned()))
                    .collect(),
            ),
            _ => Some(vec![
                (Ctor::Variant(0), 0, "None".into()),
                (Ctor::Variant(1), 1, "Some".into()),
            ]),
        },
        Ctor::Other => None,
    }
}

/// The rows of `m` whose head matches constructor `c` (of `arity`), with the head replaced by its sub-patterns.
fn specialize(m: &[Vec<Pat>], c: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    let mut out = Vec::new();
    for row in m {
        let Some((head, rest)) = row.split_first() else {
            continue;
        };
        let mut new = match &head.ctor {
            None => vec![WILD; arity],
            Some(d) if d == c => {
                let mut subs = head.subs.clone();
                subs.resize(arity, WILD);
                subs
            }
            Some(_) => continue,
        };
        new.extend(rest.iter().cloned());
        out.push(new);
    }
    out
}

/// The rows of `m` whose head is a wildcard, without it.
fn default_rows(m: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    m.iter()
        .filter_map(|row| {
            let (head, rest) = row.split_first()?;
            head.ctor.is_none().then(|| rest.to_vec())
        })
        .collect()
}

/// A value of `n` columns that no row of `m` matches, as the constructor chosen in each column on the way down, or
/// `None` when `m` covers every value. Only all-wildcard vectors are ever asked about, so this is Maranget's
/// `useful(m, _ … _)`.
fn missing(types: &TypeTable, m: &[Vec<Pat>], n: usize) -> Option<Vec<String>> {
    if n == 0 {
        return m.is_empty().then(Vec::new);
    }
    let heads: Vec<&Pat> = m.iter().filter_map(|r| r.first()).collect();
    let present = |c: &Ctor| heads.iter().any(|p| p.ctor.as_ref() == Some(c));
    match all_ctors(types, &heads) {
        Some(ctors) if ctors.iter().all(|(c, _, _)| present(c)) => {
            // Every constructor of the type heads some row: a value is missing iff one is missing under one of them.
            ctors.iter().find_map(|(c, arity, name)| {
                missing(types, &specialize(m, c, *arity), arity + n - 1).map(|rest| {
                    let mut out = vec![name.clone()];
                    out.extend(rest);
                    out
                })
            })
        }
        ctors => {
            // A constructor no row names, or a type too large to list: only the wildcard rows match it.
            let absent = ctors
                .and_then(|cs| cs.into_iter().find(|(c, _, _)| !present(c)))
                .map_or_else(
                    || "_".to_owned(),
                    |(_, arity, name)| {
                        std::iter::once(name)
                            .chain((0..arity).map(|_| "_".to_owned()))
                            .collect::<Vec<_>>()
                            .join(" ")
                    },
                );
            missing(types, &default_rows(m), n - 1).map(|rest| {
                let mut out = vec![absent];
                out.extend(rest);
                out
            })
        }
    }
}

/// A value the arms of a `match` do not cover, described by the constructor chosen in each position (outermost
/// first), or `None` when they cover every value. Arms with guards are left out.
pub(crate) fn uncovered(hir: &Hir, arms: &[(HPat, Option<HExpr>, HExpr)]) -> Option<String> {
    let rows: Vec<Vec<Pat>> = arms
        .iter()
        .filter(|(_, guard, _)| guard.is_none())
        .map(|(p, _, _)| vec![reduce(hir, p)])
        .collect();
    missing(&hir.types, &rows, 1).map(|path| path.join(" "))
}
