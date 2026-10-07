//! The law harness (TEST-083, LANGUAGE §11.8): the laws of a program's user-defined lattices and the class claims of
//! their methods.
//!
//! A product lattice's merge, ⊥ and order are its fields', so its laws hold by construction ("proven"); the harness
//! still checks them on its samples, and a failure there is a bug of the lattice library. Every classed method of a
//! product is a claim: `morphism`, `bimorphism`, `monotone`, `antitone`, `threshold` and `stable … after t` each say
//! how the method relates to the order, and the harness tests the claim on values generated from the lattice's
//! types, over small domains (so that thresholds are reached and keys collide), calling the method through the
//! oracle. A claim that fails is refuted (BLS0704) with its counterexample; one that holds on every case is
//! "tested". A method with no class claims nothing. A method whose parameters the harness cannot generate values for
//! is reported untested, with the reason.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{Diagnostic, Diagnostics, FnId, InternalError, LatticeTypeId, Symbol, TypeId, code, internal_error};
use blossom_ir::core::{LatOpImpl, LatOpKind, LatticeCtor, LawStatus, Program};
use blossom_lattice::Kind;
use blossom_lattice::laws::{self, Arg, Checked, Claim, LawError, Tally};
use blossom_oracle::{CallError, Limits, Oracle, OracleError};
use blossom_value::time::{Duration, Instant, NodeId};
use blossom_value::value::{IntValue, LatValue};
use blossom_value::{ExternRegistry, TypeDef, Value};

/// The argument tuples a claim is checked on (each against every sample of the argument it varies).
const TUPLES: usize = 64;
/// The most samples of one type.
const SAMPLES: usize = 40;
/// How deep generated values nest.
const DEPTH: u32 = 4;

/// What the harness found, per user-defined lattice.
#[derive(Debug)]
pub struct Report {
    pub lattices: Vec<LatticeLaws>,
}

/// One lattice's laws and its methods' claims.
#[derive(Debug)]
pub struct LatticeLaws {
    pub name: String,
    /// The status of its merge, ⊥ and order (`Proved` for a product).
    pub status: LawStatus,
    /// The cases its merge laws were checked on.
    pub merge: Tally,
    pub claims: Vec<ClaimLaws>,
}

