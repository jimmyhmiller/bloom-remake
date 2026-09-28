//! Molly's relations, the dialect's static rules, and the split into protocol and outcome spec.
//!
//! The static rules are Molly's (R06 §3.2) plus what the per-node reading of a `.ded` program needs
//! (LANGUAGE §21.1): every rule has a located first body predicate; a protocol rule's body atoms share that
//! location, and a deductive or `@next` head keeps it; `crash` and absolute-time atoms appear only in the spec
//! (ANA-010). The spec is `pre`, `post`, and every relation whose only readers are spec relations.

use std::collections::{BTreeMap, BTreeSet};

use blossom_artifact::ded::DedRelKind;
use blossom_base::{Diagnostic, Diagnostics, Span, Symbol, code};
use blossom_syntax::ded::{Arg, BodyAtom, BodyItem, Expr, HeadTime, Rule, Term};

use super::load::Program;

/// The spec oracle's relation name and arity: `crash(Observer, Node, Time)`.
pub(crate) const CRASH: &str = "crash";
const CRASH_ARITY: usize = 3;
/// Molly generates `clock`; a program may not use it.
const RESERVED: &str = "clock";

/// One Molly relation.
#[derive(Debug)]
pub(crate) struct RelInfo {
    pub name: Symbol,
    pub arity: usize,
    pub first: Span,
    pub kind: DedRelKind,
    /// Heads of deductive and `@next` rules.
    pub local_heads: bool,
    pub async_heads: bool,
    pub facts: bool,
}

/// The analysed program.
#[derive(Debug)]
pub(crate) struct Model {
    pub rels: Vec<RelInfo>,
    by_name: BTreeMap<Symbol, usize>,
    /// Per rule (index into `Program::rules`): whether it is a spec rule.
    pub spec_rule: Vec<bool>,
    /// Whether `pre` and `post` are both defined.
    pub has_spec: bool,
}

impl Model {
    pub fn index(&self, name: Symbol) -> Option<usize> {
        self.by_name.get(&name).copied()
    }

    pub fn rel(&self, name: Symbol) -> Option<&RelInfo> {
        self.index(name).and_then(|i| self.rels.get(i))
    }

    pub fn build(program: &Program, diags: &mut Diagnostics) -> Model {
        let mut m = Model {
            rels: Vec::new(),
            by_name: BTreeMap::new(),
            spec_rule: vec![false; program.rules.len()],
            has_spec: false,
        };
        m.collect(program, diags);
        if diags.has_errors() {
            return m;
        }
        for rule in &program.rules {
            check_rule(rule, diags);
        }
        m.classify(program, diags);
        if diags.has_errors() {
            return m;
        }
        m.check_placement(program, diags);
        m
    }

