//! Typed, nullable AST views over the lossless CST (ARCHITECTURE §13.3).
use crate::{SyntaxKind, SyntaxNode};
use std::marker::PhantomData;
/// A typed view over a CST node. Casts are total: malformed nodes return `None`.
pub trait AstNode: Sized {
    /// Whether this view accepts a node kind.
    fn can_cast(kind: SyntaxKind) -> bool;
    /// Cast a CST node if it has the expected kind.
    fn cast(node: SyntaxNode) -> Option<Self>;
    /// The underlying lossless CST node.
    fn syntax(&self) -> &SyntaxNode;
}
/// Iterator over immediate children of a particular AST type.
pub struct AstChildren<N: AstNode> {
    inner: rowan::SyntaxNodeChildren<crate::BlossomLanguage>,
    marker: PhantomData<N>,
}
impl<N: AstNode> Iterator for AstChildren<N> {
    type Item = N;
    fn next(&mut self) -> Option<N> {
        self.inner.find_map(N::cast)
    }
}
fn children<N: AstNode>(node: &SyntaxNode) -> AstChildren<N> {
    AstChildren {
        inner: node.children(),
        marker: PhantomData,
    }
}
fn child<N: AstNode>(node: &SyntaxNode) -> Option<N> {
    children(node).next()
}
mod generated;
pub use generated::*;

