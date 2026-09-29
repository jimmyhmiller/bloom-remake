//! Lowering an analysed `.ded` program to the IR (LANGUAGE §21.1, ARCHITECTURE §8.1).
//!
//! **Protocol.** Each protocol relation `r(L, x̄)` becomes a relation `r(x̄)` held at every node: the location
//! column is implicit. A protocol rule runs at its location: a location variable that is also used elsewhere is
//! bound to `$self`, a constant location becomes the guard `$self == n`. Deductive and `@next` rules keep their
//! kind. An `@async` rule sends to the generated channel `r$async(dest, x̄)`, whose deliveries a generated rule
//! inserts into `r`. Facts `r(n, c̄)@k` are input events of node `n` at tick `k`: into `r` itself when no rule
//! derives `r`, else into a generated input `r$in` that a generated rule copies into `r`. There are no implicit
//! tables: persistence is the program's own `r(X)@next :- r(X);` rule (LANG-065).
//!
//! **Spec.** `pre`, `post` and their helpers keep every column; the protocol relations they read become inputs fed
//! with every node's tuples at EOT (the node prefixed), absolute-time atoms `r(…)@k` read an input fed at tick
//! `k`, and `crash` is the crash oracle's input.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_artifact::sim::{
    EdgeTime, InputFact, LogicalEdge, LogicalIdx, LogicalKind, LogicalRel, OutcomeSpec, SimArtifact, SpecFeed,
};
use blossom_base::{
    ConstId, ConstructId, Diagnostic, Diagnostics, InternalError, QualName, RelId, RuleLabel, Span, Symbol, TypeId,
    VarId, code, internal_error,
};
use blossom_ir::build::{FrontendKind, IrBuilder, RuleBuilder};
use blossom_ir::core::{
    self as ir, AclSpec, AggCall, Atom, ChannelDecl, ChannelForm, Column, ConstructKind, EventSource, FaultModel, Head,
    HeadArg, HeadMode, Literal, Origin, Persistence, Placement, ProgramMeta, RelAttrs, RelClass, RelDecl, RuleKind,
    Schema, SurfaceRef,
};
use blossom_ir::{IrError, ValidatedProgram};
use blossom_syntax::ded::{self as ast, AggFunc, Arg, BinOp, BodyItem, Expr, HeadTime, Term};
use blossom_value::{
    TypeDef, Value,
    time::{NodeId, Tick},
    types::IntTy,
    value::IntValue,
};

use super::DedError;
use super::load::Program;
use super::model::{Model, body_atoms};
use super::types::{ColTy, Types};

/// The deployment a `.ded` program is compiled for: node names in canonical (sorted) order.
pub(crate) struct Deployment {
    names: Vec<Symbol>,
}

impl Deployment {
    pub fn new(nodes: &[&str]) -> Result<Deployment, String> {
        if nodes.is_empty() {
            return Err("a `.ded` program is compiled for a deployment: name at least one node".into());
        }
        let mut names: Vec<Symbol> = nodes.iter().map(|n| Symbol::intern(n)).collect();
        names.sort();
        if let Some(w) = names.windows(2).find(|w| w.first() == w.get(1)) {
            let dup = w.first().map(|s| s.as_str()).unwrap_or("");
            return Err(format!("node `{dup}` is listed twice"));
        }
        if let Some(bad) = names.iter().find(|n| n.as_str().is_empty()) {
            return Err(format!("`{bad}` is not a node name"));
        }
        Ok(Deployment { names })
    }

    fn id(&self, name: &str) -> Option<NodeId> {
        self.names
            .iter()
            .position(|n| n.as_str() == name)
            .and_then(|i| u32::try_from(i).ok())
            .map(NodeId)
    }
}

