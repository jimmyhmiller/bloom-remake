#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-lattice`: the built-in lattices and their operations over [`LatValue`] (LANGUAGE §11.5, R04 §2.4).
//!
//! A lattice is described by its [`Kind`]; element order is `Value`'s canonical order (LANGUAGE §5.5), which is the
//! natural order within one scalar type. Every operation has a monotonicity [`Class`] per LANGUAGE §11.4, which the
//! frontend uses for the bang rule and the analyses for polarity.
//!
//! Slice 2 (docs/design/SLICES.md) implements the core of Bloom^L: `LBool`, `LMax`, `LMin`, `LSet`, `LPSet`, `LMap` and
//! `LPoint`, with join, ⊥, order, lifts, `reveal!`, and the operations the language reads them with. S20 adds products
//! (user-defined `lattice X { … }`, LANGUAGE §11.8) and the merge and class laws ([`laws`]). The other built-ins
//! (`LBag`, `Lex`, `LDom`, causal and tombstone lattices), groups and rings and the lattice heap belong to later
//! slices (WP M3.1, M4.6).

pub mod laws;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_value::Value;
use blossom_value::class::{LatOpKind, MonoClass};
use blossom_value::value::{IntValue, LatValue};

/// A built-in lattice.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `LBool`: ⊥ = false, join = or.
    Bool,
    /// `LMax<T>`: adjoined ⊥ = −∞, join = max.
    Max,
    /// `LMin<T>`: adjoined ⊥ = +∞, join = min.
    Min,
    /// `LSet<T>`: ⊥ = ∅, join = ∪.
    Set,
    /// `LPSet<T>` over non-negative numbers: as `LSet`, with `sum`.
    PSet,
    /// `LMap<K, L>`: key union, values joined; ⊥ values are absent.
    Map(Box<Kind>),
    /// `LPoint<T>`: ⊥ or one value; two different values conflict (BLSR006).
    Point,
    /// A product of lattices (`lattice X { f: L, … }`, LANGUAGE §11.8), over [`LatValue::Seq`]: ⊥ is every field ⊥,
    /// merge and order are fieldwise.
    Product(Vec<Kind>),
}

/// Why a lattice operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LatticeError {
    /// Two different `LPoint` values were merged (BLSR006).
    #[error("conflicting values {0:?} and {1:?} merged into an LPoint")]
    Conflict(Box<Value>, Box<Value>),
    /// A value of the wrong shape reached an operation: a frontend or evaluator bug.
    #[error("{0}")]
    Shape(String),
    /// A value outside the lattice's domain (a negative number in an `LPSet`).
    #[error("{0}")]
    Domain(String),
    /// Checked arithmetic overflowed (BLSR004).
    #[error("{0}")]
    Arithmetic(String),
}

/// An operation on lattice values (LANGUAGE §11.4–11.5, R04 §2.4). The receiver, when there is one, is the first
/// argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    /// `a.join(b)`.
    Join,
    /// `reveal!(x)`: the exact value (deep); `LMax`/`LMin`/`LPoint` reveal to `Option<T>`.
    Reveal,
    /// `reveal!(x)` of a known non-⊥ `LMax`/`LMin`/`LPoint` (SEM-101 N4): `T`.
    RevealNonBot,
    /// `x.is_bot!()`.
    IsBot,
    /// `s.contains(x)` / `x in s` (set-like).
    Contains,
    /// `s.size()` (set-like, map): an `LMax<u64>`.
    Size,
    /// `s.nonempty()` (set-like).
    Nonempty,
    /// `s.is_empty!()` (set-like).
    IsEmpty,
    /// `x >= c` on `LMax` (a threshold against a scalar).
    AtLeast,
    /// `x > c` on `LMax`.
    Above,
    /// `x <= c` on `LMin`.
    AtMost,
    /// `x < c` on `LMin`.
    Below,
    /// `m.at(k)` (map): the value, ⊥ if absent.
    At,
    /// `m.has_key(k)` (map).
    HasKey,
    /// `m.key_set()` (map): an `LSet` of the keys.
    KeySet,
    /// `s.sum()` (`LPSet`): an `LMax` of the sum.
    Sum,
    /// `x.get()` (`LPoint`): `Option<T>`.
    Get,
    /// `x + c` on `LMax`/`LMin` with a scalar `c`: a morphism.
    Add,
    /// `a + b` on two `LMax`/`LMin` values: a bimorphism (tropical on `LMin`).
    AddLat,
    /// `x - c` on `LMax` with a scalar `c`: a morphism.
    Sub,
    /// `x.min_of(c)` on `LMax`: a morphism.
    MinOf,
    /// `a.leq!(b)`: antitone in `a`.
    Leq,
    /// `a.lt!(b)`: strictly below; non-monotone.
    Less,
    /// `s.intersect(t)` (set-like): a bimorphism.
    Intersect,
    /// `s.min_elem()` (set-like): an `LMin`.
    MinElem,
    /// `s.max_elem()` (set-like): an `LMax`.
    MaxElem,
    /// `a.and(b)` (`LBool`).
    And,
    /// `a.or(b)` (`LBool`).
    Or,
    /// `a.not!()` (`LBool`): antitone.
    Not,
    /// `L::of(…)`: `LMax::of(x)`, `LBool::of(b)`, `LPoint::of(x)` lift the value, `LSet::of(x)` is `{x}`,
    /// `LMap::of(k, v)` is `{k ↦ v}`.
    Of,
    /// The implicit lift of a plain value where a lattice is expected (LANGUAGE §5.6).
    Lift,
    /// The lift of a map whose values already are the value lattice (`map[k => x]` with `x: L`) into `LMap<K, L>`.
    LiftEntries,
}

