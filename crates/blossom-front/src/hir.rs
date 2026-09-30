//! The HIR (ARCHITECTURE §13.8): a resolved, instance-flattened program, independent of spelling.
//!
//! [`crate::resolve`] builds it from the surface AST of the program root and every module it imports: names are
//! resolved, instances are flattened (every relation of an instance `a` is `a.r`), constants and value parameters are
//! folded to values, roles are placed, and every body literal is classified (LANGUAGE §9.1). Types are the
//! [`TypeId`]s of the HIR's own [`TypeTable`], which lowering hands to the IR builder unchanged, so a HIR role id is
//! the IR role id and a HIR type id is the IR type id.
//!
//! What is not known yet after resolution: the column types of views and the types of rule variables. Those come from
//! [`crate::typeck`], which fills [`Hir::var_types`] and the view columns.

use blossom_base::TypeId;
use blossom_base::{InternalError, QualName, Span, Symbol, internal_error};
use blossom_value::{TypeTable, Value, types::IntTy};

use crate::ast::{BinOp, PrefixOp, Trigger, Verb};

macro_rules! hir_id {
    ($($(#[$m:meta])* $name:ident;)*) => {$(
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);
        impl $name {
            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    )*};
}

hir_id! {
    /// A role; equal to the IR's `RoleId` of the same index.
    HRoleId;
    /// A relation (declared, a view, a timer, or a built-in).
    HRelId;
    /// A variable of one rule scope.
    HVarId;
    /// A rule scope: a handler, a view alternative, a fact or a spec view.
    ScopeId;
    /// A pure function (LANGUAGE §16.1).
    HFnId;
}

/// A resolved program.
#[derive(Clone, Debug)]
pub struct Hir {
    pub name: Symbol,
    pub version: u32,
    pub edition: u16,
    pub types: TypeTable,
    /// Lattice types: `TypeDef::Lattice(id)` names `lattices[id]`, declared in this order in the IR.
    pub lattices: Vec<blossom_ir::core::LatticeCtor>,
    pub roles: Vec<HRole>,
    pub rels: Vec<HRel>,
    pub handlers: Vec<HHandler>,
    pub views: Vec<HView>,
    pub facts: Vec<HFact>,
    pub invariants: Vec<HInvariant>,
    /// Pure functions (LANGUAGE §16.1).
    pub fns: Vec<HFn>,
    /// Variable tables, one per rule scope (and one per function).
    pub scopes: Vec<HScope>,
    /// Filled by type checking: the type of every variable of every scope.
    pub var_types: Vec<Vec<TypeId>>,
}

impl Hir {
    /// The relation `id`. Ids are minted by the resolver as positions in [`Hir::rels`]; a miss is a frontend bug.
    pub fn rel(&self, id: HRelId) -> Result<&HRel, InternalError> {
        self.rels
            .get(id.index())
            .ok_or_else(|| internal_error!("HIR relation {id:?} does not exist"))
    }

    /// The role `id`.
    pub fn role(&self, id: HRoleId) -> Result<&HRole, InternalError> {
        self.roles
            .get(id.index())
            .ok_or_else(|| internal_error!("HIR role {id:?} does not exist"))
    }

    /// The rule scope `id`.
    pub fn scope(&self, id: ScopeId) -> Result<&HScope, InternalError> {
        self.scopes
            .get(id.index())
            .ok_or_else(|| internal_error!("HIR scope {id:?} does not exist"))
    }

    /// The lattice of a lattice type: its id and constructor.
    pub fn lattice_of(&self, ty: TypeId) -> Option<(blossom_base::LatticeTypeId, &blossom_ir::core::LatticeCtor)> {
        match self.types.get(ty) {
            Some(blossom_value::TypeDef::Lattice(id)) => self.lattices.get(id.index()).map(|c| (*id, c)),
            _ => None,
        }
    }

    /// `ty` with every `Node<R>` inside it widened to `Node`.
    pub fn erase_roles(&mut self, ty: TypeId) -> Result<TypeId, InternalError> {
        use blossom_value::TypeDef as D;
        let def = match self.types.get(ty).cloned() {
            Some(D::Node(Some(_))) => D::Node(None),
            Some(D::Tuple(ts)) => {
                let mut out = Vec::new();
                for t in ts {
                    out.push(self.erase_roles(t)?);
                }
                D::Tuple(out)
            }
            Some(D::Option(t)) => D::Option(self.erase_roles(t)?),
            Some(D::Vec(t)) => D::Vec(self.erase_roles(t)?),
            Some(D::Set(t)) => D::Set(self.erase_roles(t)?),
            Some(D::Map(k, v)) => {
                let k = self.erase_roles(k)?;
                D::Map(k, self.erase_roles(v)?)
            }
            _ => return Ok(ty),
        };
        self.types
            .insert(def)
            .map_err(|e| internal_error!("interning a type: {e}"))
    }

    /// Interns the lattice type with constructor `ctor`. A lattice's elements carry no role (`LSet<Node<R>>` is
    /// `LSet<Node>`): values of `Node<R>` are values of `Node`, and one lattice type keeps one operation catalogue.
    pub fn intern_lattice(&mut self, ctor: blossom_ir::core::LatticeCtor) -> Result<TypeId, InternalError> {
        use blossom_ir::core::LatticeCtor as C;
        let ctor = match ctor {
            C::Max(t) => C::Max(self.erase_roles(t)?),
            C::Min(t) => C::Min(self.erase_roles(t)?),
            C::Set(t) => C::Set(self.erase_roles(t)?),
            C::PSet(t) => C::PSet(self.erase_roles(t)?),
            C::Point(t) => C::Point(self.erase_roles(t)?),
            C::Map(k, inner) => C::Map(self.erase_roles(k)?, inner),
            other => other,
        };
        let id = match self.lattices.iter().position(|c| *c == ctor) {
            Some(i) => i,
            None => {
                self.lattices.push(ctor);
                self.lattices.len() - 1
            }
        };
        let id =
            blossom_base::LatticeTypeId::from_raw(u32::try_from(id).map_err(|_| internal_error!("too many lattices"))?);
        self.types
            .insert(blossom_value::TypeDef::Lattice(id))
            .map_err(|e| internal_error!("interning a lattice type: {e}"))
    }

    /// Variable `v` of scope `s`.
    pub fn var(&self, s: ScopeId, v: HVarId) -> Result<&HVar, InternalError> {
        self.scope(s)?
            .vars
            .get(v.index())
            .ok_or_else(|| internal_error!("HIR variable {v:?} is not in scope {s:?}"))
    }
}

impl HRel {
    /// Stands in for a relation a lookup failed to find, after the failure was recorded as an internal error (the
    /// phase then fails, so the placeholder never reaches its output).
    pub(crate) fn placeholder() -> HRel {
        HRel {
            name: QualName::single(Symbol::intern("<missing>")),
            kind: HRelKind::Scratch,
            cols: Vec::new(),
            key: None,
            durable: false,
            cell: false,
            resolve: None,
            role: None,
            span: Span::point(blossom_base::FileId::from_raw(0), 0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HRole {
    pub name: QualName,
    pub kind: RoleKind,
    pub span: Span,
}

impl HRole {
    /// Stands in for a role a lookup failed to find (see [`HRel::placeholder`]).
    pub(crate) fn placeholder() -> HRole {
        HRole {
            name: QualName::single(Symbol::intern("<missing>")),
            kind: RoleKind::Process,
            span: Span::point(blossom_base::FileId::from_raw(0), 0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleKind {
    /// Exactly one node.
    Process,
    /// One or more nodes running the same projection.
    Cluster,
    /// Clients: sessions, not nodes.
    External,
}

/// A relation.
#[derive(Clone, Debug)]
pub struct HRel {
    pub name: QualName,
    pub kind: HRelKind,
    pub cols: Vec<HCol>,
    /// Key columns; `None` means every column (a set relation).
    pub key: Option<Vec<usize>>,
    pub durable: bool,
    /// A `cell`: read only by lookup, its name as an expression being `c[]` (LANGUAGE §7.13).
    pub cell: bool,
    /// A relation-level resolution policy (LANGUAGE §10.7).
    pub resolve: Option<HResolve>,
    /// Where the relation lives; `None` in a role-free program and for shared declarations.
    pub role: Option<HRoleId>,
    pub span: Span,
}

/// `key(…) resolve P`: which candidate for a key survives to the next tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HResolve {
    pub policy: HPolicy,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HPolicy {
    /// The least seeded priority.
    Choose,
    /// The least (`most: false`) or greatest value of a column, then the least seeded priority.
    Extreme { col: usize, most: bool },
}

#[derive(Clone, Debug)]
pub struct HCol {
    pub name: Symbol,
    /// `None` for a view column until type checking.
    pub ty: Option<TypeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HRelKind {
    /// Persistent: frame rule and `$del`.
    Table,
    Scratch,
    /// Closed, tick-local, defined by its alternatives.
    View,
    /// Rows from facts; holds at every tick.
    Static,
    /// `input`: `root` for a program root's input (fed by the host), otherwise an instance's input (written by the
    /// importer).
    Input {
        root: bool,
    },
    /// `output`: `root` for a program root's output.
    Output {
        root: bool,
    },
    Channel(ChannelInfo),
    /// A physical timer `name(count: u64, at: Instant)`.
    Timer {
        every: u128,
    },
    /// `boot()`.
    Boot,
    /// `recovered()`: holds in the boot tick iff durable state was reloaded (LANGUAGE §8.4).
    Recovered,
    /// `localtick()`: a scratch written only with `next` to request another tick.
    LocalTick,
    /// `halt(kill: bool)`: `emit halt(false);` stops the node at the end of the tick (LANGUAGE §7.15).
    Halt,
    /// `R$members(n: Node<R>)`: a role's member set, a static relation filled from the deployment.
    Members(HRoleId),
    /// The node directory `node_dir(node, addr, principal, role)`, from the deployment (LANGUAGE §7.15).
    NodeDir,
}

impl HRelKind {
    /// Whether rows written in a tick persist (tables).
    pub fn is_table(&self) -> bool {
        matches!(self, HRelKind::Table)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    pub loopback: bool,
    /// `: Src -> Dst` in a multi-role program.
    pub direction: Option<(HRoleId, HRoleId)>,
    /// The column-form destination column (`@dst`), among the declared columns.
    pub dest_col: Option<usize>,
    /// `#[accept(…)]`: the explicit ACL (LANGUAGE §18.3); `None` keeps the inferred one.
    pub acl: Option<HAcl>,
}

/// An explicit ACL: the sources `#[accept(…)]` narrows the inferred ACL to (LANGUAGE §18.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HAcl {
    /// The roles whose nodes are admitted.
    pub roles: Vec<HRoleId>,
    /// Whether client sessions of the channel's external source role are admitted.
    pub external: bool,
    /// `principal in REL`: only senders whose principal is a row of this unary relation of the receiving node.
    pub principal_in: Option<HRelId>,
    pub span: Span,
}

/// A rule scope's variables.
#[derive(Clone, Debug)]
pub struct HScope {
    pub vars: Vec<HVar>,
    /// The module path of the construct, for rule ids (`M` in LANGUAGE §4.3).
    pub module: QualName,
}

#[derive(Clone, Debug)]
pub struct HVar {
    pub name: Symbol,
    pub span: Span,
    /// Generated by the resolver (an omitted column, a `_x` pattern); never a surface name that could clash.
    pub generated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerKind {
    Plain,
    Bootstrap,
    BootstrapFresh,
}

/// A handler (or bootstrap): a header body and statements.
#[derive(Clone, Debug)]
pub struct HHandler {
    pub scope: ScopeId,
    pub label: Option<Symbol>,
    pub trigger: Trigger,
    pub kind: HandlerKind,
    pub header: HBody,
    pub stmts: Vec<HStmt>,
    pub role: Option<HRoleId>,
    /// The handler's normalized header text, hashed for an unlabelled handler's id.
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HStmt {
    Verb(HVerbStmt),
    /// An `if`/`for`/`else` block: its condition conjoined to the enclosing one.
    Block {
        kind: BlockKind,
        cond: HBody,
        stmts: Vec<HStmt>,
        /// Normalized condition text, hashed for the block's id.
        text: String,
        span: Span,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    If,
    For,
    Else,
}

#[derive(Clone, Debug)]
pub struct HVerbStmt {
    pub verb: Verb,
    pub target: HRelId,
    /// One argument per column, in column order.
    pub args: Vec<HHeadArg>,
    /// `send … to d`: the destination.
    pub to: Option<HExpr>,
    pub allow_self_negation: bool,
    /// Normalized statement text, hashed when two statements share verb and target.
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HHeadArg {
    Expr(HExpr),
    Agg(HAgg),
}

/// A head aggregate.
#[derive(Clone, Debug)]
pub struct HAgg {
    pub func: AggKind,
    /// The aggregated expressions; empty for `count!(*)`.
    pub args: Vec<HExpr>,
    /// `default e` (with a `per` driver or no grouping columns).
    pub default: Option<HExpr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggKind {
    /// `count!(*)` when `args` is empty, `count!(e)` otherwise.
    Count,
    Sum,
    Min,
    Max,
    /// `index!()` (LANGUAGE §10.5): the dense 0-based rank of each head tuple in canonical order, per tick. A view
    /// column only.
    Index,
}

/// A view: a closed relation defined by its alternatives.
#[derive(Clone, Debug)]
pub struct HView {
    pub rel: HRelId,
    /// One scope and body per alternative.
    pub alternatives: Vec<(ScopeId, HBody)>,
    /// Each alternative's normalized text, hashed for its rule id (LANGUAGE §4.3).
    pub texts: Vec<String>,
    pub shape: HViewShape,
    /// `monotone view`: the view's rules must contain no point of order (ANA-020, checked by the analyses).
    pub monotone: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HViewShape {
    /// No aggregate column: per column, the variable with the column's name in each alternative.
    Plain { cols: Vec<Vec<HVarId>> },
    /// Some column is an aggregate (LANGUAGE §8.3, §10.1–10.2): the alternatives are unioned into `v$u` over the
    /// variables that occur in every alternative (`union`'s variables, in order), and aggregated once.
    Aggregate {
        union: ScopeId,
        /// Per alternative: its variable for each variable of `union`.
        shared: Vec<Vec<HVarId>>,
        cols: Vec<HViewAggCol>,
        /// `per r(…)` (single alternative only), in the first alternative's scope.
        driver: Option<HAtom>,
    },
}

#[derive(Clone, Debug)]
pub enum HViewAggCol {
    /// A grouping column: a variable of the union scope.
    Group(HVarId),
    /// An aggregate over the union scope's variables.
    Agg(HAgg),
}

/// `invariant name ["message"]: never BODY;` (LANGUAGE §17.1): every valuation of the body is a violation.
#[derive(Clone, Debug)]
pub struct HInvariant {
    pub name: Symbol,
    pub message: Option<String>,
    pub scope: ScopeId,
    pub body: HBody,
    pub role: Option<HRoleId>,
    pub span: Span,
}

/// `fact r(…);`
#[derive(Clone, Debug)]
pub struct HFact {
    pub rel: HRelId,
    pub row: Vec<HExpr>,
    pub scope: ScopeId,
    pub span: Span,
}

/// A body: an unordered conjunction.
#[derive(Clone, Debug, Default)]
pub struct HBody {
    pub lits: Vec<HLit>,
    pub span: Option<Span>,
}

#[derive(Clone, Debug)]
pub enum HLit {
    Atom(HAtom),
    Not(HAtom),
    /// `not { B }`: variables first bound inside are local to it.
    NotBody(HBody, Span),
    Let {
        pat: HPat,
        expr: HExpr,
        span: Span,
    },
    Guard(HExpr),
    /// `pat in lo..hi` and the other range forms with an unbound variable: a generator.
    RangeGen {
        pat: HPat,
        lo: HExpr,
        hi: HExpr,
        kind: RangeKind,
        span: Span,
    },
    /// `choose!(Ȳ per X̄ [least c | most c] [sticky])` (LANGUAGE §10.4): the functional dependency X̄ → Ȳ over the
    /// body's valuations in this tick.
    Choose(Box<HChoose>),
    /// `pat in e` with an unbound variable over a value collection or a set-like lattice (LANGUAGE §9.4); type
    /// checking tells which.
    Gen {
        pat: HPat,
        src: HExpr,
        span: Span,
    },
    /// `p in R` with `p` unbound: the role's members.
    RoleGen {
        pat: HPat,
        role: HRoleId,
        /// The role's `R$members` relation.
        members: HRelId,
        span: Span,
    },
    Outer(HAtom),
    Delta {
        inserted: bool,
        atom: HAtom,
    },
    Any(Vec<HBody>, Span),
    Forall {
        domain: Box<HLit>,
        body: HBody,
        span: Span,
    },
    /// `per r(…)`: an aggregate driver (views only).
    Per(HAtom),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKind {
    HalfOpen,
    Closed,
    OpenOpen,
    OpenClosed,
}

/// A positional atom.
#[derive(Clone, Debug)]
pub struct HChoose {
    /// Ȳ: the chosen values.
    pub chosen: Vec<HExpr>,
    /// X̄: the group (empty: one group per tick).
    pub per: Vec<HExpr>,
    /// `least c` (`false`) or `most c` (`true`): the cost ordered first, then the seeded priority.
    pub cost: Option<(HExpr, bool)>,
    /// Keep last tick's choice while it is still a candidate (LANG-115).
    pub sticky: bool,
    /// An order filter, `argmin!(c per X̄)` / `argmax!(c per X̄)` (LANGUAGE §10.3): every valuation whose cost is
    /// least (greatest) in its group survives, every tie included; `chosen` is empty and no seed is involved.
    pub ties: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct HAtom {
    pub rel: HRelId,
    /// One pattern per column.
    pub args: Vec<HPat>,
    /// `from s`: channels and loopbacks only.
    pub from: Option<HPat>,
    pub span: Span,
}

/// An argument pattern of an atom or a `let`.
#[derive(Clone, Debug)]
pub enum HPat {
    /// A variable: binds if unbound, joins if bound.
    Var(HVarId, Span),
    Wild(Span),
    /// An expression over bound variables, or a constant: an equality test.
    Expr(HExpr),
    Tuple(Vec<HPat>, Span),
    /// An enum variant (including `Some`, whose `ty` is the `Option` type) with positional fields.
    Variant {
        ty: TypeRef,
        variant: u32,
        fields: Vec<HPat>,
        span: Span,
    },
}

impl HPat {
    pub fn span(&self) -> Span {
        match self {
            HPat::Var(_, s) | HPat::Wild(s) | HPat::Tuple(_, s) | HPat::Variant { span: s, .. } => *s,
            HPat::Expr(e) => e.span,
        }
    }
}

/// A type known at resolution (a declared enum or struct), or the `Option` constructor whose element type type
/// checking infers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeRef {
    Known(TypeId),
    Option,
}

#[derive(Clone, Debug)]
pub struct HExpr {
    pub kind: HExprKind,
    pub span: Span,
    /// The expression's type, filled by type checking.
    pub ty: Option<TypeId>,
}

impl HExpr {
    pub fn new(kind: HExprKind, span: Span) -> HExpr {
        HExpr { kind, span, ty: None }
    }
}

#[derive(Clone, Debug)]
pub enum HExprKind {
    Var(HVarId),
    /// A constant with a known type.
    Value(Value, TypeId),
    /// An unsuffixed integer literal: its type comes from context (i64 when nothing constrains it).
    IntLit(u128, bool),
    /// A suffixed integer literal.
    TypedInt(u128, IntTy, bool),
    Binary {
        op: BinOp,
        lhs: Box<HExpr>,
        rhs: Box<HExpr>,
    },
    Prefix {
        op: PrefixOp,
        arg: Box<HExpr>,
    },
    Tuple(Vec<HExpr>),
    /// An enum variant value (including `Some(e)` / `None`).
    Variant {
        ty: TypeRef,
        variant: u32,
        fields: Vec<HExpr>,
    },
    Struct {
        ty: TypeId,
        fields: Vec<HExpr>,
    },
    TupleIndex {
        base: Box<HExpr>,
        index: u32,
    },
    /// A struct field: its position is known once type checking has typed the base.
    Field {
        base: Box<HExpr>,
        name: Symbol,
        index: Option<u32>,
    },
    If {
        cond: Box<HExpr>,
        then: Box<HExpr>,
        els: Box<HExpr>,
    },
    Match {
        scrut: Box<HExpr>,
        arms: Vec<(HPat, Option<HExpr>, HExpr)>,
    },
    Cast {
        expr: Box<HExpr>,
        ty: TypeId,
    },
    /// `self`.
    SelfNode,
    /// `now()`.
    Now,
    /// `tick()`.
    Tick,
    /// A built-in method or function.
    Builtin {
        f: Builtin,
        args: Vec<HExpr>,
    },
    /// `r[k̄]` on a lattice-valued relation: the cell's value, ⊥ if absent (LANGUAGE §9.9, §11.3).
    Lookup {
        rel: HRelId,
        key: Vec<HExpr>,
    },
    /// `x in e` with `x` bound: membership in a set-like lattice (a threshold) or a collection (LANGUAGE §9.4).
    In {
        elem: Box<HExpr>,
        coll: Box<HExpr>,
    },
    /// `[a, b]`, `set[a, b]`, `map[k => v]` (a map's entries as key, value, key, value, …).
    Collection {
        kind: CollectionKind,
        elems: Vec<HExpr>,
    },
    /// A method call (or `reveal!(x)`) that type checking resolves by the receiver's type.
    Method {
        recv: Box<HExpr>,
        name: Symbol,
        banged: bool,
        args: Vec<HExpr>,
    },
    /// `LMax::of(x)`, `LSet::of(x)`, `LMap::of(k, v)`, … and `L::bot()` (LANGUAGE §11.5); the lattice's element
    /// types are inferred.
    LatCtor {
        kind: LatCtorKind,
        /// `bot()` rather than `of(…)`.
        bot: bool,
        args: Vec<HExpr>,
    },
    /// A lattice operation, resolved by type checking: the receiver first (LANGUAGE §11.4–11.5).
    LatOp {
        lattice: TypeId,
        op: blossom_lattice::Op,
        args: Vec<HExpr>,
    },
    /// A value lifted into a lattice where a lattice is expected (LANGUAGE §5.6), inserted by type checking.
    Lift {
        expr: Box<HExpr>,
        lattice: TypeId,
    },
    /// A call of a pure function.
    Call {
        f: HFnId,
        args: Vec<HExpr>,
    },
    /// `let pat[: ty] = value; body` (function bodies only).
    Let {
        pat: Box<HPat>,
        ty: Option<TypeId>,
        value: Box<HExpr>,
        body: Box<HExpr>,
    },
    /// `|a, b| body`: an argument of a built-in combinator (function bodies only).
    Closure {
        params: Vec<HVarId>,
        body: Box<HExpr>,
    },
}

/// A pure function: total, non-recursive (LANGUAGE §16.1), or a host function (§16.2). Its variables live in
/// `scope`: the parameters first, then every `let` and closure binding of its body.
#[derive(Clone, Debug)]
pub struct HFn {
    pub name: QualName,
    pub scope: ScopeId,
    pub params: Vec<(HVarId, TypeId)>,
    pub ret: TypeId,
    pub body: HFnBody,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HFnBody {
    Expr(HExpr),
    /// An `extern fn`: the host function's path, checked against the standard catalog.
    Extern(std::sync::Arc<str>),
}

/// The integer type a byte-access name spells (FOREIGN-PROTOCOLS §3): `u8`, `i8`, and the big-endian `u16_be` …
/// `i64_be`, as in `b.u16_be_at(p)`, `b.put_u16_be(p, x)` and `Bytes::from_u16_be(x)`.
pub fn byte_int(name: &str) -> Option<IntTy> {
    Some(match name {
        "u8" => IntTy::U8,
        "i8" => IntTy::I8,
        "u16_be" => IntTy::U16,
        "i16_be" => IntTy::I16,
        "u32_be" => IntTy::U32,
        "i32_be" => IntTy::I32,
        "u64_be" => IntTy::U64,
        "i64_be" => IntTy::I64,
        _ => return None,
    })
}

/// The built-in lattice named by a constructor path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatCtorKind {
    Bool,
    Max,
    Min,
    Set,
    PSet,
    Map,
    Point,
}

impl LatCtorKind {
    pub fn named(name: &str) -> Option<LatCtorKind> {
        Some(match name {
            "LBool" => LatCtorKind::Bool,
            "LMax" => LatCtorKind::Max,
            "LMin" => LatCtorKind::Min,
            "LSet" => LatCtorKind::Set,
            "LPSet" => LatCtorKind::PSet,
            "LMap" => LatCtorKind::Map,
            "LPoint" => LatCtorKind::Point,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    Vec,
    Set,
    Map,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// `s.len()` on `String`, `Bytes`, `Vec`, `Set`, `Map`.
    Len,
    /// `R.size()`: a role's cardinality.
    RoleSize(HRoleId),
    /// `c.contains(x)` on a `Vec` or `Set` (an element) or a `Map` (a key); the receiver first.
    Contains,
    /// `rand_range(lo, hi, k…)` (LANGUAGE §15.1): an unbiased value in `[lo, hi)`, the same for the same key within a
    /// node's tick and incarnation. Arguments: `lo`, `hi`, then the key.
    RandRange,
    /// `majority(s, R)` (LANGUAGE §10.9): `|s ∩ R| > |R| / 2` for a set of nodes `s` and a role `R`.
    Majority(HRoleId),
    /// A function or method of the built-in library (Appendix B); the receiver, if any, first.
    Lib(blossom_ir::core::LibFn),
}