pub(crate) fn lower(
    program: &Program,
    model: &Model,
    types: &Types,
    deployment: &Deployment,
    diags: &mut Diagnostics,
) -> Result<SimArtifact, DedError> {
    let mut rels: Vec<LogicalRel> = model
        .rels
        .iter()
        .map(|r| LogicalRel {
            name: r.name,
            arity: r.arity,
            kind: r.kind,
            protocol: None,
            channel: None,
            input: None,
            spec: None,
            spec_at: Vec::new(),
        })
        .collect();
    let values = Values { deployment, types };

    let mut p = Lowerer::new(Symbol::intern("ded"))?;
    p.protocol_relations(model, types, &mut rels)?;
    let mut labels = Labels::default();
    for (i, rule) in program.rules.iter().enumerate() {
        if !model.spec_rule.get(i).copied().unwrap_or(false) {
            p.protocol_rule(rule, model, &rels, &values, &mut labels, diags)?;
        }
    }
    let inputs = facts(program, model, &rels, &values, diags);
    let protocol = p.finish("protocol")?;

    let spec = if model.has_spec {
        let mut s = Lowerer::new(Symbol::intern("ded$spec"))?;
        let feeds = s.spec_relations(program, model, types, &mut rels)?;
        for (i, rule) in program.rules.iter().enumerate() {
            if model.spec_rule.get(i).copied().unwrap_or(false) {
                s.spec_rule(rule, model, &rels, &values, &mut labels, diags)?;
            }
        }
        let find = |name: &str| {
            model
                .index(Symbol::intern(name))
                .and_then(|i| rels.get(i))
                .and_then(|r| r.spec)
                .ok_or_else(|| internal_error!("the spec has no `{name}` relation"))
        };
        let (pre, post) = (find("pre")?, find("post")?);
        Some(OutcomeSpec {
            program: s.finish("spec")?,
            pre,
            post,
            feeds,
        })
    } else {
        None
    };

    let mut edges = Vec::new();
    for rule in &program.rules {
        let Some(to) = model.index(rule.head.rel.text).and_then(idx) else {
            continue;
        };
        let time = match rule.time {
            HeadTime::Now => EdgeTime::Deductive,
            HeadTime::Next => EdgeTime::Next,
            HeadTime::Async => EdgeTime::Async,
        };
        for atom in body_atoms(rule) {
            if let Some(from) = model.index(atom.rel.text).and_then(idx) {
                edges.push(LogicalEdge {
                    from,
                    to,
                    time,
                    negated: atom.negated,
                });
            }
        }
    }
    edges.sort();
    edges.dedup();

    Ok(SimArtifact {
        nodes: deployment.names.clone(),
        roles: Vec::new(),
        profile: blossom_artifact::sim::Profile::Molly,
        protocol,
        inputs,
        statics: Vec::new(),
        halt: None,
        rels,
        edges,
        spec,
    })
}

fn idx(i: usize) -> Option<LogicalIdx> {
    u32::try_from(i).ok().map(LogicalIdx)
}

fn ir_internal(e: IrError) -> InternalError {
    internal_error!("lowering a `.ded` program: {e}")
}

/// Unique rule labels across both programs: `rel#k`.
#[derive(Default)]
struct Labels {
    next: BTreeMap<String, u32>,
}

impl Labels {
    fn label(&mut self, base: &str) -> RuleLabel {
        let n = self.next.entry(base.to_owned()).or_insert(0);
        *n += 1;
        RuleLabel::new(format!("{base}#{n}"))
    }
}

/// Typed constants.
struct Values<'a> {
    deployment: &'a Deployment,
    types: &'a Types,
}