/// An operation's monotonicity: its kind and the class of each argument (receiver first). Arguments that are not
/// lattice values are [`MonoClass::Constant`]: the natural order says nothing about them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sig {
    pub kind: LatOpKind,
    pub params: Vec<MonoClass>,
}

impl Sig {
    /// Whether a call needs a bang (`op!`, LANGUAGE §11.4).
    pub fn needs_bang(&self) -> bool {
        matches!(self.kind, LatOpKind::Antitone | LatOpKind::NonMonotone)
    }
}

impl Op {
    /// Every operation, in catalogue order.
    pub const ALL: [Op; 32] = [
        Op::Join,
        Op::Reveal,
        Op::RevealNonBot,
        Op::IsBot,
        Op::Contains,
        Op::Size,
        Op::Nonempty,
        Op::IsEmpty,
        Op::AtLeast,
        Op::Above,
        Op::AtMost,
        Op::Below,
        Op::At,
        Op::HasKey,
        Op::KeySet,
        Op::Sum,
        Op::Get,
        Op::Add,
        Op::AddLat,
        Op::Sub,
        Op::MinOf,
        Op::Leq,
        Op::Less,
        Op::Intersect,
        Op::MinElem,
        Op::MaxElem,
        Op::And,
        Op::Or,
        Op::Not,
        Op::Of,
        Op::Lift,
        Op::LiftEntries,
    ];

    /// Whether the lattice `kind` has this operation.
    pub fn applies(self, kind: &Kind) -> bool {
        let set_like = matches!(kind, Kind::Set | Kind::PSet);
        let chain = matches!(kind, Kind::Max | Kind::Min);
        let product = matches!(kind, Kind::Product(_));
        match self {
            Op::Join | Op::Reveal | Op::IsBot | Op::Leq | Op::Of => true,
            // No plain type lifts into a product: its values are written as literals.
            Op::Lift => !product,
            Op::RevealNonBot => matches!(kind, Kind::Max | Kind::Min | Kind::Point),
            Op::Contains | Op::Nonempty | Op::IsEmpty | Op::Intersect | Op::MinElem | Op::MaxElem => set_like,
            Op::Size => set_like || matches!(kind, Kind::Map(_)),
            Op::AtLeast | Op::Above | Op::Sub | Op::MinOf | Op::Less => matches!(kind, Kind::Max),
            Op::AtMost | Op::Below => matches!(kind, Kind::Min),
            Op::Add | Op::AddLat => chain,
            Op::At | Op::HasKey | Op::KeySet => matches!(kind, Kind::Map(_)),
            Op::Sum => matches!(kind, Kind::PSet),
            Op::Get => matches!(kind, Kind::Point),
            Op::And | Op::Or | Op::Not => matches!(kind, Kind::Bool),
            Op::LiftEntries => matches!(kind, Kind::Map(_)),
        }
    }

    /// The operations of lattice `kind` (its catalogue, R04 §2.4).
    pub fn catalogue(kind: &Kind) -> Vec<Op> {
        Op::ALL.iter().copied().filter(|op| op.applies(kind)).collect()
    }

    /// The operation a method call `recv.name(…)` names on lattice `kind`, if any. Operators (`>=`, `+`, …),
    /// constructors and `reveal!` are resolved by the frontend directly.
    pub fn method(kind: &Kind, name: &str) -> Option<Op> {
        let op = match name {
            "join" => Op::Join,
            "is_bot" => Op::IsBot,
            "contains" => Op::Contains,
            "size" => Op::Size,
            "nonempty" => Op::Nonempty,
            "is_empty" => Op::IsEmpty,
            "at" => Op::At,
            "has_key" => Op::HasKey,
            "key_set" => Op::KeySet,
            "sum" => Op::Sum,
            "get" => Op::Get,
            "min_of" => Op::MinOf,
            "leq" => Op::Leq,
            "lt" => Op::Less,
            "intersect" => Op::Intersect,
            "min_elem" => Op::MinElem,
            "max_elem" => Op::MaxElem,
            "and" => Op::And,
            "or" => Op::Or,
            "not" => Op::Not,
            _ => return None,
        };
        op.applies(kind).then_some(op)
    }

    /// The operation named `name` in the IR catalogue of lattice `kind`.
    pub fn from_name(kind: &Kind, name: &str) -> Option<Op> {
        Op::catalogue(kind).into_iter().find(|op| op.name() == name)
    }

