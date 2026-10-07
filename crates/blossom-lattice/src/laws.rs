//! The lattice laws and class claims (TEST-083, LANGUAGE §11.8), checked on sample values.
//!
//! [`merge_laws`] checks that a lattice's merge is associative, commutative and idempotent, that ⊥ is its identity,
//! and that its order agrees with it (`a ⊑ b` exactly when `a ⊔ b = b`). [`check_claim`] checks an operation's
//! declared class against the order: a morphism preserves joins, a monotone operation the order, an antitone one
//! reverses it, a threshold once true (or `Some(v)`) stays so, and a `stable … after t` operation no longer changes
//! once `t` holds. Values come from the caller (the program-level harness generates them from the lattice's types);
//! a pair with no join (two different `LPoint` values merged, BLSR006) is not a case, and neither is a case on which
//! the operation fails at run time.

use blossom_value::Value;
use blossom_value::value::LatValue;

use crate::{Kind, LatticeError};

/// A law or claim that failed: its name, the values it failed on, and what went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterexample {
    /// The law (`associativity`, `threshold`, …).
    pub law: &'static str,
    /// The values, named as `detail` names them (`a`, `b`, `f(a)`, …).
    pub values: Vec<(String, Value)>,
    /// What failed, in words.
    pub detail: String,
    /// The argument `a` and `b` stand for (0, the receiver, unless a bimorphism's other lattice argument failed).
    pub varied: usize,
}

/// Why a check stopped.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LawError {
    /// A law or claim does not hold.
    #[error("{} does not hold: {}", .0.law, .0.detail)]
    Refuted(Box<Counterexample>),
    /// A value of the wrong shape reached a lattice operation: the caller's bug.
    #[error(transparent)]
    Lattice(LatticeError),
}

/// How many cases a check ran, and how many it set aside (no join, or the operation failed at run time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub checked: usize,
    pub skipped: usize,
}

/// `a ⊔ b`, or `None` when the two have no join (an `LPoint` conflict).
fn join(kind: &Kind, a: &LatValue, b: &LatValue) -> Result<Option<LatValue>, LawError> {
    match kind.join(a, b) {
        Ok(j) => Ok(Some(j)),
        Err(LatticeError::Conflict(..)) => Ok(None),
        Err(e) => Err(LawError::Lattice(e)),
    }
}

fn leq(kind: &Kind, a: &LatValue, b: &LatValue) -> Result<bool, LawError> {
    kind.leq(a, b).map_err(LawError::Lattice)
}

fn lat(v: &LatValue) -> Value {
    Value::Lattice(v.clone())
}

fn refuted(law: &'static str, values: Vec<(String, Value)>, detail: impl Into<String>) -> LawError {
    LawError::Refuted(Box::new(Counterexample {
        law,
        values,
        detail: detail.into(),
        varied: 0,
    }))
}

/// Names for a counterexample's values.
fn named(values: Vec<(&str, Value)>) -> Vec<(String, Value)> {
    values.into_iter().map(|(n, v)| (n.to_owned(), v)).collect()
}

