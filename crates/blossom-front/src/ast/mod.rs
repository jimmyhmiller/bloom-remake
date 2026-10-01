//! The owned surface AST of a `.bls` file (LANGUAGE §3), converted once from the lossless CST by [`convert`].
//!
//! The typed CST views of `blossom-syntax` are nullable and borrow the tree; the phases after parsing want total,
//! owned data with spans. [`convert`] builds it and reports every construct it cannot represent (a malformed node
//! left by error recovery, or a construct this build does not accept yet) as a diagnostic, never by dropping it.
//!
//! Nothing here is resolved: a body literal is still an expression (`r(x)` could be an atom or a call), and a path is
//! still a list of names. [`crate::resolve`] classifies them.

// FEATURE: LANG-002

pub mod attrs;
mod convert;
mod desugar;

pub use convert::convert;

use blossom_base::{Span, Symbol};

/// An identifier with its span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: Symbol,
    pub span: Span,
}

impl Ident {
    pub fn as_str(&self) -> &'static str {
        self.name.as_str()
    }
}

/// One parsed file.
#[derive(Clone, Debug)]
pub struct File {
    /// `#![…]` at the top of the file.
    pub inner_attrs: Vec<Attr>,
    pub header: Option<ProgramHeader>,
    pub items: Vec<Item>,
    pub span: Span,
}

/// `program NAME version N [edition E];`
#[derive(Clone, Debug)]
pub struct ProgramHeader {
    pub name: Ident,
    pub version: u32,
    pub edition: u16,
    pub span: Span,
}

/// An attribute `#[name]`, `#[name(args…)]` or `#[name = e]`: one per comma-separated body, so `#[a, b]` is two.
/// A path name `a::b` is kept whole as the name (no built-in attribute has one).
#[derive(Clone, Debug)]
pub struct Attr {
    pub name: Ident,
    pub args: Vec<Arg>,
    pub value: Option<Expr>,
    pub span: Span,
}

/// An item with its attributes and visibility.
#[derive(Clone, Debug)]
pub struct Item {
    pub attrs: Vec<Attr>,
    pub is_pub: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Use(UseTree),
    Import(Import),
    /// `include M;` (a module path) or `include "file";`.
    Include(IncludeTarget),
    Const {
        name: Ident,
        ty: Type,
        value: Expr,
    },
    Param {
        name: Ident,
        ty: Type,
        default: Option<Expr>,
    },
    TypeAlias {
        name: Ident,
        generics: Vec<GenericParam>,
        ty: Type,
    },
    Struct(StructItem),
    Enum(EnumItem),
    Module(ModuleItem),
    Protocol(ProtocolItem),
    Role {
        name: Ident,
        kind: Option<Ident>,
    },
    At {
        role: Ident,
        items: Vec<Item>,
    },
    Rel(RelDecl),
    Timer(TimerDecl),
    View(ViewDecl),
    Handler(Handler),
    Bootstrap {
        fresh: bool,
        block: Block,
    },
    Fact(Fact),
    Invariant(Invariant),
    Interpose(Interpose),
    Spec(SpecItem),
    /// `fn name(params) -> ret { body }` (LANGUAGE §16.1).
    Fn(FnItem),
    /// `extern fn name(params) -> ret = "path";` (LANGUAGE §16.2).
    ExternFn(ExternFnItem),
    /// `stream name: listen;` or `stream name: connect;` (FOREIGN-PROTOCOLS §1).
    Stream {
        name: Ident,
        kind: Ident,
    },
    /// A construct this build parses but does not accept yet; the converter has already reported it (BLS0908).
    Unsupported {
        what: &'static str,
    },
}

/// A pure function: total, non-recursive, its body a block of `let`s and a final expression (LANGUAGE §16.1).
#[derive(Clone, Debug)]
pub struct FnItem {
    pub name: Ident,
    /// Type parameters (LANGUAGE §16.1): a generic function is instantiated per call.
    pub generics: Vec<GenericParam>,
    pub params: Vec<(Ident, Type)>,
    pub ret: Type,
    pub body: Expr,
    pub span: Span,
}

/// A host function: pure by declaration, implemented in Rust at `path` (LANGUAGE §16.2).
#[derive(Clone, Debug)]
pub struct ExternFnItem {
    pub name: Ident,
    pub params: Vec<(Ident, Type)>,
    pub ret: Type,
    pub path: String,
    pub path_span: Span,
    pub span: Span,
}

/// `use a::b::{C, D};`: every imported path, with its last segment as the local name.
#[derive(Clone, Debug)]
pub struct UseTree {
    pub paths: Vec<Vec<Ident>>,
}