    /// The number of arguments, the receiver included (`Of` on `LMap` takes a key and a value).
    pub fn arity(self, kind: &Kind) -> usize {
        match self {
            Op::Reveal
            | Op::RevealNonBot
            | Op::IsBot
            | Op::Size
            | Op::Nonempty
            | Op::IsEmpty
            | Op::KeySet
            | Op::Sum
            | Op::Get
            | Op::MinElem
            | Op::MaxElem
            | Op::Not
            | Op::Lift
            | Op::LiftEntries => 1,
            Op::Of => match kind {
                Kind::Map(_) => 2,
                Kind::Product(fields) => fields.len(),
                _ => 1,
            },
            _ => 2,
        }
    }

    /// The operation's monotonicity on lattice `kind` (LANGUAGE §11.4–11.5).
    pub fn sig(self, kind: &Kind) -> Sig {
        use LatOpKind as K;
        use MonoClass as C;
        let (k, params) = match self {
            Op::Join | Op::AddLat | Op::Intersect | Op::And | Op::Or => {
                (K::Bimorphism, vec![C::Bimorphism, C::Bimorphism])
            }
            Op::Reveal | Op::RevealNonBot => (K::NonMonotone, vec![C::NonMonotone]),
            Op::IsBot | Op::IsEmpty | Op::Not => (K::Antitone, vec![C::Antitone]),
            Op::Contains | Op::AtLeast | Op::Above | Op::AtMost | Op::Below | Op::HasKey => {
                (K::Threshold, vec![C::Threshold, C::Constant])
            }
            Op::Nonempty | Op::Get => (K::Threshold, vec![C::Threshold]),
            Op::Size | Op::Sum => (K::Monotone, vec![C::Monotone]),
            Op::At | Op::Add | Op::Sub | Op::MinOf => (K::Morphism, vec![C::Morphism, C::Constant]),
            Op::KeySet | Op::MinElem | Op::MaxElem => (K::Morphism, vec![C::Morphism]),
            Op::Leq => (K::Antitone, vec![C::Antitone, C::Monotone]),
            Op::Less => (K::NonMonotone, vec![C::NonMonotone, C::NonMonotone]),
            Op::Of => match kind {
                Kind::Map(_) => (K::Morphism, vec![C::Constant, C::Morphism]),
                // A product is built from its fields: a morphism in each (fieldwise merge).
                Kind::Product(fields) if fields.len() == 1 => (K::Morphism, vec![C::Morphism]),
                Kind::Product(fields) => (K::Bimorphism, vec![C::Bimorphism; fields.len()]),
                _ => (K::Morphism, vec![C::Constant]),
            },
            Op::Lift => (K::Morphism, vec![C::Constant]),
            Op::LiftEntries => (K::Morphism, vec![C::Morphism]),
        };
        Sig { kind: k, params }
    }

    /// The name the IR's operation catalogue uses.
    pub const fn name(self) -> &'static str {
        match self {
            Op::Join => "join",
            Op::Reveal => "reveal",
            Op::RevealNonBot => "reveal_nonbot",
            Op::IsBot => "is_bot",
            Op::Contains => "contains",
            Op::Size => "size",
            Op::Nonempty => "nonempty",
            Op::IsEmpty => "is_empty",
            Op::AtLeast => "at_least",
            Op::Above => "above",
            Op::AtMost => "at_most",
            Op::Below => "below",
            Op::At => "at",
            Op::HasKey => "has_key",
            Op::KeySet => "key_set",
            Op::Sum => "sum",
            Op::Get => "get",
            Op::Add => "add",
            Op::AddLat => "add_lat",
            Op::Sub => "sub",
            Op::MinOf => "min_of",
            Op::Leq => "leq",
            Op::Less => "lt",
            Op::Intersect => "intersect",
            Op::MinElem => "min_elem",
            Op::MaxElem => "max_elem",
            Op::And => "and",
            Op::Or => "or",
            Op::Not => "not",
            Op::Of => "of",
            Op::Lift => "lift",
            Op::LiftEntries => "lift_entries",
        }
    }
}

fn shape(what: &str, v: &LatValue) -> LatticeError {
    LatticeError::Shape(format!("{what}: unexpected lattice value {v:?}"))
}

impl Kind {
    /// ⊥.
    pub fn bottom(&self) -> LatValue {
        match self {
            Kind::Bool => LatValue::Bool(false),
            Kind::Max | Kind::Min | Kind::Point => LatValue::Bottom,
            Kind::Set | Kind::PSet => LatValue::Set(Arc::new(BTreeSet::new())),
            Kind::Map(_) => LatValue::Map(Arc::new(BTreeMap::new())),
            Kind::Product(fields) => LatValue::Seq(fields.iter().map(Kind::bottom).collect()),
        }
    }

    /// Whether `v` is ⊥ (SEM-101: a ⊥ cell is absent).
    pub fn is_bottom(&self, v: &LatValue) -> bool {
        match (self, v) {
            (Kind::Bool, LatValue::Bool(b)) => !b,
            (_, LatValue::Bottom) => true,
            (Kind::Set | Kind::PSet, LatValue::Set(s)) => s.is_empty(),
            (Kind::Map(_), LatValue::Map(m)) => m.is_empty(),
            (Kind::Product(fields), LatValue::Seq(vs)) => {
                fields.len() == vs.len() && fields.iter().zip(vs.iter()).all(|(k, v)| k.is_bottom(v))
            }
            _ => false,
        }
    }

