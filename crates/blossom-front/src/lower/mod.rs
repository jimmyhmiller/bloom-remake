//! Lowering the HIR to the IR through `IrBuilder` (ARCHITECTURE §13.9, LANGUAGE §4, §7–§10).
//!
//! Every construct is lowered to its normative expansion: a table gets its `$del` relation and frame rule (a
//! `Persist` construct); a handler's header is materialized once as `M::L$when` and every statement is one rule that
//! reads it (a `HandlerHeader` construct); each `if`/`for` block is its own relation; views get one rule per
//! alternative, or a union relation and an aggregate rule; `upsert` goes through a keyed staging relation; delta
//! reads through a `$prev` shadow; `not { … }` and `forall` through helper relations. Rule labels follow LANGUAGE §4.3.
//!
//! A HIR role id is the IR role id and a HIR type id is the IR type id: roles are declared in HIR order and the
//! builder starts from the HIR's type table.

mod expr;
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{ColIdx, InternalError, QualName, RelId, RoleId, RuleLabel, Span, Symbol, TypeId, internal_error};
use blossom_ir::build::{FrontendKind, IrBuilder};
use blossom_ir::core::*;
use blossom_ir::{IrError, ValidatedProgram};
use blossom_value::time::{Duration, NodeId};
use blossom_value::{TypeDef, Value};

use crate::hir::{self, HRelId, HRelKind, HRoleId, Hir};

/// The nodes of a deployment: `nodes[i]` is `NodeId(i)`, and `roles[i]` its role (`None` in a role-free program).
pub struct Deployment<'a> {
    pub nodes: &'a [Symbol],
    pub roles: &'a [Option<HRoleId>],
}

/// A lowered program and the IR relation of every HIR relation.
pub struct Lowered {
    pub program: ValidatedProgram,
    pub rels: Vec<RelId>,
    /// For every HIR relation: the IR column of each declared column.
    pub surface: Vec<(RelId, Vec<usize>)>,
}

/// Lowers a type-checked HIR for a deployment.
pub fn lower(hir: &Hir, deployment: &Deployment<'_>) -> Result<Lowered, InternalError> {
    let meta = ProgramMeta {
        name: hir.name,
        version: hir.version,
        edition: hir.edition,
        compiler: Arc::from(concat!("blossom ", env!("CARGO_PKG_VERSION"))),
        prf_version: blossom_value::PRF_VERSION,
        encoding_version: blossom_value::ENCODING_VERSION,
        program_id: program_id(hir.name),
    };
    let mut b = IrBuilder::new(meta, FrontendKind::Blossom);
    *b.types() = hir.types.clone();
    // The validator types guards, `$self`, `$tick` and `$now` through these; intern them whether or not a column uses
    // them.
    for def in [
        TypeDef::Bool,
        TypeDef::Node(None),
        TypeDef::Int(blossom_value::types::IntTy::U64),
        TypeDef::Instant,
        TypeDef::Duration,
    ] {
        b.types()
            .insert(def)
            .map_err(|e| internal_error!("interning a type: {e}"))?;
    }
    let mut l = Lowerer {
        hir,
        b,
        rels: Vec::new(),
        del: BTreeMap::new(),
        prev: BTreeMap::new(),
        ups: BTreeMap::new(),
        labels: BTreeSet::new(),
        rel_names: BTreeSet::new(),
    };
    for r in &hir.roles {
        let kind = match r.kind {
            hir::RoleKind::Process => RoleKind::Process,
            hir::RoleKind::Cluster => RoleKind::Cluster,
            hir::RoleKind::External => RoleKind::External,
        };
        l.b.declare_role(r.name.clone(), kind, r.span).map_err(ir)?;
    }
    for i in 0..hir.rels.len() {
        let id = l.declare_hrel(HRelId(i as u32))?;
        l.rels.push(id);
    }
    l.members(deployment)?;
    l.tables()?;
    l.facts(deployment)?;
    l.handlers()?;
    l.views()?;
    let rels = l.rels.clone();
    let mut surface = Vec::new();
    for i in 0..hir.rels.len() {
        let h = HRelId(i as u32);
        let n = hir.rel(h)?.cols.len();
        let mut cols = Vec::new();
        for c in 0..n {
            cols.push(l.ir_col(h, c)?);
        }
        surface.push((l.rel(h)?, cols));
    }
    let program = l.b.finish().map_err(|errors| {
        internal_error!(
            "the lowered Blossom program is invalid: {}",
            errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
        )
    })?;
    Ok(Lowered { program, rels, surface })
}

