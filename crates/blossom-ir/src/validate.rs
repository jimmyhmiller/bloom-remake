//! The twelve structural invariants at the syntax-independent frontend boundary.
use crate::{
    IrError,
    core::*,
    visit::{Mapper, Remap},
};
use blossom_base::{RuleLabel, Span, idx::*};
use blossom_value::{TypeDef, types::IntTy};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
struct Refs {
    vars: BTreeSet<VarId>,
    sites: BTreeSet<SiteId>,
}
impl Mapper for Refs {
    fn varid(&mut self, id: VarId) -> VarId {
        self.vars.insert(id);
        id
    }
    fn siteid(&mut self, id: SiteId) -> SiteId {
        self.sites.insert(id);
        id
    }
}
/// What expression typing reads of where the expression is: the variables' types, the placement (for `$self`), and
/// whether it is a function body (which reads no scalar: functions are pure, LANGUAGE §16.1).
#[derive(Clone, Copy)]
struct Cx<'a> {
    vars: &'a IndexVec<VarId, VarDecl>,
    role: Option<RoleId>,
    in_fn: bool,
}

impl<'a> Cx<'a> {
    fn rule(r: &'a Rule) -> Cx<'a> {
        Cx {
            vars: &r.body.vars,
            role: r.role,
            in_fn: false,
        }
    }

    fn function(f: &'a FnDecl) -> Cx<'a> {
        Cx {
            vars: &f.vars,
            role: None,
            in_fn: true,
        }
    }
}

fn vars(x: &impl Remap) -> BTreeSet<VarId> {
    let mut r = Refs::default();
    x.remap(&mut r);
    r.vars
}
fn atoms(rule: &Rule) -> impl Iterator<Item = &Atom> {
    rule.body.lits.iter().filter_map(|l| match l {
        Literal::Pos(a) | Literal::Neg(a) => Some(a),
        _ => None,
    })
}
fn bound_vars(rule: &Rule) -> BTreeSet<VarId> {
    let mut bound = BTreeSet::new();
    for l in &rule.body.lits {
        if let Literal::Pos(a) = l {
            bound.extend(vars(a));
        }
    }
    loop {
        let old = bound.len();
        for l in &rule.body.lits {
            match l {
                Literal::Bind { pat, expr } if vars(expr).is_subset(&bound) => bound.extend(vars(pat)),
                Literal::Lookup { var, key, .. } if vars(key).is_subset(&bound) => {
                    bound.insert(*var);
                }
                Literal::Gen { pat, src } if vars(src).is_subset(&bound) => bound.extend(vars(pat)),
                _ => {}
            }
        }
        if old == bound.len() {
            break;
        }
    }
    bound
}
/// The identifiers an IR fragment references, of the kinds function checks need.
#[derive(Default)]
struct Mentions {
    fns: BTreeSet<FnId>,
    vars: BTreeSet<VarId>,
}
impl Mapper for Mentions {
    fn fnid(&mut self, id: FnId) -> FnId {
        self.fns.insert(id);
        id
    }
    fn varid(&mut self, id: VarId) -> VarId {
        self.vars.insert(id);
        id
    }
}

/// A function with an IR body (LANGUAGE §16.1): its first variables are its parameters, its variables are its own,
/// and its body has its declared result type.
fn check_function(p: &Program, f: &FnDecl, body: &Expr) -> Result<(), String> {
    let name = &f.name;
    if f.vars.len() < f.params.len() {
        return Err(format!("function {name}: fewer variables than parameters"));
    }
    for ((_, ty), var) in f.params.iter().zip(f.vars.iter()) {
        if var.ty != *ty {
            return Err(format!("function {name}: a parameter variable's type differs from the parameter's"));
        }
    }
    let mut m = Mentions::default();
    body.remap(&mut m);
    if let Some(v) = m.vars.iter().find(|v| f.vars.get(**v).is_none()) {
        return Err(format!("function {name}: unknown variable {v:?}"));
    }
    let ty = expr_type(p, Cx::function(f), body).map_err(|e| format!("function {name}: {e}"))?;
    if !assignable(p, ty, f.ret) {
        return Err(format!("function {name}: the body's type is not the declared result type"));
    }
    Ok(())
}

/// The qualified name of the built-in `Part` enum (FOREIGN-PROTOCOLS §1.1): `$` never begins a source identifier,
/// so no program's own type can take it.
pub const PART_TYPE: &str = "$builtin::Part";

/// A stream's relations (FOREIGN-PROTOCOLS §1.1): each exists with its class and schema, the connect-only ones
/// exactly for a connect stream, and each belongs to one stream.
fn check_stream(p: &Program, st: &StreamDecl) -> Result<(), String> {
    let name = &st.name;
    let ty = |d: TypeDef| p.types.lookup(&d).ok_or_else(|| format!("stream {name}: type {d:?} is not interned"));
    let conn = ty(TypeDef::Conn)?;
    let u64t = ty(TypeDef::Int(IntTy::U64))?;
    let text = ty(TypeDef::Str)?;
    let rel = |id: RelId, class: RelClass, cols: Vec<TypeId>, what: &str| -> Result<(), String> {
        let r = p.rels.get(id).ok_or_else(|| format!("stream {name}: `{what}` is not a relation"))?;
        if r.class != class {
            return Err(format!("stream {name}: `{what}` has class {:?}", r.class));
        }
        if r.placement != st.placement {
            return Err(format!("stream {name}: `{what}` is placed elsewhere than its stream"));
        }
        let have: Vec<TypeId> = r.schema.cols.iter().map(|c| c.ty).collect();
        if have != cols {
            return Err(format!("stream {name}: `{what}` has the wrong columns"));
        }
        Ok(())
    };
    let ev = |e: StreamEvent| RelClass::Event(EventSource::Stream(e));
    let instant = ty(TypeDef::Instant)?;
    let opened = match st.kind {
        StreamKind::Listen => vec![conn, text, instant],
        StreamKind::Connect => vec![conn, u64t, text, instant],
    };
    rel(st.opened, ev(StreamEvent::Opened), opened, "opened")?;
    rel(st.data, ev(StreamEvent::Data), vec![conn, u64t, ty(TypeDef::Bytes)?], "data")?;
    rel(st.closed, ev(StreamEvent::Closed), vec![conn, text], "closed")?;
    rel(st.close, RelClass::HostOut(HostOp::Close), vec![conn], "close")?;
    // `write(c, seq, parts: Vec<Part>)`: `Part` is the built-in enum whose variant 0 is `Bytes(Bytes)`.
    let w = p.rels.get(st.write).ok_or_else(|| format!("stream {name}: `write` is not a relation"))?;
    let parts = w.schema.cols.get(2).map(|c| c.ty).ok_or_else(|| format!("stream {name}: `write` has no parts"))?;
    let part_ok = match p.types.get(parts) {
        Some(TypeDef::Vec(part)) => match p.types.get(*part) {
            Some(TypeDef::Enum(e)) => {
                e.name.to_string() == PART_TYPE
                    && e.variants.iter().any(|v| {
                        v.number == 0
                            && v.payload.len() == 1
                            && v.payload.first().is_some_and(|f| p.types.get(f.ty) == Some(&TypeDef::Bytes))
                    })
            }
            _ => false,
        },
        _ => false,
    };
    if !part_ok {
        return Err(format!("stream {name}: `write`'s parts are not a Vec<Part>"));
    }
    rel(st.write, RelClass::HostOut(HostOp::Write), vec![conn, u64t, parts], "write")?;
    match (st.kind, st.failed, st.dial) {
        (StreamKind::Listen, None, None) => {}
        (StreamKind::Connect, Some(failed), Some(dial)) => {
            rel(failed, ev(StreamEvent::Failed), vec![u64t, text], "failed")?;
            rel(dial, RelClass::HostOut(HostOp::Dial), vec![u64t, text], "dial")?;
        }
        _ => return Err(format!("stream {name}: `failed` and `dial` belong exactly to connect streams")),
    }
    let mine = [Some(st.opened), Some(st.data), Some(st.closed), st.failed, Some(st.write), Some(st.close), st.dial];
    for other in p.streams.iter().filter(|o| o.name != st.name) {
        let theirs = [Some(other.opened), Some(other.data), Some(other.closed), other.failed, Some(other.write), Some(other.close), other.dial];
        if mine.iter().flatten().any(|r| theirs.iter().flatten().any(|o| o == r)) {
            return Err(format!("stream {name} shares a relation with stream {}", other.name));
        }
    }
    Ok(())
}

/// A cycle of calls among IR-bodied functions, which LANGUAGE §16.1 forbids (functions are total).
fn function_cycle(p: &Program) -> Option<String> {
    let callees: Vec<(FnId, BTreeSet<FnId>)> = p
        .fns
        .iter()
        .map(|f| match &f.body {
            FnBody::Ir(body) => {
                let mut m = Mentions::default();
                body.remap(&mut m);
                (f.id, m.fns)
            }
            _ => (f.id, BTreeSet::new()),
        })
        .collect();
    // Kahn's algorithm: a function whose callees are all done is done; what remains lies on or reaches a cycle.
    let mut done: BTreeSet<FnId> = BTreeSet::new();
    loop {
        let before = done.len();
        for (id, cs) in &callees {
            if !done.contains(id) && cs.iter().all(|c| done.contains(c)) {
                done.insert(*id);
            }
        }
        if done.len() == before {
            break;
        }
    }
    p.fns
        .iter()
        .find(|f| !done.contains(&f.id))
        .map(|f| format!("function {} is recursive (LANGUAGE §16.1)", f.name))
}

struct Bounds<'a> {
    p: &'a Program,
    errors: Vec<String>,
}
macro_rules! bounds {($($method:ident:$ty:ident=>$field:ident),*)=>{$(fn $method(&mut self,id:$ty)->$ty{if self.p.$field.get(id).is_none(){self.errors.push(format!("unknown {} {id:?}",stringify!($ty)));}id})*};}
impl Mapper for Bounds<'_> {
    fn typeid(&mut self, id: TypeId) -> TypeId {
        if self.p.types.get(id).is_none() {
            self.errors.push(format!("unknown type {id:?}"));
        }
        id
    }
    bounds!(latticetypeid:LatticeTypeId=>lattices,grouptypeid:GroupTypeId=>groups,constid:ConstId=>consts,paramid:ParamId=>params,fnid:FnId=>fns,udaid:UdaId=>udas,serviceid:ServiceId=>services,roleid:RoleId=>roles,relid:RelId=>rels,ruleid:RuleId=>rules,siteid:SiteId=>sites,constructid:ConstructId=>constructs,invariantid:InvariantId=>invariants);
}
/// Checks all protocol-program invariants, collecting independent errors.
pub(crate) fn validate(p: &Program) -> Vec<IrError> {
    let mut errors = Vec::new();
    let mut check = |ok: bool, v: u8, rule: Option<&Rule>, span: Option<Span>, detail: String| {
        if !ok {
            errors.push(IrError::validation(
                v,
                rule.map(|r| r.id),
                span.or_else(|| rule.map(|r| r.span)),
                detail,
            ));
        }
    };
    // Typed reference traversal covers every construct and expression variant.
    let mut refs = Bounds { p, errors: Vec::new() };
    p.remap(&mut refs);
    for (_, def) in p.types.iter() {
        def.remap(&mut refs);
    }
    for detail in refs.errors {
        check(false, 8, None, None, detail);
    }
    for (id, role) in p.roles.iter_enumerated() {
        check(id == role.id, 8, None, None, "role id is not its table position".into());
    }
    for (id, lat) in p.lattices.iter_enumerated() {
        check(
            id == lat.id,
            8,
            None,
            None,
            "lattice id is not its table position".into(),
        );
    }
    for (id, group) in p.groups.iter_enumerated() {
        check(
            id == group.id,
            8,
            None,
            None,
            "group id is not its table position".into(),
        );
    }
    for (id, f) in p.fns.iter_enumerated() {
        check(
            id == f.id,
            8,
            None,
            None,
            "function id is not its table position".into(),
        );
        if let FnBody::Ir(body) = &f.body {
            let result = check_function(p, f, body);
            check(result.is_ok(), 8, None, None, result.err().unwrap_or_default());
        }
    }
    for st in &p.streams {
        let result = check_stream(p, st);
        check(result.is_ok(), 8, None, None, result.err().unwrap_or_default());
    }
    let recursion = function_cycle(p);
    check(recursion.is_none(), 8, None, None, recursion.unwrap_or_default());
    for (id, param) in p.params.iter_enumerated() {
        check(
            id == param.id,
            8,
            None,
            Some(param.span),
            "parameter id is not its table position".into(),
        );
    }
    for (id, uda) in p.udas.iter_enumerated() {
        check(
            id == uda.id,
            8,
            None,
            None,
            "aggregate id is not its table position".into(),
        );
    }
    for (id, svc) in p.services.iter_enumerated() {
        check(
            id == svc.id,
            8,
            None,
            None,
            "service id is not its table position".into(),
        );
    }
    for (id, inv) in p.invariants.iter_enumerated() {
        check(
            id == inv.id,
            8,
            None,
            Some(inv.span),
            "invariant id is not its table position".into(),
        );
    }
    for (id, r) in p.rels.iter_enumerated() {
        check(
            id == r.id,
            8,
            None,
            Some(r.span),
            "relation id is not its table position".into(),
        );
        let n = r.schema.cols.len();
        let mut columns = BTreeSet::new();
        let mut valid = true;
        for col in r
            .schema
            .key
            .iter()
            .chain(&r.schema.payload)
            .chain(r.schema.lattice.iter().map(|(c, _)| c))
        {
            valid &= col.index() < n && columns.insert(*col);
        }
        valid &= columns.len() == n;
        for (col, lat) in &r.schema.lattice {
            valid &= matches!(r.schema.cols.get(col.index()).and_then(|c|p.types.get(c.ty)),Some(TypeDef::Lattice(l)) if l==lat);
        }
        // A generated relation holds valuations: a lattice value in it is plain data (compared exactly, never merged).
        if !matches!(r.origin, Origin::Generated { .. }) {
            for col in r.schema.key.iter().chain(&r.schema.payload) {
                valid &= !matches!(
                    r.schema.cols.get(col.index()).and_then(|c| p.types.get(c.ty)),
                    Some(TypeDef::Lattice(_))
                );
            }
        }
        check(
            valid,
            4,
            None,
            Some(r.span),
            format!("{}: key, payload and lattice columns must partition the schema", r.name),
        );
        if let RelClass::Channel(c) = &r.class {
            check(
                matches!(
                    r.schema.cols.first().and_then(|c| p.types.get(c.ty)),
                    Some(TypeDef::Node(_) | TypeDef::Session)
                ),
                4,
                None,
                Some(r.span),
                "channel destination must be Node or Session".into(),
            );
            if let ChannelForm::Direction { dst, .. } = c.form {
                // A channel to an external role replies to sessions (LANGUAGE §18.4).
                let external = p.roles.get(dst).is_some_and(|r| r.kind == RoleKind::External);
                check(
                    match r.schema.cols.first().and_then(|c| p.types.get(c.ty)) {
                        Some(TypeDef::Session) => external,
                        Some(TypeDef::Node(None)) => !external,
                        Some(TypeDef::Node(Some(role))) => *role == dst && !external,
                        _ => false,
                    },
                    4,
                    None,
                    Some(r.span),
                    "direction channel destination has the wrong role".into(),
                );
            }
            check(
                c.wrapper.is_some()
                    || !r
                        .schema
                        .cols
                        .iter()
                        .any(|c| matches!(p.types.get(c.ty), Some(TypeDef::Group(_)))),
                9,
                None,
                Some(r.span),
                "group payload on an unwrapped channel".into(),
            );
        }
        match r.origin {
            Origin::User(_) => check(
                !r.name.to_string().contains('$') && r.name.to_string() != "violation",
                6,
                None,
                Some(r.span),
                "user relation name contains $".into(),
            ),
            Origin::Generated { construct } => {
                check(
                    r.name.to_string().contains('$'),
                    6,
                    None,
                    Some(r.span),
                    "generated relation name must contain $".into(),
                );
                let owners = p.constructs.iter().filter(|c| c.rels.contains(&id)).count();
                check(
                    owners == 1 && p.constructs.get(construct).is_some_and(|c| c.rels.contains(&id)),
                    6,
                    None,
                    Some(r.span),
                    "generated relation must have exactly one matching owner".into(),
                );
            }
        }
        match &r.persistence {
            Persistence::Frame { rule, del } => check(
                p.rules
                    .get(*rule)
                    .is_some_and(|rule| persist_exact(p, r, rule, *del, false)),
                5,
                None,
                Some(r.span),
                "frame rule is not the exact persistence expansion".into(),
            ),
            Persistence::Identity { rule } => check(
                p.rules
                    .get(*rule)
                    .is_some_and(|rule| persist_exact(p, r, rule, None, true)),
                5,
                None,
                Some(r.span),
                "identity rule is not the exact persistence expansion".into(),
            ),
            Persistence::Resolved { construct } => check(
                matches!(p.constructs.get(*construct).map(|c|&c.kind),Some(ConstructKind::Resolve(s)) if s.rel==id),
                5,
                None,
                Some(r.span),
                "resolved persistence does not name its resolve construct".into(),
            ),
            Persistence::Soft { construct } => check(
                matches!(p.constructs.get(*construct).map(|c|&c.kind),Some(ConstructKind::SoftTable(s)) if s.rel==id),
                5,
                None,
                Some(r.span),
                "soft persistence does not name its soft construct".into(),
            ),
            Persistence::None => {}
        }
    }
    let mut labels = BTreeSet::new();
    for (id, rule) in p.rules.iter_enumerated() {
        check(
            id == rule.id,
            8,
            Some(rule),
            None,
            "rule id is not its table position".into(),
        );
        check(
            labels.insert(rule.label.text.clone()) && RuleLabel::new(rule.label.text.clone()).hash == rule.label.hash,
            10,
            Some(rule),
            None,
            "rule label is duplicate or has an inconsistent hash".into(),
        );
        let bound = bound_vars(rule);
        let all = vars(&rule.head)
            .into_iter()
            .chain(rule.body.lits.iter().flat_map(vars))
            .collect::<BTreeSet<_>>();
        check(
            all.is_subset(&bound),
            1,
            Some(rule),
            None,
            "unbound head, negation, filter, destination or weight variable".into(),
        );
        check(
            all.iter().all(|v| rule.body.vars.get(*v).is_some()),
            8,
            Some(rule),
            None,
            "variable id outside the rule namespace".into(),
        );
        if let Some(rel) = p.rels.get(rule.head.rel) {
            let legal = match rule.kind {
                RuleKind::Async => matches!(rel.class, RelClass::Channel(_) | RelClass::HostOut(_)),
                RuleKind::Deductive | RuleKind::Inductive => matches!(rel.class, RelClass::Idb | RelClass::Weighted(_)),
            };
            let mode = match rule.head.mode {
                HeadMode::ZAdd { .. } => matches!(rel.class, RelClass::Weighted(_)),
                HeadMode::Insert => !matches!(rel.class, RelClass::Weighted(_)),
                HeadMode::Violation { .. } => {
                    rel.name.to_string().ends_with("$violation") && matches!(rel.origin, Origin::Generated { .. })
                }
            };
            check(
                legal && mode,
                2,
                Some(rule),
                None,
                "head kind, mode and relation class disagree".into(),
            );
            check(
                rule.head.args.len() == rel.schema.cols.len(),
                11,
                Some(rule),
                None,
                "head arity differs from its schema".into(),
            );
            for (arg, col) in rule.head.args.iter().zip(&rel.schema.cols) {
                let compatible = match arg {
                    HeadArg::Term(t) => !matches!(t, Term::Wild) && term_type(p, Cx::rule(rule), t, col.ty),
                    HeadArg::Agg(a) => agg_type(p, Cx::rule(rule), a, col.ty),
                };
                check(
                    compatible,
                    8,
                    Some(rule),
                    None,
                    "head argument type differs from its column".into(),
                );
            }
            if let HeadMode::ZAdd { weight } = &rule.head.mode {
                check(
                    !matches!(weight, Term::Wild) && term_matches(p, Cx::rule(rule), weight, &TypeDef::Int(IntTy::I64)),
                    8,
                    Some(rule),
                    None,
                    "weight must be i64".into(),
                );
            }
        }
        let mut lattice_vars = BTreeSet::new();
        let mut occurrences: BTreeMap<VarId, usize> = BTreeMap::new();
        for atom in atoms(rule) {
            check(
                atom.spec.is_none(),
                3,
                Some(rule),
                Some(atom.span),
                "protocol body carries a location/time annotation".into(),
            );
            check(
                atom.spec.is_none(),
                12,
                Some(rule),
                Some(atom.span),
                "SpecAt is legal only in a spec program".into(),
            );
            if let Some(rel) = p.rels.get(atom.rel) {
                let local = match rel.placement {
                    Placement::Shared => true,
                    Placement::Role(role) => rule.role == Some(role),
                } && match &rel.class {
                    RelClass::Channel(channel) => match channel.form {
                        ChannelForm::Direction { dst, .. } => rule.role.is_none_or(|role| role == dst),
                        _ => true,
                    },
                    _ => true,
                };
                check(
                    local,
                    3,
                    Some(rule),
                    Some(atom.span),
                    "body reads another role's local relation".into(),
                );
                check(
                    atom.args.len() == rel.schema.cols.len(),
                    11,
                    Some(rule),
                    Some(atom.span),
                    "atom arity differs from its schema".into(),
                );
                for (t, c) in atom.args.iter().zip(&rel.schema.cols) {
                    check(
                        term_type(p, Cx::rule(rule), t, c.ty),
                        8,
                        Some(rule),
                        Some(atom.span),
                        "atom argument type differs from its column".into(),
                    );
                    if let Term::Var(v) = t {
                        *occurrences.entry(*v).or_default() += 1;
                    }
                }
                for (col, _) in &rel.schema.lattice {
                    if let Some(Term::Var(v)) = atom.args.get(col.index()) {
                        lattice_vars.insert(*v);
                    }
                }
                check(
                    !matches!(rel.class, RelClass::HostOut(_)),
                    2,
                    Some(rule),
                    Some(atom.span),
                    "a request to the host (a stream's write, close or dial) is never read".into(),
                );
                check(
                    (atom.sender.is_none() && atom.principal.is_none()) || matches!(rel.class, RelClass::Channel(_)),
                    2,
                    Some(rule),
                    Some(atom.span),
                    "sender/principal bindings require a channel".into(),
                );
                if let Some(t) = &atom.sender {
                    check(
                        term_matches(p, Cx::rule(rule), t, &TypeDef::Node(None)) || term_matches(p, Cx::rule(rule), t, &TypeDef::Session),
                        8,
                        Some(rule),
                        Some(atom.span),
                        "sender is not a node or session".into(),
                    );
                }
                if let Some(t) = &atom.principal {
                    check(
                        term_matches(p, Cx::rule(rule), t, &TypeDef::Principal),
                        8,
                        Some(rule),
                        Some(atom.span),
                        "principal is not Principal".into(),
                    );
                }
                check(
                    atom.weight.is_none() || matches!(rel.class, RelClass::Weighted(_)),
                    2,
                    Some(rule),
                    Some(atom.span),
                    "weight binding requires a weighted relation".into(),
                );
                if let Some(t) = &atom.weight {
                    check(
                        term_matches(p, Cx::rule(rule), t, &TypeDef::Int(IntTy::I64)),
                        8,
                        Some(rule),
                        Some(atom.span),
                        "weight is not i64".into(),
                    );
                }
            }
        }
        check(
            lattice_vars.iter().all(|v| occurrences.get(v).is_some_and(|n| *n == 1)),
            4,
            Some(rule),
            None,
            "a lattice column is used as a join key".into(),
        );
        for lit in &rule.body.lits {
            let result = check_literal(p, Cx::rule(rule), lit);
            check(result.is_ok(), 8, Some(rule), None, result.err().unwrap_or_default());
        }
        let owners = p.constructs.iter().filter(|c| c.rules.contains(&id)).count();
        check(
            match rule.construct {
                Some(c) => owners == 1 && p.constructs.get(c).is_some_and(|c| c.rules.contains(&id)),
                None => owners == 0,
            },
            5,
            Some(rule),
            None,
            "rule construct ownership is inconsistent".into(),
        );
        let mut refs = Refs::default();
        rule.remap(&mut refs);
        check(
            refs.sites.iter().all(|s| {
                p.sites
                    .get(*s)
                    .is_some_and(|s| !s.stable.is_empty() && s.key == RuleLabel::new(s.stable.clone()).hash)
            }),
            7,
            Some(rule),
            None,
            "seeded operator references an invalid stable site".into(),
        );
    }
    for (id, c) in p.constructs.iter_enumerated() {
        check(
            id == c.id,
            5,
            None,
            Some(c.surface.span),
            "construct id differs from its table position".into(),
        );
        check(
            c.rules.iter().collect::<BTreeSet<_>>().len() == c.rules.len()
                && c.rels.iter().collect::<BTreeSet<_>>().len() == c.rels.len(),
            5,
            None,
            Some(c.surface.span),
            "duplicate construct member".into(),
        );
        check(
            c.rules
                .iter()
                .all(|r| p.rules.get(*r).is_some_and(|r| r.construct == Some(id)))
                && c.rels.iter().all(|r| {
                    p.rels.get(*r).is_some_and(|r| {
                        r.origin == Origin::Generated { construct: id } && r.name.to_string().contains('$')
                    })
                }),
            5,
            None,
            Some(c.surface.span),
            "construct members have inconsistent ownership".into(),
        );
        check(
            !matches!(c.kind, ConstructKind::Quorum(_) | ConstructKind::SpecOracle),
            12,
            None,
            Some(c.surface.span),
            "spec-only construct in a protocol program".into(),
        );
    }
    let mut stable = BTreeSet::new();
    for (id, s) in p.sites.iter_enumerated() {
        check(
            s.id == id && stable.insert(s.stable.clone()),
            10,
            None,
            None,
            "duplicate stable site or inconsistent site id".into(),
        );
        check(
            !s.stable.is_empty()
                && s.key == RuleLabel::new(s.stable.clone()).hash
                && p.constructs.get(s.construct).is_some(),
            7,
            None,
            None,
            "site is not a valid label-derived domain separator".into(),
        );
    }
    for f in &p.facts {
        if let Some(rel) = p.rels.get(f.rel) {
            check(
                matches!(rel.class, RelClass::Static),
                2,
                None,
                Some(f.span),
                "fact targets a non-static relation".into(),
            );
            check(
                f.row.len() == rel.schema.cols.len(),
                11,
                None,
                Some(f.span),
                "fact arity differs from its schema".into(),
            );
            check(
                f.row
                    .iter()
                    .zip(&rel.schema.cols)
                    .all(|(id, c)| p.consts.get(*id).is_some_and(|v| p.types.check_value(c.ty, v).is_ok())),
                8,
                None,
                Some(f.span),
                "fact value differs from its column type".into(),
            );
        }
    }
    errors
}
fn persist_exact(p: &Program, rel: &RelDecl, rule: &Rule, del: Option<RelId>, identity: bool) -> bool {
    if rule.kind != RuleKind::Inductive
        || rule.head.rel != rel.id
        || !matches!(rule.head.mode, HeadMode::Insert)
        || rule.head.args.len() != rel.schema.cols.len()
    {
        return false;
    }
    let terms = rule
        .head
        .args
        .iter()
        .map(|a| match a {
            HeadArg::Term(t) => Some(t),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(terms) = terms else { return false };
    let ids = terms
        .iter()
        .map(|t| match t {
            Term::Var(v) => Some(*v),
            _ => None,
        })
        .collect::<Option<BTreeSet<_>>>();
    if ids.as_ref().is_none_or(|ids| ids.len() != terms.len()) {
        return false;
    }
    let args = terms.into_iter().cloned().collect::<Vec<_>>();
    let expected = if del.is_some() { 2 } else { 1 };
    if rule.body.lits.len() != expected {
        return false;
    }
    let positive=rule.body.lits.iter().filter(|l|matches!(l,Literal::Pos(a) if a.rel==rel.id&&a.args==args&&a.sender.is_none()&&a.principal.is_none()&&a.weight.is_none()&&a.spec.is_none())).count()==1;
    let negative=del.is_none_or(|del|rule.body.lits.iter().any(|l|matches!(l,Literal::Neg(a) if a.rel==del&&a.args==args&&a.sender.is_none()&&a.principal.is_none()&&a.weight.is_none()&&a.spec.is_none())));
    let construct = rule.construct.and_then(|id| p.constructs.get(id));
    let tag = if identity {
        matches!(construct.map(|c|&c.kind),Some(ConstructKind::Identity{rel:r}) if *r==rel.id)
    } else {
        matches!(construct.map(|c|&c.kind),Some(ConstructKind::Persist{rel:r,del:d}) if *r==rel.id&&*d==del)
    };
    positive && negative && tag
}
/// Whether a value of type `actual` may stand where `expected` is required: equal types, or `Node<R>` where `Node`
/// is expected (LANGUAGE §5.3: `Node<R>` is a subtype of `Node`).
/// Whether a value of type `actual` may stand where `expected` is required: equal types, or `Node<R>` where `Node`
/// is expected, also inside tuples, options and collections (values are immutable, so covariance is sound).
fn assignable(p: &Program, actual: TypeId, expected: TypeId) -> bool {
    if actual == expected {
        return true;
    }
    match (p.types.get(actual), p.types.get(expected)) {
        (Some(TypeDef::Node(Some(_))), Some(TypeDef::Node(None))) => true,
        (Some(TypeDef::Tuple(a)), Some(TypeDef::Tuple(b))) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| assignable(p, *x, *y))
        }
        (Some(TypeDef::Option(a)), Some(TypeDef::Option(b)))
        | (Some(TypeDef::Vec(a)), Some(TypeDef::Vec(b)))
        | (Some(TypeDef::Set(a)), Some(TypeDef::Set(b))) => assignable(p, *a, *b),
        (Some(TypeDef::Map(ka, va)), Some(TypeDef::Map(kb, vb))) => assignable(p, *ka, *kb) && assignable(p, *va, *vb),
        _ => false,
    }
}
/// The least type both `a` and `b` are assignable to (LANGUAGE §5.3): what a merge of the two holds (`if` branches,
/// `match` arms, a collection's elements, `push`, a fold's accumulator). `None` when they differ in more than roles,
/// or when the frontend did not intern the join (it interns every join it types).
fn join(p: &Program, a: TypeId, b: TypeId) -> Option<TypeId> {
    if assignable(p, a, b) {
        return Some(b);
    }
    if assignable(p, b, a) {
        return Some(a);
    }
    let def = match (p.types.get(a)?, p.types.get(b)?) {
        (TypeDef::Node(_), TypeDef::Node(_)) => TypeDef::Node(None),
        (TypeDef::Tuple(xs), TypeDef::Tuple(ys)) if xs.len() == ys.len() => {
            let mut out = Vec::new();
            for (x, y) in xs.iter().zip(ys) {
                out.push(join(p, *x, *y)?);
            }
            TypeDef::Tuple(out)
        }
        (TypeDef::Option(x), TypeDef::Option(y)) => TypeDef::Option(join(p, *x, *y)?),
        (TypeDef::Vec(x), TypeDef::Vec(y)) => TypeDef::Vec(join(p, *x, *y)?),
        (TypeDef::Set(x), TypeDef::Set(y)) => TypeDef::Set(join(p, *x, *y)?),
        (TypeDef::Map(k1, v1), TypeDef::Map(k2, v2)) => TypeDef::Map(join(p, *k1, *k2)?, join(p, *v1, *v2)?),
        _ => return None,
    };
    p.types.lookup(&def)
}
/// Whether `a` and `b` are the same type but for the roles of their `Node`s.
fn same_but_roles(p: &Program, a: TypeId, b: TypeId) -> bool {
    if a == b {
        return true;
    }
    match (p.types.get(a), p.types.get(b)) {
        (Some(TypeDef::Node(_)), Some(TypeDef::Node(_))) => true,
        (Some(TypeDef::Tuple(xs)), Some(TypeDef::Tuple(ys))) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same_but_roles(p, *x, *y))
        }
        (Some(TypeDef::Option(x)), Some(TypeDef::Option(y)))
        | (Some(TypeDef::Vec(x)), Some(TypeDef::Vec(y)))
        | (Some(TypeDef::Set(x)), Some(TypeDef::Set(y))) => same_but_roles(p, *x, *y),
        (Some(TypeDef::Map(k1, v1)), Some(TypeDef::Map(k2, v2))) => {
            same_but_roles(p, *k1, *k2) && same_but_roles(p, *v1, *v2)
        }
        _ => false,
    }
}
fn term_type(p: &Program, r: Cx<'_>, t: &Term, ty: TypeId) -> bool {
    match t {
        Term::Var(id) => r.vars.get(*id).is_some_and(|v| assignable(p, v.ty, ty)),
        Term::Const(id) => p.consts.get(*id).is_some_and(|v| p.types.check_value(ty, v).is_ok()),
        Term::Wild => true,
    }
}
fn term_matches(p: &Program, r: Cx<'_>, t: &Term, ty: &TypeDef) -> bool {
    match t {
        Term::Var(id) => r
            .vars
            .get(*id)
            .and_then(|v| p.types.get(v.ty))
            .is_some_and(|v| v == ty || matches!((v, ty), (TypeDef::Node(_), TypeDef::Node(None)))),
        Term::Const(id) => p
            .types
            .lookup(ty)
            .is_some_and(|ty| term_type(p, r, &Term::Const(*id), ty)),
        Term::Wild => true,
    }
}
fn agg_type(p: &Program, r: Cx<'_>, a: &AggCall, ty: TypeId) -> bool {
    let is_numeric = |arg: &Term| {
        !matches!(arg, Term::Wild)
            && p.types
                .iter()
                .any(|(id, def)| matches!(def, TypeDef::Int(_) | TypeDef::F64) && term_type(p, r, arg, id))
    };
    let is_f64 = |arg: &Term| {
        !matches!(arg, Term::Wild) && p.types.lookup(&TypeDef::F64).is_some_and(|id| term_type(p, r, arg, id))
    };
    let ola_result = || matches!(p.types.get(ty), Some(TypeDef::Tuple(fields)) if fields.len()==3 && fields.iter().all(|field| matches!(p.types.get(*field),Some(TypeDef::F64))));
    match &a.func {
        // A count may land in any integer column (checked on overflow, BLSR004): `u64` for Blossom's `count`, `i64` for
        // Molly's `count<X>` (LANGUAGE §21.1).
        // The counted tuple may have any width (LANGUAGE §10.1: `count<(S, L, P)>` for `count!(*)`).
        AggFunc::Count => {
            a.args.iter().all(|arg| !matches!(arg, Term::Wild)) && matches!(p.types.get(ty), Some(TypeDef::Int(_)))
        }
        // `sum` adds its first argument once per distinct argument tuple: the rest name the valuation it varies
        // over (LANGUAGE §10.1: `sum!(n)` adds `n` once per distinct valuation).
        AggFunc::Sum => {
            a.args.first().is_some_and(|t| term_type(p, r, t, ty))
                && a.args.iter().all(|arg| !matches!(arg, Term::Wild))
        }
        AggFunc::OlaCount => (1..=2).contains(&a.args.len()) && ola_result() && a.args.first().is_some_and(is_f64),
        AggFunc::BoolAnd | AggFunc::BoolOr => {
            a.args.len() == 1
                && matches!(p.types.get(ty), Some(TypeDef::Bool))
                && a.args.first().is_some_and(|arg| term_type(p, r, arg, ty))
        }
        AggFunc::Avg => {
            a.args.len() == 1 && matches!(p.types.get(ty), Some(TypeDef::F64)) && a.args.first().is_some_and(is_numeric)
        }
        AggFunc::OlaSum | AggFunc::OlaAvg => {
            (2..=3).contains(&a.args.len())
                && ola_result()
                && a.args.first().is_some_and(is_numeric)
                && a.args.get(1).is_some_and(is_f64)
        }
        AggFunc::Uda(id) => p
            .udas
            .get(*id)
            .and_then(|u| p.fns.get(u.finish))
            .is_some_and(|f| f.ret == ty && a.args.len() == 1),
        // `collect` holds its first argument once per distinct argument tuple: the rest name the valuation, as for
        // `sum`.
        AggFunc::CollectVec => {
            matches!(p.types.get(ty), Some(TypeDef::Vec(t)) if a.args.first().is_some_and(|arg| term_type(p, r, arg, *t)))
                && a.args.iter().all(|arg| !matches!(arg, Term::Wild))
        }
        AggFunc::CollectSet => {
            matches!(p.types.get(ty),Some(TypeDef::Set(t)) if a.args.len()==1&&a.args.first().is_some_and(|arg|term_type(p,r,arg,*t)))
        }
        AggFunc::CollectMap => {
            matches!(p.types.get(ty),Some(TypeDef::Map(k,v)) if a.args.len()==2&&a.args.first().is_some_and(|arg|term_type(p,r,arg,*k))&&a.args.get(1).is_some_and(|arg|term_type(p,r,arg,*v)))
        }
        _ => a.args.len() == 1 && a.args.first().is_some_and(|t| term_type(p, r, t, ty)),
    }
}
fn check_literal(p: &Program, r: Cx<'_>, l: &Literal) -> Result<(), String> {
    match l {
        Literal::Bind { pat, expr } => {
            if let Pattern::Var(id) = pat {
                let expected = r.vars.get(*id).ok_or("unknown binding variable")?.ty;
                if expr_matches_type(p, r, expr, expected) {
                    return Ok(());
                }
                // A rule variable's role may be narrower than the value bound to it: the body is a conjunction, and
                // another literal (an atom, an `==`) narrows it (LANGUAGE §5.3). The shapes must agree.
                if expr_type(p, r, expr).is_ok_and(|ty| same_but_roles(p, ty, expected)) {
                    return Ok(());
                }
            }
            let ty = expr_type(p, r, expr)?;
            pattern_type(p, r, pat, ty)
        }
        Literal::Guard(e) => {
            let ty = expr_type(p, r, e)?;
            if matches!(p.types.get(ty), Some(TypeDef::Bool)) {
                Ok(())
            } else {
                Err("guard is not bool".into())
            }
        }
        Literal::Lookup { var, rel, key } => {
            let decl = p.rels.get(*rel).ok_or("lookup names unknown relation")?;
            if !decl.schema.lattice.is_empty() && decl.schema.lattice.len() == 1 && key.len() == decl.schema.key.len() {
                for (term, col) in key.iter().zip(&decl.schema.key) {
                    let column = decl.schema.cols.get(col.index()).ok_or("missing key column")?;
                    if !term_type(p, r, term, column.ty) {
                        return Err("lookup key type mismatch".into());
                    }
                }
                let valcol = decl.schema.lattice.first().ok_or("missing lattice column")?.0;
                let col = decl.schema.cols.get(valcol.index()).ok_or("missing lattice column")?;
                if r.vars.get(*var).is_some_and(|v| v.ty == col.ty) {
                    Ok(())
                } else {
                    Err("lookup output type mismatch".into())
                }
            } else {
                Err("lookup requires a single lattice cell and the full key".into())
            }
        }
        Literal::Gen { pat, src } => match src {
            GenSource::Value(e) => {
                let ty = expr_type(p, r, e)?;
                let inner = match p.types.get(ty) {
                    Some(TypeDef::Vec(t) | TypeDef::Set(t)) => *t,
                    Some(TypeDef::Map(k, v)) => p
                        .types
                        .lookup(&TypeDef::Tuple(vec![*k, *v]))
                        .ok_or("map pair type not interned")?,
                    _ => return Err("generator source is not a collection".into()),
                };
                pattern_type(p, r, pat, inner)
            }
            GenSource::Lattice(e) => {
                expr_type(p, r, e)?;
                Ok(())
            }
            GenSource::TableFn { f, inputs } => {
                let f = p.fns.get(*f).ok_or("unknown table function")?;
                let FnBody::TableFn { outputs, .. } = &f.body else {
                    return Err("generator requires table function".into());
                };
                if inputs.len() != f.params.len() {
                    return Err("table function arity".into());
                }
                for (term, (_, ty)) in inputs.iter().zip(&f.params) {
                    if !term_type(p, r, term, *ty) {
                        return Err("table function argument type".into());
                    }
                }
                let ty = if outputs.len() == 1 {
                    outputs.first().ok_or("missing table output")?.1
                } else {
                    p.types
                        .lookup(&TypeDef::Tuple(outputs.iter().map(|(_, ty)| *ty).collect()))
                        .ok_or("table output tuple type not interned")?
                };
                pattern_type(p, r, pat, ty)
            }
            GenSource::Range { lo, hi, .. } => {
                let a = expr_type(p, r, lo)?;
                let b = expr_type(p, r, hi)?;
                if a != b || !matches!(p.types.get(a), Some(TypeDef::Int(_) | TypeDef::Mod { .. })) {
                    return Err("range bounds must agree and be integral".into());
                }
                pattern_type(p, r, pat, a)
            }
        },
        Literal::Pos(_) | Literal::Neg(_) => Ok(()),
    }
}
fn pattern_type(p: &Program, r: Cx<'_>, pat: &Pattern, ty: TypeId) -> Result<(), String> {
    match pat {
        Pattern::Var(v) => {
            if r
                .vars
                .get(*v)
                .is_some_and(|x| x.ty == ty || assignable(p, ty, x.ty))
            {
                Ok(())
            } else {
                Err("pattern variable type mismatch".into())
            }
        }
        Pattern::Wild => Ok(()),
        Pattern::Const(c) => {
            if p.consts.get(*c).is_some_and(|v| p.types.check_value(ty, v).is_ok()) {
                Ok(())
            } else {
                Err("pattern constant type mismatch".into())
            }
        }
        Pattern::Tuple(parts) => {
            let Some(TypeDef::Tuple(ts)) = p.types.get(ty) else {
                return Err("tuple pattern needs tuple type".into());
            };
            if parts.len() != ts.len() {
                return Err("tuple pattern arity mismatch".into());
            }
            for (x, ty) in parts.iter().zip(ts) {
                pattern_type(p, r, x, *ty)?
            }
            Ok(())
        }
        Pattern::Variant { ty: t, number, fields } => {
            if *t != ty {
                return Err("variant pattern type mismatch".into());
            }
            // `Option<T>` matches as the enum `None #0 | Some(T) #1`.
            if let Some(TypeDef::Option(inner)) = p.types.get(ty) {
                return match (number, fields.as_slice()) {
                    (0, []) => Ok(()),
                    (1, [x]) => pattern_type(p, r, x, *inner),
                    _ => Err("an `Option` pattern is `None` or `Some(x)`".into()),
                };
            }
            let Some(TypeDef::Enum(e)) = p.types.get(ty) else {
                return Err("variant pattern needs enum".into());
            };
            let v = e
                .variants
                .iter()
                .find(|v| v.number == *number)
                .ok_or("unknown variant")?;
            if fields.len() != v.payload.len() {
                return Err("variant pattern arity mismatch".into());
            }
            for (x, f) in fields.iter().zip(&v.payload) {
                pattern_type(p, r, x, f.ty)?
            }
            Ok(())
        }
        Pattern::Struct { ty: t, fields } => {
            if *t != ty {
                return Err("struct pattern type mismatch".into());
            }
            let Some(TypeDef::Struct(s)) = p.types.get(ty) else {
                return Err("struct pattern needs struct".into());
            };
            for (i, x) in fields {
                let f = s.fields.get(*i as usize).ok_or("unknown struct field")?;
                pattern_type(p, r, x, f.ty)?
            }
            Ok(())
        }
    }
}
fn expr_matches_type(p: &Program, r: Cx<'_>, e: &Expr, expected: TypeId) -> bool {
    match e {
        Expr::Term(Term::Const(id)) => p
            .consts
            .get(*id)
            .is_some_and(|v| p.types.check_value(expected, v).is_ok()),
        _ => expr_type(p, r, e).is_ok_and(|actual| assignable(p, actual, expected)),
    }
}
fn expr_type(p: &Program, r: Cx<'_>, e: &Expr) -> Result<TypeId, String> {
    let lookup = |d: TypeDef| {
        p.types
            .lookup(&d)
            .ok_or(format!("expression needs uninerned type {d:?}"))
    };
    match e {
        Expr::Term(Term::Var(v)) => r
            .vars
            .get(*v)
            .map(|v| v.ty)
            .ok_or("unknown expression variable".into()),
        Expr::Term(Term::Const(c)) => {
            let v = p.consts.get(*c).ok_or("unknown expression constant")?;
            p.types
                .iter()
                .find(|(ty, _)| p.types.check_value(*ty, v).is_ok())
                .map(|(ty, _)| ty)
                .ok_or("constant has no declared type".into())
        }
        Expr::Term(Term::Wild) => Err("wildcard is not an expression".into()),
        Expr::Param(id) => p.params.get(*id).map(|d| d.ty).ok_or("unknown parameter".into()),
        Expr::Scalar(_) if r.in_fn => Err("a function reads no scalar (`now()`, `tick()`, `self`)".into()),
        Expr::Scalar(s) => match s {
            BuiltinScalar::Now => lookup(TypeDef::Instant),
            BuiltinScalar::Tick => lookup(TypeDef::Int(IntTy::U64)),
            // A rule placed at role R runs on R's members: its `$self` is a `Node<R>` (LANGUAGE §6.10).
            BuiltinScalar::SelfNode => match r.role.and_then(|role| p.types.lookup(&TypeDef::Node(Some(role)))) {
                Some(t) => Ok(t),
                None => lookup(TypeDef::Node(None)),
            },
            BuiltinScalar::Incarnation => lookup(TypeDef::Int(IntTy::U64)),
            BuiltinScalar::Host => lookup(TypeDef::Node(None)),
        },
        Expr::Unary { op, arg } => {
            let ty = expr_type(p, r, arg)?;
            let def = p.types.get(ty).ok_or("unknown unary type")?;
            let valid = match op {
                UnOp::Not => matches!(def, TypeDef::Bool),
                UnOp::Neg => matches!(
                    def,
                    TypeDef::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128) | TypeDef::F64
                ),
                UnOp::BitNot => matches!(def, TypeDef::Int(_) | TypeDef::Mod { .. }),
            };
            if valid {
                Ok(ty)
            } else {
                Err("unary operator type mismatch".into())
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            let a = expr_type(p, r, lhs)?;
            let b = expr_type(p, r, rhs)?;
            // Time arithmetic (LANGUAGE §5.1): `Instant - Instant` is a `Duration`, `Instant ± Duration` an `Instant`.
            match (op, p.types.get(a), p.types.get(b)) {
                (BinOp::Sub, Some(TypeDef::Instant), Some(TypeDef::Instant)) => return lookup(TypeDef::Duration),
                (BinOp::Add | BinOp::Sub, Some(TypeDef::Instant), Some(TypeDef::Duration)) => return Ok(a),
                (BinOp::Add, Some(TypeDef::Duration), Some(TypeDef::Instant)) => return Ok(b),
                _ => {}
            }
            let a = if a != b && expr_matches_type(p, r, lhs, b) {
                b
            } else {
                a
            };
            if a != b && !expr_matches_type(p, r, rhs, a) {
                return Err("binary operand type mismatch".into());
            }
            let def = p.types.get(a).ok_or("unknown binary type")?;
            match op {
                BinOp::Eq | BinOp::Ne | BinOp::CanonLt | BinOp::CanonLe => lookup(TypeDef::Bool),
                // The numeric orders are for scalars of one kind; any other value compares by `CanonLt`/`CanonLe`.
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                    if matches!(
                        def,
                        TypeDef::Int(_)
                            | TypeDef::Duration
                            | TypeDef::Instant
                            | TypeDef::Str
                            | TypeDef::Bytes
                            | TypeDef::Node(_)
                    ) =>
                {
                    lookup(TypeDef::Bool)
                }
                BinOp::And | BinOp::Or if matches!(def, TypeDef::Bool) => Ok(a),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
                    if matches!(
                        def,
                        TypeDef::Int(_) | TypeDef::F64 | TypeDef::Duration | TypeDef::Mod { .. }
                    ) =>
                {
                    Ok(a)
                }
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr
                    if matches!(def, TypeDef::Int(_) | TypeDef::Mod { .. }) =>
                {
                    Ok(a)
                }
                _ => Err("binary operator type mismatch".into()),
            }
        }
        Expr::Call { f, args } => match f {
            FnRef::Fn(id) => {
                let decl = p.fns.get(*id).ok_or("unknown function")?;
                if args.len() != decl.params.len() {
                    return Err("function arity mismatch".into());
                }
                for (arg, (_, ty)) in args.iter().zip(&decl.params) {
                    if !expr_matches_type(p, r, arg, *ty) {
                        return Err("function argument type mismatch".into());
                    }
                }
                Ok(decl.ret)
            }
            FnRef::Builtin(builtin) => builtin_type(p, r, builtin, args),
        },
        Expr::Construct { ty, variant, fields } => {
            let def = p.types.get(*ty).ok_or("unknown constructed type")?;
            let expected = match (def, variant) {
                (TypeDef::Tuple(ts), None) => ts.clone(),
                (TypeDef::Struct(s), None) => s.fields.iter().map(|f| f.ty).collect(),
                (TypeDef::Enum(e), Some(v)) => e
                    .variants
                    .iter()
                    .find(|x| x.number == *v)
                    .ok_or("unknown constructed variant")?
                    .payload
                    .iter()
                    .map(|f| f.ty)
                    .collect(),
                (TypeDef::Option(t), Some(1)) => vec![*t],
                (TypeDef::Option(_), Some(0)) => Vec::new(),
                _ => return Err("type does not match constructor".into()),
            };
            if fields.len() != expected.len() {
                return Err("constructor arity mismatch".into());
            }
            for (x, t) in fields.iter().zip(expected) {
                if !expr_matches_type(p, r, x, t) {
                    return Err("constructor field type mismatch".into());
                }
            }
            Ok(*ty)
        }
        Expr::Field { base, index } => {
            let ty = expr_type(p, r, base)?;
            match p.types.get(ty) {
                Some(TypeDef::Tuple(ts)) => ts
                    .get(*index as usize)
                    .copied()
                    .ok_or("tuple field out of bounds".into()),
                Some(TypeDef::Struct(s)) => s
                    .fields
                    .get(*index as usize)
                    .map(|f| f.ty)
                    .ok_or("struct field out of bounds".into()),
                _ => Err("field projection requires tuple or struct".into()),
            }
        }
        Expr::If { cond, then, els } => {
            let c = expr_type(p, r, cond)?;
            if !matches!(p.types.get(c), Some(TypeDef::Bool)) {
                return Err("if condition must be bool".into());
            }
            let a = expr_type(p, r, then)?;
            let b = expr_type(p, r, els)?;
            if a == b {
                return Ok(a);
            }
            // A constant branch fits the other's type; otherwise the value is the branches' join.
            if matches!(**then, Expr::Term(Term::Const(_))) && expr_matches_type(p, r, then, b) {
                return Ok(b);
            }
            if matches!(**els, Expr::Term(Term::Const(_))) && expr_matches_type(p, r, els, a) {
                return Ok(a);
            }
            join(p, a, b).ok_or("if branch type mismatch".into())
        }
        Expr::Match { scrut, arms } => {
            let scrut_ty = expr_type(p, r, scrut)?;
            let mut types = Vec::with_capacity(arms.len());
            for (pat, guard, body) in arms {
                pattern_type(p, r, pat, scrut_ty)?;
                if let Some(g) = guard {
                    let t = expr_type(p, r, g)?;
                    if !matches!(p.types.get(t), Some(TypeDef::Bool)) {
                        return Err("match guard must be bool".into());
                    }
                }
                types.push(expr_type(p, r, body)?);
            }
            // A constant arm's type is ambiguous (an empty collection, `None`): the match has a non-constant arm's
            // type, and every arm must fit it.
            let ret = arms
                .iter()
                .zip(&types)
                .find(|((_, _, body), _)| !matches!(body, Expr::Term(Term::Const(_))))
                .or_else(|| arms.iter().zip(&types).next())
                .map(|(_, t)| *t)
                .ok_or("match has no arms")?;
            // The value is the arms' join; a constant arm only has to fit it.
            let mut ret = ret;
            for ((_, _, body), t) in arms.iter().zip(&types) {
                if *t == ret || (matches!(body, Expr::Term(Term::Const(_))) && expr_matches_type(p, r, body, ret)) {
                    continue;
                }
                ret = join(p, ret, *t).ok_or("match arm type mismatch")?;
            }
            Ok(ret)
        }
        Expr::Collection { kind, elems } => {
            let types = elems
                .iter()
                .map(|e| expr_type(p, r, e))
                .collect::<Result<Vec<_>, _>>()?;
            // The element type is the one every element is assignable to (`[self, d]` with `d: Node<R>` is a
            // `Vec<Node>`).
            let missing = if types.is_empty() {
                "empty collection requires a type annotation"
            } else {
                "collection element type mismatch"
            };
            let mut first = *types.first().ok_or(missing)?;
            for t in &types {
                first = join(p, first, *t).ok_or(missing)?;
            }
            match kind {
                CollKind::Vec => lookup(TypeDef::Vec(first)),
                CollKind::Set => lookup(TypeDef::Set(first)),
                CollKind::Map => {
                    let Some(TypeDef::Tuple(ts)) = p.types.get(first) else {
                        return Err("map entries must be pairs".into());
                    };
                    if let [k, v] = ts.as_slice() {
                        lookup(TypeDef::Map(*k, *v))
                    } else {
                        Err("map entries must be pairs".into())
                    }
                }
            }
        }
        Expr::Lattice { op, args } => {
            let def = p.lattices.get(op.lattice).ok_or("unknown lattice")?;
            let decl = def
                .ops
                .iter()
                .find(|d| d.name == op.op)
                .ok_or("unknown lattice operation")?;
            if args.len() != decl.params.len() {
                return Err("lattice operation arity".into());
            }
            for (arg, (ty, _)) in args.iter().zip(&decl.params) {
                if !assignable(p, expr_type(p, r, arg)?, *ty) {
                    return Err("lattice operation argument type".into());
                }
            }
            Ok(decl.ret)
        }
        Expr::Let { .. } if !r.in_fn => Err("a `let` outside a function body".into()),
        Expr::Let { pat, value, body } => {
            let t = expr_type(p, r, value)?;
            pattern_type(p, r, pat, t)?;
            expr_type(p, r, body)
        }
        Expr::Closure { .. } => Err("a closure outside a combinator's argument".into()),
        Expr::Typed { ty, expr } => {
            if expr_matches_type(p, r, expr, *ty) {
                Ok(*ty)
            } else {
                Err("an ascribed expression does not have its type".into())
            }
        }
    }
}