    /// Field `i` of a product value (a morphism, LANGUAGE §11.8).
    pub fn field(&self, v: &LatValue, i: usize) -> Result<LatValue, LatticeError> {
        match (self, v) {
            (Kind::Product(fields), LatValue::Seq(vs)) if fields.len() == vs.len() => {
                vs.get(i).cloned().ok_or_else(|| shape("a field read", v))
            }
            _ => Err(shape("a field read", v)),
        }
    }

    /// The fields of a product value, after checking its shape.
    fn fields<'v>(&self, fields: &[Kind], v: &'v LatValue) -> Result<&'v [LatValue], LatticeError> {
        match v {
            LatValue::Seq(vs) if vs.len() == fields.len() => Ok(vs),
            other => Err(LatticeError::Shape(format!("{other:?} is not a value of {self:?}"))),
        }
    }

    /// `a ⊔ b`.
    pub fn join(&self, a: &LatValue, b: &LatValue) -> Result<LatValue, LatticeError> {
        Ok(match (self, a, b) {
            (Kind::Bool, LatValue::Bool(x), LatValue::Bool(y)) => LatValue::Bool(*x || *y),
            (Kind::Max | Kind::Min | Kind::Point, LatValue::Bottom, other)
            | (Kind::Max | Kind::Min | Kind::Point, other, LatValue::Bottom) => other.clone(),
            (Kind::Max, LatValue::Elem(x), LatValue::Elem(y)) => {
                LatValue::Elem(if x >= y { x.clone() } else { y.clone() })
            }
            (Kind::Min, LatValue::Elem(x), LatValue::Elem(y)) => {
                LatValue::Elem(if x <= y { x.clone() } else { y.clone() })
            }
            (Kind::Point, LatValue::Elem(x), LatValue::Elem(y)) => {
                if x == y {
                    LatValue::Elem(x.clone())
                } else {
                    return Err(LatticeError::Conflict(Box::new((**x).clone()), Box::new((**y).clone())));
                }
            }
            (Kind::Set | Kind::PSet, LatValue::Set(x), LatValue::Set(y)) => {
                if y.is_subset(x) {
                    LatValue::Set(x.clone())
                } else if x.is_subset(y) {
                    LatValue::Set(y.clone())
                } else {
                    LatValue::Set(Arc::new(x.union(y).cloned().collect()))
                }
            }
            (Kind::Map(inner), LatValue::Map(x), LatValue::Map(y)) => {
                let mut out = (**x).clone();
                for (k, v) in y.iter() {
                    let merged = match out.get(k) {
                        Some(old) => inner.join(old, v)?,
                        None => v.clone(),
                    };
                    if !inner.is_bottom(&merged) {
                        out.insert(k.clone(), merged);
                    }
                }
                LatValue::Map(Arc::new(out))
            }
            (Kind::Product(fields), x, y) => {
                let (xs, ys) = (self.fields(fields, x)?, self.fields(fields, y)?);
                let mut out = Vec::with_capacity(fields.len());
                for ((k, a), b) in fields.iter().zip(xs).zip(ys) {
                    out.push(k.join(a, b)?);
                }
                LatValue::Seq(out.into())
            }
            (_, x, y) => return Err(LatticeError::Shape(format!("join of {x:?} and {y:?} in {self:?}"))),
        })
    }

    /// `a ⊑ b`, by the order itself (not through `join`, which fails on two different `LPoint` values).
    pub fn leq(&self, a: &LatValue, b: &LatValue) -> Result<bool, LatticeError> {
        Ok(match (self, a, b) {
            (Kind::Bool, LatValue::Bool(x), LatValue::Bool(y)) => !x || *y,
            (Kind::Max | Kind::Min | Kind::Point, LatValue::Bottom, _) => true,
            (Kind::Max | Kind::Min | Kind::Point, LatValue::Elem(_), LatValue::Bottom) => false,
            (Kind::Max, LatValue::Elem(x), LatValue::Elem(y)) => x <= y,
            (Kind::Min, LatValue::Elem(x), LatValue::Elem(y)) => x >= y,
            (Kind::Point, LatValue::Elem(x), LatValue::Elem(y)) => x == y,
            (Kind::Set | Kind::PSet, LatValue::Set(x), LatValue::Set(y)) => x.is_subset(y),
            (Kind::Map(inner), LatValue::Map(x), LatValue::Map(y)) => {
                for (k, v) in x.iter() {
                    match y.get(k) {
                        Some(w) if inner.leq(v, w)? => {}
                        _ => return Ok(false),
                    }
                }
                true
            }
            (Kind::Product(fields), x, y) => {
                let (xs, ys) = (self.fields(fields, x)?, self.fields(fields, y)?);
                for ((k, a), b) in fields.iter().zip(xs).zip(ys) {
                    if !k.leq(a, b)? {
                        return Ok(false);
                    }
                }
                true
            }
            (_, x, y) => return Err(LatticeError::Shape(format!("comparing {x:?} and {y:?} in {self:?}"))),
        })
    }

    /// Lifts a plain value into the lattice (LANGUAGE §5.6): `T` into `LMax`/`LMin`/`LPoint`, `bool` into `LBool`,
    /// `Set<T>` into `LSet<T>`, `Map<K, V>` into `LMap<K, L>` with each value lifted.
    pub fn lift(&self, v: &Value) -> Result<LatValue, LatticeError> {
        Ok(match (self, v) {
            (_, Value::Lattice(l)) => l.clone(),
            (Kind::Bool, Value::Bool(b)) => LatValue::Bool(*b),
            (Kind::Max | Kind::Min | Kind::Point, x) => LatValue::Elem(Arc::new(x.clone())),
            (Kind::Set | Kind::PSet, Value::Set(s)) => {
                for x in s.iter() {
                    self.check_element(x)?;
                }
                LatValue::Set(s.clone())
            }
            (Kind::Map(inner), Value::Map(m)) => {
                let mut out = BTreeMap::new();
                for (k, x) in m.iter() {
                    let lv = inner.lift(x)?;
                    if !inner.is_bottom(&lv) {
                        out.insert(k.clone(), lv);
                    }
                }
                LatValue::Map(Arc::new(out))
            }
            (_, other) => return Err(LatticeError::Shape(format!("cannot lift {other:?} into {self:?}"))),
        })
    }

    /// `reveal!(x)`: the exact value, deep (LANGUAGE §11.4). `LMax`/`LMin`/`LPoint` reveal to `Option<T>` unless
    /// `non_bottom` (the non-⊥ refinement), `LSet` to `Set<T>`, `LMap` to a map of revealed non-⊥ values.
    pub fn reveal(&self, v: &LatValue, non_bottom: bool) -> Result<Value, LatticeError> {
        Ok(match (self, v) {
            (Kind::Bool, LatValue::Bool(b)) => Value::Bool(*b),
            (Kind::Max | Kind::Min | Kind::Point, LatValue::Bottom) => {
                if non_bottom {
                    return Err(LatticeError::Shape("reveal of ⊥ under the non-⊥ refinement".into()));
                }
                Value::none()
            }
            (Kind::Max | Kind::Min | Kind::Point, LatValue::Elem(x)) => {
                if non_bottom {
                    (**x).clone()
                } else {
                    Value::some((**x).clone())
                }
            }
            (Kind::Set | Kind::PSet, LatValue::Set(s)) => Value::Set(s.clone()),
            (Kind::Map(inner), LatValue::Map(m)) => {
                let mut out = BTreeMap::new();
                for (k, x) in m.iter() {
                    out.insert(k.clone(), inner.reveal(x, true)?);
                }
                Value::Map(Arc::new(out))
            }
            // The struct of the fields' reveals (a field may be ⊥, so it is not refined).
            (Kind::Product(fields), v) => {
                let vs = self.fields(fields, v)?;
                let mut out = Vec::with_capacity(fields.len());
                for (k, x) in fields.iter().zip(vs) {
                    out.push(k.reveal(x, false)?);
                }
                Value::Struct(out.into())
            }
            (_, other) => return Err(shape("reveal", other)),
        })
    }

    /// Evaluates `op` on `args`, the receiver first (a lattice value) for every operation but [`Op::Of`] and
    /// [`Op::Lift`].
    pub fn eval(&self, op: Op, args: &[Value]) -> Result<Value, LatticeError> {
        if args.len() != op.arity(self) {
            return Err(LatticeError::Shape(format!(
                "{} takes {} argument(s), {} given",
                op.name(),
                op.arity(self),
                args.len()
            )));
        }
        let arg = |i: usize| {
            args.get(i)
                .ok_or_else(|| LatticeError::Shape(format!("{} is missing argument {i}", op.name())))
        };
        let lat = |i: usize| -> Result<&LatValue, LatticeError> {
            match arg(i)? {
                Value::Lattice(l) => Ok(l),
                other => Err(LatticeError::Shape(format!(
                    "{}: {other:?} is not a lattice value",
                    op.name()
                ))),
            }
        };
        let elems = |v: &LatValue| -> Result<Arc<BTreeSet<Value>>, LatticeError> {
            match v {
                LatValue::Set(s) => Ok(s.clone()),
                other => Err(shape(op.name(), other)),
            }
        };
        let elem = |v: Value| Value::Lattice(LatValue::Elem(Arc::new(v)));
        let max_u64 = |n: usize| -> Result<Value, LatticeError> {
            let n = u64::try_from(n).map_err(|_| LatticeError::Arithmetic("size overflows u64".into()))?;
            Ok(elem(Value::Int(IntValue::U64(n))))
        };
        let threshold = |cmp: fn(&Value, &Value) -> bool| -> Result<Value, LatticeError> {
            let c = arg(1)?;
            Ok(Value::Bool(match lat(0)? {
                LatValue::Bottom => false,
                LatValue::Elem(x) => cmp(x, c),
                other => return Err(shape(op.name(), other)),
            }))
        };
        // `x ∘ c` on a chain element; ⊥ (an adjoined infinity) absorbs.
        let scalar = |f: fn(IntValue, IntValue) -> Result<IntValue, LatticeError>| -> Result<Value, LatticeError> {
            match (lat(0)?, arg(1)?) {
                (LatValue::Bottom, _) => Ok(Value::Lattice(LatValue::Bottom)),
                (LatValue::Elem(x), Value::Int(c)) => match &**x {
                    Value::Int(i) => Ok(elem(Value::Int(f(*i, *c)?))),
                    other => Err(LatticeError::Shape(format!("{} over {other:?}", op.name()))),
                },
                (other, c) => Err(LatticeError::Shape(format!("{} of {other:?} and {c:?}", op.name()))),
            }
        };
        match op {
            Op::Join => {
                let other = match arg(1)? {
                    Value::Lattice(l) => l.clone(),
                    plain => self.lift(plain)?,
                };
                Ok(Value::Lattice(self.join(lat(0)?, &other)?))
            }
            Op::Reveal => self.reveal(lat(0)?, false),
            Op::RevealNonBot => self.reveal(lat(0)?, true),
            Op::IsBot => Ok(Value::Bool(self.is_bottom(lat(0)?))),
            Op::Contains => Ok(Value::Bool(elems(lat(0)?)?.contains(arg(1)?))),
            Op::Size => match lat(0)? {
                LatValue::Set(s) => max_u64(s.len()),
                LatValue::Map(m) => max_u64(m.len()),
                other => Err(shape("size", other)),
            },
            Op::Nonempty => Ok(Value::Bool(!elems(lat(0)?)?.is_empty())),
            Op::IsEmpty => Ok(Value::Bool(elems(lat(0)?)?.is_empty())),
            Op::AtLeast => threshold(|x, c| x >= c),
            Op::Above => threshold(|x, c| x > c),
            Op::AtMost => threshold(|x, c| x <= c),
            Op::Below => threshold(|x, c| x < c),
            Op::At => {
                let Kind::Map(inner) = self else {
                    return Err(shape("at", lat(0)?));
                };
                match lat(0)? {
                    LatValue::Map(m) => Ok(Value::Lattice(
                        m.get(arg(1)?).cloned().unwrap_or_else(|| inner.bottom()),
                    )),
                    other => Err(shape("at", other)),
                }
            }
            Op::HasKey => match lat(0)? {
                LatValue::Map(m) => Ok(Value::Bool(m.contains_key(arg(1)?))),
                other => Err(shape("has_key", other)),
            },
            Op::KeySet => match lat(0)? {
                LatValue::Map(m) => Ok(Value::Lattice(LatValue::Set(Arc::new(m.keys().cloned().collect())))),
                other => Err(shape("key_set", other)),
            },
            Op::Sum => {
                let s = elems(lat(0)?)?;
                let mut acc: Option<IntValue> = None;
                for v in s.iter() {
                    let Value::Int(i) = v else {
                        return Err(LatticeError::Shape(format!("sum over {v:?}")));
                    };
                    acc = Some(match acc {
                        None => *i,
                        Some(a) => add(a, *i)?,
                    });
                }
                Ok(Value::Lattice(match acc {
                    None => LatValue::Bottom,
                    Some(a) => LatValue::Elem(Arc::new(Value::Int(a))),
                }))
            }
            Op::Get => Ok(match lat(0)? {
                LatValue::Bottom => Value::none(),
                LatValue::Elem(x) => Value::some((**x).clone()),
                other => return Err(shape("get", other)),
            }),
            Op::Add => scalar(add),
            Op::Sub => scalar(sub),
            Op::MinOf => match (lat(0)?, arg(1)?) {
                (LatValue::Bottom, _) => Ok(Value::Lattice(LatValue::Bottom)),
                (LatValue::Elem(x), c) => Ok(elem(if **x <= *c { (**x).clone() } else { c.clone() })),
                (other, _) => Err(shape("min_of", other)),
            },
            Op::AddLat => match (lat(0)?, lat(1)?) {
                (LatValue::Bottom, _) | (_, LatValue::Bottom) => Ok(Value::Lattice(LatValue::Bottom)),
                (LatValue::Elem(x), LatValue::Elem(y)) => match (&**x, &**y) {
                    (Value::Int(a), Value::Int(b)) => Ok(elem(Value::Int(add(*a, *b)?))),
                    (a, b) => Err(LatticeError::Shape(format!("add_lat of {a:?} and {b:?}"))),
                },
                (a, b) => Err(LatticeError::Shape(format!("add_lat of {a:?} and {b:?}"))),
            },
            Op::Leq => Ok(Value::Bool(self.leq(lat(0)?, lat(1)?)?)),
            Op::Less => {
                let (a, b) = (lat(0)?, lat(1)?);
                Ok(Value::Bool(a != b && self.leq(a, b)?))
            }
            Op::Intersect => {
                let (a, b) = (elems(lat(0)?)?, elems(lat(1)?)?);
                Ok(Value::Lattice(LatValue::Set(Arc::new(
                    a.intersection(&b).cloned().collect(),
                ))))
            }
            Op::MinElem | Op::MaxElem => {
                let s = elems(lat(0)?)?;
                let pick = if op == Op::MinElem {
                    s.iter().next()
                } else {
                    s.iter().next_back()
                };
                Ok(Value::Lattice(match pick {
                    None => LatValue::Bottom,
                    Some(x) => LatValue::Elem(Arc::new(x.clone())),
                }))
            }
            Op::And | Op::Or => match (lat(0)?, lat(1)?) {
                (LatValue::Bool(a), LatValue::Bool(b)) => Ok(Value::Lattice(LatValue::Bool(if op == Op::And {
                    *a && *b
                } else {
                    *a || *b
                }))),
                (a, b) => Err(LatticeError::Shape(format!("{} of {a:?} and {b:?}", op.name()))),
            },
            Op::Not => match lat(0)? {
                LatValue::Bool(a) => Ok(Value::Lattice(LatValue::Bool(!a))),
                other => Err(shape("not", other)),
            },
            Op::Of => match self {
                Kind::Set | Kind::PSet => {
                    let x = arg(0)?;
                    self.check_element(x)?;
                    Ok(Value::Lattice(LatValue::Set(Arc::new(BTreeSet::from([x.clone()])))))
                }
                Kind::Map(inner) => {
                    let v = inner.lift(arg(1)?)?;
                    let mut m = BTreeMap::new();
                    if !inner.is_bottom(&v) {
                        m.insert(arg(0)?.clone(), v);
                    }
                    Ok(Value::Lattice(LatValue::Map(Arc::new(m))))
                }
                Kind::Product(fields) => {
                    let mut out = Vec::with_capacity(fields.len());
                    for (k, x) in fields.iter().zip(args) {
                        out.push(k.lift(x)?);
                    }
                    Ok(Value::Lattice(LatValue::Seq(out.into())))
                }
                _ => Ok(Value::Lattice(self.lift(arg(0)?)?)),
            },
            Op::Lift | Op::LiftEntries => Ok(Value::Lattice(self.lift(arg(0)?)?)),
        }
    }

    /// `LPSet` holds non-negative numbers only (LANGUAGE §11.5), so that `sum` is monotone.
    fn check_element(&self, x: &Value) -> Result<(), LatticeError> {
        if *self == Kind::PSet {
            let negative = match x {
                Value::Int(i) => i.to_i128().is_none_or(|n| n < 0),
                _ => true,
            };
            if negative {
                return Err(LatticeError::Domain(format!(
                    "{x:?} is not a non-negative number (LPSet)"
                )));
            }
        }
        Ok(())
    }
}