pub(crate) fn ir(e: IrError) -> InternalError {
    internal_error!("lowering a Blossom program: {e}")
}

pub(crate) struct Lowerer<'h> {
    pub hir: &'h Hir,
    pub b: IrBuilder,
    /// HIR relation → IR relation.
    pub rels: Vec<RelId>,
    /// Each table's `$del`.
    pub del: BTreeMap<HRelId, RelId>,
    /// Each delta-read relation's `$prev` shadow.
    pub prev: BTreeMap<HRelId, RelId>,
    /// Each upserted table's `$ups` staging relation.
    pub ups: BTreeMap<HRelId, RelId>,
    labels: BTreeSet<String>,
    rel_names: BTreeSet<String>,
}

pub(crate) fn attrs() -> RelAttrs {
    RelAttrs {
        nondet: None,
        deterministic: false,
        final_output: false,
        atomic: false,
        handler: None,
        materialize: None,
        finite: None,
        range_col: None,
        partition: None,
        sealed_by: None,
    }
}

pub(crate) fn col_idx(i: usize) -> ColIdx {
    ColIdx::from_raw(i as u32)
}

pub(crate) fn column(name: Symbol, ty: TypeId, hidden_dest: bool) -> Column {
    Column {
        name,
        ty,
        field_no: None,
        default: None,
        since: None,
        deprecated: None,
        hidden_dest,
    }
}

/// A schema over `cols` keyed by `key` (every column when `None`).
pub(crate) fn schema(cols: Vec<Column>, key: Option<&[usize]>) -> Schema {
    let n = cols.len();
    let key: Vec<ColIdx> = match key {
        Some(k) => k.iter().map(|i| col_idx(*i)).collect(),
        None => (0..n).map(col_idx).collect(),
    };
    let payload = (0..n).map(col_idx).filter(|c| !key.contains(c)).collect();
    Schema {
        cols,
        key,
        payload,
        lattice: Vec::new(),
    }
}

