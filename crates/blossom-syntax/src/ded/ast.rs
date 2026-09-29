//! The `.ded` syntax tree: one file's clauses, with spans. Nothing here is resolved or typed.

use std::sync::Arc;

use blossom_base::{Span, Symbol};

/// One parsed `.ded` file.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DedFile {
    /// The clauses in source order.
    pub clauses: Vec<Clause>,
}

/// A top-level clause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clause {
    /// `include "path";`
    Include(Include),
    /// `p(c1, …)@k;`
    Fact(Fact),
    /// `head :- body;`
    Rule(Rule),
}

/// `include "path";`: the path is relative to the including file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Include {
    pub path: Arc<str>,
    pub span: Span,
}

/// `p(c1, …)@k;`: an input fact at Molly time `k`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub rel: Ident,
    /// Constants only ([`Term::Int`] and [`Term::Str`]).
    pub args: Vec<Term>,
    pub time: u64,
    pub span: Span,
}

/// `head[@next|@async] :- item, …;`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub head: Head,
    pub time: HeadTime,
    pub body: Vec<BodyItem>,
    pub span: Span,
}

/// When a rule's head holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadTime {
    /// A deductive rule: the same node and time as its body.
    Now,
    /// `@next`: the same node, the next time.
    Next,
    /// `@async`: the node named by the head's first column, a later time.
    Async,
}

/// A rule head: a relation and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub rel: Ident,
    pub args: Vec<Arg>,
    pub span: Span,
}

/// A body item: an atom or a comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyItem {
    Atom(BodyAtom),
    /// A comparison qualifier such as `K > 1` or `S == P + 1`.
    Qual(Expr),
}

/// `[notin] p(a1, …)[@k]` in a rule body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyAtom {
    pub negated: bool,
    pub rel: Ident,
    pub args: Vec<Arg>,
    /// `@k`: the atom reads the relation at absolute time `k`.
    pub time: Option<u64>,
    pub span: Span,
}

/// An argument position of a fact, head or body atom. Which forms are legal where is checked by the frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    Expr(Expr),
    /// `count<X>`, `min<X>`, `max<X>`, `sum<X>`.
    Agg(Aggregate),
}

/// A head aggregate over one variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aggregate {
    pub func: AggFunc,
    pub var: Ident,
    pub span: Span,
}

/// Molly's aggregate functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    Min,
    Max,
    Sum,
}

impl AggFunc {
    /// The function's name as written.
    pub const fn name(self) -> &'static str {
        match self {
            AggFunc::Count => "count",
            AggFunc::Min => "min",
            AggFunc::Max => "max",
            AggFunc::Sum => "sum",
        }
    }
}

/// An expression. Molly parses `a op b op c` as `a op (b op c)`, without precedence; `lhs` is always a term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Term(Term),
    Binary {
        lhs: Term,
        op: BinOp,
        rhs: Box<Expr>,
        span: Span,
    },
}

impl Expr {
    /// The expression's span.
    pub fn span(&self) -> Span {
        match self {
            Expr::Term(t) => t.span(),
            Expr::Binary { span, .. } => *span,
        }
    }
}

/// A variable, a wildcard or a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    /// A capitalized identifier.
    Var(Ident),
    /// `_`.
    Wild(Span),
    Int(i64, Span),
    Str(Arc<str>, Span),
}

impl Term {
    /// The term's span.
    pub fn span(&self) -> Span {
        match self {
            Term::Var(i) => i.span,
            Term::Wild(s) | Term::Int(_, s) | Term::Str(_, s) => *s,
        }
    }
}

/// Molly's binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

impl BinOp {
    /// Whether the operator is a comparison (the only operators a body qualifier may use at its root).
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
        )
    }

    /// The operator as written.
    pub const fn text(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
        }
    }
}

/// An identifier with its span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ident {
    pub text: Symbol,
    pub span: Span,
}