fn sub(a: IntValue, b: IntValue) -> Result<IntValue, LatticeError> {
    let overflow = || LatticeError::Arithmetic(format!("{a:?} - {b:?} overflows"));
    if a.ty() != b.ty() {
        return Err(LatticeError::Shape(format!("subtracting {b:?} from {a:?}")));
    }
    let (Some(x), Some(y)) = (a.to_i128(), b.to_i128()) else {
        return Err(overflow());
    };
    let s = x.checked_sub(y).ok_or_else(overflow)?;
    IntValue::from_i128(a.ty(), s).ok_or_else(overflow)
}

fn add(a: IntValue, b: IntValue) -> Result<IntValue, LatticeError> {
    let overflow = || LatticeError::Arithmetic(format!("{a:?} + {b:?} overflows"));
    if a.ty() != b.ty() {
        return Err(LatticeError::Shape(format!("adding {a:?} and {b:?}")));
    }
    let (Some(x), Some(y)) = (a.to_i128(), b.to_i128()) else {
        return Err(overflow());
    };
    let s = x.checked_add(y).ok_or_else(overflow)?;
    IntValue::from_i128(a.ty(), s).ok_or_else(overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: u64) -> Value {
        Value::Int(IntValue::U64(n))
    }

    fn set(xs: &[u64]) -> LatValue {
        LatValue::Set(Arc::new(xs.iter().map(|x| int(*x)).collect()))
    }

    #[test]
    fn set_join_is_union_and_size_is_monotone() {
        let k = Kind::Set;
        let j = k.join(&set(&[1, 2]), &set(&[2, 3])).unwrap();
        assert_eq!(j, set(&[1, 2, 3]));
        assert!(k.leq(&set(&[1]), &j).unwrap());
        assert_eq!(
            k.eval(Op::Size, &[Value::Lattice(j.clone())]).unwrap(),
            Value::Lattice(LatValue::Elem(Arc::new(int(3))))
        );
        assert_eq!(
            k.eval(Op::Contains, &[Value::Lattice(j.clone()), int(2)]).unwrap(),
            Value::Bool(true)
        );
        assert!(k.is_bottom(&k.bottom()));
    }

    #[test]
    fn max_min_thresholds_and_bottom() {
        let e = |n| LatValue::Elem(Arc::new(int(n)));
        assert_eq!(Kind::Max.join(&e(3), &e(5)).unwrap(), e(5));
        assert_eq!(Kind::Min.join(&e(3), &e(5)).unwrap(), e(3));
        assert_eq!(Kind::Max.join(&LatValue::Bottom, &e(1)).unwrap(), e(1));
        assert_eq!(
            Kind::Max.eval(Op::AtLeast, &[Value::Lattice(e(5)), int(4)]).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            Kind::Max
                .eval(Op::AtLeast, &[Value::Lattice(LatValue::Bottom), int(0)])
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(Kind::Max.reveal(&LatValue::Bottom, false).unwrap(), Value::none());
    }

    #[test]
    fn point_conflicts_and_map_joins_values() {
        let e = |n| LatValue::Elem(Arc::new(int(n)));
        assert!(matches!(
            Kind::Point.join(&e(1), &e(2)),
            Err(LatticeError::Conflict(..))
        ));
        let k = Kind::Map(Box::new(Kind::Max));
        let m = |pairs: &[(u64, u64)]| LatValue::Map(Arc::new(pairs.iter().map(|(a, b)| (int(*a), e(*b))).collect()));
        assert_eq!(
            k.join(&m(&[(1, 3)]), &m(&[(1, 5), (2, 1)])).unwrap(),
            m(&[(1, 5), (2, 1)])
        );
        assert_eq!(
            k.eval(Op::At, &[Value::Lattice(m(&[(1, 3)])), int(9)]).unwrap(),
            Value::Lattice(LatValue::Bottom)
        );
    }

    #[test]
    fn classes_follow_the_bang_rule() {
        assert!(!Op::method(&Kind::Set, "contains").unwrap().sig(&Kind::Set).needs_bang());
        assert!(Op::method(&Kind::Set, "is_empty").unwrap().sig(&Kind::Set).needs_bang());
        assert!(Op::Reveal.sig(&Kind::Max).needs_bang());
        assert!(Op::method(&Kind::Max, "leq").unwrap().sig(&Kind::Max).needs_bang());
        assert!(Op::method(&Kind::Min, "contains").is_none());
        for kind in [
            Kind::Bool,
            Kind::Max,
            Kind::Min,
            Kind::Set,
            Kind::PSet,
            Kind::Point,
            Kind::Map(Box::new(Kind::Max)),
        ] {
            for op in Op::catalogue(&kind) {
                assert_eq!(Op::from_name(&kind, op.name()), Some(op), "{kind:?} {op:?}");
                assert_eq!(op.sig(&kind).params.len(), op.arity(&kind), "{kind:?} {op:?}");
            }
        }
    }

    #[test]
    fn order_without_join() {
        let e = |n| LatValue::Elem(Arc::new(int(n)));
        assert!(!Kind::Point.leq(&e(5), &e(6)).unwrap());
        assert!(Kind::Point.leq(&LatValue::Bottom, &e(6)).unwrap());
        assert!(Kind::Min.leq(&e(6), &e(5)).unwrap());
        let m = Kind::Map(Box::new(Kind::Point));
        let map = |pairs: &[(u64, u64)]| LatValue::Map(Arc::new(pairs.iter().map(|(a, b)| (int(*a), e(*b))).collect()));
        assert!(!m.leq(&map(&[(1, 5)]), &map(&[(1, 6)])).unwrap());
        assert!(m.leq(&map(&[(1, 5)]), &map(&[(1, 5), (2, 1)])).unwrap());
        assert_eq!(
            Kind::Point
                .eval(Op::Leq, &[Value::Lattice(e(5)), Value::Lattice(e(6))])
                .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn chain_arithmetic_constructors_and_domains() {
        let e = |n| LatValue::Elem(Arc::new(int(n)));
        let l = Value::Lattice;
        assert_eq!(Kind::Min.eval(Op::Add, &[l(e(3)), int(4)]).unwrap(), l(e(7)));
        assert_eq!(
            Kind::Min.eval(Op::Add, &[l(LatValue::Bottom), int(4)]).unwrap(),
            l(LatValue::Bottom)
        );
        assert_eq!(Kind::Min.eval(Op::AddLat, &[l(e(3)), l(e(4))]).unwrap(), l(e(7)));
        assert!(matches!(
            Kind::Max.eval(Op::Sub, &[l(e(3)), int(4)]),
            Err(LatticeError::Arithmetic(_))
        ));
        assert_eq!(Kind::Set.eval(Op::Of, &[int(2)]).unwrap(), l(set(&[2])));
        assert_eq!(Kind::Max.eval(Op::Of, &[int(2)]).unwrap(), l(e(2)));
        let neg = Value::Int(IntValue::I64(-1));
        assert!(matches!(Kind::PSet.eval(Op::Of, &[neg]), Err(LatticeError::Domain(_))));
        assert_eq!(Kind::Set.eval(Op::MinElem, &[l(set(&[4, 2]))]).unwrap(), l(e(2)));
        assert_eq!(Kind::Max.eval(Op::RevealNonBot, &[l(e(2))]).unwrap(), int(2));
        assert!(Kind::Max.eval(Op::Size, &[l(e(2))]).is_err());
    }
}