impl Lowerer<'_> {
    pub fn rel(&self, h: HRelId) -> Result<RelId, InternalError> {
        self.rels
            .get(h.index())
            .copied()
            .ok_or_else(|| internal_error!("HIR relation {h:?} was not lowered"))
    }

    /// A unique rule label: `base`, or `base#2`, `base#3`, … on a collision.
    pub fn label(&mut self, base: String) -> RuleLabel {
        let mut text = base.clone();
        let mut n = 2;
        while !self.labels.insert(text.clone()) {
            text = format!("{base}#{n}");
            n += 1;
        }
        RuleLabel::new(text)
    }

    /// A unique relation name from `segments` (the last segment gets `#2`, … on a collision).
    pub fn rel_name(&mut self, mut segments: Vec<Symbol>) -> QualName {
        let base = segments.last().map(|s| s.as_str().to_owned()).unwrap_or_default();
        let mut n = 2;
        loop {
            let q = QualName::new(segments.clone());
            if self.rel_names.insert(q.to_string()) {
                return q;
            }
            if let Some(last) = segments.last_mut() {
                *last = Symbol::intern(&format!("{base}#{n}"));
            }
            n += 1;
        }
    }

    pub fn placement(role: Option<HRoleId>) -> Placement {
        match role {
            Some(r) => Placement::Role(RoleId::from_raw(r.0)),
            None => Placement::Shared,
        }
    }

    fn col_ty(&self, h: HRelId, c: usize) -> Result<TypeId, InternalError> {
        let r = self.hir.rel(h)?;
        r.cols
            .get(c)
            .and_then(|c| c.ty)
            .ok_or_else(|| internal_error!("column {c} of {} has no type", r.name))
    }

    /// The IR columns of a HIR relation: a channel's destination first (CR-14).
    pub fn ir_columns(&mut self, h: HRelId) -> Result<(Vec<Column>, Option<Vec<usize>>), InternalError> {
        let r = self.hir.rel(h)?.clone();
        let mut cols = Vec::new();
        for (i, c) in r.cols.iter().enumerate() {
            cols.push(column(c.name, self.col_ty(h, i)?, false));
        }
        let key = r.key.clone();
        if let HRelKind::Channel(ch) = &r.kind {
            return Ok((self.channel_columns(&r, ch, cols)?, None));
        }
        Ok((cols, key))
    }

    fn channel_columns(
        &mut self,
        r: &hir::HRel,
        ch: &hir::ChannelInfo,
        mut cols: Vec<Column>,
    ) -> Result<Vec<Column>, InternalError> {
        if let Some(d) = ch.dest_col {
            if d >= cols.len() {
                return Err(internal_error!("the destination column of {} is out of range", r.name));
            }
            let dest = cols.remove(d);
            cols.insert(0, dest);
            return Ok(cols);
        }
        let dst_ty = match ch.direction {
            Some((_, dst)) if self.hir.role(dst)?.kind == hir::RoleKind::External => TypeDef::Session,
            Some((_, dst)) => TypeDef::Node(Some(RoleId::from_raw(dst.0))),
            None => TypeDef::Node(None),
        };
        let t = self
            .b
            .types()
            .insert(dst_ty)
            .map_err(|e| internal_error!("interning a type: {e}"))?;
        cols.insert(0, column(Symbol::intern("dst"), t, true));
        Ok(cols)
    }

    /// The position of HIR column `c` among the IR columns of `h`.
    pub fn ir_col(&self, h: HRelId, c: usize) -> Result<usize, InternalError> {
        Ok(match &self.hir.rel(h)?.kind {
            HRelKind::Channel(ch) => match ch.dest_col {
                Some(d) if c == d => 0,
                Some(d) if c < d => c + 1,
                Some(_) => c,
                None => c + 1,
            },
            _ => c,
        })
    }

    fn declare_hrel(&mut self, h: HRelId) -> Result<RelId, InternalError> {
        let r = self.hir.rel(h)?.clone();
        let (cols, key) = self.ir_columns(h)?;
        let schema = schema(cols, key.as_deref());
        let (class, interface) = match &r.kind {
            HRelKind::Table | HRelKind::Scratch | HRelKind::View | HRelKind::LocalTick => (RelClass::Idb, None),
            HRelKind::Static | HRelKind::Members(_) => (RelClass::Static, None),
            HRelKind::Input { root: true } => (RelClass::Event(EventSource::Input), Some(InterfaceDir::Input)),
            HRelKind::Input { root: false } => (RelClass::Idb, Some(InterfaceDir::Input)),
            HRelKind::Output { .. } | HRelKind::Halt => (RelClass::Idb, Some(InterfaceDir::Output)),
            HRelKind::Boot => (RelClass::Event(EventSource::Boot), None),
            HRelKind::Timer { every } => {
                let every = i64::try_from(*every)
                    .map(Duration::from_nanos)
                    .map_err(|_| internal_error!("timer period out of range"))?;
                (
                    RelClass::Event(EventSource::Timer(TimerDecl {
                        clock: TimerClock::Physical,
                        every: Some(every),
                        ticks: None,
                        times: None,
                        once_after: None,
                        once: false,
                    })),
                    None,
                )
            }
            HRelKind::Channel(ch) => {
                let form = match (ch.direction, ch.dest_col) {
                    (Some((src, dst)), _) => ChannelForm::Direction {
                        src: RoleId::from_raw(src.0),
                        dst: RoleId::from_raw(dst.0),
                    },
                    (None, Some(_)) => ChannelForm::Column,
                    (None, None) => ChannelForm::NodeToNode,
                };
                let egress = match ch.direction {
                    Some((_, dst)) => self.hir.role(dst)?.kind == hir::RoleKind::External,
                    None => false,
                };
                (
                    RelClass::Channel(ChannelDecl {
                        form,
                        loopback: ch.loopback,
                        host_endpoint: false,
                        fault: FaultModel::Lossy,
                        partition: None,
                        sealed_by: None,
                        wrapper: None,
                        acl: AclSpec::Inferred,
                        egress_to_external: egress,
                        replicated: false,
                    }),
                    None,
                )
            }
        };
        let placement = match &r.kind {
            HRelKind::Channel(_) | HRelKind::Static | HRelKind::Members(_) | HRelKind::Boot | HRelKind::Halt => {
                Placement::Shared
            }
            _ => Self::placement(r.role),
        };
        let generated = r.name.to_string().contains('$');
        let construct = if generated {
            let kind = match &r.kind {
                HRelKind::Members(role) => ConstructKind::Members {
                    role: RoleId::from_raw(role.0),
                },
                _ => ConstructKind::Interpose,
            };
            Some(
                self.b
                    .begin_construct(kind, surface(&r.name, None, r.span))
                    .map_err(ir)?,
            )
        } else {
            None
        };
        self.rel_names.insert(r.name.to_string());
        let id = self
            .b
            .declare_relation(RelDecl {
                id: RelId::from_raw(0),
                name: r.name.clone(),
                class,
                schema,
                persistence: Persistence::None,
                durable: r.durable,
                interface,
                placement,
                origin: if generated {
                    Origin::Generated {
                        construct: blossom_base::ConstructId::from_raw(0),
                    }
                } else {
                    Origin::User(r.span)
                },
                attrs: attrs(),
                span: r.span,
            })
            .map_err(ir)?;
        if let Some(c) = construct {
            self.b.end_construct(c).map_err(ir)?;
        }
        Ok(id)
    }

    /// A generated relation in the currently open construct.
    pub fn generated(
        &mut self,
        segments: Vec<Symbol>,
        cols: Vec<Column>,
        key: Option<&[usize]>,
        role: Option<HRoleId>,
        durable: bool,
        span: Span,
    ) -> Result<RelId, InternalError> {
        let name = self.rel_name(segments);
        self.b
            .declare_relation(RelDecl {
                id: RelId::from_raw(0),
                name,
                class: RelClass::Idb,
                schema: schema(cols, key),
                persistence: Persistence::None,
                durable,
                interface: None,
                placement: Self::placement(role),
                origin: Origin::Generated {
                    construct: blossom_base::ConstructId::from_raw(0),
                },
                attrs: attrs(),
                span,
            })
            .map_err(ir)
    }

    /// `R$members` rows from the deployment.
    fn members(&mut self, d: &Deployment<'_>) -> Result<(), InternalError> {
        for (i, r) in self.hir.rels.iter().enumerate() {
            let HRelKind::Members(role) = r.kind else { continue };
            let rel = *self
                .rels
                .get(i)
                .ok_or_else(|| internal_error!("relation {i} was not lowered"))?;
            for (n, nr) in d.roles.iter().enumerate() {
                if *nr == Some(role) {
                    let c = self.b.intern_const(Value::Node(NodeId(n as u32))).map_err(ir)?;
                    self.b.fact(rel, vec![c], r.span).map_err(ir)?;
                }
            }
        }
        Ok(())
    }

    /// Every table's `$del` relation and frame rule (LANGUAGE §7.2).
    fn tables(&mut self) -> Result<(), InternalError> {
        for i in 0..self.hir.rels.len() {
            let h = HRelId(i as u32);
            let r = self.hir.rel(h)?.clone();
            if !matches!(r.kind, HRelKind::Table) {
                continue;
            }
            let rel = self.rel(h)?;
            let construct = self
                .b
                .begin_construct(
                    ConstructKind::Persist { rel, del: None },
                    surface(&r.name, None, r.span),
                )
                .map_err(ir)?;
            let (cols, _) = self.ir_columns(h)?;
            let del = self.generated(suffixed(&r.name, "$del"), cols.clone(), None, r.role, false, r.span)?;
            self.b
                .set_construct_kind(construct, ConstructKind::Persist { rel, del: Some(del) })
                .map_err(ir)?;
            let label = self.label(format!("{}$persist", r.name));
            let mut rb = self.b.rule(RuleKind::Inductive, label, r.span);
            let mut vars = Vec::new();
            for (i, c) in cols.iter().enumerate() {
                vars.push(rb.var(Symbol::intern(&format!("X{i}")), c.ty).map_err(ir)?);
            }
            let args: Vec<Term> = vars.iter().map(|v| Term::Var(*v)).collect();
            rb.lit(Literal::Pos(atom(rel, args.clone(), r.span)));
            rb.lit(Literal::Neg(atom(del, args.clone(), r.span)));
            let rule = rb
                .head(
                    Head {
                        rel,
                        args: args.into_iter().map(HeadArg::Term).collect(),
                        mode: HeadMode::Insert,
                    },
                    r.role.map(|x| RoleId::from_raw(x.0)),
                )
                .map_err(ir)?;
            self.b.end_construct(construct).map_err(ir)?;
            self.b
                .set_persistence(rel, Persistence::Frame { rule, del: Some(del) })
                .map_err(ir)?;
            self.del.insert(h, del);
        }
        Ok(())
    }

    /// `fact r(…);` rows of static relations.
    fn facts(&mut self, d: &Deployment<'_>) -> Result<(), InternalError> {
        for f in &self.hir.facts {
            let rel = self.rel(f.rel)?;
            let mut row = Vec::new();
            for e in &f.row {
                let v = match expr::const_eval(self.hir, e)? {
                    // A node named by a string (checked against the deployment before lowering).
                    Value::Str(name) if matches!(e.ty.and_then(|t| self.hir.types.get(t)), Some(TypeDef::Node(_))) => {
                        let i = d.nodes.iter().position(|n| n.as_str() == &*name).ok_or_else(|| {
                            internal_error!("fact names the node `{name}`, which the deployment lacks")
                        })?;
                        Value::Node(NodeId(i as u32))
                    }
                    v => v,
                };
                row.push(self.b.intern_const(v).map_err(ir)?);
            }
            self.b.fact(rel, row, f.span).map_err(ir)?;
        }
        Ok(())
    }
}

pub(crate) fn atom(rel: RelId, args: Vec<Term>, span: Span) -> Atom {
    Atom {
        rel,
        args,
        sender: None,
        principal: None,
        weight: None,
        spec: None,
        span,
    }
}

pub(crate) fn surface(module: &QualName, label: Option<Symbol>, span: Span) -> SurfaceRef {
    SurfaceRef {
        module: module.clone(),
        label,
        stmt: None,
        span,
    }
}

/// `name` with `suffix` appended to its last segment.
pub(crate) fn suffixed(name: &QualName, suffix: &str) -> Vec<Symbol> {
    let mut segs = name.segments().to_vec();
    if let Some(last) = segs.last_mut() {
        *last = Symbol::intern(&format!("{}{suffix}", last.as_str()));
    }
    segs
}

fn program_id(name: Symbol) -> [u8; 16] {
    let a = blossom_base::span::stable_hash(name.as_str().as_bytes());
    let b = blossom_base::span::stable_hash(format!("{name}#bls").as_bytes());
    let mut id = [0u8; 16];
    for (slot, byte) in id.iter_mut().zip(a.to_le_bytes().into_iter().chain(b.to_le_bytes())) {
        *slot = byte;
    }
    id
}