/// One method's claim and what became of it.
#[derive(Debug)]
pub struct ClaimLaws {
    pub method: Symbol,
    /// The method's function (its span is the artifact's `methods`).
    pub f: FnId,
    pub claim: Claim,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub enum Outcome {
    /// The claim held on every case.
    Tested(Tally),
    /// The claim failed on a case.
    Refuted(Box<Refutation>),
    /// The harness cannot generate the method's arguments.
    Untested(String),
}

/// A refuted claim: the law, what failed, and the values it failed on as Blossom writes them.
#[derive(Debug)]
pub struct Refutation {
    pub law: &'static str,
    pub detail: String,
    pub values: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum LawsError {
    #[error(transparent)]
    Oracle(#[from] OracleError),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// Checks the laws of every user-defined lattice of `artifact` and the claims of its methods, which may call the
/// host functions in `externs`.
pub fn check(artifact: &BlsArtifact, externs: Arc<ExternRegistry>) -> Result<Report, LawsError> {
    let p = artifact.program.get();
    let products: Vec<LatticeTypeId> = p
        .lattices
        .iter_enumerated()
        .filter(|(_, l)| matches!(l.ctor, LatticeCtor::Product { .. }))
        .map(|(id, _)| id)
        .collect();
    let mut out = Report { lattices: Vec::new() };
    if products.is_empty() {
        return Ok(out);
    }
    let oracle = Oracle::with_externs(artifact.program.clone(), Limits::default(), externs)?;
    let samples_of = Gen { p };
    for id in products {
        let def = p
            .lattices
            .get(id)
            .ok_or_else(|| internal_error!("lattice {id:?} is not declared"))?;
        let kind = samples_of
            .kind(id)
            .ok_or_else(|| internal_error!("lattice {} has no kind", def.name))?;
        let samples = samples_of
            .lattice(id, DEPTH)
            .map_err(|e| internal_error!("lattice {}: {e}", def.name))?;
        let merge = match laws::merge_laws(&kind, &samples) {
            Ok(t) => t,
            Err(e) => return Err(internal_error!("the merge of {} breaks a law: {e}", def.name).into()),
        };
        let mut claims = Vec::new();
        for op in &def.ops {
            let LatOpImpl::Method(f) = op.imp else { continue };
            let claim = match op.kind {
                LatOpKind::Morphism => Claim::Morphism,
                LatOpKind::Bimorphism => Claim::Bimorphism,
                LatOpKind::Monotone => Claim::Monotone,
                LatOpKind::Antitone => Claim::Antitone,
                LatOpKind::Threshold => Claim::Threshold,
                LatOpKind::Stable { .. } => Claim::Stable,
                // No class, or a stable method's exact entry: nothing claimed.
                LatOpKind::NonMonotone => continue,
            };
            let method = Symbol::intern(op.name.as_str());
            let outcome = claim_outcome(&samples_of, &oracle, id, op, claim)?;
            claims.push(ClaimLaws {
                method,
                f,
                claim,
                outcome,
            });
        }
        out.lattices.push(LatticeLaws {
            name: def.name.to_string(),
            status: def.laws,
            merge,
            claims,
        });
    }
    Ok(out)
}

/// Tests one claim.
fn claim_outcome(
    samples_of: &Gen<'_>,
    oracle: &Oracle,
    lattice: LatticeTypeId,
    op: &blossom_ir::core::LatOpDecl,
    claim: Claim,
) -> Result<Outcome, LawsError> {
    let mut args = Vec::new();
    for (ty, _) in &op.params {
        let samples = match samples_of.values(*ty, DEPTH) {
            Ok(s) => s,
            Err(why) => return Ok(Outcome::Untested(why)),
        };
        args.push(Arg {
            kind: samples_of.kind_of_type(*ty),
            samples,
        });
    }
    // A program error is a case set aside; an evaluator error stops the harness.
    let fatal: RefCell<Option<OracleError>> = RefCell::new(None);
    let call = |name: Symbol, xs: &[Value]| -> Result<Value, String> {
        oracle.eval_op(lattice, name, xs).map_err(|e| match e {
            CallError::Program(m) => m,
            CallError::Oracle(e) => {
                let text = e.to_string();
                fatal.borrow_mut().get_or_insert(e);
                text
            }
        })
    };
    let eval = |xs: &[Value]| call(op.name, xs);
    let after = match op.kind {
        LatOpKind::Stable { after } => Some(after),
        _ => None,
    };
    // A threshold's and a stable read's claims say something only where the threshold holds, which random samples
    // seldom reach: add the joins of two samples on which it does.
    let threshold = match claim {
        Claim::Threshold => Some(op.name),
        Claim::Stable => after,
        _ => None,
    };
    if let (Some(t), Some(kind), Some(first)) = (
        threshold,
        args.first().and_then(|a| a.kind.clone()),
        args.first().map(|a| a.samples.clone()),
    ) {
        let mut reached = BTreeSet::new();
        'outer: for x in &first {
            for y in &first {
                let (Value::Lattice(a), Value::Lattice(b)) = (x, y) else {
                    continue;
                };
                let Ok(j) = kind.join(a, b) else { continue };
                // The threshold alone, on the receiver (a threshold method takes no other lattice).
                let mut xs = vec![Value::Lattice(j.clone())];
                xs.extend(args.iter().skip(1).filter_map(|a| a.samples.first().cloned()));
                let on = if claim == Claim::Stable {
                    xs.get(..1).unwrap_or(&[])
                } else {
                    &xs[..]
                };
                let holds = match call(t, on) {
                    Ok(Value::Bool(b)) => b,
                    Ok(Value::Option(o)) => o.is_some(),
                    _ => false,
                };
                if holds && !first.contains(&Value::Lattice(j.clone())) {
                    reached.insert(Value::Lattice(j));
                    if reached.len() >= SAMPLES {
                        break 'outer;
                    }
                }
            }
        }
        if let Some(a) = args.first_mut() {
            a.samples.extend(reached);
        }
    }
    let guard = |xs: &[Value]| match after {
        Some(t) => call(t, xs),
        None => Err("no threshold".to_owned()),
    };
    let result = {
        let checked = Checked {
            claim,
            args,
            result: samples_of.kind_of_type(op.ret),
            eval: &eval,
            guard: after.map(|_| &guard as laws::Eval<'_>),
        };
        laws::check_claim(&checked, TUPLES)
    };
    if let Some(e) = fatal.borrow_mut().take() {
        return Err(e.into());
    }
    Ok(match result {
        Ok(t) => Outcome::Tested(t),
        Err(LawError::Refuted(c)) => {
            let ret = op.ret;
            let varied = op.params.get(c.varied).map(|(t, _)| *t);
            let printed = c
                .values
                .iter()
                .map(|(name, v)| {
                    // Results are of the method's result type; everything else of its receiver's or arguments'.
                    // `a` and `b` are of the argument varied; the others by position.
                    let ty = if name.starts_with("f(") {
                        Some(ret)
                    } else if name == "a" || name == "b" {
                        varied
                    } else {
                        name.strip_prefix("argument ")
                            .and_then(|k| k.parse::<usize>().ok())
                            .and_then(|k| op.params.get(k))
                            .map(|(t, _)| *t)
                    };
                    (name.clone(), text(samples_of.p, v, ty))
                })
                .collect();
            Outcome::Refuted(Box::new(Refutation {
                law: c.law,
                detail: c.detail,
                values: printed,
            }))
        }
        Err(LawError::Lattice(e)) => {
            return Err(internal_error!("checking `{}` of lattice {lattice:?}: {e}", op.name).into());
        }
    })
}

/// A value as Blossom writes it.
fn text(p: &Program, v: &Value, ty: Option<TypeId>) -> String {
    match ty {
        Some(t) => blossom_ir::printer::to_string_text(p, v, t, &[]),
        None => blossom_ir::printer::value_text(Some(p), v, None, &|n: NodeId| format!("node#{}", n.0)),
    }
}

/// The BLS0704 error of every refuted claim of `report` (at the method), with its counterexample.
pub fn diagnostics(report: &Report, artifact: &BlsArtifact) -> Diagnostics {
    let mut out = Diagnostics::new();
    for l in &report.lattices {
        for c in &l.claims {
            let Outcome::Refuted(r) = &c.outcome else { continue };
            let mut d = Diagnostic::new(
                code!("BLS0704"),
                format!(
                    "`{}` of `{}` is declared `{}`, but the law harness refutes it: {}",
                    c.method,
                    l.name,
                    claim_word(c.claim),
                    r.detail
                ),
            );
            if let Some(span) = artifact.methods.get(&c.f) {
                d = d.with_primary(*span);
            }
            for (name, v) in &r.values {
                d = d.with_note(format!("{name} = {v}"));
            }
            out.push(d);
        }
    }
    out
}

/// A claim as the method's class prefix spells it.
pub fn claim_word(c: Claim) -> &'static str {
    match c {
        Claim::Morphism => "morphism",
        Claim::Bimorphism => "bimorphism",
        Claim::Monotone => "monotone",
        Claim::Antitone => "antitone",
        Claim::Threshold => "threshold",
        Claim::Stable => "stable",
    }
}

/// Generates sample values of a program's types: small domains, so that values collide and thresholds are reached.
struct Gen<'p> {
    p: &'p Program,
}

impl Gen<'_> {
    fn def(&self, ty: TypeId) -> Result<&TypeDef, String> {
        self.p
            .types
            .get(ty)
            .ok_or_else(|| format!("type {ty:?} is not declared"))
    }

