#![allow(dead_code, unused_variables)]
//! Exhaustive traversal of typed IR references. Used by validation and canonical relabeling.
use super::core::*;
use blossom_base::{IndexVec, QualName, RuleLabel, Span, Symbol, idx::*};
use blossom_value::{TypeTable, Value, time::Duration, types::FieldNo};
use std::sync::Arc;
/// Typed reference mapping. Identity defaults make focused traversals exhaustive.
pub(crate) trait Mapper {
    fn typeid(&mut self, id: TypeId) -> TypeId {
        id
    }
    fn latticetypeid(&mut self, id: LatticeTypeId) -> LatticeTypeId {
        id
    }
    fn grouptypeid(&mut self, id: GroupTypeId) -> GroupTypeId {
        id
    }
    fn constid(&mut self, id: ConstId) -> ConstId {
        id
    }
    fn paramid(&mut self, id: ParamId) -> ParamId {
        id
    }
    fn fnid(&mut self, id: FnId) -> FnId {
        id
    }
    fn udaid(&mut self, id: UdaId) -> UdaId {
        id
    }
    fn serviceid(&mut self, id: ServiceId) -> ServiceId {
        id
    }
    fn roleid(&mut self, id: RoleId) -> RoleId {
        id
    }
    fn relid(&mut self, id: RelId) -> RelId {
        id
    }
    fn ruleid(&mut self, id: RuleId) -> RuleId {
        id
    }
    fn varid(&mut self, id: VarId) -> VarId {
        id
    }
    fn siteid(&mut self, id: SiteId) -> SiteId {
        id
    }
    fn constructid(&mut self, id: ConstructId) -> ConstructId {
        id
    }
    fn stratumid(&mut self, id: StratumId) -> StratumId {
        id
    }
    fn colidx(&mut self, id: ColIdx) -> ColIdx {
        id
    }
    fn invariantid(&mut self, id: InvariantId) -> InvariantId {
        id
    }
    fn occid(&mut self, id: OccId) -> OccId {
        id
    }
    fn span(&mut self, span: Span) -> Span {
        span
    }
}
pub(crate) trait Remap: Sized {
    fn remap(&self, m: &mut impl Mapper) -> Self;
}
impl Remap for TypeId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.typeid(*self)
    }
}
impl Remap for LatticeTypeId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.latticetypeid(*self)
    }
}
impl Remap for GroupTypeId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.grouptypeid(*self)
    }
}
impl Remap for ConstId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.constid(*self)
    }
}
impl Remap for ParamId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.paramid(*self)
    }
}
impl Remap for FnId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.fnid(*self)
    }
}
impl Remap for UdaId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.udaid(*self)
    }
}
impl Remap for ServiceId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.serviceid(*self)
    }
}
impl Remap for RoleId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.roleid(*self)
    }
}
impl Remap for RelId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.relid(*self)
    }
}
impl Remap for RuleId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.ruleid(*self)
    }
}
impl Remap for VarId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.varid(*self)
    }
}
impl Remap for SiteId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.siteid(*self)
    }
}
impl Remap for ConstructId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.constructid(*self)
    }
}
impl Remap for StratumId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.stratumid(*self)
    }
}
impl Remap for ColIdx {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.colidx(*self)
    }
}
impl Remap for InvariantId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.invariantid(*self)
    }
}
impl Remap for OccId {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.occid(*self)
    }
}
impl Remap for Span {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        m.span(*self)
    }
}
impl Remap for u16 {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for u32 {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for u64 {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for bool {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for Arc<str> {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        self.clone()
    }
}
impl Remap for QualName {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        self.clone()
    }
}
impl Remap for RuleLabel {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        self.clone()
    }
}
impl Remap for Symbol {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for Value {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        self.clone()
    }
}
impl Remap for FieldNo {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for Duration {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for MonoClass {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for HeightClass {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for LawStatus {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for Claim {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for LatOpKind {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for ProofStatus {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl Remap for TypeTable {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        self.clone()
    }
}
impl Remap for [u8; 16] {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        *self
    }
}
impl<T: Remap> Remap for Vec<T> {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        self.iter().map(|x| x.remap(m)).collect()
    }
}
impl<T: Remap> Remap for Option<T> {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        self.as_ref().map(|x| x.remap(m))
    }
}
impl<T: Remap> Remap for Box<T> {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Box::new((**self).remap(m))
    }
}
impl<I: Idx, T: Remap + Clone> Remap for IndexVec<I, T> {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        let mut x = self.clone();
        for v in x.iter_mut() {
            *v = v.remap(m);
        }
        x
    }
}
impl<A: Remap, B: Remap> Remap for (A, B) {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        (self.0.remap(m), self.1.remap(m))
    }
}
impl<A: Remap, B: Remap, C: Remap> Remap for (A, B, C) {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        (self.0.remap(m), self.1.remap(m), self.2.remap(m))
    }
}
impl Remap for LatticeDef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            ctor: self.ctor.remap(m),
            ops: self.ops.remap(m),
            height: self.height.remap(m),
            laws: self.laws.remap(m),
            distributive: self.distributive.remap(m),
            dense_domain: self.dense_domain.remap(m),
        }
    }
}
impl Remap for LatticeCtor {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Bool => Self::Bool,
            Self::Max(v0) => Self::Max(v0.remap(m)),
            Self::Min(v0) => Self::Min(v0.remap(m)),
            Self::Set(v0) => Self::Set(v0.remap(m)),
            Self::Map(v0, v1) => Self::Map(v0.remap(m), v1.remap(m)),
            Self::Bag(v0) => Self::Bag(v0.remap(m)),
            Self::PSet(v0) => Self::PSet(v0.remap(m)),
            Self::Pair(v0, v1) => Self::Pair(v0.remap(m), v1.remap(m)),
            Self::Product(v0) => Self::Product(v0.remap(m)),
            Self::Lex { chain, inner } => Self::Lex {
                chain: chain.remap(m),
                inner: inner.remap(m),
            },
            Self::WithBot(v0) => Self::WithBot(v0.remap(m)),
            Self::WithTop(v0) => Self::WithTop(v0.remap(m)),
            Self::Conflict(v0) => Self::Conflict(v0.remap(m)),
            Self::Point(v0) => Self::Point(v0.remap(m)),
            Self::Unit => Self::Unit,
            Self::VecUnion(v0) => Self::VecUnion(v0.remap(m)),
            Self::UnionFind(v0) => Self::UnionFind(v0.remap(m)),
            Self::Dom { version, value } => Self::Dom {
                version: version.remap(m),
                value: value.remap(m),
            },
            Self::Causal(v0) => Self::Causal(v0.remap(m)),
            Self::Tombstone { base, tomb } => Self::Tombstone {
                base: base.remap(m),
                tomb: tomb.remap(m),
            },
            Self::DomPairUnsafe(v0, v1) => Self::DomPairUnsafe(v0.remap(m), v1.remap(m)),
            Self::Extern(v0) => Self::Extern(v0.remap(m)),
        }
    }
}
impl Remap for LatOpDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            name: self.name.remap(m),
            params: self.params.remap(m),
            ret: self.ret.remap(m),
            kind: self.kind.remap(m),
            join_prime: self.join_prime.remap(m),
            derivative: self.derivative.remap(m),
            incompatible_thresholds: self.incompatible_thresholds.remap(m),
        }
    }
}
impl Remap for FnDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            params: self.params.remap(m),
            ret: self.ret.remap(m),
            vars: self.vars.remap(m),
            body: self.body.remap(m),
            props: self.props.remap(m),
        }
    }
}
impl Remap for FnBody {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Ir(v0) => Self::Ir(v0.remap(m)),
            Self::Extern { path, memo } => Self::Extern {
                path: path.remap(m),
                memo: memo.remap(m),
            },
            Self::TableFn { path, outputs } => Self::TableFn {
                path: path.remap(m),
                outputs: outputs.remap(m),
            },
            Self::Builtin(v0) => Self::Builtin(v0.remap(m)),
        }
    }
}
impl Remap for FnProps {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            classes: self.classes.remap(m),
            injective: self.injective.remap(m),
            commutative: self.commutative.remap(m),
            associative: self.associative.remap(m),
            idempotent: self.idempotent.remap(m),
            stable_after: self.stable_after.remap(m),
        }
    }
}
impl Remap for UdaDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            state: self.state.remap(m),
            init: self.init.remap(m),
            step: self.step.remap(m),
            combine: self.combine.remap(m),
            finish: self.finish.remap(m),
            props: self.props.remap(m),
        }
    }
}
impl Remap for ServiceDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            call: self.call.remap(m),
            result: self.result.remap(m),
        }
    }
}
impl Remap for StreamDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            name: self.name.remap(m),
            kind: self.kind,
            placement: self.placement.remap(m),
            opened: self.opened.remap(m),
            data: self.data.remap(m),
            closed: self.closed.remap(m),
            failed: self.failed.remap(m),
            write: self.write.remap(m),
            close: self.close.remap(m),
            dial: self.dial.remap(m),
        }
    }
}
impl Remap for Program {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            meta: self.meta.remap(m),
            types: self.types.remap(m),
            lattices: self.lattices.remap(m),
            groups: self.groups.remap(m),
            consts: self.consts.remap(m),
            params: self.params.remap(m),
            fns: self.fns.remap(m),
            udas: self.udas.remap(m),
            services: self.services.remap(m),
            streams: self.streams.remap(m),
            roles: self.roles.remap(m),
            rels: self.rels.remap(m),
            rules: self.rules.remap(m),
            facts: self.facts.remap(m),
            constructs: self.constructs.remap(m),
            sites: self.sites.remap(m),
            invariants: self.invariants.remap(m),
            migrations: self.migrations.remap(m),
            translations: self.translations.remap(m),
        }
    }
}
impl Remap for ProgramMeta {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            name: self.name.remap(m),
            version: self.version.remap(m),
            edition: self.edition.remap(m),
            compiler: self.compiler.remap(m),
            prf_version: self.prf_version.remap(m),
            encoding_version: self.encoding_version.remap(m),
            program_id: self.program_id.remap(m),
        }
    }
}
impl Remap for RoleDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            kind: self.kind.remap(m),
        }
    }
}
impl Remap for RoleKind {
    fn remap(&self, _m: &mut impl Mapper) -> Self {
        match self {
            Self::Process => Self::Process,
            Self::Cluster => Self::Cluster,
            Self::External => Self::External,
        }
    }
}
impl Remap for RelDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            class: self.class.remap(m),
            schema: self.schema.remap(m),
            persistence: self.persistence.remap(m),
            durable: self.durable.remap(m),
            interface: self.interface.remap(m),
            placement: self.placement.remap(m),
            origin: self.origin.remap(m),
            attrs: self.attrs.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for RelClass {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Idb => Self::Idb,
            Self::Static => Self::Static,
            Self::Event(v0) => Self::Event(v0.remap(m)),
            Self::Channel(v0) => Self::Channel(v0.remap(m)),
            Self::Weighted(v0) => Self::Weighted(v0.remap(m)),
            Self::HostTable => Self::HostTable,
            Self::HostOut(op) => Self::HostOut(*op),
        }
    }
}
impl Remap for EventSource {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Input => Self::Input,
            Self::InputSeal { input } => Self::InputSeal { input: input.remap(m) },
            Self::Timer(v0) => Self::Timer(v0.remap(m)),
            Self::Boot => Self::Boot,
            Self::Recovered => Self::Recovered,
            Self::Stdin => Self::Stdin,
            Self::SessionOpen => Self::SessionOpen,
            Self::SessionClosed => Self::SessionClosed,
            Self::ServiceResult(v0) => Self::ServiceResult(v0.remap(m)),
            Self::Stream(e) => Self::Stream(*e),
            Self::ClusterVersion => Self::ClusterVersion,
        }
    }
}
impl Remap for TimerDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            clock: self.clock.remap(m),
            every: self.every.remap(m),
            ticks: self.ticks.remap(m),
            times: self.times.remap(m),
            once_after: self.once_after.remap(m),
            once: self.once.remap(m),
        }
    }
}
impl Remap for ChannelDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            form: self.form.remap(m),
            loopback: self.loopback.remap(m),
            host_endpoint: self.host_endpoint.remap(m),
            fault: self.fault.remap(m),
            partition: self.partition.remap(m),
            sealed_by: self.sealed_by.remap(m),
            wrapper: self.wrapper.remap(m),
            acl: self.acl.remap(m),
            egress_to_external: self.egress_to_external.remap(m),
            replicated: self.replicated.remap(m),
        }
    }
}
impl Remap for Schema {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            cols: self.cols.remap(m),
            key: self.key.remap(m),
            payload: self.payload.remap(m),
            lattice: self.lattice.remap(m),
        }
    }
}
impl Remap for Column {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            name: self.name.remap(m),
            ty: self.ty.remap(m),
            field_no: self.field_no.remap(m),
            default: self.default.remap(m),
            since: self.since.remap(m),
            deprecated: self.deprecated.remap(m),
            hidden_dest: self.hidden_dest.remap(m),
        }
    }
}
impl Remap for Persistence {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::None => Self::None,
            Self::Frame { rule, del } => Self::Frame {
                rule: rule.remap(m),
                del: del.remap(m),
            },
            Self::Identity { rule } => Self::Identity { rule: rule.remap(m) },
            Self::Resolved { construct } => Self::Resolved {
                construct: construct.remap(m),
            },
            Self::Soft { construct } => Self::Soft {
                construct: construct.remap(m),
            },
        }
    }
}
impl Remap for RelAttrs {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            nondet: self.nondet.remap(m),
            deterministic: self.deterministic.remap(m),
            monotone: self.monotone.remap(m),
            final_output: self.final_output.remap(m),
            atomic: self.atomic.remap(m),
            handler: self.handler.remap(m),
            materialize: self.materialize.remap(m),
            finite: self.finite.remap(m),
            range_col: self.range_col.remap(m),
            partition: self.partition.remap(m),
            sealed_by: self.sealed_by.remap(m),
        }
    }
}
impl Remap for Fact {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            row: self.row.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for Rule {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            label: self.label.remap(m),
            kind: self.kind.remap(m),
            head: self.head.remap(m),
            body: self.body.remap(m),
            role: self.role.remap(m),
            construct: self.construct.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for RuleKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Deductive => Self::Deductive,
            Self::Inductive => Self::Inductive,
            Self::Async => Self::Async,
        }
    }
}
impl Remap for Head {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            args: self.args.remap(m),
            mode: self.mode.remap(m),
        }
    }
}
impl Remap for HeadArg {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Term(v0) => Self::Term(v0.remap(m)),
            Self::Agg(v0) => Self::Agg(v0.remap(m)),
        }
    }
}
impl Remap for HeadMode {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Insert => Self::Insert,
            Self::ZAdd { weight } => Self::ZAdd {
                weight: weight.remap(m),
            },
            Self::Violation { invariant } => Self::Violation {
                invariant: invariant.remap(m),
            },
        }
    }
}
impl Remap for AggCall {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            func: self.func.remap(m),
            args: self.args.remap(m),
            order: self.order.remap(m),
        }
    }
}
impl Remap for AggFunc {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Count => Self::Count,
            Self::Sum => Self::Sum,
            Self::Min => Self::Min,
            Self::Max => Self::Max,
            Self::Avg => Self::Avg,
            Self::BoolAnd => Self::BoolAnd,
            Self::BoolOr => Self::BoolOr,
            Self::CollectVec => Self::CollectVec,
            Self::CollectSet => Self::CollectSet,
            Self::CollectMap => Self::CollectMap,
            Self::Percentile { num, den } => Self::Percentile {
                num: num.remap(m),
                den: den.remap(m),
            },
            Self::OlaSum => Self::OlaSum,
            Self::OlaCount => Self::OlaCount,
            Self::OlaAvg => Self::OlaAvg,
            Self::Uda(v0) => Self::Uda(v0.remap(m)),
        }
    }
}
impl Remap for Body {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            vars: self.vars.remap(m),
            lits: self.lits.remap(m),
        }
    }
}
impl Remap for VarDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            name: self.name.remap(m),
            ty: self.ty.remap(m),
            non_bottom: self.non_bottom.remap(m),
        }
    }
}
impl Remap for Literal {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Pos(v0) => Self::Pos(v0.remap(m)),
            Self::Neg(v0) => Self::Neg(v0.remap(m)),
            Self::Bind { pat, expr } => Self::Bind {
                pat: pat.remap(m),
                expr: expr.remap(m),
            },
            Self::Guard(v0) => Self::Guard(v0.remap(m)),
            Self::Lookup { var, rel, key } => Self::Lookup {
                var: var.remap(m),
                rel: rel.remap(m),
                key: key.remap(m),
            },
            Self::Gen { pat, src } => Self::Gen {
                pat: pat.remap(m),
                src: src.remap(m),
            },
        }
    }
}
impl Remap for Atom {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            args: self.args.remap(m),
            sender: self.sender.remap(m),
            principal: self.principal.remap(m),
            weight: self.weight.remap(m),
            spec: self.spec.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for SpecAt {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            loc: self.loc.remap(m),
            time: self.time.remap(m),
        }
    }
}
impl Remap for SpecTime {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Eval => Self::Eval,
            Self::At(v0) => Self::At(v0.remap(m)),
            Self::Ever => Self::Ever,
            Self::Sent => Self::Sent,
        }
    }
}
impl Remap for Term {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Var(v0) => Self::Var(v0.remap(m)),
            Self::Const(v0) => Self::Const(v0.remap(m)),
            Self::Wild => Self::Wild,
        }
    }
}
impl Remap for GenSource {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Value(v0) => Self::Value(v0.remap(m)),
            Self::Lattice(v0) => Self::Lattice(v0.remap(m)),
            Self::TableFn { f, inputs } => Self::TableFn {
                f: f.remap(m),
                inputs: inputs.remap(m),
            },
            Self::Range {
                lo,
                hi,
                kind,
                ring_bits,
            } => Self::Range {
                lo: lo.remap(m),
                hi: hi.remap(m),
                kind: kind.remap(m),
                ring_bits: ring_bits.remap(m),
            },
        }
    }
}
impl Remap for Pattern {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Var(v0) => Self::Var(v0.remap(m)),
            Self::Wild => Self::Wild,
            Self::Const(v0) => Self::Const(v0.remap(m)),
            Self::Tuple(v0) => Self::Tuple(v0.remap(m)),
            Self::Variant { ty, number, fields } => Self::Variant {
                ty: ty.remap(m),
                number: number.remap(m),
                fields: fields.remap(m),
            },
            Self::Struct { ty, fields } => Self::Struct {
                ty: ty.remap(m),
                fields: fields.remap(m),
            },
        }
    }
}
impl Remap for Expr {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Term(v0) => Self::Term(v0.remap(m)),
            Self::Param(v0) => Self::Param(v0.remap(m)),
            Self::Scalar(v0) => Self::Scalar(v0.remap(m)),
            Self::Unary { op, arg } => Self::Unary {
                op: op.remap(m),
                arg: arg.remap(m),
            },
            Self::Binary { op, lhs, rhs } => Self::Binary {
                op: op.remap(m),
                lhs: lhs.remap(m),
                rhs: rhs.remap(m),
            },
            Self::Call { f, args } => Self::Call {
                f: f.remap(m),
                args: args.remap(m),
            },
            Self::Construct { ty, variant, fields } => Self::Construct {
                ty: ty.remap(m),
                variant: variant.remap(m),
                fields: fields.remap(m),
            },
            Self::Field { base, index } => Self::Field {
                base: base.remap(m),
                index: index.remap(m),
            },
            Self::If { cond, then, els } => Self::If {
                cond: cond.remap(m),
                then: then.remap(m),
                els: els.remap(m),
            },
            Self::Match { scrut, arms } => Self::Match {
                scrut: scrut.remap(m),
                arms: arms.remap(m),
            },
            Self::Collection { kind, elems } => Self::Collection {
                kind: kind.remap(m),
                elems: elems.remap(m),
            },
            Self::Lattice { op, args } => Self::Lattice {
                op: op.remap(m),
                args: args.remap(m),
            },
            Self::Let { pat, value, body } => Self::Let {
                pat: pat.remap(m),
                value: value.remap(m),
                body: body.remap(m),
            },
            Self::Closure { params, body } => Self::Closure {
                params: params.remap(m),
                body: body.remap(m),
            },
        }
    }
}
impl Remap for FnRef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Fn(v0) => Self::Fn(v0.remap(m)),
            Self::Builtin(v0) => Self::Builtin(v0.remap(m)),
        }
    }
}
impl Remap for BuiltinFn {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Prio { site } => Self::Prio { site: site.remap(m) },
            Self::RandPrio { site } => Self::RandPrio { site: site.remap(m) },
            Self::Rand => Self::Rand,
            Self::RandFloat => Self::RandFloat,
            Self::RandRange => Self::RandRange,
            Self::Route { role } => Self::Route { role: role.remap(m) },
            Self::Majority { domain } => Self::Majority {
                domain: domain.remap(m),
            },
            Self::ClusterVersionAtLeast(v0) => Self::ClusterVersionAtLeast(v0.remap(m)),
            Self::ZWeight { rel } => Self::ZWeight { rel: rel.remap(m) },
            Self::ZDelta { rel } => Self::ZDelta { rel: rel.remap(m) },
            Self::Unwrap { rel } => Self::Unwrap { rel: rel.remap(m) },
            Self::Entries => Self::Entries,
            Self::PrincipalOf => Self::PrincipalOf,
            Self::RoleOf => Self::RoleOf,
            Self::Size { role } => Self::Size { role: role.remap(m) },
            Self::Len => Self::Len,
            Self::IntCast(t) => Self::IntCast(*t),
            Self::Lib(f) => Self::Lib(*f),
            Self::Concat => Self::Concat,
            Self::Contains => Self::Contains,
            Self::Keys => Self::Keys,
            Self::Values => Self::Values,
            Self::ToString => Self::ToString,
            Self::Hash64 => Self::Hash64,
            Self::Fingerprint => Self::Fingerprint,
            Self::Error => Self::Error,
        }
    }
}
impl Remap for Construct {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            kind: self.kind.remap(m),
            rules: self.rules.remap(m),
            rels: self.rels.remap(m),
            surface: self.surface.remap(m),
        }
    }
}
impl Remap for SurfaceRef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            module: self.module.remap(m),
            label: self.label.remap(m),
            stmt: self.stmt.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for ConstructKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::HandlerHeader { when } => Self::HandlerHeader { when: when.remap(m) },
            Self::Block { rel } => Self::Block { rel: rel.remap(m) },
            Self::ViewAlternatives { view } => Self::ViewAlternatives { view: view.remap(m) },
            Self::Projection { rel, proj } => Self::Projection {
                rel: rel.remap(m),
                proj: proj.remap(m),
            },
            Self::NotExists { helper } => Self::NotExists {
                helper: helper.remap(m),
            },
            Self::Outer => Self::Outer,
            Self::Any => Self::Any,
            Self::Forall { fa, miss, closed } => Self::Forall {
                fa: fa.remap(m),
                miss: miss.remap(m),
                closed: *closed,
            },
            Self::DeltaRead { rel, prev } => Self::DeltaRead {
                rel: rel.remap(m),
                prev: prev.remap(m),
            },
            Self::Interpose => Self::Interpose,
            Self::Localize => Self::Localize,
            Self::DedRelation { rel } => Self::DedRelation { rel: rel.remap(m) },
            Self::Members { role } => Self::Members { role: role.remap(m) },
            Self::Invariant { id } => Self::Invariant { id: id.remap(m) },
            Self::SpecOracle => Self::SpecOracle,
            Self::Service { id } => Self::Service { id: id.remap(m) },
            Self::Persist { rel, del } => Self::Persist {
                rel: rel.remap(m),
                del: del.remap(m),
            },
            Self::Identity { rel } => Self::Identity { rel: rel.remap(m) },
            Self::Upsert { rel, staging, del } => Self::Upsert {
                rel: rel.remap(m),
                staging: staging.remap(m),
                del: del.remap(m),
            },
            Self::Resolve(v0) => Self::Resolve(v0.remap(m)),
            Self::Choose(v0) => Self::Choose(v0.remap(m)),
            Self::MultiChoose(v0) => Self::MultiChoose(v0.remap(m)),
            Self::Index(v0) => Self::Index(v0.remap(m)),
            Self::Seq(v0) => Self::Seq(v0.remap(m)),
            Self::FoldOrdered(v0) => Self::FoldOrdered(v0.remap(m)),
            Self::ArgExt(v0) => Self::ArgExt(v0.remap(m)),
            Self::AggDefault(v0) => Self::AggDefault(v0.remap(m)),
            Self::SoftTable(v0) => Self::SoftTable(v0.remap(m)),
            Self::Sealed { rel, sealed } => Self::Sealed {
                rel: rel.remap(m),
                sealed: sealed.remap(m),
            },
            Self::Range { rel, col } => Self::Range {
                rel: rel.remap(m),
                col: col.remap(m),
            },
            Self::LogicalTimer { rel, every } => Self::LogicalTimer {
                rel: rel.remap(m),
                every: every.remap(m),
            },
            Self::Seal(v0) => Self::Seal(v0.remap(m)),
            Self::Snapshot(v0) => Self::Snapshot(v0.remap(m)),
            Self::Wrapped(v0) => Self::Wrapped(v0.remap(m)),
            Self::LatticeFold { cell } => Self::LatticeFold { cell: cell.remap(m) },
            Self::Finality(v0) => Self::Finality(v0.remap(m)),
            Self::Quorum(v0) => Self::Quorum(v0.remap(m)),
        }
    }
}
impl Remap for ChooseSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            site: self.site.remap(m),
            candidates: self.candidates.remap(m),
            group: self.group.remap(m),
            choice: self.choice.remap(m),
            policy: self.policy.remap(m),
            sticky: self.sticky.remap(m),
            overrides: self.overrides.remap(m),
            output: self.output.remap(m),
        }
    }
}
impl Remap for FinalitySpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            output: self.output.remap(m),
            lower: self.lower.remap(m),
            upper: self.upper.remap(m),
            status: self.status.remap(m),
        }
    }
}
impl Remap for Site {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            stable: self.stable.remap(m),
            key: self.key.remap(m),
            kind: self.kind.remap(m),
            construct: self.construct.remap(m),
        }
    }
}
impl Remap for DotStoreKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Set(v0) => Self::Set(v0.remap(m)),
            Self::Map(v0, v1) => Self::Map(v0.remap(m), v1.remap(m)),
        }
    }
}
impl Remap for TombKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::RemoveWins => Self::RemoveWins,
            Self::AddWins => Self::AddWins,
        }
    }
}
impl Remap for ExternLatticeRef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            path: self.path.remap(m),
            codec: self.codec.remap(m),
        }
    }
}
impl Remap for GroupDef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            ctor: self.ctor.remap(m),
            ring: self.ring.remap(m),
        }
    }
}
impl Remap for GroupCtor {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Z => Self::Z,
            Self::Zn(v0) => Self::Zn(v0.remap(m)),
            Self::ZSet(v0) => Self::ZSet(v0.remap(m)),
            Self::Tuple(v0) => Self::Tuple(v0.remap(m)),
            Self::Map(v0, v1) => Self::Map(v0.remap(m), v1.remap(m)),
            Self::User {
                name,
                zero,
                add,
                neg,
                mul,
            } => Self::User {
                name: name.remap(m),
                zero: zero.remap(m),
                add: add.remap(m),
                neg: neg.remap(m),
                mul: mul.remap(m),
            },
        }
    }
}
impl Remap for ParamDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            ty: self.ty.remap(m),
            default: self.default.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for InvariantDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            id: self.id.remap(m),
            name: self.name.remap(m),
            action: self.action.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for ViolationAction {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Record => Self::Record,
            Self::Warn => Self::Warn,
            Self::Abort => Self::Abort,
        }
    }
}
impl Remap for MigrationDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            from: self.from.remap(m),
            rules: self.rules.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for TranslationDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            channel: self.channel.remap(m),
            version: self.version.remap(m),
            direction: self.direction.remap(m),
            rules: self.rules.remap(m),
            span: self.span.remap(m),
        }
    }
}
impl Remap for TranslationDirection {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::To => Self::To,
            Self::From => Self::From,
        }
    }
}
impl Remap for InterfaceDir {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Input => Self::Input,
            Self::Output => Self::Output,
        }
    }
}
impl Remap for Placement {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Shared => Self::Shared,
            Self::Role(v0) => Self::Role(v0.remap(m)),
        }
    }
}
impl Remap for Origin {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::User(v0) => Self::User(v0.remap(m)),
            Self::Generated { construct } => Self::Generated {
                construct: construct.remap(m),
            },
        }
    }
}
impl Remap for WeightKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::ZSet => Self::ZSet,
            Self::Bag => Self::Bag,
        }
    }
}
impl Remap for TimerClock {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Physical => Self::Physical,
            Self::Logical => Self::Logical,
        }
    }
}
impl Remap for ChannelForm {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Direction { src, dst } => Self::Direction {
                src: src.remap(m),
                dst: dst.remap(m),
            },
            Self::Column => Self::Column,
            Self::NodeToNode => Self::NodeToNode,
        }
    }
}
impl Remap for FaultModel {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Lossy => Self::Lossy,
            Self::LossyDelayed => Self::LossyDelayed,
            Self::Reliable => Self::Reliable,
            Self::ReliableOrdered => Self::ReliableOrdered,
        }
    }
}
impl Remap for PartitionSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            key: self.key.remap(m),
            over: self.over.remap(m),
        }
    }
}
impl Remap for SealDecl {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            key: self.key.remap(m),
            producers: self.producers.remap(m),
        }
    }
}
impl Remap for WrapperKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Dots => Self::Dots,
            Self::Cumulative => Self::Cumulative,
            Self::Tree => Self::Tree,
        }
    }
}
impl Remap for AclExplicit {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            roles: self.roles.remap(m),
            external: self.external,
            principal_in: self.principal_in.remap(m),
        }
    }
}
impl Remap for AclSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Inferred => Self::Inferred,
            Self::Explicit(v0) => Self::Explicit(v0.remap(m)),
        }
    }
}
impl Remap for MaterializeHint {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Always => Self::Always,
            Self::Never => Self::Never,
            Self::Auto => Self::Auto,
        }
    }
}
impl Remap for FiniteSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            cols: self.cols.remap(m),
            bound: self.bound.remap(m),
        }
    }
}
impl Remap for OrderSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            keys: self.keys.remap(m),
        }
    }
}
impl Remap for OrderKey {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            term: self.term.remap(m),
            descending: self.descending.remap(m),
        }
    }
}
impl Remap for RangeKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::HalfOpen => Self::HalfOpen,
            Self::Closed => Self::Closed,
            Self::OpenOpen => Self::OpenOpen,
            Self::OpenClosed => Self::OpenClosed,
        }
    }
}
impl Remap for BuiltinScalar {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Now => Self::Now,
            Self::Tick => Self::Tick,
            Self::SelfNode => Self::SelfNode,
            Self::Incarnation => Self::Incarnation,
            Self::Host => Self::Host,
        }
    }
}
impl Remap for UnOp {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Not => Self::Not,
            Self::Neg => Self::Neg,
            Self::BitNot => Self::BitNot,
        }
    }
}
impl Remap for BinOp {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Add => Self::Add,
            Self::Sub => Self::Sub,
            Self::Mul => Self::Mul,
            Self::Div => Self::Div,
            Self::Rem => Self::Rem,
            Self::Eq => Self::Eq,
            Self::Ne => Self::Ne,
            Self::Lt => Self::Lt,
            Self::Le => Self::Le,
            Self::Gt => Self::Gt,
            Self::Ge => Self::Ge,
            Self::CanonLt => Self::CanonLt,
            Self::CanonLe => Self::CanonLe,
            Self::And => Self::And,
            Self::Or => Self::Or,
            Self::BitAnd => Self::BitAnd,
            Self::BitOr => Self::BitOr,
            Self::BitXor => Self::BitXor,
            Self::Shl => Self::Shl,
            Self::Shr => Self::Shr,
        }
    }
}
impl Remap for CollKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Vec => Self::Vec,
            Self::Set => Self::Set,
            Self::Map => Self::Map,
        }
    }
}
impl Remap for LatOpRef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            lattice: self.lattice.remap(m),
            op: self.op.remap(m),
        }
    }
}
impl Remap for MajorityDomain {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Role(v0) => Self::Role(v0.remap(m)),
            Self::Relation(v0) => Self::Relation(v0.remap(m)),
        }
    }
}
impl Remap for ChoosePolicy {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Priority => Self::Priority,
            Self::Least { cost } => Self::Least { cost: cost.remap(m) },
            Self::Most { cost } => Self::Most { cost: cost.remap(m) },
            Self::Rand => Self::Rand,
        }
    }
}
impl Remap for StickySpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            held: self.held.remap(m),
            release: self.release.remap(m),
            durable: self.durable.remap(m),
        }
    }
}
impl Remap for SiteKind {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Choose => Self::Choose,
            Self::ChooseLeast => Self::ChooseLeast,
            Self::ChooseMost => Self::ChooseMost,
            Self::ChooseRand => Self::ChooseRand,
            Self::Sticky => Self::Sticky,
            Self::Seq => Self::Seq,
            Self::Resolve => Self::Resolve,
            Self::Route => Self::Route,
        }
    }
}
impl Remap for ResolveSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            candidates: self.candidates.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
            policy: self.policy.remap(m),
            site: self.site.remap(m),
        }
    }
}
impl Remap for ResolvePolicy {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Least(v0) => Self::Least(v0.remap(m)),
            Self::Most(v0) => Self::Most(v0.remap(m)),
            Self::Choose => Self::Choose,
            Self::Merge(v0) => Self::Merge(v0.remap(m)),
            Self::Reject => Self::Reject,
        }
    }
}
impl Remap for MultiChooseSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            site: self.site.remap(m),
            candidates: self.candidates.remap(m),
            dependencies: self.dependencies.remap(m),
            output: self.output.remap(m),
        }
    }
}
impl Remap for FunctionalDependency {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            key: self.key.remap(m),
            value: self.value.remap(m),
        }
    }
}
impl Remap for IndexSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            input: self.input.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
            order: self.order.remap(m),
            mode: self.mode.remap(m),
        }
    }
}
impl Remap for IndexMode {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        match self {
            Self::Index => Self::Index,
            Self::Top(v0) => Self::Top(v0.remap(m)),
            Self::Limit(v0) => Self::Limit(v0.remap(m)),
            Self::Percentile { num, den } => Self::Percentile {
                num: num.remap(m),
                den: den.remap(m),
            },
        }
    }
}
impl Remap for SeqSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            site: self.site.remap(m),
            input: self.input.remap(m),
            output: self.output.remap(m),
            assigned: self.assigned.remap(m),
            counter: self.counter.remap(m),
            durable: self.durable.remap(m),
        }
    }
}
impl Remap for FoldSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            input: self.input.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
            order: self.order.remap(m),
            init: self.init.remap(m),
            step: self.step.remap(m),
            finish: self.finish.remap(m),
        }
    }
}
impl Remap for ArgExtSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            input: self.input.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
            cost: self.cost.remap(m),
            maximum: self.maximum.remap(m),
        }
    }
}
impl Remap for AggDefaultSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            input: self.input.remap(m),
            drivers: self.drivers.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
            default: self.default.remap(m),
        }
    }
}
impl Remap for SoftSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            storage: self.storage.remap(m),
            ttl: self.ttl.remap(m),
            max: self.max.remap(m),
        }
    }
}
impl Remap for SealSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            rel: self.rel.remap(m),
            log: self.log.remap(m),
            votes: self.votes.remap(m),
            sealed: self.sealed.remap(m),
            key: self.key.remap(m),
            producers: self.producers.remap(m),
        }
    }
}
impl Remap for SnapshotSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            input: self.input.remap(m),
            output: self.output.remap(m),
            progress: self.progress.remap(m),
            threshold: self.threshold.remap(m),
        }
    }
}
impl Remap for WrapSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            channel: self.channel.remap(m),
            output: self.output.remap(m),
            context: self.context.remap(m),
            outbuf: self.outbuf.remap(m),
            kind: self.kind.remap(m),
        }
    }
}
impl Remap for QuorumSpec {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        Self {
            domain: self.domain.remap(m),
            votes: self.votes.remap(m),
            output: self.output.remap(m),
            group: self.group.remap(m),
        }
    }
}
impl Remap for blossom_value::TypeDef {
    fn remap(&self, m: &mut impl Mapper) -> Self {
        use blossom_value::TypeDef;
        match self {
            TypeDef::Node(role) => TypeDef::Node(role.remap(m)),
            TypeDef::Tuple(types) => TypeDef::Tuple(types.remap(m)),
            TypeDef::Struct(s) => {
                let mut s = s.clone();
                for f in &mut s.fields {
                    f.ty = f.ty.remap(m);
                }
                TypeDef::Struct(s)
            }
            TypeDef::Enum(e) => {
                let mut e = e.clone();
                for v in &mut e.variants {
                    for f in &mut v.payload {
                        f.ty = f.ty.remap(m);
                    }
                }
                TypeDef::Enum(e)
            }
            TypeDef::Vec(t) => TypeDef::Vec(t.remap(m)),
            TypeDef::Set(t) => TypeDef::Set(t.remap(m)),
            TypeDef::Map(k, v) => TypeDef::Map(k.remap(m), v.remap(m)),
            TypeDef::Option(t) => TypeDef::Option(t.remap(m)),
            TypeDef::Lattice(id) => TypeDef::Lattice(id.remap(m)),
            TypeDef::Group(id) => TypeDef::Group(id.remap(m)),
            _ => self.clone(),
        }
    }
}
