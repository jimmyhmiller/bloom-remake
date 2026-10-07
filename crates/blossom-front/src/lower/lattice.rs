//! Lattice declarations: every lattice type of the program with its operation catalogue (LANGUAGE §11.5, R04 §2.4).
//!
//! The HIR's lattices are declared first and in order, so a HIR `TypeDef::Lattice(id)` is the IR's. A catalogue can
//! name lattices the program never mentions (`size` returns an `LMax<u64>`, `key_set` an `LSet<K>`); those are
//! declared after, each with its own catalogue, until nothing new is needed.

use blossom_base::{InternalError, LatticeTypeId, QualName, Symbol, TypeId, internal_error};
use blossom_ir::core::{HeightClass, LatOpDecl, LatOpImpl, LatOpKind, LatticeCtor, LatticeDef, LawStatus, MonoClass};
use blossom_lattice::{Kind, Op};
use blossom_value::types::IntTy;
use blossom_value::{TypeDef, TypeTable};

use super::{Lowerer, ir};
use crate::hir::{HClass, HMethod, Hir};
use crate::typeck::type_name;

/// The built-in lattice of constructor `ctor`, looking nested lattices up in `ctors`.
pub(crate) fn kind_of(ctors: &[LatticeCtor], ctor: &LatticeCtor) -> Option<Kind> {
    Some(match ctor {
        LatticeCtor::Bool => Kind::Bool,
        LatticeCtor::Max(_) => Kind::Max,
        LatticeCtor::Min(_) => Kind::Min,
        LatticeCtor::Set(_) => Kind::Set,
        LatticeCtor::PSet(_) => Kind::PSet,
        LatticeCtor::Point(_) => Kind::Point,
        LatticeCtor::Map(_, inner) => Kind::Map(Box::new(kind_of(ctors, ctors.get(inner.index())?)?)),
        LatticeCtor::Product { fields, .. } => Kind::Product(
            fields
                .iter()
                .map(|(_, id)| kind_of(ctors, ctors.get(id.index())?))
                .collect::<Option<Vec<Kind>>>()?,
        ),
        _ => return None,
    })
}

/// The IR catalogue name of a product's field read: `.f`, apart from every operation and method name.
pub(crate) fn field_op(field: Symbol) -> Symbol {
    Symbol::intern(&format!(".{}", field.as_str()))
}

/// The IR catalogue name of a method: its own, or `m!` for a stable method's exact entry.
pub(crate) fn method_op(name: Symbol, exact: bool) -> Symbol {
    if exact {
        Symbol::intern(&format!("{}!", name.as_str()))
    } else {
        name
    }
}

/// A method's class for each of its parameters (the receiver first) and its operation kind, as its declaration says
/// (LANGUAGE §11.4, §11.8). Plain parameters are constants; a stable method's guarded entry is monotone in its
/// receiver (`exact`: its banged entry, non-monotone).
pub(crate) fn method_sig(hir: &Hir, m: &HMethod, exact: bool) -> (Vec<MonoClass>, LatOpKind) {
    let params: Vec<TypeId> = hir
        .fns
        .get(m.f.index())
        .map(|f| f.params.iter().map(|(_, t)| *t).collect())
        .unwrap_or_default();
    let (receiver, lattice_arg, kind) = match &m.class {
        HClass::None => (MonoClass::NonMonotone, MonoClass::NonMonotone, LatOpKind::NonMonotone),
        HClass::Morphism => (MonoClass::Morphism, MonoClass::NonMonotone, LatOpKind::Morphism),
        HClass::Bimorphism => (MonoClass::Bimorphism, MonoClass::Bimorphism, LatOpKind::Bimorphism),
        HClass::Monotone => (MonoClass::Monotone, MonoClass::NonMonotone, LatOpKind::Monotone),
        HClass::Antitone => (MonoClass::Antitone, MonoClass::NonMonotone, LatOpKind::Antitone),
        HClass::Threshold => (MonoClass::Threshold, MonoClass::NonMonotone, LatOpKind::Threshold),
        HClass::Stable { .. } if exact => (MonoClass::NonMonotone, MonoClass::NonMonotone, LatOpKind::NonMonotone),
        HClass::Stable { after } => (
            MonoClass::Monotone,
            MonoClass::NonMonotone,
            LatOpKind::Stable { after: *after },
        ),
    };
    let classes = params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == 0 {
                receiver
            } else if matches!(hir.types.get(*t), Some(TypeDef::Lattice(_))) {
                lattice_arg
            } else {
                MonoClass::Constant
            }
        })
        .collect();
    (classes, kind)
}

/// A method's function's classes: its declaration's, except that a stable method is, as a function, non-monotone.
pub(crate) fn fn_classes(hir: &Hir, m: &HMethod) -> Vec<MonoClass> {
    method_sig(hir, m, true).0
}