    fn name(&self, ty: TypeId) -> String {
        blossom_ir::printer::type_text(self.p, ty)
    }

    /// The built-in lattice of lattice `id`.
    fn kind(&self, id: LatticeTypeId) -> Option<Kind> {
        let def = self.p.lattices.get(id)?;
        Some(match &def.ctor {
            LatticeCtor::Bool => Kind::Bool,
            LatticeCtor::Max(_) => Kind::Max,
            LatticeCtor::Min(_) => Kind::Min,
            LatticeCtor::Set(_) => Kind::Set,
            LatticeCtor::PSet(_) => Kind::PSet,
            LatticeCtor::Point(_) => Kind::Point,
            LatticeCtor::Map(_, inner) => Kind::Map(Box::new(self.kind(*inner)?)),
            LatticeCtor::Product { fields, .. } => Kind::Product(
                fields
                    .iter()
                    .map(|(_, f)| self.kind(*f))
                    .collect::<Option<Vec<Kind>>>()?,
            ),
            _ => return None,
        })
    }

    fn kind_of_type(&self, ty: TypeId) -> Option<Kind> {
        match self.p.types.get(ty) {
            Some(TypeDef::Lattice(id)) => self.kind(*id),
            _ => None,
        }
    }

    /// Samples of type `ty`; `Err` names a type the harness cannot generate.
    fn values(&self, ty: TypeId, depth: u32) -> Result<Vec<Value>, String> {
        let def = self.def(ty)?.clone();
        let mut out = match def {
            TypeDef::Bool => vec![Value::Bool(false), Value::Bool(true)],
            TypeDef::Unit => vec![Value::Unit],
            TypeDef::Int(t) => {
                let mut out: Vec<Value> = (0..4)
                    .filter_map(|n| IntValue::from_i128(t, n).map(Value::Int))
                    .collect();
                out.extend(IntValue::from_i128(t, -1).map(Value::Int));
                out
            }
            TypeDef::F64 => [0.0, 1.0, -0.5]
                .into_iter()
                .map(|x| Value::F64(blossom_value::float::canonical(x)))
                .collect(),
            TypeDef::Str => ["", "a", "b"].into_iter().map(|s| Value::Str(Arc::from(s))).collect(),
            TypeDef::Bytes => vec![Value::Bytes(Arc::from(&b""[..])), Value::Bytes(Arc::from(&b"a"[..]))],
            TypeDef::Duration => vec![
                Value::Duration(Duration::from_nanos(0)),
                Value::Duration(Duration::from_nanos(1_000_000)),
            ],
            TypeDef::Instant => vec![Value::Instant(Instant(0)), Value::Instant(Instant(1_000_000))],
            TypeDef::Node(_) => vec![Value::Node(NodeId(0)), Value::Node(NodeId(1))],
            _ if depth == 0 => return Err(format!("values of {} nest too deep", self.name(ty))),
            TypeDef::Option(t) => {
                let mut out = vec![Value::none()];
                out.extend(self.values(t, depth - 1)?.into_iter().take(3).map(Value::some));
                out
            }
            TypeDef::Tuple(ts) => self
                .combos(&ts, depth)?
                .into_iter()
                .map(|vs| Value::Tuple(vs.into()))
                .collect(),
            TypeDef::Struct(s) => {
                let ts: Vec<TypeId> = s.fields.iter().map(|f| f.ty).collect();
                self.combos(&ts, depth)?
                    .into_iter()
                    .map(|vs| Value::Struct(vs.into()))
                    .collect()
            }
            TypeDef::Enum(e) => {
                let mut out = Vec::new();
                for v in e.variants.iter().filter(|v| Some(v.number) != e.unknown) {
                    let ts: Vec<TypeId> = v.payload.iter().map(|f| f.ty).collect();
                    for fields in self.combos(&ts, depth)?.into_iter().take(2) {
                        out.push(Value::Enum {
                            variant: v.number,
                            fields: fields.into(),
                        });
                    }
                }
                out
            }
            TypeDef::Vec(t) => {
                let xs = self.values(t, depth - 1)?;
                prefixes(&xs).into_iter().map(|vs| Value::Vec(vs.into())).collect()
            }
            TypeDef::Set(t) => {
                let xs = self.values(t, depth - 1)?;
                prefixes(&xs)
                    .into_iter()
                    .map(|vs| Value::Set(Arc::new(vs.into_iter().collect::<BTreeSet<Value>>())))
                    .collect()
            }
            TypeDef::Map(k, v) => {
                let (ks, vs) = (self.values(k, depth - 1)?, self.values(v, depth - 1)?);
                let mut out = vec![Value::Map(Arc::new(BTreeMap::new()))];
                // The first one, two and three keys, each with a value; then the other keys alone.
                for n in 1..=3 {
                    let m: BTreeMap<Value, Value> = ks
                        .iter()
                        .take(n)
                        .enumerate()
                        .filter_map(|(j, kk)| vs.get((n + j) % vs.len().max(1)).map(|x| (kk.clone(), x.clone())))
                        .collect();
                    out.push(Value::Map(Arc::new(m)));
                }
                for (j, kk) in ks.iter().enumerate().skip(1).take(2) {
                    if let Some(x) = vs.get(j % vs.len().max(1)) {
                        out.push(Value::Map(Arc::new(BTreeMap::from([(kk.clone(), x.clone())]))));
                    }
                }
                out
            }
            TypeDef::Lattice(id) => self.lattice(id, depth)?.into_iter().map(Value::Lattice).collect(),
            _ => return Err(format!("the harness generates no values of {}", self.name(ty))),
        };
        out.truncate(SAMPLES);
        Ok(out)
    }