    fn note(&mut self, name: Symbol, arity: usize, span: Span, diags: &mut Diagnostics) {
        if name.as_str() == RESERVED {
            diags.push(Diagnostic::new(code!("BLS0201"), "`clock` is reserved: Molly generates it").with_primary(span));
            return;
        }
        match self.by_name.get(&name).and_then(|&i| self.rels.get(i)) {
            Some(r) if r.arity != arity => diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    format!("`{name}` is used with {arity} column(s) here and {} elsewhere", r.arity),
                )
                .with_primary(span)
                .with_label(r.first, "first used here"),
            ),
            Some(_) => {}
            None => {
                self.by_name.insert(name, self.rels.len());
                self.rels.push(RelInfo {
                    name,
                    arity,
                    first: span,
                    kind: DedRelKind::Protocol,
                    local_heads: false,
                    async_heads: false,
                    facts: false,
                });
            }
        }
    }

    fn rel_mut(&mut self, name: Symbol) -> Option<&mut RelInfo> {
        let i = self.index(name)?;
        self.rels.get_mut(i)
    }

    fn collect(&mut self, program: &Program, diags: &mut Diagnostics) {
        for fact in &program.facts {
            if fact.rel.text.as_str() == CRASH {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0405"),
                        "`crash` facts come from the failure spec, not from the program",
                    )
                    .with_primary(fact.span),
                );
                continue;
            }
            if fact.time < 1 {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0405"),
                        format!(
                            "a fact at time {}: Molly facts hold at times 1 and later (CR-13)",
                            fact.time
                        ),
                    )
                    .with_primary(fact.span),
                );
            }
            self.note(fact.rel.text, fact.args.len(), fact.rel.span, diags);
            if let Some(r) = self.rel_mut(fact.rel.text) {
                r.facts = true;
            }
        }
        for rule in &program.rules {
            let head = &rule.head;
            if head.rel.text.as_str() == CRASH {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0406"),
                        "`crash` is the spec's crash oracle and cannot be derived",
                    )
                    .with_primary(head.span),
                );
                continue;
            }
            self.note(head.rel.text, head.args.len(), head.rel.span, diags);
            if let Some(r) = self.rel_mut(head.rel.text) {
                match rule.time {
                    HeadTime::Now | HeadTime::Next => r.local_heads = true,
                    HeadTime::Async => r.async_heads = true,
                }
            }
            for atom in body_atoms(rule) {
                if atom.rel.text.as_str() == CRASH && atom.args.len() != CRASH_ARITY {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0301"),
                            format!("`crash` has {CRASH_ARITY} columns: crash(Observer, Node, Time)"),
                        )
                        .with_primary(atom.span),
                    );
                    continue;
                }
                self.note(atom.rel.text, atom.args.len(), atom.rel.span, diags);
            }
        }
        if let Some(r) = self.rel_mut(Symbol::intern(CRASH)) {
            r.kind = DedRelKind::Crash;
        }
    }

    /// Splits the relations into protocol and spec: `pre`, `post` and every relation read only by spec relations.
    fn classify(&mut self, program: &Program, diags: &mut Diagnostics) {
        let pre = Symbol::intern("pre");
        let post = Symbol::intern("post");
        let derived = |name: Symbol| program.rules.iter().any(|r| r.head.rel.text == name);
        match (derived(pre), derived(post)) {
            (true, true) => self.has_spec = true,
            (false, false) => return,
            (has_pre, _) => {
                let missing = if has_pre { "post" } else { "pre" };
                diags.push(Diagnostic::new(
                    code!("BLS0900"),
                    format!("the program defines one of `pre` and `post` but not `{missing}`: an outcome spec needs both (CR-30)"),
                ));
                return;
            }
        }
        if let (Some(a), Some(b)) = (self.rel(pre), self.rel(post))
            && a.arity != b.arity
        {
            diags.push(
                Diagnostic::new(
                    code!("BLS0301"),
                    "`pre` and `post` must have the same columns (TEST-022)",
                )
                .with_primary(b.first)
                .with_label(a.first, "`pre` is declared here"),
            );
            return;
        }
        let mut readers: BTreeMap<Symbol, BTreeSet<Symbol>> = BTreeMap::new();
        for rule in &program.rules {
            for atom in body_atoms(rule) {
                readers.entry(atom.rel.text).or_default().insert(rule.head.rel.text);
            }
        }
        let mut spec: BTreeSet<Symbol> = [pre, post].into_iter().collect();
        loop {
            let before = spec.len();
            for rule in &program.rules {
                let name = rule.head.rel.text;
                if spec.contains(&name) {
                    continue;
                }
                if readers
                    .get(&name)
                    .is_some_and(|rd| !rd.is_empty() && rd.is_subset(&spec))
                {
                    spec.insert(name);
                }
            }
            if spec.len() == before {
                break;
            }
        }
        for name in [pre, post] {
            if let Some(protocol_reader) = readers.get(&name).and_then(|rd| rd.iter().find(|r| !spec.contains(*r))) {
                let span = self.rel(*protocol_reader).map(|r| r.first);
                let mut d = Diagnostic::new(
                    code!("BLS0901"),
                    format!(
                        "`{name}` is read by the protocol relation `{protocol_reader}`: the spec may not feed the protocol"
                    ),
                );
                if let Some(span) = span {
                    d = d.with_primary(span);
                }
                diags.push(d);
            }
        }
        for r in &mut self.rels {
            if spec.contains(&r.name) {
                r.kind = DedRelKind::Spec;
            }
        }
        for (i, rule) in program.rules.iter().enumerate() {
            if let Some(slot) = self.spec_rule.get_mut(i) {
                *slot = spec.contains(&rule.head.rel.text);
            }
        }
    }

    fn check_placement(&self, program: &Program, diags: &mut Diagnostics) {
        for (i, rule) in program.rules.iter().enumerate() {
            let spec = self.spec_rule.get(i).copied().unwrap_or(false);
            if spec {
                if rule.time != HeadTime::Now {
                    diags.push(not_yet(
                        "an `@next` or `@async` rule that only feeds `pre`/`post`",
                        rule.span,
                    ));
                }
                if self.rel(rule.head.rel.text).is_some_and(|r| r.facts) {
                    diags.push(not_yet(
                        "`@k` facts into a relation that only feeds `pre`/`post`",
                        rule.span,
                    ));
                }
                continue;
            }
            for atom in body_atoms(rule) {
                if atom.rel.text.as_str() == CRASH {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0509"),
                            format!(
                                "`crash` is the spec's oracle, but `{}` is a protocol relation (ANA-010)",
                                rule.head.rel.text
                            ),
                        )
                        .with_primary(atom.span),
                    );
                } else if atom.time.is_some() {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0509"),
                            format!(
                                "an absolute-time atom is legal only in rules that feed `pre`/`post`, and `{}` is a protocol relation",
                                rule.head.rel.text
                            ),
                        )
                        .with_primary(atom.span),
                    );
                } else if self.rel(atom.rel.text).is_some_and(|r| r.kind == DedRelKind::Spec) {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0901"),
                            format!(
                                "the protocol relation `{}` reads the spec relation `{}`",
                                rule.head.rel.text, atom.rel.text
                            ),
                        )
                        .with_primary(atom.span),
                    );
                }
            }
            self.check_locality(rule, diags);
        }
    }

    /// A protocol rule runs at one node: every body atom has the rule's location in its first column, and a
    /// deductive or `@next` head is placed there too.
    fn check_locality(&self, rule: &Rule, diags: &mut Diagnostics) {
        let Some(first) = body_atoms(rule).next() else {
            return;
        };
        let Some(loc) = first.args.first().and_then(arg_term) else {
            return;
        };
        if matches!(loc, Term::Wild(_)) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0508"),
                    "a protocol rule needs a location: the first column of its first body predicate is `_`",
                )
                .with_primary(first.span),
            );
            return;
        }
        for atom in body_atoms(rule).skip(1) {
            let same = atom.args.first().and_then(arg_term).is_some_and(|t| same_term(t, loc));
            if !same {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0508"),
                        format!(
                            "`{}` is read at another location than the rule's (the first column of `{}`): protocol rules are local to one node",
                            atom.rel.text, first.rel.text
                        ),
                    )
                    .with_primary(atom.span)
                    .with_label(first.span, "the rule's location comes from here"),
                );
            }
        }
        if rule.time != HeadTime::Async {
            let same = rule
                .head
                .args
                .first()
                .and_then(|a| match a {
                    Arg::Expr(Expr::Term(t)) => Some(t),
                    _ => None,
                })
                .is_some_and(|t| same_term(t, loc));
            if !same {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0508"),
                        "a deductive or `@next` head is placed at the rule's location: use `@async` to derive a fact at another node",
                    )
                    .with_primary(rule.head.span)
                    .with_label(first.span, "the rule's location comes from here"),
                );
            }
        }
    }
}