/// A closure argument's parameters (whose types must be `params`) and its body's type. Closures appear only in
/// function bodies, so their parameters are variables of the function.
fn closure_type(p: &Program, r: Cx<'_>, e: &Expr, params: &[TypeId]) -> Result<TypeId, String> {
    let Expr::Closure { params: vs, body } = e else {
        return Err("a combinator's last argument must be a closure".into());
    };
    if !r.in_fn {
        return Err("a closure outside a function body".into());
    }
    if vs.len() != params.len() {
        return Err("closure arity mismatch".into());
    }
    // A parameter may be wider than what is passed (a fold's accumulator holds the initial value and every step's).
    for (v, passed) in vs.iter().zip(params) {
        let have = r.vars.get(*v).ok_or("unknown closure parameter")?.ty;
        if !assignable(p, *passed, have) {
            return Err(format!(
                "closure parameter type mismatch: {:?} is passed where {:?} is expected",
                p.types.get(*passed),
                p.types.get(have)
            ));
        }
    }
    expr_type(p, r, body)
}

/// The type of a library call (LANGUAGE Appendix B), checking its arguments.
fn lib_type(p: &Program, r: Cx<'_>, f: LibFn, args: &[Expr]) -> Result<TypeId, String> {
    let lookup = |d: TypeDef| p.types.lookup(&d).ok_or(format!("library result type {d:?} is not interned"));
    let ty = |i: usize| -> Result<TypeId, String> {
        expr_type(p, r, args.get(i).ok_or(format!("{f:?}: missing argument {i}"))?)
    };
    let def = |t: TypeId| p.types.get(t).cloned().ok_or_else(|| "unknown type".to_string());
    let arity = |n: usize| {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{f:?} expects {n} arguments, got {}", args.len()))
        }
    };
    let elem = |t: TypeId| match def(t)? {
        TypeDef::Vec(e) => Ok(e),
        other => Err(format!("{f:?} expects a Vec, got {other:?}")),
    };
    let inner = |t: TypeId| match def(t)? {
        TypeDef::Option(e) => Ok(e),
        other => Err(format!("{f:?} expects an Option, got {other:?}")),
    };
    let u64t = || lookup(TypeDef::Int(IntTy::U64));
    let boolt = || lookup(TypeDef::Bool);
    let same = |a: TypeId, b: TypeId, what: &str| if a == b { Ok(()) } else { Err(format!("{f:?}: {what}")) };
    match f {
        LibFn::Range => {
            arity(2)?;
            let u = u64t()?;
            same(ty(0)?, u, "range bounds are u64")?;
            same(ty(1)?, u, "range bounds are u64")?;
            lookup(TypeDef::Vec(u))
        }
        LibFn::VecGet => {
            arity(2)?;
            let v = ty(0)?;
            same(ty(1)?, u64t()?, "an index is u64")?;
            lookup(TypeDef::Option(elem(v)?))
        }
        LibFn::VecFirst | LibFn::VecLast => {
            arity(1)?;
            lookup(TypeDef::Option(elem(ty(0)?)?))
        }
        LibFn::VecPush => {
            arity(2)?;
            let e = join(p, elem(ty(0)?)?, ty(1)?).ok_or(format!("{f:?}: push an element of the vector's type"))?;
            lookup(TypeDef::Vec(e))
        }
        LibFn::VecConcat => {
            arity(2)?;
            let (a, b) = (ty(0)?, ty(1)?);
            let e = join(p, elem(a)?, elem(b)?).ok_or(format!("{f:?}: concatenate vectors of one type"))?;
            lookup(TypeDef::Vec(e))
        }
        LibFn::VecIsEmpty => {
            arity(1)?;
            elem(ty(0)?)?;
            boolt()
        }
        LibFn::VecReverse => {
            arity(1)?;
            let v = ty(0)?;
            elem(v)?;
            Ok(v)
        }
        LibFn::VecEnumerate => {
            arity(1)?;
            let e = elem(ty(0)?)?;
            let pair = lookup(TypeDef::Tuple(vec![u64t()?, e]))?;
            lookup(TypeDef::Vec(pair))
        }
        LibFn::VecMap => {
            arity(2)?;
            let e = elem(ty(0)?)?;
            let out = closure_type(p, r, args.get(1).ok_or("missing closure")?, &[e])?;
            lookup(TypeDef::Vec(out))
        }
        LibFn::VecFilter | LibFn::VecAll | LibFn::VecAny => {
            arity(2)?;
            let v = ty(0)?;
            let e = elem(v)?;
            same(closure_type(p, r, args.get(1).ok_or("missing closure")?, &[e])?, boolt()?, "a predicate returns bool")?;
            if f == LibFn::VecFilter { Ok(v) } else { boolt() }
        }
        LibFn::VecFilterMap => {
            arity(2)?;
            let e = elem(ty(0)?)?;
            let out = closure_type(p, r, args.get(1).ok_or("missing closure")?, &[e])?;
            lookup(TypeDef::Vec(inner(out)?))
        }
        LibFn::VecFold => {
            arity(3)?;
            let e = elem(ty(0)?)?;
            let init = ty(1)?;
            // The accumulator is the closure's first parameter: it holds the initial value and every step's result.
            let closure = args.get(2).ok_or("missing closure")?;
            let acc = match closure {
                Expr::Closure { params, .. } => params
                    .first()
                    .and_then(|v| r.vars.get(*v))
                    .map(|v| v.ty)
                    .ok_or("a fold's closure takes the accumulator")?,
                _ => return Err("a combinator's last argument must be a closure".into()),
            };
            let step = closure_type(p, r, closure, &[init, e])?;
            if !assignable(p, step, acc) {
                return Err(format!("{f:?}: a fold step returns its accumulator"));
            }
            Ok(acc)
        }
        LibFn::DurationFromMillis => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Int(IntTy::I64))?, "from_millis of an i64")?;
            lookup(TypeDef::Duration)
        }
        LibFn::DurationAsMillis => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Duration)?, "as_millis of a Duration")?;
            lookup(TypeDef::Int(IntTy::I64))
        }
        LibFn::BlobOf => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Bytes)?, "a blob is made of Bytes")?;
            lookup(TypeDef::Blob)
        }
        LibFn::BlobRead => {
            arity(3)?;
            same(ty(0)?, lookup(TypeDef::Blob)?, "read of a Blob")?;
            same(ty(1)?, u64t()?, "positions are u64")?;
            same(ty(2)?, u64t()?, "positions are u64")?;
            lookup(TypeDef::Option(lookup(TypeDef::Bytes)?))
        }
        LibFn::OptIsSome | LibFn::OptIsNone => {
            arity(1)?;
            inner(ty(0)?)?;
            boolt()
        }
        LibFn::OptUnwrapOr => {
            arity(2)?;
            let e = inner(ty(0)?)?;
            join(p, e, ty(1)?).ok_or(format!("{f:?}: the default has the option's type"))
        }
        LibFn::OptMap => {
            arity(2)?;
            let e = inner(ty(0)?)?;
            let out = closure_type(p, r, args.get(1).ok_or("missing closure")?, &[e])?;
            lookup(TypeDef::Option(out))
        }
        LibFn::OptAndThen => {
            arity(2)?;
            let e = inner(ty(0)?)?;
            let out = closure_type(p, r, args.get(1).ok_or("missing closure")?, &[e])?;
            inner(out)?;
            Ok(out)
        }
        LibFn::BytesSlice => {
            arity(3)?;
            let b = lookup(TypeDef::Bytes)?;
            same(ty(0)?, b, "slice of Bytes")?;
            same(ty(1)?, u64t()?, "positions are u64")?;
            same(ty(2)?, u64t()?, "positions are u64")?;
            lookup(TypeDef::Option(b))
        }
        LibFn::BytesConcat => {
            arity(2)?;
            let b = lookup(TypeDef::Bytes)?;
            same(ty(0)?, b, "concat of Bytes")?;
            same(ty(1)?, b, "concat of Bytes")?;
            Ok(b)
        }
        LibFn::StrSplitWhitespace => {
            arity(1)?;
            let st = lookup(TypeDef::Str)?;
            same(ty(0)?, st, "split_whitespace of a String")?;
            lookup(TypeDef::Vec(st))
        }
        LibFn::StrToLowercase => {
            arity(1)?;
            let st = lookup(TypeDef::Str)?;
            same(ty(0)?, st, "to_lowercase of a String")?;
            Ok(st)
        }
        LibFn::StrParseI64 => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Str)?, "parse_i64 of a String")?;
            lookup(TypeDef::Option(lookup(TypeDef::Int(IntTy::I64))?))
        }
        LibFn::StrToUtf8 => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Str)?, "to_utf8 of a String")?;
            lookup(TypeDef::Bytes)
        }
        LibFn::BytesFromUtf8 => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Bytes)?, "from_utf8 of Bytes")?;
            lookup(TypeDef::Option(lookup(TypeDef::Str)?))
        }
        LibFn::BytesRead(it) | LibFn::BytesPut(it) | LibFn::BytesFrom(it) if !matches!(it.bits(), 8 | 16 | 32 | 64) => {
            Err(format!("{f:?}: byte access is for 8- to 64-bit integers"))
        }
        LibFn::BytesRead(it) => {
            arity(2)?;
            same(ty(0)?, lookup(TypeDef::Bytes)?, "reads from Bytes")?;
            same(ty(1)?, u64t()?, "a position is u64")?;
            lookup(TypeDef::Option(lookup(TypeDef::Int(it))?))
        }
        LibFn::BytesPut(it) => {
            arity(3)?;
            let b = lookup(TypeDef::Bytes)?;
            same(ty(0)?, b, "writes into Bytes")?;
            same(ty(1)?, u64t()?, "a position is u64")?;
            same(ty(2)?, lookup(TypeDef::Int(it))?, "the written integer has the method's type")?;
            lookup(TypeDef::Option(b))
        }
        LibFn::BytesFrom(it) => {
            arity(1)?;
            same(ty(0)?, lookup(TypeDef::Int(it))?, "the integer has the constructor's type")?;
            lookup(TypeDef::Bytes)
        }
        LibFn::BytesUvarintAt | LibFn::BytesVarintAt => {
            arity(2)?;
            same(ty(0)?, lookup(TypeDef::Bytes)?, "reads from Bytes")?;
            same(ty(1)?, u64t()?, "a position is u64")?;
            let v = if f == LibFn::BytesUvarintAt {
                u64t()?
            } else {
                lookup(TypeDef::Int(IntTy::I64))?
            };
            lookup(TypeDef::Option(lookup(TypeDef::Tuple(vec![v, u64t()?]))?))
        }
        LibFn::BytesUvarint | LibFn::BytesVarint => {
            arity(1)?;
            let v = if f == LibFn::BytesUvarint {
                u64t()?
            } else {
                lookup(TypeDef::Int(IntTy::I64))?
            };
            same(ty(0)?, v, "a varint's value")?;
            lookup(TypeDef::Bytes)
        }
        LibFn::BytesEmpty => {
            arity(0)?;
            lookup(TypeDef::Bytes)
        }
        LibFn::BytesJoin => {
            arity(1)?;
            let b = lookup(TypeDef::Bytes)?;
            same(ty(0)?, lookup(TypeDef::Vec(b))?, "join of a Vec<Bytes>")?;
            Ok(b)
        }
    }
}