    /// Combinations of samples of `ts`, one per position varying in turn, then mixed: enough to vary every field.
    fn combos(&self, ts: &[TypeId], depth: u32) -> Result<Vec<Vec<Value>>, String> {
        let per: Vec<Vec<Value>> = ts
            .iter()
            .map(|t| self.values(*t, depth.saturating_sub(1)))
            .collect::<Result<_, _>>()?;
        if per.iter().any(Vec::is_empty) {
            return Ok(Vec::new());
        }
        let width = per.iter().map(Vec::len).max().unwrap_or(1);
        let mut out = BTreeSet::new();
        // Each position walks its samples while the others take theirs at offsets, so the mixes differ.
        for k in 0..width * 3 {
            let row: Vec<Value> = per
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.get((k * (i + 1) + k / width) % s.len()).cloned())
                .collect();
            out.insert(row);
        }
        Ok(out.into_iter().take(SAMPLES).collect())
    }

    /// Samples of lattice `id`: ⊥, elements, and their joins.
    fn lattice(&self, id: LatticeTypeId, depth: u32) -> Result<Vec<LatValue>, String> {
        let def = self
            .p
            .lattices
            .get(id)
            .ok_or_else(|| format!("lattice {id:?} is not declared"))?;
        let kind = self
            .kind(id)
            .ok_or_else(|| format!("the harness generates no values of {}", def.name))?;
        let d = depth.saturating_sub(1);
        let base: Vec<LatValue> = match &def.ctor {
            LatticeCtor::Bool => vec![LatValue::Bool(false), LatValue::Bool(true)],
            LatticeCtor::Max(e) | LatticeCtor::Min(e) | LatticeCtor::Point(e) => {
                let mut out = vec![LatValue::Bottom];
                out.extend(
                    self.values(*e, d)?
                        .into_iter()
                        .take(4)
                        .map(|v| LatValue::Elem(Arc::new(v))),
                );
                out
            }
            LatticeCtor::Set(e) | LatticeCtor::PSet(e) => {
                let xs: Vec<Value> = self
                    .values(*e, d)?
                    .into_iter()
                    .filter(|x| kind != Kind::PSet || matches!(x, Value::Int(i) if i.to_i128().is_some_and(|n| n >= 0)))
                    .collect();
                let mut out = vec![LatValue::Set(Arc::new(BTreeSet::new()))];
                for x in xs.iter().take(3) {
                    out.push(LatValue::Set(Arc::new(BTreeSet::from([x.clone()]))));
                }
                out.push(LatValue::Set(Arc::new(xs.iter().take(3).cloned().collect())));
                out
            }
            LatticeCtor::Map(k, inner) => {
                let ks = self.values(*k, d)?;
                let inner_kind = self
                    .kind(*inner)
                    .ok_or_else(|| format!("the harness generates no values of {}", def.name))?;
                let vs: Vec<LatValue> = self
                    .lattice(*inner, d)?
                    .into_iter()
                    .filter(|v| !inner_kind.is_bottom(v))
                    .collect();
                let mut out = vec![LatValue::Map(Arc::new(BTreeMap::new()))];
                for (i, key) in ks.iter().take(4).enumerate() {
                    if let Some(v) = vs.get(i % vs.len().max(1)) {
                        out.push(LatValue::Map(Arc::new(BTreeMap::from([(key.clone(), v.clone())]))));
                    }
                }
                // Keys 0..n each with a value, so counts reach the small numbers the other samples use.
                for n in 2..=3 {
                    let m: BTreeMap<Value, LatValue> = ks
                        .iter()
                        .take(n)
                        .enumerate()
                        .filter_map(|(i, key)| vs.get(i % vs.len().max(1)).map(|v| (key.clone(), v.clone())))
                        .collect();
                    out.push(LatValue::Map(Arc::new(m)));
                }
                out
            }
            LatticeCtor::Product { fields, .. } => {
                let per: Vec<Vec<LatValue>> = fields
                    .iter()
                    .map(|(_, f)| self.lattice(*f, d))
                    .collect::<Result<_, _>>()?;
                let width = per.iter().map(Vec::len).max().unwrap_or(1);
                let mut out = BTreeSet::new();
                out.insert(kind.bottom());
                for k in 0..width * 4 {
                    let row: Vec<LatValue> = per
                        .iter()
                        .enumerate()
                        .filter_map(|(i, s)| s.get((k * (i + 1) + k / width) % s.len().max(1)).cloned())
                        .collect();
                    out.insert(LatValue::Seq(row.into()));
                }
                out.into_iter().collect()
            }
            _ => return Err(format!("the harness generates no values of {}", def.name)),
        };
        // Pairwise joins (where they exist), so the samples hold larger values too.
        let mut all: BTreeSet<LatValue> = base.iter().cloned().collect();
        for a in &base {
            for b in &base {
                if all.len() >= SAMPLES {
                    break;
                }
                if let Ok(j) = kind.join(a, b) {
                    all.insert(j);
                }
            }
        }
        Ok(all.into_iter().take(SAMPLES).collect())
    }
}

/// `[]`, `[x0]`, `[x0, x1]`, `[x1]`.
fn prefixes(xs: &[Value]) -> Vec<Vec<Value>> {
    let mut out = vec![Vec::new()];
    if let Some(a) = xs.first() {
        out.push(vec![a.clone()]);
        if let Some(b) = xs.get(1) {
            out.push(vec![a.clone(), b.clone()]);
            out.push(vec![b.clone()]);
        }
    }
    out
}