/// The built-in lattice of lattice type `ty`.
pub(crate) fn kind_of_type(types: &TypeTable, ctors: &[LatticeCtor], ty: TypeId) -> Option<Kind> {
    match types.get(ty) {
        Some(TypeDef::Lattice(id)) => kind_of(ctors, ctors.get(id.index())?),
        _ => None,
    }
}

/// A lattice's display name: `LMax<u64>`, `LMap<String, LSet<Node>>`.
pub(crate) fn lattice_name(types: &TypeTable, ctors: &[LatticeCtor], ctor: &LatticeCtor) -> String {
    let t = |ty: &TypeId| match types.get(*ty) {
        Some(TypeDef::Lattice(id)) => match ctors.get(id.index()) {
            Some(c) => lattice_name(types, ctors, c),
            None => "?".to_owned(),
        },
        _ => type_name(types, *ty),
    };
    match ctor {
        LatticeCtor::Bool => "LBool".to_owned(),
        LatticeCtor::Max(e) => format!("LMax<{}>", t(e)),
        LatticeCtor::Min(e) => format!("LMin<{}>", t(e)),
        LatticeCtor::Set(e) => format!("LSet<{}>", t(e)),
        LatticeCtor::PSet(e) => format!("LPSet<{}>", t(e)),
        LatticeCtor::Point(e) => format!("LPoint<{}>", t(e)),
        LatticeCtor::Map(k, inner) => {
            let v = ctors
                .get(inner.index())
                .map(|c| lattice_name(types, ctors, c))
                .unwrap_or_else(|| "?".to_owned());
            format!("LMap<{}, {v}>", t(k))
        }
        LatticeCtor::Product { name, .. } => name.to_string(),
        other => format!("{other:?}"),
    }
}

/// Whether a lattice's thresholds may have exact supports (TEST-140): every lattice but `LPoint` (and a product with an
/// `LPoint` field).
fn distributive(kind: &Kind) -> bool {
    match kind {
        Kind::Point => false,
        Kind::Product(fields) => fields.iter().all(distributive),
        _ => true,
    }
}