/// Molly's per-rule static rules (R06 §3.2).
fn check_rule(rule: &Rule, diags: &mut Diagnostics) {
    let Some(first) = body_atoms(rule).next() else {
        diags.push(
            Diagnostic::new(
                code!("BLS0110"),
                "a rule needs a body predicate: its first column is the rule's location",
            )
            .with_primary(rule.span),
        );
        return;
    };
    match first.args.first() {
        Some(Arg::Expr(Expr::Term(Term::Var(_) | Term::Str(..) | Term::Int(..) | Term::Wild(_)))) => {}
        _ => diags.push(
            Diagnostic::new(
                code!("BLS0110"),
                "the first column of the first body predicate is the rule's location: it must be a variable or a constant",
            )
            .with_primary(first.span),
        ),
    }
    if rule.time == HeadTime::Async && matches!(first.args.first(), Some(Arg::Expr(Expr::Term(Term::Wild(_))))) {
        diags.push(
            Diagnostic::new(
                code!("BLS0110"),
                "an `@async` rule needs a located sender: its first body predicate starts with `_`",
            )
            .with_primary(first.span),
        );
    }
    let mut positive = BTreeSet::new();
    for atom in body_atoms(rule) {
        for arg in &atom.args {
            match arg {
                Arg::Expr(Expr::Term(t)) => {
                    if let (Term::Var(v), false) = (t, atom.negated) {
                        positive.insert(v.text);
                    }
                }
                Arg::Expr(e) => diags.push(
                    Diagnostic::new(
                        code!("BLS0110"),
                        "an expression inside a body predicate: bind it with a comparison (`X == Y + 1`)",
                    )
                    .with_primary(e.span()),
                ),
                Arg::Agg(g) => diags.push(
                    Diagnostic::new(code!("BLS0110"), "an aggregate is allowed only in a rule head")
                        .with_primary(g.span),
                ),
            }
        }
    }
    let unbound = |vars: Vec<(Symbol, Span)>, what: &str, diags: &mut Diagnostics| {
        for (v, span) in vars {
            if !positive.contains(&v) {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0500"),
                        format!("`{v}` in {what} is not bound by a positive body predicate (ANA-001)"),
                    )
                    .with_primary(span),
                );
            }
        }
    };
    for item in &rule.body {
        match item {
            BodyItem::Atom(a) if a.negated => {
                let vars = a.args.iter().filter_map(arg_term).filter_map(var_of).collect();
                unbound(vars, &format!("`notin {}`", a.rel.text), diags);
            }
            BodyItem::Atom(_) => {}
            BodyItem::Qual(e) => unbound(expr_vars(e), "a comparison", diags),
        }
    }
    let mut aggs = 0;
    for (i, arg) in rule.head.args.iter().enumerate() {
        match arg {
            Arg::Agg(g) => {
                aggs += 1;
                unbound(vec![(g.var.text, g.var.span)], "an aggregate", diags);
                if i == 0 {
                    diags.push(
                        Diagnostic::new(
                            code!("BLS0110"),
                            "the first head column is the location and cannot be an aggregate",
                        )
                        .with_primary(g.span),
                    );
                }
            }
            Arg::Expr(Expr::Term(Term::Wild(span))) => diags.push(
                Diagnostic::new(code!("BLS0500"), "`_` in a rule head: every head column needs a value")
                    .with_primary(*span),
            ),
            Arg::Expr(e) => {
                if contains_comparison(e) {
                    diags.push(
                        Diagnostic::new(code!("BLS0300"), "a comparison in a rule head: head columns are values")
                            .with_primary(e.span()),
                    );
                }
                unbound(expr_vars(e), "the head", diags);
            }
        }
    }
    if aggs > 1 {
        diags.push(
            Diagnostic::new(code!("BLS0110"), "at most one aggregate per rule head").with_primary(rule.head.span),
        );
    }
    if aggs > 0 {
        if rule.time != HeadTime::Now {
            diags.push(not_yet("an aggregate in an `@next` or `@async` head", rule.head.span));
        }
        if rule
            .head
            .args
            .iter()
            .any(|a| matches!(a, Arg::Expr(Expr::Binary { .. })))
        {
            diags.push(not_yet(
                "an aggregate head that also computes an expression",
                rule.head.span,
            ));
        }
    }
    if rule.time == HeadTime::Async && rule.head.args.is_empty() {
        diags.push(
            Diagnostic::new(
                code!("BLS0403"),
                "an `@async` head needs a destination in its first column",
            )
            .with_primary(rule.head.span),
        );
    }
    for atom in body_atoms(rule) {
        if atom.time == Some(0) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0110"),
                    "an absolute-time atom at time 0: Molly's times start at 1",
                )
                .with_primary(atom.span),
            );
        }
    }
}