/// `import M<T…>(K = v, …) as a [with (R = S, …)];`
#[derive(Clone, Debug)]
pub struct Import {
    pub module: Vec<Ident>,
    pub type_args: Vec<Type>,
    pub args: Vec<Arg>,
    pub alias: Ident,
    pub roles: Vec<(Ident, Ident)>,
}

#[derive(Clone, Debug)]
pub enum IncludeTarget {
    Module(Vec<Ident>),
    File(String),
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<Type>,
}

/// A written type: `Name<Args>` or a tuple `(A, B)`.
#[derive(Clone, Debug)]
pub enum Type {
    Named {
        path: Vec<Ident>,
        args: Vec<Type>,
        span: Span,
    },
    Tuple {
        elems: Vec<Type>,
        span: Span,
    },
    /// `unsafe T`: a type the compiler accepts only when spelled so (`unsafe DomPair<K, V>`, LANG-136).
    Unsafe {
        inner: Box<Type>,
        span: Span,
    },
    /// `fn(A, B) -> R`: the type of a function parameter, whose argument is a named function (LANGUAGE §16.1).
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
        span: Span,
    },
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Named { span, .. } | Type::Tuple { span, .. } | Type::Unsafe { span, .. } | Type::Fn { span, .. } => {
                *span
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructItem {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// Named fields, or positional field types for a tuple struct.
    pub fields: Vec<FieldDecl>,
    pub tuple: bool,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub attrs: Vec<Attr>,
    pub name: Option<Ident>,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumItem {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub attrs: Vec<Attr>,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub tuple: bool,
    pub span: Span,
}

/// `module`, or `choreography` (a module with roles).
#[derive(Clone, Debug)]
pub struct ModuleItem {
    pub name: Ident,
    pub choreography: bool,
    pub generics: Vec<GenericParam>,
    pub params: Vec<ModParam>,
    pub protocols: Vec<Type>,
    pub items: Vec<Item>,
}

/// A module parameter: a value `NAME: T [= e]` or a relation `name: rel(col: T, …)`.
#[derive(Clone, Debug)]
pub struct ModParam {
    pub name: Ident,
    pub kind: ModParamKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ModParamKind {
    Value { ty: Type, default: Option<Expr> },
    Rel { cols: Vec<(Ident, Type)> },
}

#[derive(Clone, Debug)]
pub struct ProtocolItem {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub items: Vec<Item>,
}

/// Relation kinds (LANGUAGE §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelKind {
    Table,
    Scratch,
    Channel,
    Input,
    Output,
    Static,
    Loopback,
}

/// Relation modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelMods {
    pub durable: bool,
    pub soft: bool,
    pub sealed: bool,
    pub zset: bool,
    pub bag: bool,
    pub final_: bool,
    /// `cell name: L;`: a 0-ary lattice relation with the one column `value` (LANGUAGE §7.13).
    pub cell: bool,
}

#[derive(Clone, Debug)]
pub struct RelDecl {
    pub name: Ident,
    pub kind: RelKind,
    pub mods: RelMods,
    pub cols: Vec<ColDecl>,
    pub like: Option<Vec<Ident>>,
    pub key: Option<(Vec<Ident>, Span)>,
    /// `: Src -> Dst`.
    pub direction: Option<(Ident, Ident)>,
    /// `resolve P` (LANGUAGE §10.7).
    pub resolve: Option<(RelPolicy, Span)>,
    /// Clauses this build does not implement yet, by name and span (reported by the resolver when used).
    pub other_clauses: Vec<(&'static str, Span)>,
    /// `while BODY`: a row persists to the next tick only while the body holds for it (LANGUAGE §7.2).
    pub guard: Option<Body>,
    pub span: Span,
}

/// A relation-level resolution policy (LANGUAGE §10.7).
#[derive(Clone, Debug)]
pub enum RelPolicy {
    Choose {
        sticky: bool,
    },
    ChooseRand {
        sticky: bool,
    },
    Least(Expr),
    Most(Expr),
    Merge,
    /// `prefer(rule, …)`: among one tick's writes to a key, those of the earliest listed handler win.
    Prefer(Vec<Ident>),
}

#[derive(Clone, Debug)]
pub struct ColDecl {
    pub attrs: Vec<Attr>,
    /// `@name`: the destination column of a column-form channel.
    pub dest: bool,
    pub name: Ident,
    pub ty: Type,
    pub default: Option<Expr>,
    pub span: Span,
}

/// `timer name every d;` and its variants: the words and expressions after the name, in order.
#[derive(Clone, Debug)]
pub struct TimerDecl {
    pub name: Ident,
    pub words: Vec<Ident>,
    pub exprs: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ViewDecl {
    pub name: Ident,
    pub monotone: bool,
    pub cols: Vec<ViewCol>,
    pub alternatives: Vec<Body>,
    pub span: Span,
}

/// A view column: `name`, `name: T` or `name = agg!(…)`.
#[derive(Clone, Debug)]
pub struct ViewCol {
    pub name: Ident,
    pub ty: Option<Type>,
    pub agg: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    On,
    While,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub label: Option<Ident>,
    pub monotone: bool,
    pub trigger: Trigger,
    pub header: Body,
    pub block: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Verb(Box<VerbStmt>),
    If {
        attrs: Vec<Attr>,
        cond: Body,
        then: Block,
        els: Option<Box<Else>>,
        span: Span,
    },
    For {
        attrs: Vec<Attr>,
        cond: Body,
        block: Block,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub enum Else {
    Block(Block),
    If(Box<Stmt>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verb {
    Emit,
    Next,
    Send,
    Delete,
    Upsert,
    Seal,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Emit => "emit",
            Verb::Next => "next",
            Verb::Send => "send",
            Verb::Delete => "delete",
            Verb::Upsert => "upsert",
            Verb::Seal => "seal",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerbStmt {
    pub attrs: Vec<Attr>,
    pub verb: Verb,
    pub head: Head,
    pub to: Option<Expr>,
    pub weight: Option<Expr>,
    pub resolve: Option<Policy>,
    pub span: Span,
}

/// A head `r(args)` or `a.r(args)`.
#[derive(Clone, Debug)]
pub struct Head {
    pub rel: Vec<Ident>,
    pub args: Vec<Arg>,
    pub span: Span,
}

/// `resolve NAME [e]`.
#[derive(Clone, Debug)]
pub struct Policy {
    pub name: Ident,
    pub arg: Option<Expr>,
    pub span: Span,
}

/// `fact r(…);`, and in a spec `fact r(…) @ n [at tick k];`.
#[derive(Clone, Debug)]
pub struct Fact {
    pub head: Head,
    pub at: Option<Expr>,
    /// `from s`: the sender of a scenario message on a channel from an external role (a session number).
    pub from: Option<Expr>,
    pub tick: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Invariant {
    pub name: Ident,
    pub message: Option<String>,
    pub body: Body,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Interpose {
    pub target: Vec<Ident>,
    pub outside: Ident,
    pub inside: Ident,
    pub items: Vec<Item>,
}

/// A body: literals and `where` guards.
#[derive(Clone, Debug)]
pub struct Body {
    pub lits: Vec<Lit>,
    pub guards: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Lit {
    /// A plain literal: an atom, a generator, a membership test, a guard or a bang filter.
    Plain(AtomLit),
    Not(Box<Lit>, Span),
    NotBody(Body, Span),
    Let {
        pat: Expr,
        value: Expr,
        span: Span,
    },
    Outer(AtomLit),
    Inserted(AtomLit),
    Deleted(AtomLit),
    Sealed(AtomLit),
    Final(AtomLit),
    Per(AtomLit),
    Any(Vec<Body>, Span),
    Forall {
        domain: AtomLit,
        body: Body,
        span: Span,
    },
    /// Spec-only literals (`ever`, `sent`, `quorum`), kept for the spec phase.
    Spec(SpecLit),
}

impl Lit {
    pub fn span(&self) -> Span {
        match self {
            Lit::Plain(a)
            | Lit::Outer(a)
            | Lit::Inserted(a)
            | Lit::Deleted(a)
            | Lit::Sealed(a)
            | Lit::Final(a)
            | Lit::Per(a) => a.span,
            Lit::Not(_, s) | Lit::NotBody(_, s) | Lit::Any(_, s) => *s,
            Lit::Let { span, .. } | Lit::Forall { span, .. } => *span,
            Lit::Spec(s) => s.span(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SpecLit {
    Ever(AtomLit),
    Sent(AtomLit),
    Quorum {
        var: Ident,
        role: Vec<Ident>,
        body: Body,
        span: Span,
    },
}

impl SpecLit {
    pub fn span(&self) -> Span {
        match self {
            SpecLit::Ever(a) | SpecLit::Sent(a) => a.span,
            SpecLit::Quorum { span, .. } => *span,
        }
    }
}

/// An expression literal with its suffixes (`from s`, `principal p`, `weight w`, `@ n`, `at tick k`).
#[derive(Clone, Debug)]
pub struct AtomLit {
    pub expr: Expr,
    pub from: Option<Expr>,
    pub principal: Option<Expr>,
    pub weight: Option<Expr>,
    pub at: Option<Expr>,
    pub at_tick: Option<Expr>,
    pub span: Span,
}

/// An argument: positional `e`, named `f: e`, a pun `f` in named mode (parsed as positional), `..`, or `*`.
#[derive(Clone, Debug)]
pub enum Arg {
    Pos(Expr),
    Named(Ident, Expr),
    Rest(Span),
    Star(Span),
}

impl Arg {
    pub fn span(&self) -> Span {
        match self {
            Arg::Pos(e) => e.span,
            Arg::Named(n, e) => n.span.to(e.span).unwrap_or(n.span),
            Arg::Rest(s) | Arg::Star(s) => *s,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    In,
    /// `a..b`
    Range,
    /// `a..=b`
    RangeEq,
    /// `a<..b`
    OpenRange,
    /// `a<..=b`
    OpenRangeEq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixOp {
    Not,
    Neg,
    BitNot,
}

/// A literal constant.
#[derive(Clone, Debug, PartialEq)]
pub enum LitValue {
    /// An integer with its optional suffix (`3u64`).
    Int {
        value: u128,
        suffix: Option<Symbol>,
    },
    Float(f64),
    /// Nanoseconds.
    Duration(u128),
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(LitValue),
    /// `a`, `a::b`, `T::<U>`.
    Path(Vec<Ident>, Vec<Type>),
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Method {
        receiver: Box<Expr>,
        name: Ident,
        args: Vec<Arg>,
    },
    /// `name!(args clauses…)`: aggregates, choices, order filters.
    Bang {
        name: Ident,
        args: Vec<Arg>,
        clauses: Vec<BangClause>,
    },
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    TupleIndex {
        base: Box<Expr>,
        index: u32,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Prefix {
        op: PrefixOp,
        arg: Box<Expr>,
    },
    Cast {
        expr: Box<Expr>,
        ty: Type,
    },
    Tuple(Vec<Expr>),
    Vec(Vec<Expr>),
    Set(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Option<Box<Expr>>,
    },
    Match {
        scrut: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    StructLit {
        path: Vec<Ident>,
        fields: Vec<(Ident, Option<Expr>)>,
    },
    Wildcard,
    SelfNode,
    /// `{ let p = e; …; result }`: `let`s bind in order (function bodies only, LANGUAGE §16.1).
    Block {
        lets: Vec<BlockLet>,
        result: Box<Expr>,
    },
    /// `|a, b| body`: only as an argument of a built-in combinator, in function bodies (LANGUAGE §16.1).
    Closure {
        params: Vec<Ident>,
        body: Box<Expr>,
    },
    /// `e?`: `e`'s value if it is `Some`, else the enclosing function returns `None` (EXTENSIONS 2.1). Function
    /// bodies desugar it into `match`es before name resolution (`ast::desugar`); anywhere else it is BLS0218.
    Try(Box<Expr>),
}

/// `let pat [: T] = value;` in a block.
#[derive(Clone, Debug)]
pub struct BlockLet {
    pub pat: Expr,
    pub ty: Option<Type>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pat: Expr,
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A clause of a bang call: its keyword (`per`, `by`, `most`, `least`, `default`, …) and expressions.
#[derive(Clone, Debug)]
pub struct BangClause {
    pub keyword: Ident,
    pub exprs: Vec<Expr>,
    /// `by k desc` keys: each expression and whether it is descending.
    pub order: Vec<(Expr, bool)>,
    pub span: Span,
}

/// `spec Name [for Target] { members }`.
#[derive(Clone, Debug)]
pub struct SpecItem {
    pub name: Option<Ident>,
    pub target: Option<Vec<Ident>>,
    pub members: Vec<SpecMember>,
    /// The attributes written on its members (no built-in attribute applies to one).
    pub member_attrs: Vec<Attr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum SpecMember {
    Nodes(Vec<Ident>),
    Assign {
        role: Ident,
        nodes: Vec<Ident>,
        span: Span,
    },
    Faults(Vec<(Ident, Expr)>, Span),
    Include(Vec<Ident>, Span),
    Check {
        kind: Ident,
        options: Vec<(Ident, Expr)>,
        expect: Option<Ident>,
        span: Span,
    },
    Fact(Fact),
    View(ViewDecl),
    Invariant(Invariant),
    Const {
        name: Ident,
        ty: Type,
        value: Expr,
    },
    /// A member this build does not accept yet; already reported.
    Unsupported {
        what: &'static str,
        span: Span,
    },
}

#[cfg(test)]
mod tests;