impl Lowerer<'_> {
    fn intern_type(&mut self, def: TypeDef) -> Result<TypeId, InternalError> {
        self.b
            .types()
            .insert(def)
            .map_err(|e| internal_error!("interning a type: {e}"))
    }

    /// The type of the lattice with constructor `ctor`, adding it to `ctors` (to be declared) if it is new.
    fn lattice_type(&mut self, ctors: &mut Vec<LatticeCtor>, ctor: LatticeCtor) -> Result<TypeId, InternalError> {
        let i = match ctors.iter().position(|c| *c == ctor) {
            Some(i) => i,
            None => {
                ctors.push(ctor);
                ctors.len() - 1
            }
        };
        let id = LatticeTypeId::from_raw(u32::try_from(i).map_err(|_| internal_error!("too many lattices"))?);
        self.intern_type(TypeDef::Lattice(id))
    }

    /// The type of lattice `id`.
    fn lattice_id_type(&mut self, id: LatticeTypeId) -> Result<TypeId, InternalError> {
        self.intern_type(TypeDef::Lattice(id))
    }

    /// `reveal!`'s result type (LANGUAGE §11.4): `R(L)`, or `R⁺(L)` with `nonbot`.
    fn reveal_type(
        &mut self,
        ctors: &[LatticeCtor],
        ctor: &LatticeCtor,
        nonbot: bool,
    ) -> Result<TypeId, InternalError> {
        match ctor {
            LatticeCtor::Bool => self.intern_type(TypeDef::Bool),
            LatticeCtor::Max(e) | LatticeCtor::Min(e) | LatticeCtor::Point(e) => {
                if nonbot {
                    Ok(*e)
                } else {
                    self.intern_type(TypeDef::Option(*e))
                }
            }
            LatticeCtor::Set(e) | LatticeCtor::PSet(e) => self.intern_type(TypeDef::Set(*e)),
            LatticeCtor::Map(k, inner) => {
                let c = ctors
                    .get(inner.index())
                    .cloned()
                    .ok_or_else(|| internal_error!("lattice {inner:?} is not declared"))?;
                let v = self.reveal_type(ctors, &c, true)?;
                self.intern_type(TypeDef::Map(*k, v))
            }
            // The struct of the fields' reveals, named after the product (as `Hir::reveal_type` builds it).
            LatticeCtor::Product { name, fields } => {
                let mut out = Vec::new();
                for (f, id) in fields {
                    let c = ctors
                        .get(id.index())
                        .cloned()
                        .ok_or_else(|| internal_error!("lattice {id:?} is not declared"))?;
                    out.push(blossom_value::types::FieldDef {
                        name: *f,
                        ty: self.reveal_type(ctors, &c, false)?,
                        field_no: None,
                        default: None,
                        since: None,
                        deprecated: None,
                        renamed_from: None,
                    });
                }
                self.intern_type(TypeDef::Struct(blossom_value::types::StructDef {
                    name: name.clone(),
                    fields: out,
                    reserved: Vec::new(),
                }))
            }
            other => Err(internal_error!("lattice {other:?} reached lowering")),
        }
    }

    /// The plain type a value lifts from (LANGUAGE §5.6).
    fn lift_source(&mut self, ctors: &[LatticeCtor], ctor: &LatticeCtor) -> Result<TypeId, InternalError> {
        match ctor {
            LatticeCtor::Map(k, inner) => {
                let c = ctors
                    .get(inner.index())
                    .cloned()
                    .ok_or_else(|| internal_error!("lattice {inner:?} is not declared"))?;
                let v = self.lift_source(ctors, &c)?;
                self.intern_type(TypeDef::Map(*k, v))
            }
            other => self.reveal_type(ctors, other, true),
        }
    }

    /// Declares every lattice of the HIR, then every lattice their catalogues name.
    pub(crate) fn declare_lattices(&mut self) -> Result<(), InternalError> {
        let mut ctors = self.hir.lattices.clone();
        let mut i = 0;
        while let Some(ctor) = ctors.get(i).cloned() {
            let id = LatticeTypeId::from_raw(u32::try_from(i).map_err(|_| internal_error!("too many lattices"))?);
            let ty = self.lattice_id_type(id)?;
            let kind = kind_of(&ctors, &ctor).ok_or_else(|| internal_error!("lattice {ctor:?} reached lowering"))?;
            let mut ops = Vec::new();
            for op in Op::catalogue(&kind) {
                ops.push(self.op_decl(&mut ctors, ty, &ctor, &kind, op)?);
            }
            if let LatticeCtor::Product { fields, .. } = &ctor {
                ops.extend(self.product_ops(ty, fields)?);
            }
            let name = lattice_name(self.b.types(), &ctors, &ctor);
            let declared = self
                .b
                .declare_lattice(LatticeDef {
                    id,
                    name: QualName::new(vec![Symbol::intern(&name)]),
                    ctor: ctor.clone(),
                    ops,
                    height: match kind {
                        Kind::Bool | Kind::Point => HeightClass::Acc,
                        _ => HeightClass::Unknown,
                    },
                    // A product's merge, ⊥ and order are its fields', so its laws hold by construction.
                    laws: match kind {
                        Kind::Product(_) => LawStatus::Proved,
                        _ => LawStatus::Builtin,
                    },
                    distributive: distributive(&kind),
                    dense_domain: None,
                })
                .map_err(ir)?;
            if declared != id {
                return Err(internal_error!(
                    "lattice {name} was declared as {declared:?}, not {id:?}"
                ));
            }
            i += 1;
        }
        Ok(())
    }

    /// A product's field reads (morphisms) and its methods (LANGUAGE §11.8): a stable method has its guarded entry and
    /// its exact one (`m!`).
    fn product_ops(&mut self, ty: TypeId, fields: &[(Symbol, LatticeTypeId)]) -> Result<Vec<LatOpDecl>, InternalError> {
        let mut ops = Vec::new();
        for (i, (f, id)) in fields.iter().enumerate() {
            let ret = self.lattice_id_type(*id)?;
            ops.push(LatOpDecl {
                name: field_op(*f),
                params: vec![(ty, MonoClass::Morphism)],
                ret,
                kind: LatOpKind::Morphism,
                join_prime: false,
                derivative: None,
                incompatible_thresholds: false,
                imp: LatOpImpl::Field(u32::try_from(i).map_err(|_| internal_error!("too many fields"))?),
            });
        }
        let methods: Vec<HMethod> = self.hir.methods.iter().filter(|m| m.lattice == ty).cloned().collect();
        for m in methods {
            let f = self
                .hir
                .fns
                .get(m.f.index())
                .ok_or_else(|| internal_error!("method `{}` has no function", m.name))?;
            let ret = f.ret;
            let types: Vec<TypeId> = f.params.iter().map(|(_, t)| *t).collect();
            let fid = *self
                .fns
                .get(m.f.index())
                .ok_or_else(|| internal_error!("method `{}` has no IR function", m.name))?;
            let entries: &[bool] = if matches!(m.class, HClass::Stable { .. }) {
                &[false, true]
            } else {
                &[false]
            };
            for &exact in entries {
                let (classes, kind) = method_sig(self.hir, &m, exact);
                ops.push(LatOpDecl {
                    name: method_op(m.name, exact),
                    params: types.iter().copied().zip(classes).collect(),
                    ret,
                    kind,
                    // Not claimed: only a checked claim may say a threshold is join-prime.
                    join_prime: false,
                    derivative: None,
                    incompatible_thresholds: false,
                    imp: LatOpImpl::Method(fid),
                });
            }
        }
        Ok(ops)
    }

    /// The catalogue entry of `op` on the lattice `ty` (constructor `ctor`).
    fn op_decl(
        &mut self,
        ctors: &mut Vec<LatticeCtor>,
        ty: TypeId,
        ctor: &LatticeCtor,
        kind: &Kind,
        op: Op,
    ) -> Result<LatOpDecl, InternalError> {
        let bool_t = self.intern_type(TypeDef::Bool)?;
        let u64_t = self.intern_type(TypeDef::Int(IntTy::U64))?;
        let elem = match ctor {
            LatticeCtor::Max(e)
            | LatticeCtor::Min(e)
            | LatticeCtor::Set(e)
            | LatticeCtor::PSet(e)
            | LatticeCtor::Point(e) => Some(*e),
            _ => None,
        };
        let map = match ctor {
            LatticeCtor::Map(k, inner) => Some((*k, self.lattice_id_type(*inner)?)),
            _ => None,
        };
        let need =
            |x: Option<TypeId>| x.ok_or_else(|| internal_error!("{} has no element for {}", op.name(), ty.index()));
        let (args, ret): (Vec<TypeId>, TypeId) = match op {
            Op::Join | Op::AddLat | Op::Intersect | Op::And | Op::Or => (vec![ty, ty], ty),
            Op::Leq | Op::Less => (vec![ty, ty], bool_t),
            Op::Reveal => (vec![ty], self.reveal_type(ctors, ctor, false)?),
            Op::RevealNonBot => (vec![ty], self.reveal_type(ctors, ctor, true)?),
            Op::IsBot | Op::Nonempty | Op::IsEmpty => (vec![ty], bool_t),
            Op::Not => (vec![ty], ty),
            Op::Contains | Op::AtLeast | Op::Above | Op::AtMost | Op::Below => (vec![ty, need(elem)?], bool_t),
            Op::Size => (vec![ty], self.lattice_type(ctors, LatticeCtor::Max(u64_t))?),
            Op::At => {
                let (k, v) = map.ok_or_else(|| internal_error!("`at` on a non-map"))?;
                (vec![ty, k], v)
            }
            Op::HasKey => {
                let (k, _) = map.ok_or_else(|| internal_error!("`has_key` on a non-map"))?;
                (vec![ty, k], bool_t)
            }
            Op::KeySet => {
                let (k, _) = map.ok_or_else(|| internal_error!("`key_set` on a non-map"))?;
                (vec![ty], self.lattice_type(ctors, LatticeCtor::Set(k))?)
            }
            Op::Sum => (vec![ty], self.lattice_type(ctors, LatticeCtor::Max(need(elem)?))?),
            Op::Get => {
                let e = need(elem)?;
                (vec![ty], self.intern_type(TypeDef::Option(e))?)
            }
            Op::Add | Op::Sub | Op::MinOf => (vec![ty, need(elem)?], ty),
            Op::MinElem => (vec![ty], self.lattice_type(ctors, LatticeCtor::Min(need(elem)?))?),
            Op::MaxElem => (vec![ty], self.lattice_type(ctors, LatticeCtor::Max(need(elem)?))?),
            Op::Of => match (ctor, map) {
                (LatticeCtor::Product { fields, .. }, _) => {
                    let mut args = Vec::new();
                    for (_, id) in fields {
                        args.push(self.lattice_id_type(*id)?);
                    }
                    (args, ty)
                }
                (LatticeCtor::Bool, _) => (vec![bool_t], ty),
                (_, Some((k, v))) => (vec![k, v], ty),
                _ => (vec![need(elem)?], ty),
            },
            Op::Lift => (vec![self.lift_source(ctors, ctor)?], ty),
            Op::LiftEntries => {
                let (k, v) = map.ok_or_else(|| internal_error!("`lift_entries` on a non-map"))?;
                (vec![self.intern_type(TypeDef::Map(k, v))?], ty)
            }
        };
        let sig = op.sig(kind);
        if sig.params.len() != args.len() {
            return Err(internal_error!(
                "`{}` has {} classes for {} parameters",
                op.name(),
                sig.params.len(),
                args.len()
            ));
        }
        let threshold = sig.kind == LatOpKind::Threshold;
        Ok(LatOpDecl {
            name: Symbol::intern(op.name()),
            params: args.into_iter().zip(sig.params).collect::<Vec<(TypeId, MonoClass)>>(),
            ret,
            kind: sig.kind,
            // Every built-in threshold is join-prime: t(a ⊔ b) ⇒ t(a) ∨ t(b).
            join_prime: threshold,
            derivative: None,
            incompatible_thresholds: false,
            imp: LatOpImpl::Builtin,
        })
    }
}
