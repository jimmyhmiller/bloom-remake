//! Syntax-independent construction with explicit construct membership.
use crate::{IrError, ValidatedProgram, core::*};
use blossom_base::{IndexVec, QualName, RuleLabel, Span, Symbol, idx::*};
use blossom_value::{TypeTable, Value};
use std::sync::Arc;
/// Frontend responsible for this program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontendKind {
    Blossom,
    Ded,
    Overlog,
    Hydro,
    Bloom,
}
/// A lattice declaration whose id is assigned by the builder.
pub type LatticeDefInput = LatticeDef;
/// A function declaration whose id is assigned by the builder.
pub type FnDeclInput = FnDecl;
/// A relation declaration whose id and construct ownership are assigned by the builder.
pub type RelDeclInput = RelDecl;
/// A construct's semantic specification.
pub type ConstructKindInput = ConstructKind;
/// An invariant declaration whose id is assigned by the builder.
pub type InvariantDeclInput = InvariantDecl;
/// Builds the only program form accepted across the IR boundary.
pub struct IrBuilder {
    program: Program,
    stack: Vec<ConstructId>,
    frontend: FrontendKind,
}
impl Program {
    /// Empty program for incremental construction.
    pub fn new(meta: ProgramMeta) -> Self {
        Self {
            meta,
            types: TypeTable::new(),
            lattices: IndexVec::new(),
            groups: IndexVec::new(),
            consts: IndexVec::new(),
            params: IndexVec::new(),
            fns: IndexVec::new(),
            udas: IndexVec::new(),
            services: IndexVec::new(),
            streams: Vec::new(),
            roles: IndexVec::new(),
            rels: IndexVec::new(),
            rules: IndexVec::new(),
            facts: Vec::new(),
            constructs: IndexVec::new(),
            sites: IndexVec::new(),
            invariants: IndexVec::new(),
            migrations: Vec::new(),
            translations: Vec::new(),
        }
    }
}
impl IrBuilder {
    /// Starts a program for a particular frontend.
    pub fn new(meta: ProgramMeta, frontend: FrontendKind) -> Self {
        Self {
            program: Program::new(meta),
            stack: Vec::new(),
            frontend,
        }
    }
    /// Originating frontend, used to select how validation failures are rendered.
    pub fn frontend(&self) -> FrontendKind {
        self.frontend
    }
    /// The program built so far (read-only).
    pub fn program(&self) -> &Program {
        &self.program
    }
    /// Structural type interning table.
    pub fn types(&mut self) -> &mut TypeTable {
        &mut self.program.types
    }
    /// Interns a folded constant, retaining one id per canonical value.
    pub fn intern_const(&mut self, v: Value) -> Result<ConstId, IrError> {
        if let Some((id, _)) = self.program.consts.iter_enumerated().find(|(_, x)| *x == &v) {
            return Ok(id);
        }
        self.program.consts.push(v).map_err(|e| IrError::builder(e.to_string()))
    }
    /// Declares a role, rejecting duplicate names.
    pub fn declare_role(&mut self, name: QualName, kind: RoleKind, _span: Span) -> Result<RoleId, IrError> {
        if self.program.roles.iter().any(|r| r.name == name) {
            return Err(IrError::builder(format!("duplicate role {name}")));
        }
        let id = self
            .program
            .roles
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.program
            .roles
            .push(RoleDecl { id, name, kind })
            .map_err(|e| IrError::builder(e.to_string()))
    }
    /// Declares a lattice operation catalogue.
    pub fn declare_lattice(&mut self, mut def: LatticeDefInput) -> Result<LatticeTypeId, IrError> {
        if self.program.lattices.iter().any(|r| r.name == def.name) {
            return Err(IrError::builder("duplicate lattice"));
        }
        def.id = self
            .program
            .lattices
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.program
            .lattices
            .push(def)
            .map_err(|e| IrError::builder(e.to_string()))
    }
    /// Declares a byte stream (FOREIGN-PROTOCOLS §1); its relations are declared first.
    pub fn declare_stream(&mut self, s: StreamDecl) -> Result<(), IrError> {
        if self.program.streams.iter().any(|x| x.name == s.name) {
            return Err(IrError::builder("duplicate stream"));
        }
        self.program.streams.push(s);
        Ok(())
    }
    /// Declares a pure function.
    pub fn declare_fn(&mut self, mut f: FnDeclInput) -> Result<FnId, IrError> {
        if self.program.fns.iter().any(|r| r.name == f.name) {
            return Err(IrError::builder("duplicate function"));
        }
        f.id = self
            .program
            .fns
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.program.fns.push(f).map_err(|e| IrError::builder(e.to_string()))
    }
    /// Declares a relation; generated declarations join the currently open construct.
    pub fn declare_relation(&mut self, mut d: RelDeclInput) -> Result<RelId, IrError> {
        if matches!(d.origin, Origin::User(_)) && d.name.to_string() == "violation" {
            return Err(IrError::builder(
                "`violation` is reserved for invariant and seal reports",
            ));
        }
        if self.program.rels.iter().any(|r| r.name == d.name) {
            return Err(IrError::builder(format!("duplicate relation {}", d.name)));
        }
        if matches!(d.origin, Origin::Generated { .. }) {
            let id = *self
                .stack
                .last()
                .ok_or_else(|| IrError::builder("generated relation outside a construct"))?;
            d.origin = Origin::Generated { construct: id };
        }
        d.id = self
            .program
            .rels
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        let owner = match d.origin {
            Origin::Generated { construct } => Some(construct),
            _ => None,
        };
        let id = self.program.rels.push(d).map_err(|e| IrError::builder(e.to_string()))?;
        if let Some(c) = owner {
            self.program
                .constructs
                .get_mut(c)
                .ok_or_else(|| IrError::builder("missing construct"))?
                .rels
                .push(id);
        }
        Ok(id)
    }
    /// Sets a relation's persistence once the rule that realizes it exists (a table's frame rule, a lattice's
    /// identity rule); the validator checks that the rule is the exact expansion.
    pub fn set_persistence(&mut self, rel: RelId, persistence: Persistence) -> Result<(), IrError> {
        let r = self
            .program
            .rels
            .get_mut(rel)
            .ok_or_else(|| IrError::builder("unknown relation"))?;
        r.persistence = persistence;
        Ok(())
    }
    /// Sets a channel's ACL once every relation it names is declared (an explicit ACL's `principal in` relation may
    /// be declared after the channel).
    pub fn set_acl(&mut self, rel: RelId, acl: AclSpec) -> Result<(), IrError> {
        let r = self
            .program
            .rels
            .get_mut(rel)
            .ok_or_else(|| IrError::builder("unknown relation"))?;
        match &mut r.class {
            RelClass::Channel(ch) => {
                ch.acl = acl;
                Ok(())
            }
            _ => Err(IrError::builder(format!("{} is not a channel: it has no ACL", r.name))),
        }
    }
    /// Replaces a construct's specification: for expansions whose spec names relations declared inside the
    /// construct (a table's `$del`).
    pub fn set_construct_kind(&mut self, id: ConstructId, kind: ConstructKindInput) -> Result<(), IrError> {
        let c = self
            .program
            .constructs
            .get_mut(id)
            .ok_or_else(|| IrError::builder("unknown construct"))?;
        c.kind = kind;
        Ok(())
    }
    /// Opens a grouping; nested groups own only their directly declared members.
    pub fn begin_construct(&mut self, kind: ConstructKindInput, surface: SurfaceRef) -> Result<ConstructId, IrError> {
        let id = self
            .program
            .constructs
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.program
            .constructs
            .push(Construct {
                id,
                kind,
                rules: Vec::new(),
                rels: Vec::new(),
                surface,
            })
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.stack.push(id);
        Ok(id)
    }
    /// Closes exactly the innermost construct.
    pub fn end_construct(&mut self, id: ConstructId) -> Result<(), IrError> {
        if self.stack.last() != Some(&id) {
            return Err(IrError::builder("constructs must close in stack order"));
        }
        self.stack.pop();
        Ok(())
    }
    /// Registers a seeded operation with its stable PRF domain separator.
    pub fn declare_site(&mut self, stable: Arc<str>, kind: SiteKind) -> Result<SiteId, IrError> {
        let construct = *self
            .stack
            .last()
            .ok_or_else(|| IrError::builder("site outside a construct"))?;
        if self.program.sites.iter().any(|s| s.stable == stable) {
            return Err(IrError::builder("duplicate site"));
        }
        let id = self
            .program
            .sites
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        let key = RuleLabel::new(stable.clone()).hash;
        self.program
            .sites
            .push(Site {
                id,
                stable,
                key,
                kind,
                construct,
            })
            .map_err(|e| IrError::builder(e.to_string()))
    }
    /// Registers an invariant for violation reporting.
    pub fn declare_invariant(&mut self, mut d: InvariantDeclInput) -> Result<InvariantId, IrError> {
        if self.program.invariants.iter().any(|r| r.name == d.name) {
            return Err(IrError::builder("duplicate invariant"));
        }
        d.id = self
            .program
            .invariants
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        self.program
            .invariants
            .push(d)
            .map_err(|e| IrError::builder(e.to_string()))
    }
    /// Opens a rule with a fresh variable namespace.
    pub fn rule(&mut self, kind: RuleKind, label: RuleLabel, span: Span) -> RuleBuilder<'_> {
        RuleBuilder {
            builder: self,
            kind,
            label,
            span,
            body: Body {
                vars: IndexVec::new(),
                lits: Vec::new(),
            },
        }
    }
    /// Adds an explicitly typed row to a static relation.
    pub fn fact(&mut self, rel: RelId, row: Vec<ConstId>, span: Span) -> Result<(), IrError> {
        let r = self
            .program
            .rels
            .get(rel)
            .ok_or_else(|| IrError::builder("unknown fact relation"))?;
        if !matches!(r.class, RelClass::Static) {
            return Err(IrError::validation(
                2,
                None,
                Some(span),
                "facts require a static relation",
            ));
        }
        if row.len() != r.schema.cols.len() {
            return Err(IrError::validation(11, None, Some(span), "fact arity mismatch"));
        }
        for (c, id) in r.schema.cols.iter().zip(&row) {
            let v = self
                .program
                .consts
                .get(*id)
                .ok_or_else(|| IrError::builder("unknown fact constant"))?;
            self.program
                .types
                .check_value(c.ty, v)
                .map_err(|e| IrError::builder(e.to_string()))?;
        }
        self.program.facts.push(Fact { rel, row, span });
        Ok(())
    }
    /// Validates the completed expansion, rejecting unclosed constructs.
    pub fn finish(self) -> Result<ValidatedProgram, Vec<IrError>> {
        if !self.stack.is_empty() {
            return Err(vec![IrError::builder("unclosed construct")]);
        }
        ValidatedProgram::validate(self.program)
    }
}
/// A rule's local variable namespace and unordered conjunction.
pub struct RuleBuilder<'b> {
    builder: &'b mut IrBuilder,
    kind: RuleKind,
    label: RuleLabel,
    span: Span,
    body: Body,
}
impl RuleBuilder<'_> {
    /// Declares a typed variable in this rule.
    pub fn var(&mut self, name: Symbol, ty: TypeId) -> Result<VarId, IrError> {
        if self.body.vars.iter().any(|v| v.name == name) {
            return Err(IrError::builder("duplicate variable"));
        }
        self.body
            .vars
            .push(VarDecl {
                name,
                ty,
                non_bottom: false,
            })
            .map_err(|e| IrError::builder(e.to_string()))
    }
    /// Adds a conjunction member.
    pub fn lit(&mut self, l: Literal) -> &mut Self {
        self.body.lits.push(l);
        self
    }
    /// Completes the rule and associates it with the innermost construct.
    pub fn head(self, h: Head, role: Option<RoleId>) -> Result<RuleId, IrError> {
        let id = self
            .builder
            .program
            .rules
            .next_idx()
            .map_err(|e| IrError::builder(e.to_string()))?;
        let construct = self.builder.stack.last().copied();
        self.builder
            .program
            .rules
            .push(Rule {
                id,
                label: self.label,
                kind: self.kind,
                head: h,
                body: self.body,
                role,
                construct,
                span: self.span,
            })
            .map_err(|e| IrError::builder(e.to_string()))?;
        if let Some(c) = construct {
            self.builder
                .program
                .constructs
                .get_mut(c)
                .ok_or_else(|| IrError::builder("missing construct"))?
                .rules
                .push(id);
        }
        Ok(id)
    }
}