/// Checks the merge laws of `kind` on every pair and triple of `samples` (which should include ⊥ and joins).
pub fn merge_laws(kind: &Kind, samples: &[LatValue]) -> Result<Tally, LawError> {
    let mut t = Tally::default();
    let bot = kind.bottom();
    for a in samples {
        if !kind.is_bottom(&bot) {
            return Err(refuted("⊥", named(vec![("⊥", lat(&bot))]), "⊥ is not bottom"));
        }
        match join(kind, a, a)? {
            Some(j) if j == *a => {}
            Some(j) => {
                return Err(refuted(
                    "idempotence",
                    named(vec![("a", lat(a)), ("a ⊔ a", lat(&j))]),
                    "a ⊔ a ≠ a",
                ));
            }
            None => {
                return Err(refuted(
                    "idempotence",
                    named(vec![("a", lat(a))]),
                    "a has no join with itself",
                ));
            }
        }
        match join(kind, a, &bot)? {
            Some(j) if j == *a => {}
            _ => return Err(refuted("⊥ identity", named(vec![("a", lat(a))]), "a ⊔ ⊥ ≠ a")),
        }
        if !leq(kind, &bot, a)? {
            return Err(refuted("⊥ identity", named(vec![("a", lat(a))]), "⊥ ⋢ a"));
        }
        for b in samples {
            let (ab, ba) = (join(kind, a, b)?, join(kind, b, a)?);
            if ab != ba {
                return Err(refuted(
                    "commutativity",
                    named(vec![("a", lat(a)), ("b", lat(b))]),
                    "a ⊔ b ≠ b ⊔ a",
                ));
            }
            let Some(ab) = ab else {
                t.skipped += 1;
                continue;
            };
            if leq(kind, a, b)? != (ab == *b) {
                return Err(refuted(
                    "order agrees with merge",
                    named(vec![("a", lat(a)), ("b", lat(b)), ("a ⊔ b", lat(&ab))]),
                    "a ⊑ b disagrees with a ⊔ b = b",
                ));
            }
            for c in samples {
                let left = join(kind, &ab, c)?;
                let right = match join(kind, b, c)? {
                    Some(bc) => join(kind, a, &bc)?,
                    None => None,
                };
                match (left, right) {
                    (Some(l), Some(r)) if l != r => {
                        return Err(refuted(
                            "associativity",
                            named(vec![("a", lat(a)), ("b", lat(b)), ("c", lat(c))]),
                            "(a ⊔ b) ⊔ c ≠ a ⊔ (b ⊔ c)",
                        ));
                    }
                    (Some(_), Some(_)) => t.checked += 1,
                    (None, None) => t.skipped += 1,
                    _ => {
                        return Err(refuted(
                            "associativity",
                            named(vec![("a", lat(a)), ("b", lat(b)), ("c", lat(c))]),
                            "one association has a join and the other does not",
                        ));
                    }
                }
            }
        }
    }
    Ok(t)
}

/// A class claim on an operation (LANGUAGE §11.4, §11.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// `f(a ⊔ b) = f(a) ⊔ f(b)` in the receiver.
    Morphism,
    /// A morphism in each lattice argument.
    Bimorphism,
    /// `a ⊑ b ⇒ f(a) ⊑ f(b)` in the receiver.
    Monotone,
    /// `a ⊑ b ⇒ f(b) ⊑ f(a)` in the receiver.
    Antitone,
    /// `a ⊑ b ∧ f(a) ⇒ f(b)`; for an `Option` result, `f(a) = Some(v) ⇒ f(b) = Some(v)`.
    Threshold,
    /// `a ⊑ b ∧ t(a) ∧ t(b) ⇒ f(a) = f(b)`, with `t` the guarding threshold: a read guarded by `t` never sees a
    /// state `t` fails on (a run-time error aborts the tick first).
    Stable,
}

impl Claim {
    const fn name(self) -> &'static str {
        match self {
            Claim::Morphism => "morphism",
            Claim::Bimorphism => "bimorphism",
            Claim::Monotone => "monotone",
            Claim::Antitone => "antitone",
            Claim::Threshold => "threshold",
            Claim::Stable => "stable",
        }
    }
}

/// One argument of a checked operation: its lattice (`None` for a plain argument) and its sample values.
#[derive(Clone, Debug)]
pub struct Arg {
    pub kind: Option<Kind>,
    pub samples: Vec<Value>,
}

/// An operation's evaluation: its result, or a run-time failure (the case is set aside).
pub type Eval<'f> = &'f dyn Fn(&[Value]) -> Result<Value, String>;

/// The claimed operation: its arguments (the receiver first), its result lattice (`None` for a plain result), its
/// evaluation and, for `stable`, the guarding threshold's (on the receiver alone).
pub struct Checked<'f> {
    pub claim: Claim,
    pub args: Vec<Arg>,
    pub result: Option<Kind>,
    pub eval: Eval<'f>,
    pub guard: Option<Eval<'f>>,
}