impl SourceFile {
    /// Inner attributes in source order.
    pub fn inner_attrs(&self) -> AstChildren<InnerAttr> {
        children(&self.0)
    }
    /// Optional program header.
    pub fn header(&self) -> Option<ProgramHeader> {
        child(&self.0)
    }
    /// Top-level items in source order.
    pub fn items(&self) -> AstChildren<Item> {
        children(&self.0)
    }
}
impl Item {
    /// The outer attributes attached to this item.
    pub fn attrs(&self) -> AstChildren<Attr> {
        children(self.syntax())
    }
    /// Whether this item carries the `pub` modifier.
    pub fn is_pub(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .any(|c| c.kind() == SyntaxKind::PUB_KW)
    }
}
impl RelDecl {
    /// Relation columns.
    pub fn columns(&self) -> AstChildren<ColDecl> {
        children(&self.0)
    }
    /// Relation name.
    pub fn relation_name(&self) -> Option<Name> {
        child(&self.0)
    }
    /// `like` target, if any.
    pub fn like(&self) -> Option<RelPath> {
        child(&self.0)
    }
}
impl HandlerItem {
    /// Label if present.
    pub fn label(&self) -> Option<Name> {
        child(&self.0)
    }
    /// Header condition.
    pub fn header(&self) -> Option<Body> {
        child(&self.0)
    }
    /// Consequence block.
    pub fn block(&self) -> Option<Block> {
        child(&self.0)
    }
}
impl Block {
    /// Consequence statements.
    pub fn stmts(&self) -> AstChildren<Stmt> {
        children(&self.0)
    }
}
impl Body {
    /// Body literals.
    pub fn literals(&self) -> AstChildren<Literal> {
        children(&self.0)
    }
}
impl SpecItem {
    /// Direct members of a spec.
    pub fn members(&self) -> impl Iterator<Item = SyntaxNode> + '_ {
        self.0.children()
    }
}
/// Relation persistence and collection modifiers, in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelMod {
    Durable,
    Soft,
    Sealed,
    ZSet,
    Bag,
    Final,
}
/// Relation declaration kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    Table,
    Scratch,
    Channel,
    Input,
    Output,
    Static,
    Loopback,
}
/// Handler trigger kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    On,
    While,
}
/// Consequence verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Emit,
    Next,
    Send,
    Delete,
    Upsert,
    Seal,
}
/// One typed declaration clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelClause {
    /// Channel source and destination.
    Direction(DirectionClause),
    /// Key fields.
    Key(KeyClause),
    /// Soft-state TTL.
    Ttl(TtlClause),
    /// Soft-state bound.
    Max(MaxClause),
    /// Range key.
    Range(RangeClause),
    /// Conflict policy.
    Resolve(ResolveClause),
    /// Partition expression.
    PartitionBy(PartitionClause),
    /// Seal producers.
    SealedBy(SealedByClause),
    /// Exactly-once capability.
    ExactlyOnce(ExactlyOnceClause),
}
impl AstNode for RelClause {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::DIRECTIONCLAUSE
                | SyntaxKind::KEYCLAUSE
                | SyntaxKind::TTLCLAUSE
                | SyntaxKind::MAXCLAUSE
                | SyntaxKind::RANGECLAUSE
                | SyntaxKind::RESOLVECLAUSE
                | SyntaxKind::PARTITIONCLAUSE
                | SyntaxKind::SEALEDBYCLAUSE
                | SyntaxKind::EXACTLYONCECLAUSE
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::DIRECTIONCLAUSE => Self::Direction(DirectionClause(node)),
            SyntaxKind::KEYCLAUSE => Self::Key(KeyClause(node)),
            SyntaxKind::TTLCLAUSE => Self::Ttl(TtlClause(node)),
            SyntaxKind::MAXCLAUSE => Self::Max(MaxClause(node)),
            SyntaxKind::RANGECLAUSE => Self::Range(RangeClause(node)),
            SyntaxKind::RESOLVECLAUSE => Self::Resolve(ResolveClause(node)),
            SyntaxKind::PARTITIONCLAUSE => Self::PartitionBy(PartitionClause(node)),
            SyntaxKind::SEALEDBYCLAUSE => Self::SealedBy(SealedByClause(node)),
            SyntaxKind::EXACTLYONCECLAUSE => Self::ExactlyOnce(ExactlyOnceClause(node)),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Direction(n) => &n.0,
            Self::Key(n) => &n.0,
            Self::Ttl(n) => &n.0,
            Self::Max(n) => &n.0,
            Self::Range(n) => &n.0,
            Self::Resolve(n) => &n.0,
            Self::PartitionBy(n) => &n.0,
            Self::SealedBy(n) => &n.0,
            Self::ExactlyOnce(n) => &n.0,
        }
    }
}
/// Atom location or security suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomSuffix {
    From(FromSuffix),
    Principal(PrincipalSuffix),
    Weight(WeightSuffix),
    At(AtSuffix),
    AtTick(AtTickSuffix),
}
impl AstNode for AtomSuffix {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::FROMSUFFIX
                | SyntaxKind::PRINCIPALSUFFIX
                | SyntaxKind::WEIGHTSUFFIX
                | SyntaxKind::ATSUFFIX
                | SyntaxKind::ATTICKSUFFIX
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::FROMSUFFIX => Self::From(FromSuffix(node)),
            SyntaxKind::PRINCIPALSUFFIX => Self::Principal(PrincipalSuffix(node)),
            SyntaxKind::WEIGHTSUFFIX => Self::Weight(WeightSuffix(node)),
            SyntaxKind::ATSUFFIX => Self::At(AtSuffix(node)),
            SyntaxKind::ATTICKSUFFIX => Self::AtTick(AtTickSuffix(node)),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::From(n) => &n.0,
            Self::Principal(n) => &n.0,
            Self::Weight(n) => &n.0,
            Self::At(n) => &n.0,
            Self::AtTick(n) => &n.0,
        }
    }
}
/// Typed spec member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecMember {
    Nodes(NodesMember),
    Assign(AssignMember),
    Faults(FaultsMember),
    Liveness(LivenessMember),
    Prove(ProveMember),
    Expect(ExpectMember),
    Check(CheckMember),
    Const(ConstItem),
    Fact(FactItem),
    View(ViewDecl),
    Invariant(InvariantItem),
}
impl AstNode for SpecMember {
    fn can_cast(k: SyntaxKind) -> bool {
        matches!(
            k,
            SyntaxKind::NODESMEMBER
                | SyntaxKind::ASSIGNMEMBER
                | SyntaxKind::FAULTSMEMBER
                | SyntaxKind::LIVENESSMEMBER
                | SyntaxKind::PROVEMEMBER
                | SyntaxKind::EXPECTMEMBER
                | SyntaxKind::CHECKMEMBER
                | SyntaxKind::CONSTITEM
                | SyntaxKind::FACTITEM
                | SyntaxKind::VIEWDECL
                | SyntaxKind::INVARIANTITEM
        )
    }
    fn cast(n: SyntaxNode) -> Option<Self> {
        Some(match n.kind() {
            SyntaxKind::NODESMEMBER => Self::Nodes(NodesMember(n)),
            SyntaxKind::ASSIGNMEMBER => Self::Assign(AssignMember(n)),
            SyntaxKind::FAULTSMEMBER => Self::Faults(FaultsMember(n)),
            SyntaxKind::LIVENESSMEMBER => Self::Liveness(LivenessMember(n)),
            SyntaxKind::PROVEMEMBER => Self::Prove(ProveMember(n)),
            SyntaxKind::EXPECTMEMBER => Self::Expect(ExpectMember(n)),
            SyntaxKind::CHECKMEMBER => Self::Check(CheckMember(n)),
            SyntaxKind::CONSTITEM => Self::Const(ConstItem(n)),
            SyntaxKind::FACTITEM => Self::Fact(FactItem(n)),
            SyntaxKind::VIEWDECL => Self::View(ViewDecl(n)),
            SyntaxKind::INVARIANTITEM => Self::Invariant(InvariantItem(n)),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Nodes(n) => &n.0,
            Self::Assign(n) => &n.0,
            Self::Faults(n) => &n.0,
            Self::Liveness(n) => &n.0,
            Self::Prove(n) => &n.0,
            Self::Expect(n) => &n.0,
            Self::Check(n) => &n.0,
            Self::Const(n) => &n.0,
            Self::Fact(n) => &n.0,
            Self::View(n) => &n.0,
            Self::Invariant(n) => &n.0,
        }
    }
}
impl Name {
    /// Source spelling, including the `r#` marker on a raw identifier.
    pub fn text(&self) -> String {
        self.0
            .children_with_tokens()
            .find_map(|e| {
                e.as_token()
                    .filter(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string())
            })
            .unwrap_or_default()
    }
}
impl RelDecl {
    /// Declaration modifiers in source order.
    pub fn modifiers(&self) -> impl Iterator<Item = RelMod> + '_ {
        self.0
            .children_with_tokens()
            .filter_map(|e| match e.as_token().map(|t| t.text().to_string()).as_deref() {
                Some("durable") => Some(RelMod::Durable),
                Some("soft") => Some(RelMod::Soft),
                Some("sealed") => Some(RelMod::Sealed),
                Some("zset") => Some(RelMod::ZSet),
                Some("bag") => Some(RelMod::Bag),
                Some("final") => Some(RelMod::Final),
                _ => None,
            })
    }
    /// Relation kind, if present.
    pub fn kind(&self) -> Option<RelKind> {
        self.0.children_with_tokens().find_map(|e| match e.kind() {
            SyntaxKind::TABLE_KW => Some(RelKind::Table),
            SyntaxKind::SCRATCH_KW => Some(RelKind::Scratch),
            SyntaxKind::CHANNEL_KW => Some(RelKind::Channel),
            SyntaxKind::INPUT_KW => Some(RelKind::Input),
            SyntaxKind::OUTPUT_KW => Some(RelKind::Output),
            SyntaxKind::STATIC_KW => Some(RelKind::Static),
            SyntaxKind::LOOPBACK_KW => Some(RelKind::Loopback),
            _ => None,
        })
    }
    /// Typed relation clauses.
    pub fn clauses(&self) -> AstChildren<RelClause> {
        children(&self.0)
    }
}
impl HandlerItem {
    /// Whether this handler carries `monotone`.
    pub fn monotone(&self) -> bool {
        self.0
            .children_with_tokens()
            .any(|e| e.as_token().is_some_and(|t| t.text() == "monotone"))
    }
    /// Edge- or level-triggered handler, if present.
    pub fn trigger(&self) -> Option<Trigger> {
        self.0.children_with_tokens().find_map(|e| match e.kind() {
            SyntaxKind::ON_KW => Some(Trigger::On),
            SyntaxKind::WHILE_KW => Some(Trigger::While),
            _ => None,
        })
    }
}
impl VerbStmt {
    /// Statement attributes.
    pub fn attrs(&self) -> AstChildren<Attr> {
        children(&self.0)
    }
    /// Consequence verb, if present.
    pub fn verb(&self) -> Option<Verb> {
        self.0.children_with_tokens().find_map(|e| match e.kind() {
            SyntaxKind::EMIT_KW => Some(Verb::Emit),
            SyntaxKind::NEXT_KW => Some(Verb::Next),
            SyntaxKind::SEND_KW => Some(Verb::Send),
            SyntaxKind::DELETE_KW => Some(Verb::Delete),
            SyntaxKind::UPSERT_KW => Some(Verb::Upsert),
            SyntaxKind::SEAL_KW => Some(Verb::Seal),
            _ => None,
        })
    }
    /// Relation head.
    pub fn head(&self) -> Option<Head> {
        child(&self.0)
    }
    /// Optional resolve policy.
    pub fn resolve(&self) -> Option<Policy> {
        child(&self.0)
    }
    /// `to` address, if present.
    pub fn to(&self) -> Option<Expr> {
        if matches!(self.verb(), Some(Verb::Send | Verb::Seal)) {
            child(&self.0)
        } else {
            None
        }
    }
    /// `weight` expression, if present.
    pub fn weight(&self) -> Option<Expr> {
        if matches!(self.verb(), Some(Verb::Emit | Verb::Next)) {
            child(&self.0)
        } else {
            None
        }
    }
}
impl Body {
    /// Direct `where` guards (not expressions nested in literals).
    pub fn where_guards(&self) -> AstChildren<Expr> {
        children(&self.0)
    }
}
impl AtomLit {
    /// Atom expression.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
    /// Suffixes in source order.
    pub fn suffixes(&self) -> AstChildren<AtomSuffix> {
        children(&self.0)
    }
}
impl SpecItem {
    /// Typed spec members.
    pub fn typed_members(&self) -> AstChildren<SpecMember> {
        children(&self.0)
    }
}