/// BLS0908 for a `.ded` construct this build does not lower yet.
pub(crate) fn not_yet(what: &str, span: Span) -> Diagnostic {
    let err = blossom_base::unimplemented_error!("LANG-220", "{what}");
    Diagnostic::from_unimplemented(&err, "slice 2 (docs/design/SLICES.md)").with_primary(span)
}

pub(crate) fn body_atoms(rule: &Rule) -> impl Iterator<Item = &BodyAtom> {
    rule.body.iter().filter_map(|b| match b {
        BodyItem::Atom(a) => Some(a),
        BodyItem::Qual(_) => None,
    })
}

pub(crate) fn arg_term(a: &Arg) -> Option<&Term> {
    match a {
        Arg::Expr(Expr::Term(t)) => Some(t),
        _ => None,
    }
}

fn var_of(t: &Term) -> Option<(Symbol, Span)> {
    match t {
        Term::Var(v) => Some((v.text, v.span)),
        _ => None,
    }
}

pub(crate) fn expr_vars(e: &Expr) -> Vec<(Symbol, Span)> {
    match e {
        Expr::Term(t) => var_of(t).into_iter().collect(),
        Expr::Binary { lhs, rhs, .. } => var_of(lhs).into_iter().chain(expr_vars(rhs)).collect(),
    }
}

fn contains_comparison(e: &Expr) -> bool {
    match e {
        Expr::Term(_) => false,
        Expr::Binary { op, rhs, .. } => op.is_comparison() || contains_comparison(rhs),
    }
}

/// Whether two location terms denote the same location in every valuation.
fn same_term(a: &Term, b: &Term) -> bool {
    match (a, b) {
        (Term::Var(x), Term::Var(y)) => x.text == y.text,
        (Term::Str(x, _), Term::Str(y, _)) => x == y,
        (Term::Int(x, _), Term::Int(y, _)) => x == y,
        _ => false,
    }
}