fn builtin_type(p: &Program, r: Cx<'_>, b: &BuiltinFn, args: &[Expr]) -> Result<TypeId, String> {
    let arity = |n: usize| {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("builtin {b:?} expects {n} arguments, got {}", args.len()))
        }
    };
    let lookup = |d: TypeDef| {
        p.types
            .lookup(&d)
            .ok_or(format!("builtin return type {d:?} is not interned"))
    };
    if let BuiltinFn::Lib(f) = b {
        return lib_type(p, r, *f, args);
    }
    let types = args.iter().map(|e| expr_type(p, r, e)).collect::<Result<Vec<_>, _>>()?;
    match b {
        BuiltinFn::Lib(_) => Err("the library is typed by lib_type".into()),
        BuiltinFn::Prio { .. } | BuiltinFn::RandPrio { .. } => {
            arity(2)?;
            let choice = *types.get(1).ok_or("missing choice argument")?;
            let priority = lookup(TypeDef::Int(IntTy::U64))?;
            lookup(TypeDef::Tuple(vec![priority, choice]))
        }
        BuiltinFn::Rand => {
            if types.is_empty() {
                return Err("rand needs a stable key".into());
            }
            lookup(TypeDef::Int(IntTy::U64))
        }
        BuiltinFn::RandFloat => {
            if types.is_empty() {
                return Err("rand_float needs a stable key".into());
            }
            lookup(TypeDef::F64)
        }
        BuiltinFn::Error { ty } => {
            arity(1)?;
            if types.first() != Some(&lookup(TypeDef::Str)?) {
                return Err("error takes a String message".into());
            }
            Ok(*ty)
        }
        BuiltinFn::RandRange => {
            if types.len() < 3 {
                return Err("rand_range needs lower, upper and a stable key".into());
            }
            let lo = *types.first().ok_or("missing lower bound")?;
            if types.get(1) != Some(&lo) || !matches!(p.types.get(lo), Some(TypeDef::Int(_) | TypeDef::Duration)) {
                return Err("rand_range bounds must be the same integer or Duration type".into());
            }
            Ok(lo)
        }
        BuiltinFn::Route { role } => {
            arity(1)?;
            if p.roles.get(*role).is_none() {
                return Err("route references unknown role".into());
            }
            lookup(TypeDef::Node(Some(*role))).or_else(|_| lookup(TypeDef::Node(None)))
        }
        BuiltinFn::Majority { domain } => {
            arity(1)?;
            let member = match domain {
                MajorityDomain::Role(role) => {
                    p.roles.get(*role).ok_or("majority references unknown role")?;
                    p.types
                        .lookup(&TypeDef::Node(Some(*role)))
                        .or_else(|| p.types.lookup(&TypeDef::Node(None)))
                }
                MajorityDomain::Relation(rel) => {
                    let decl = p.rels.get(*rel).ok_or("majority references unknown relation")?;
                    if decl.schema.cols.len() != 1 || !matches!(decl.class, RelClass::Static) {
                        return Err("majority domain must be a closed unary relation".into());
                    }
                    decl.schema.cols.first().map(|col| col.ty)
                }
            }
            .ok_or("majority member type is not interned")?;
            let input = types
                .first()
                .and_then(|id| p.types.get(*id))
                .ok_or("missing majority input")?;
            let TypeDef::Lattice(lattice) = input else {
                return Err("majority expects a set-like lattice".into());
            };
            if !matches!(p.lattices.get(*lattice).map(|d| &d.ctor), Some(LatticeCtor::Set(element) | LatticeCtor::PSet(element)) if *element == member || matches!((p.types.get(*element),p.types.get(member)),(Some(TypeDef::Node(_)),Some(TypeDef::Node(_)))))
            {
                return Err("majority lattice element does not match its domain".into());
            }
            lookup(TypeDef::Bool)
        }
        BuiltinFn::ClusterVersionAtLeast(_) => {
            arity(0)?;
            lookup(TypeDef::Bool)
        }
        BuiltinFn::ZWeight { rel } | BuiltinFn::ZDelta { rel } => {
            arity(1)?;
            let decl = p.rels.get(*rel).ok_or("weighted builtin references unknown relation")?;
            if !matches!(decl.class, RelClass::Weighted(_)) {
                return Err("weighted builtin requires a weighted relation".into());
            }
            lookup(TypeDef::Int(IntTy::I64))
        }
        BuiltinFn::PrincipalOf => {
            arity(1)?;
            if !matches!(
                types.first().and_then(|t| p.types.get(*t)),
                Some(TypeDef::Node(_) | TypeDef::Session)
            ) {
                return Err("principal_of expects Node or Session".into());
            }
            lookup(TypeDef::Principal)
        }
        BuiltinFn::RoleOf => {
            arity(1)?;
            if !matches!(types.first().and_then(|t| p.types.get(*t)), Some(TypeDef::Node(_))) {
                return Err("role_of expects Node".into());
            }
            lookup(TypeDef::Str)
        }
        BuiltinFn::Size { .. } => {
            arity(0)?;
            lookup(TypeDef::Int(IntTy::U64))
        }
        BuiltinFn::IntCast(to) => {
            arity(1)?;
            if !matches!(types.first().and_then(|t| p.types.get(*t)), Some(TypeDef::Int(_))) {
                return Err("an integer cast expects an integer".into());
            }
            lookup(TypeDef::Int(*to))
        }
        BuiltinFn::Len => {
            arity(1)?;
            if !matches!(
                types.first().and_then(|t| p.types.get(*t)),
                Some(
                    TypeDef::Vec(_) | TypeDef::Set(_) | TypeDef::Map(..) | TypeDef::Str | TypeDef::Bytes | TypeDef::Blob
                )
            ) {
                return Err("len expects a collection, String, Bytes or a Blob".into());
            }
            lookup(TypeDef::Int(IntTy::U64))
        }
        BuiltinFn::Concat => {
            arity(2)?;
            let (a, b) = (
                *types.first().ok_or("missing operand")?,
                *types.get(1).ok_or("missing operand")?,
            );
            // The wider operand's type (`Vec<Node<R>> ++ Vec<Node>` is a `Vec<Node>`).
            let ty = if assignable(p, a, b) { b } else { a };
            if !assignable(p, a, ty)
                || !assignable(p, b, ty)
                || !matches!(p.types.get(ty), Some(TypeDef::Str | TypeDef::Bytes | TypeDef::Vec(_)))
            {
                return Err("concat expects two Strings, Bytes or Vecs of one type".into());
            }
            Ok(ty)
        }
        BuiltinFn::Contains => {
            arity(2)?;
            let container = types.first().and_then(|t| p.types.get(*t)).ok_or("missing container")?;
            let needle = *types.get(1).ok_or("missing needle")?;
            if !matches!(container,TypeDef::Vec(t)|TypeDef::Set(t) if *t==needle) {
                return Err("contains element type mismatch".into());
            }
            lookup(TypeDef::Bool)
        }
        BuiltinFn::Keys | BuiltinFn::Values => {
            arity(1)?;
            let map = types.first().and_then(|t| p.types.get(*t)).ok_or("missing map")?;
            let TypeDef::Map(k, v) = map else {
                return Err("keys/values expects Map".into());
            };
            lookup(TypeDef::Vec(if matches!(b, BuiltinFn::Keys) { *k } else { *v }))
        }
        BuiltinFn::ToString => {
            arity(1)?;
            lookup(TypeDef::Str)
        }
        BuiltinFn::Hash64 | BuiltinFn::Fingerprint => {
            arity(1)?;
            lookup(TypeDef::Int(IntTy::U64))
        }
        BuiltinFn::Unwrap { .. } | BuiltinFn::Entries => {
            let mut candidates = p.fns.iter().filter(|f| matches!(&f.body,FnBody::Builtin(x) if x==b));
            let decl = candidates.next().ok_or("builtin signature is not declared")?;
            if candidates.next().is_some() {
                return Err("ambiguous builtin signature".into());
            }
            if args.len() != decl.params.len() {
                return Err("builtin arity mismatch".into());
            }
            for (arg_ty, (_, param_ty)) in types.iter().zip(&decl.params) {
                if arg_ty != param_ty {
                    return Err("builtin argument type mismatch".into());
                }
            }
            Ok(decl.ret)
        }
    }
}