/// The lattice value of `v`.
fn as_lat(v: &Value) -> Result<&LatValue, LawError> {
    match v {
        Value::Lattice(l) => Ok(l),
        other => Err(LawError::Lattice(LatticeError::Shape(format!(
            "{other:?} is not a lattice value"
        )))),
    }
}

/// Every combination of argument samples, at most `budget` of them: the counting order over the samples, with the
/// first argument varying fastest, so a small budget still covers every receiver.
fn tuples(args: &[Arg], budget: usize) -> Vec<Vec<Value>> {
    let sizes: Vec<usize> = args.iter().map(|a| a.samples.len()).collect();
    if sizes.contains(&0) {
        return Vec::new();
    }
    let total = sizes
        .iter()
        .try_fold(1usize, |acc, n| acc.checked_mul(*n))
        .unwrap_or(usize::MAX);
    let mut out = Vec::new();
    let mut n = 0usize;
    while n < total.min(budget) {
        let mut rest = n;
        let mut t = Vec::with_capacity(args.len());
        for (a, size) in args.iter().zip(&sizes) {
            if let Some(v) = a.samples.get(rest % size) {
                t.push(v.clone());
            }
            rest /= size;
        }
        out.push(t);
        n += 1;
    }
    out
}

/// Checks `op`'s claim on up to `budget` argument tuples, each against every sample of the argument it varies.
pub fn check_claim(op: &Checked<'_>, budget: usize) -> Result<Tally, LawError> {
    let mut t = Tally::default();
    let positions: Vec<usize> = match op.claim {
        Claim::Bimorphism => (0..op.args.len())
            .filter(|i| op.args.get(*i).is_some_and(|a| a.kind.is_some()))
            .collect(),
        _ => vec![0],
    };
    let result_kind = || {
        op.result.as_ref().ok_or_else(|| {
            LawError::Lattice(LatticeError::Shape(format!(
                "a {} claim needs a lattice result",
                op.claim.name()
            )))
        })
    };
    for base in tuples(&op.args, budget) {
        for &i in &positions {
            let Some(arg) = op.args.get(i) else { continue };
            let Some(kind) = &arg.kind else { continue };
            let Some(a) = base.get(i) else { continue };
            let a = as_lat(a)?.clone();
            for b in &arg.samples {
                let b = as_lat(b)?;
                let Some(ab) = join(kind, &a, b)? else {
                    t.skipped += 1;
                    continue;
                };
                let with = |v: &LatValue| {
                    let mut args = base.clone();
                    if let Some(slot) = args.get_mut(i) {
                        *slot = Value::Lattice(v.clone());
                    }
                    args
                };
                let (at_a, at_ab) = (with(&a), with(&ab));
                // `a` and `b` (the argument varied), the other arguments by position, then what failed.
                let named = |extra: Vec<(&'static str, Value)>| {
                    let mut vs: Vec<(String, Value)> = vec![("a".to_owned(), lat(&a)), ("b".to_owned(), lat(b))];
                    for (k, v) in base.iter().enumerate() {
                        if k != i {
                            vs.push((format!("argument {k}"), v.clone()));
                        }
                    }
                    vs.extend(extra.into_iter().map(|(n, v)| (n.to_owned(), v)));
                    vs
                };
                let outcome = match op.claim {
                    Claim::Morphism | Claim::Bimorphism => {
                        let out = result_kind()?;
                        let (Ok(fa), Ok(fb), Ok(fab)) = ((op.eval)(&at_a), (op.eval)(&with(b)), (op.eval)(&at_ab))
                        else {
                            t.skipped += 1;
                            continue;
                        };
                        let Some(joined) = join(out, as_lat(&fa)?, as_lat(&fb)?)? else {
                            return Err(refuted(
                                op.claim.name(),
                                named(vec![("f(a)", fa), ("f(b)", fb)]),
                                "f(a) and f(b) have no join, but a ⊔ b does",
                            ));
                        };
                        if *as_lat(&fab)? != joined {
                            Err(refuted(
                                op.claim.name(),
                                named(vec![("f(a ⊔ b)", fab), ("f(a) ⊔ f(b)", Value::Lattice(joined))]),
                                "f(a ⊔ b) ≠ f(a) ⊔ f(b)",
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    Claim::Monotone | Claim::Antitone => {
                        let out = result_kind()?;
                        let (Ok(fa), Ok(fab)) = ((op.eval)(&at_a), (op.eval)(&at_ab)) else {
                            t.skipped += 1;
                            continue;
                        };
                        let holds = if op.claim == Claim::Monotone {
                            leq(out, as_lat(&fa)?, as_lat(&fab)?)?
                        } else {
                            leq(out, as_lat(&fab)?, as_lat(&fa)?)?
                        };
                        if holds {
                            Ok(())
                        } else {
                            let what = if op.claim == Claim::Monotone {
                                "a ⊑ a ⊔ b but f(a) ⋢ f(a ⊔ b)"
                            } else {
                                "a ⊑ a ⊔ b but f(a ⊔ b) ⋢ f(a)"
                            };
                            Err(refuted(
                                op.claim.name(),
                                named(vec![("f(a)", fa), ("f(a ⊔ b)", fab)]),
                                what,
                            ))
                        }
                    }
                    Claim::Threshold => {
                        let (Ok(fa), Ok(fab)) = ((op.eval)(&at_a), (op.eval)(&at_ab)) else {
                            t.skipped += 1;
                            continue;
                        };
                        let reached = match &fa {
                            Value::Bool(x) => *x,
                            Value::Option(o) => o.is_some(),
                            other => {
                                return Err(LawError::Lattice(LatticeError::Shape(format!(
                                    "a threshold gave {other:?}, not a bool or an option"
                                ))));
                            }
                        };
                        if reached && fa != fab {
                            Err(refuted(
                                "threshold",
                                named(vec![("f(a)", fa), ("f(a ⊔ b)", fab)]),
                                "f(a) has been reached but f(a ⊔ b) differs",
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    Claim::Stable => {
                        let Some(guard) = op.guard else {
                            return Err(LawError::Lattice(LatticeError::Shape(
                                "a stable claim needs its threshold".into(),
                            )));
                        };
                        let Ok(g) = guard(&[Value::Lattice(a.clone())]) else {
                            t.skipped += 1;
                            continue;
                        };
                        // A state the threshold fails on (a run-time error) is one no guarded read reaches; one it
                        // does not hold on breaks the threshold's own claim, which that claim's check reports.
                        let g_ab = guard(&[Value::Lattice(ab.clone())]);
                        if g != Value::Bool(true) || g_ab != Ok(Value::Bool(true)) {
                            t.skipped += 1;
                            continue;
                        }
                        let (Ok(fa), Ok(fab)) = ((op.eval)(&at_a), (op.eval)(&at_ab)) else {
                            t.skipped += 1;
                            continue;
                        };
                        if fa != fab {
                            Err(refuted(
                                "stable",
                                named(vec![("f(a)", fa), ("f(a ⊔ b)", fab)]),
                                "the threshold holds at a, but f(a ⊔ b) ≠ f(a)",
                            ))
                        } else {
                            Ok(())
                        }
                    }
                };
                if let Err(LawError::Refuted(mut c)) = outcome {
                    c.varied = i;
                    return Err(LawError::Refuted(c));
                }
                outcome?;
                t.checked += 1;
            }
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use blossom_value::value::IntValue;

    use super::*;
    use crate::Op;

    fn int(n: u64) -> Value {
        Value::Int(IntValue::U64(n))
    }

    fn set(xs: &[u64]) -> LatValue {
        LatValue::Set(Arc::new(xs.iter().map(|x| int(*x)).collect::<BTreeSet<_>>()))
    }

    fn elem(n: u64) -> LatValue {
        LatValue::Elem(Arc::new(int(n)))
    }

    fn samples(kind: &Kind) -> Vec<LatValue> {
        match kind {
            Kind::Bool => vec![LatValue::Bool(false), LatValue::Bool(true)],
            Kind::Max | Kind::Min | Kind::Point => vec![LatValue::Bottom, elem(1), elem(2), elem(5)],
            Kind::Set | Kind::PSet => vec![set(&[]), set(&[1]), set(&[2]), set(&[1, 3])],
            Kind::Map(inner) => {
                let vs = samples(inner);
                let mut out = vec![LatValue::Map(Arc::new(BTreeMap::new()))];
                for (i, v) in vs.iter().enumerate().filter(|(_, v)| !inner.is_bottom(v)) {
                    out.push(LatValue::Map(Arc::new(BTreeMap::from([(
                        int(i as u64 % 2),
                        v.clone(),
                    )]))));
                }
                out
            }
            Kind::Product(fields) => {
                let per: Vec<Vec<LatValue>> = fields.iter().map(samples).collect();
                (0..4)
                    .map(|i| LatValue::Seq(per.iter().map(|s| s[(i * 3 + 1) % s.len()].clone()).collect()))
                    .chain([kind.bottom()])
                    .collect()
            }
        }
    }

    #[test]
    fn every_built_in_lattice_and_a_product_obey_the_merge_laws() {
        let product = Kind::Product(vec![Kind::Max, Kind::Set, Kind::Map(Box::new(Kind::Point))]);
        for kind in [
            Kind::Bool,
            Kind::Max,
            Kind::Min,
            Kind::Set,
            Kind::PSet,
            Kind::Point,
            Kind::Map(Box::new(Kind::Max)),
            Kind::Map(Box::new(Kind::Point)),
            product,
        ] {
            let t = merge_laws(&kind, &samples(&kind)).unwrap();
            assert!(t.checked > 0, "{kind:?}");
        }
    }

    fn size(args: &[Value]) -> Result<Value, String> {
        Kind::Set.eval(Op::Size, args).map_err(|e| e.to_string())
    }

    fn is_empty(args: &[Value]) -> Result<Value, String> {
        Kind::Set.eval(Op::IsEmpty, args).map_err(|e| e.to_string())
    }

    fn sets() -> Arg {
        Arg {
            kind: Some(Kind::Set),
            samples: [set(&[]), set(&[1]), set(&[2]), set(&[1, 2])].iter().map(lat).collect(),
        }
    }

    #[test]
    fn true_claims_hold_and_false_ones_are_refuted() {
        let mono = Checked {
            claim: Claim::Monotone,
            args: vec![sets()],
            result: Some(Kind::Max),
            eval: &size,
            guard: None,
        };
        assert!(check_claim(&mono, 100).unwrap().checked > 0);
        // `size` is not a morphism: |{1} ∪ {1}| ≠ max(|{1}|, |{1}|) holds, but |{1} ∪ {2}| = 2 ≠ 1.
        let morph = Checked {
            claim: Claim::Morphism,
            ..mono
        };
        assert!(matches!(check_claim(&morph, 100), Err(LawError::Refuted(c)) if c.law == "morphism"));
        // `is_empty` is antitone, so as a threshold it is refuted: true at ∅, false at {1}.
        let thresh = Checked {
            claim: Claim::Threshold,
            args: vec![sets()],
            result: None,
            eval: &is_empty,
            guard: None,
        };
        assert!(matches!(check_claim(&thresh, 100), Err(LawError::Refuted(c)) if c.law == "threshold"));
    }

    #[test]
    fn a_stable_read_may_change_until_its_threshold_holds() {
        let at_least_two = |args: &[Value]| -> Result<Value, String> {
            match size(args)? {
                Value::Lattice(LatValue::Elem(n)) => Ok(Value::Bool(*n >= int(2))),
                other => Err(format!("{other:?}")),
            }
        };
        let stable = Checked {
            claim: Claim::Stable,
            args: vec![sets()],
            result: None,
            eval: &size,
            guard: Some(&at_least_two),
        };
        // Within {1, 2} the size is fixed once it reaches 2.
        assert!(check_claim(&stable, 100).is_ok());
        let wider = Checked {
            args: vec![Arg {
                kind: Some(Kind::Set),
                samples: [set(&[1, 2]), set(&[3])].iter().map(lat).collect(),
            }],
            ..stable
        };
        assert!(matches!(check_claim(&wider, 100), Err(LawError::Refuted(c)) if c.law == "stable"));
    }
}