impl Values<'_> {
    fn ty_at(&self, span: Span) -> Result<ColTy, InternalError> {
        self.types
            .at
            .get(&span)
            .copied()
            .ok_or_else(|| internal_error!("no inferred type for the term at {span:?}"))
    }

    /// The value of a literal of type `ty`, or a diagnostic when it cannot have that type.
    fn literal(&self, t: &Term, ty: ColTy) -> Result<Result<Value, Diagnostic>, InternalError> {
        Ok(Ok(match (t, ty) {
            (Term::Int(n, _), ColTy::I64) => Value::Int(IntValue::I64(*n)),
            (Term::Str(text, _), ColTy::Str) => Value::Str(text.clone()),
            (Term::Str(text, s), ColTy::Node) => match self.deployment.id(text) {
                Some(id) => Value::Node(id),
                None => {
                    return Ok(Err(Diagnostic::new(
                        code!("BLS0200"),
                        format!(
                            "`\"{text}\"` is used as a node, but the deployment's nodes are {}",
                            self.deployment
                                .names
                                .iter()
                                .map(|n| format!("`{n}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                    .with_primary(*s)));
                }
            },
            (t, ty) => return Err(internal_error!("the literal {t:?} was inferred as {ty:?}")),
        }))
    }
}

fn facts(
    program: &Program,
    model: &Model,
    rels: &[LogicalRel],
    values: &Values<'_>,
    diags: &mut Diagnostics,
) -> Vec<InputFact> {
    let mut out = Vec::new();
    for fact in &program.facts {
        let Some(input) = model
            .index(fact.rel.text)
            .and_then(|i| rels.get(i))
            .and_then(|r| r.input)
        else {
            continue;
        };
        let mut row = Vec::with_capacity(fact.args.len());
        let mut node = None;
        let mut ok = true;
        for (i, t) in fact.args.iter().enumerate() {
            let v = values.ty_at(t.span()).and_then(|ty| values.literal(t, ty));
            match v {
                Ok(Ok(Value::Node(id))) if i == 0 => node = Some(id),
                Ok(Ok(v)) => row.push(v),
                Ok(Err(d)) => {
                    diags.push(d);
                    ok = false;
                }
                Err(e) => {
                    diags.push(Diagnostic::new(code!("BLS0300"), e.to_string()).with_primary(t.span()));
                    ok = false;
                }
            }
        }
        if let (true, Some(node)) = (ok, node) {
            out.push(InputFact {
                node,
                tick: Tick(fact.time),
                rel: input,
                row,
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

struct Lowerer {
    b: IrBuilder,
    ty: BTreeMap<ColTy, TypeId>,
}

impl Lowerer {
    fn new(name: Symbol) -> Result<Lowerer, InternalError> {
        let meta = ProgramMeta {
            name,
            version: 1,
            edition: 1,
            compiler: Arc::from(concat!("blossom ", env!("CARGO_PKG_VERSION"))),
            prf_version: blossom_value::PRF_VERSION,
            encoding_version: blossom_value::ENCODING_VERSION,
            program_id: program_id(name),
        };
        let mut b = IrBuilder::new(meta, FrontendKind::Ded);
        let mut ty = BTreeMap::new();
        for (t, def) in [
            (ColTy::I64, TypeDef::Int(IntTy::I64)),
            (ColTy::Str, TypeDef::Str),
            (ColTy::Node, TypeDef::Node(None)),
        ] {
            let id = b
                .types()
                .insert(def)
                .map_err(|e| internal_error!("interning a type: {e}"))?;
            ty.insert(t, id);
        }
        b.types()
            .insert(TypeDef::Bool)
            .map_err(|e| internal_error!("interning bool: {e}"))?;
        Ok(Lowerer { b, ty })
    }

    fn tid(&self, t: ColTy) -> Result<TypeId, InternalError> {
        self.ty
            .get(&t)
            .copied()
            .ok_or_else(|| internal_error!("type {t:?} is not interned"))
    }

    fn finish(self, what: &str) -> Result<ValidatedProgram, InternalError> {
        self.b.finish().map_err(|errors| {
            internal_error!(
                "the lowered `.ded` {what} program is invalid: {}",
                errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
            )
        })
    }

    fn schema(&self, cols: &[(usize, ColTy)]) -> Result<Schema, InternalError> {
        let mut out = Vec::with_capacity(cols.len());
        for &(molly_col, t) in cols {
            out.push(Column {
                name: Symbol::intern(&format!("c{molly_col}")),
                ty: self.tid(t)?,
                field_no: None,
                default: None,
                since: None,
                deprecated: None,
                hidden_dest: false,
            });
        }
        let key = (0..out.len())
            .filter_map(|i| u32::try_from(i).ok().map(blossom_base::ColIdx::from_raw))
            .collect();
        Ok(Schema {
            cols: out,
            key,
            payload: Vec::new(),
            lattice: Vec::new(),
        })
    }

    fn declare(
        &mut self,
        name: &str,
        class: RelClass,
        cols: &[(usize, ColTy)],
        origin: Origin,
        span: Span,
    ) -> Result<RelId, InternalError> {
        let schema = self.schema(cols)?;
        self.b
            .declare_relation(RelDecl {
                id: RelId::from_raw(0),
                name: QualName::single(Symbol::intern(name)),
                class,
                schema,
                persistence: Persistence::None,
                durable: false,
                interface: None,
                placement: Placement::Shared,
                origin,
                attrs: attrs(),
                span,
            })
            .map_err(ir_internal)
    }

    fn protocol_relations(
        &mut self,
        model: &Model,
        types: &Types,
        rels: &mut [LogicalRel],
    ) -> Result<(), InternalError> {
        for (i, info) in model.rels.iter().enumerate() {
            if info.kind != LogicalKind::Protocol {
                continue;
            }
            let cols: Vec<(usize, ColTy)> = (1..info.arity)
                .map(|c| col_ty(types, i, c).map(|t| (c, t)))
                .collect::<Result<_, _>>()?;
            let has_rules = info.local_heads || info.async_heads;
            let class = if has_rules || !info.facts {
                RelClass::Idb
            } else {
                RelClass::Event(EventSource::Input)
            };
            let rel = self.declare(info.name.as_str(), class, &cols, Origin::User(info.first), info.first)?;
            let entry = rels
                .get_mut(i)
                .ok_or_else(|| internal_error!("relation table out of step"))?;
            entry.protocol = Some(rel);
            if !has_rules && info.facts {
                entry.input = Some(rel);
            }
            if !(info.async_heads || (info.facts && has_rules)) {
                continue;
            }
            let construct = self
                .b
                .begin_construct(
                    ConstructKind::DedRelation { rel },
                    SurfaceRef {
                        module: QualName::single(Symbol::intern("ded")),
                        label: None,
                        stmt: None,
                        span: info.first,
                    },
                )
                .map_err(ir_internal)?;
            let generated = Origin::Generated {
                construct: ConstructId::from_raw(0),
            };
            if info.async_heads {
                let mut chan_cols = vec![(0, ColTy::Node)];
                chan_cols.extend(cols.iter().copied());
                let chan = self.declare(
                    &format!("{}$async", info.name),
                    RelClass::Channel(channel()),
                    &chan_cols,
                    generated.clone(),
                    info.first,
                )?;
                self.bridge(rel, chan, true, &cols, info.first, &format!("{}$async", info.name))?;
                entry.channel = Some(chan);
            }
            if info.facts && has_rules {
                let input = self.declare(
                    &format!("{}$in", info.name),
                    RelClass::Event(EventSource::Input),
                    &cols,
                    generated,
                    info.first,
                )?;
                self.bridge(rel, input, false, &cols, info.first, &format!("{}$in", info.name))?;
                entry.input = Some(input);
            }
            self.b.end_construct(construct).map_err(ir_internal)?;
        }
        Ok(())
    }

    /// `rel(x̄) :- src(_, x̄)` for a channel, `rel(x̄) :- src(x̄)` for an input.
    fn bridge(
        &mut self,
        rel: RelId,
        src: RelId,
        channel: bool,
        cols: &[(usize, ColTy)],
        span: Span,
        name: &str,
    ) -> Result<(), InternalError> {
        let mut vars = Vec::with_capacity(cols.len());
        let mut rb = self
            .b
            .rule(RuleKind::Deductive, RuleLabel::new(format!("{name}#bridge")), span);
        for &(c, t) in cols {
            let ty = *self
                .ty
                .get(&t)
                .ok_or_else(|| internal_error!("type {t:?} is not interned"))?;
            vars.push(rb.var(Symbol::intern(&format!("C{c}")), ty).map_err(ir_internal)?);
        }
        let mut args: Vec<ir::Term> = Vec::with_capacity(cols.len() + 1);
        if channel {
            args.push(ir::Term::Wild);
        }
        args.extend(vars.iter().map(|v| ir::Term::Var(*v)));
        rb.lit(Literal::Pos(atom(src, args, span)));
        rb.head(
            Head {
                rel,
                args: vars.iter().map(|v| HeadArg::Term(ir::Term::Var(*v))).collect(),
                mode: HeadMode::Insert,
            },
            None,
        )
        .map_err(ir_internal)?;
        Ok(())
    }

    fn spec_relations(
        &mut self,
        program: &Program,
        model: &Model,
        types: &Types,
        rels: &mut [LogicalRel],
    ) -> Result<Vec<SpecFeed>, InternalError> {
        // What the spec reads: protocol relations now and at fixed times, and the crash oracle.
        let mut now: BTreeSet<usize> = BTreeSet::new();
        let mut at: BTreeSet<(usize, u64)> = BTreeSet::new();
        for (ri, rule) in program.rules.iter().enumerate() {
            if !model.spec_rule.get(ri).copied().unwrap_or(false) {
                continue;
            }
            for a in body_atoms(rule) {
                let Some(i) = model.index(a.rel.text) else { continue };
                let kind = model.rels.get(i).map(|r| r.kind);
                match (kind, a.time) {
                    (Some(LogicalKind::Spec), _) => {}
                    (_, Some(k)) => {
                        at.insert((i, k));
                    }
                    (_, None) => {
                        now.insert(i);
                    }
                }
            }
        }
        let mut feeds = Vec::new();
        for (i, info) in model.rels.iter().enumerate() {
            let cols: Vec<(usize, ColTy)> = (0..info.arity)
                .map(|c| col_ty(types, i, c).map(|t| (c, t)))
                .collect::<Result<_, _>>()?;
            let entry = rels
                .get_mut(i)
                .ok_or_else(|| internal_error!("relation table out of step"))?;
            let ded_idx = idx(i).ok_or_else(|| internal_error!("too many relations"))?;
            match info.kind {
                LogicalKind::Spec => {
                    let rel = self.declare(
                        info.name.as_str(),
                        RelClass::Idb,
                        &cols,
                        Origin::User(info.first),
                        info.first,
                    )?;
                    entry.spec = Some(rel);
                }
                LogicalKind::Crash | LogicalKind::Protocol if now.contains(&i) => {
                    let rel = self.declare(
                        info.name.as_str(),
                        RelClass::Event(EventSource::Input),
                        &cols,
                        Origin::User(info.first),
                        info.first,
                    )?;
                    entry.spec = Some(rel);
                    feeds.push(if info.kind == LogicalKind::Crash {
                        SpecFeed::Crash { spec: rel }
                    } else {
                        SpecFeed::AtEot {
                            spec: rel,
                            rel: ded_idx,
                        }
                    });
                }
                LogicalKind::Crash | LogicalKind::Protocol => {}
            }
            for &(_, k) in at.range((i, 0)..=(i, u64::MAX)) {
                let rel = self.declare(
                    &format!("{}@{k}", info.name),
                    RelClass::Event(EventSource::Input),
                    &cols,
                    Origin::User(info.first),
                    info.first,
                )?;
                entry.spec_at.push((Tick(k), rel));
                feeds.push(SpecFeed::AtTick {
                    spec: rel,
                    rel: ded_idx,
                    tick: Tick(k),
                });
            }
        }
        Ok(feeds)
    }

    fn protocol_rule(
        &mut self,
        rule: &ast::Rule,
        model: &Model,
        rels: &[LogicalRel],
        values: &Values<'_>,
        labels: &mut Labels,
        diags: &mut Diagnostics,
    ) -> Result<(), InternalError> {
        let head_name = rule.head.rel.text;
        let head_rel = rels_of(model, rels, head_name)?;
        let (kind, target) = match rule.time {
            HeadTime::Now => (RuleKind::Deductive, head_rel.protocol),
            HeadTime::Next => (RuleKind::Inductive, head_rel.protocol),
            HeadTime::Async => (RuleKind::Async, head_rel.channel),
        };
        let target = target.ok_or_else(|| internal_error!("`{head_name}` has no protocol relation for its rule"))?;
        let first = body_atoms(rule)
            .next()
            .ok_or_else(|| internal_error!("a checked rule has no body atom"))?;
        let loc = first
            .args
            .first()
            .and_then(super::model::arg_term)
            .ok_or_else(|| internal_error!("a checked rule has no location"))?
            .clone();
        // Variables used outside location positions get IR variables; the location variable among them is `$self`.
        let mut used: BTreeMap<Symbol, Span> = BTreeMap::new();
        for a in body_atoms(rule) {
            for arg in a.args.iter().skip(1) {
                collect_arg(arg, &mut used);
            }
        }
        for item in &rule.body {
            if let BodyItem::Qual(e) = item {
                collect_expr(e, &mut used);
            }
        }
        let head_skip = usize::from(rule.time != HeadTime::Async);
        for arg in rule.head.args.iter().skip(head_skip) {
            collect_arg(arg, &mut used);
        }
        let label = labels.label(head_name.as_str());
        let Some(mut rl) = RuleLowerer::new(self, rule, kind, label, values, diags)? else {
            return Ok(());
        };
        for (v, span) in &used {
            let ty = values.ty_at(*span)?;
            rl.declare_var(*v, ty)?;
        }
        match &loc {
            Term::Var(v) if used.contains_key(&v.text) => {
                let var = rl.var(v.text)?;
                rl.rb.lit(Literal::Bind {
                    pat: ir::Pattern::Var(var),
                    expr: ir::Expr::Scalar(ir::BuiltinScalar::SelfNode),
                });
            }
            Term::Var(_) => {}
            Term::Str(..) | Term::Int(..) => {
                let c = rl.constant(&loc)?;
                rl.rb.lit(Literal::Guard(ir::Expr::Binary {
                    op: ir::BinOp::Eq,
                    lhs: Box::new(ir::Expr::Scalar(ir::BuiltinScalar::SelfNode)),
                    rhs: Box::new(ir::Expr::Term(c)),
                }));
            }
            Term::Wild(_) => return Err(internal_error!("a checked protocol rule has a `_` location")),
        }
        for item in &rule.body {
            match item {
                BodyItem::Atom(a) => {
                    let rel = rels_of(model, rels, a.rel.text)?
                        .protocol
                        .ok_or_else(|| internal_error!("`{}` has no protocol relation", a.rel.text))?;
                    let args = rl.terms(a.args.iter().skip(1))?;
                    let at = atom(rel, args, a.span);
                    rl.rb.lit(if a.negated { Literal::Neg(at) } else { Literal::Pos(at) });
                }
                BodyItem::Qual(e) => {
                    let e = rl.expr(e)?;
                    rl.rb.lit(Literal::Guard(e));
                }
            }
        }
        let args = rl.head_args(rule.head.args.iter().skip(head_skip), values)?;
        rl.finish(target, args)
    }

    fn spec_rule(
        &mut self,
        rule: &ast::Rule,
        model: &Model,
        rels: &[LogicalRel],
        values: &Values<'_>,
        labels: &mut Labels,
        diags: &mut Diagnostics,
    ) -> Result<(), InternalError> {
        let head_name = rule.head.rel.text;
        let target = rels_of(model, rels, head_name)?
            .spec
            .ok_or_else(|| internal_error!("`{head_name}` has no spec relation"))?;
        let mut used: BTreeMap<Symbol, Span> = BTreeMap::new();
        for a in body_atoms(rule) {
            for arg in &a.args {
                collect_arg(arg, &mut used);
            }
        }
        for item in &rule.body {
            if let BodyItem::Qual(e) = item {
                collect_expr(e, &mut used);
            }
        }
        for arg in &rule.head.args {
            collect_arg(arg, &mut used);
        }
        let label = labels.label(head_name.as_str());
        let Some(mut rl) = RuleLowerer::new(self, rule, RuleKind::Deductive, label, values, diags)? else {
            return Ok(());
        };
        for (v, span) in &used {
            let ty = values.ty_at(*span)?;
            rl.declare_var(*v, ty)?;
        }
        for item in &rule.body {
            match item {
                BodyItem::Atom(a) => {
                    let r = rels_of(model, rels, a.rel.text)?;
                    let rel = match a.time {
                        None => r.spec,
                        Some(k) if r.kind == LogicalKind::Spec => {
                            diags.push(super::model::not_yet(
                                &format!("`{}@{k}`: reading a spec relation at a fixed time", a.rel.text),
                                a.span,
                            ));
                            return Ok(());
                        }
                        Some(k) => r.spec_at.iter().find(|(t, _)| *t == Tick(k)).map(|(_, rel)| *rel),
                    }
                    .ok_or_else(|| internal_error!("`{}` has no spec relation", a.rel.text))?;
                    let args = rl.terms(a.args.iter())?;
                    let at = atom(rel, args, a.span);
                    rl.rb.lit(if a.negated { Literal::Neg(at) } else { Literal::Pos(at) });
                }
                BodyItem::Qual(e) => {
                    let e = rl.expr(e)?;
                    rl.rb.lit(Literal::Guard(e));
                }
            }
        }
        let args = rl.head_args(rule.head.args.iter(), values)?;
        rl.finish(target, args)
    }
}

/// Lowers one rule's terms and expressions into a [`RuleBuilder`].
struct RuleLowerer<'l> {
    rb: RuleBuilder<'l>,
    ty: BTreeMap<ColTy, TypeId>,
    vars: BTreeMap<Symbol, VarId>,
    /// Every literal of the rule, interned before the rule builder borrows the program builder.
    consts: BTreeMap<Span, ConstId>,
    temps: u32,
}

impl<'l> RuleLowerer<'l> {
    /// Interns the rule's literals, then opens the rule. `None` when a literal cannot have its inferred type (the
    /// diagnostics say why).
    fn new(
        l: &'l mut Lowerer,
        rule: &ast::Rule,
        kind: RuleKind,
        label: RuleLabel,
        values: &Values<'_>,
        diags: &mut Diagnostics,
    ) -> Result<Option<RuleLowerer<'l>>, InternalError> {
        let mut literals = Vec::new();
        for a in body_atoms(rule) {
            for arg in &a.args {
                literal_terms(arg, &mut literals);
            }
        }
        for item in &rule.body {
            if let BodyItem::Qual(e) = item {
                expr_literals(e, &mut literals);
            }
        }
        for arg in &rule.head.args {
            literal_terms(arg, &mut literals);
        }
        let mut consts = BTreeMap::new();
        let mut ok = true;
        for t in literals {
            let ty = values.ty_at(t.span())?;
            match values.literal(t, ty)? {
                Ok(v) => {
                    consts.insert(t.span(), l.b.intern_const(v).map_err(ir_internal)?);
                }
                Err(d) => {
                    diags.push(d);
                    ok = false;
                }
            }
        }
        if !ok {
            return Ok(None);
        }
        let ty = l.ty.clone();
        Ok(Some(RuleLowerer {
            rb: l.b.rule(kind, label, rule.span),
            ty,
            vars: BTreeMap::new(),
            consts,
            temps: 0,
        }))
    }

    fn tid(&self, t: ColTy) -> Result<TypeId, InternalError> {
        self.ty
            .get(&t)
            .copied()
            .ok_or_else(|| internal_error!("type {t:?} is not interned"))
    }

    fn declare_var(&mut self, name: Symbol, ty: ColTy) -> Result<VarId, InternalError> {
        let id = self.rb.var(name, self.tid(ty)?).map_err(ir_internal)?;
        self.vars.insert(name, id);
        Ok(id)
    }

    fn var(&self, name: Symbol) -> Result<VarId, InternalError> {
        self.vars
            .get(&name)
            .copied()
            .ok_or_else(|| internal_error!("variable `{name}` was not declared"))
    }

    fn constant(&self, t: &Term) -> Result<ir::Term, InternalError> {
        self.consts
            .get(&t.span())
            .map(|c| ir::Term::Const(*c))
            .ok_or_else(|| internal_error!("the literal at {:?} was not interned", t.span()))
    }

    fn term(&self, t: &Term) -> Result<ir::Term, InternalError> {
        match t {
            Term::Var(v) => Ok(ir::Term::Var(self.var(v.text)?)),
            Term::Wild(_) => Ok(ir::Term::Wild),
            Term::Int(..) | Term::Str(..) => self.constant(t),
        }
    }

    fn terms<'a>(&self, args: impl Iterator<Item = &'a Arg>) -> Result<Vec<ir::Term>, InternalError> {
        args.map(|a| match a {
            Arg::Expr(Expr::Term(t)) => self.term(t),
            _ => Err(internal_error!("a checked body atom holds a non-term")),
        })
        .collect()
    }

    fn expr(&self, e: &Expr) -> Result<ir::Expr, InternalError> {
        Ok(match e {
            Expr::Term(t) => ir::Expr::Term(self.term(t)?),
            Expr::Binary { lhs, op, rhs, .. } => ir::Expr::Binary {
                op: binop(*op),
                lhs: Box::new(ir::Expr::Term(self.term(lhs)?)),
                rhs: Box::new(self.expr(rhs)?),
            },
        })
    }

    fn head_args<'a>(
        &mut self,
        args: impl Iterator<Item = &'a Arg>,
        values: &Values<'_>,
    ) -> Result<Vec<HeadArg>, InternalError> {
        let mut out = Vec::new();
        for a in args {
            match a {
                Arg::Expr(Expr::Term(t)) => out.push(HeadArg::Term(self.term(t)?)),
                Arg::Expr(e @ Expr::Binary { span, .. }) => {
                    let ty = values.ty_at(*span)?;
                    self.temps += 1;
                    let tmp = self.declare_var(Symbol::intern(&format!("$h{}", self.temps)), ty)?;
                    let value = self.expr(e)?;
                    self.rb.lit(Literal::Bind {
                        pat: ir::Pattern::Var(tmp),
                        expr: value,
                    });
                    out.push(HeadArg::Term(ir::Term::Var(tmp)));
                }
                Arg::Agg(g) => {
                    let func = match g.func {
                        AggFunc::Count => ir::AggFunc::Count,
                        AggFunc::Min => ir::AggFunc::Min,
                        AggFunc::Max => ir::AggFunc::Max,
                        AggFunc::Sum => ir::AggFunc::Sum,
                    };
                    out.push(HeadArg::Agg(AggCall {
                        func,
                        args: vec![ir::Term::Var(self.var(g.var.text)?)],
                        order: None,
                    }));
                }
            }
        }
        Ok(out)
    }

    fn finish(self, rel: RelId, args: Vec<HeadArg>) -> Result<(), InternalError> {
        self.rb
            .head(
                Head {
                    rel,
                    args,
                    mode: HeadMode::Insert,
                },
                None,
            )
            .map_err(ir_internal)?;
        Ok(())
    }
}

fn binop(op: BinOp) -> ir::BinOp {
    match op {
        BinOp::Add => ir::BinOp::Add,
        BinOp::Sub => ir::BinOp::Sub,
        BinOp::Mul => ir::BinOp::Mul,
        BinOp::Div => ir::BinOp::Div,
        BinOp::Lt => ir::BinOp::Lt,
        BinOp::Gt => ir::BinOp::Gt,
        BinOp::Le => ir::BinOp::Le,
        BinOp::Ge => ir::BinOp::Ge,
        BinOp::Eq => ir::BinOp::Eq,
        BinOp::Ne => ir::BinOp::Ne,
    }
}

fn atom(rel: RelId, args: Vec<ir::Term>, span: Span) -> Atom {
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

fn channel() -> ChannelDecl {
    ChannelDecl {
        form: ChannelForm::NodeToNode,
        loopback: false,
        host_endpoint: false,
        fault: FaultModel::Lossy,
        partition: None,
        sealed_by: None,
        wrapper: None,
        acl: AclSpec::Inferred,
        egress_to_external: false,
        replicated: false,
    }
}

fn attrs() -> RelAttrs {
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

fn program_id(name: Symbol) -> [u8; 16] {
    let a = blossom_base::span::stable_hash(name.as_str().as_bytes());
    let b = blossom_base::span::stable_hash(format!("{name}#ded").as_bytes());
    let mut id = [0u8; 16];
    for (slot, byte) in id.iter_mut().zip(a.to_le_bytes().into_iter().chain(b.to_le_bytes())) {
        *slot = byte;
    }
    id
}

fn col_ty(types: &Types, rel: usize, col: usize) -> Result<ColTy, InternalError> {
    types
        .col(rel, col)
        .ok_or_else(|| internal_error!("no inferred type for column {col} of relation {rel}"))
}

fn rels_of<'r>(model: &Model, rels: &'r [LogicalRel], name: Symbol) -> Result<&'r LogicalRel, InternalError> {
    model
        .index(name)
        .and_then(|i| rels.get(i))
        .ok_or_else(|| internal_error!("unknown relation `{name}`"))
}

fn literal_terms<'a>(a: &'a Arg, out: &mut Vec<&'a Term>) {
    if let Arg::Expr(e) = a {
        expr_literals(e, out);
    }
}

fn expr_literals<'a>(e: &'a Expr, out: &mut Vec<&'a Term>) {
    let mut term = |t: &'a Term| {
        if matches!(t, Term::Int(..) | Term::Str(..)) {
            out.push(t);
        }
    };
    match e {
        Expr::Term(t) => term(t),
        Expr::Binary { lhs, rhs, .. } => {
            term(lhs);
            expr_literals(rhs, out);
        }
    }
}

fn collect_arg(a: &Arg, out: &mut BTreeMap<Symbol, Span>) {
    match a {
        Arg::Expr(e) => collect_expr(e, out),
        Arg::Agg(g) => {
            out.entry(g.var.text).or_insert(g.var.span);
        }
    }
}

fn collect_expr(e: &Expr, out: &mut BTreeMap<Symbol, Span>) {
    let mut term = |t: &Term| {
        if let Term::Var(v) = t {
            out.entry(v.text).or_insert(v.span);
        }
    };
    match e {
        Expr::Term(t) => term(t),
        Expr::Binary { lhs, rhs, .. } => {
            term(lhs);
            collect_expr(rhs, out);
        }
    }
}
