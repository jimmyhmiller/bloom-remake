//! Deterministic textual form of Dedalus core IR (LANGUAGE §4.1).
use crate::core::*;
use blossom_base::idx::*;
use blossom_value::TypeDef;
use std::fmt::Write;

use blossom_value::Value;
use blossom_value::time::NodeId;
use blossom_value::value::{GroupValue, LatValue};

fn type_name(p: &Program, id: TypeId) -> String {
    match p.types.get(id) {
        None => format!("<type:{}>", id.raw()),
        Some(TypeDef::Bool) => "bool".into(),
        Some(TypeDef::Int(t)) => t.name().into(),
        Some(TypeDef::F64) => "f64".into(),
        Some(TypeDef::Str) => "String".into(),
        Some(TypeDef::Bytes) => "Bytes".into(),
        Some(TypeDef::Unit) => "()".into(),
        Some(TypeDef::Duration) => "Duration".into(),
        Some(TypeDef::Instant) => "Instant".into(),
        Some(TypeDef::Mod { bits }) => format!("Mod<{bits}>"),
        Some(TypeDef::Blob) => "Blob".into(),
        Some(TypeDef::Session) => "Session".into(),
        Some(TypeDef::Conn) => "Conn".into(),
        Some(TypeDef::Principal) => "Principal".into(),
        Some(TypeDef::Node(role)) => role
            .and_then(|r| p.roles.get(r))
            .map_or("Node".into(), |r| format!("Node<{}>", r.name)),
        Some(TypeDef::Tuple(ts)) => format!(
            "({})",
            ts.iter().map(|t| type_name(p, *t)).collect::<Vec<_>>().join(", ")
        ),
        Some(TypeDef::Struct(s)) => s.name.to_string(),
        Some(TypeDef::Enum(e)) => e.name.to_string(),
        Some(TypeDef::Vec(t)) => format!("Vec<{}>", type_name(p, *t)),
        Some(TypeDef::Set(t)) => format!("Set<{}>", type_name(p, *t)),
        Some(TypeDef::Map(k, v)) => format!("Map<{}, {}>", type_name(p, *k), type_name(p, *v)),
        Some(TypeDef::Option(t)) => format!("Option<{}>", type_name(p, *t)),
        Some(TypeDef::Lattice(id)) => p
            .lattices
            .get(*id)
            .map_or(format!("<lattice:{}>", id.raw()), |l| l.name.to_string()),
        Some(TypeDef::Group(id)) => p
            .groups
            .get(*id)
            .map_or(format!("<group:{}>", id.raw()), |g| format!("{:?}", g.ctor)),
        Some(TypeDef::Extern(x)) => x.name.to_string(),
    }
}
fn generator(p: &Program, r: &Rule, src: &GenSource) -> String {
    match src {
        GenSource::Value(e) | GenSource::Lattice(e) => expr(p, r, e),
        GenSource::TableFn { f, inputs } => format!(
            "{}({})",
            p.fns
                .get(*f)
                .map_or(format!("<fn:{}>", f.raw()), |f| f.name.to_string()),
            inputs.iter().map(|t| term(p, r, t)).collect::<Vec<_>>().join(", ")
        ),
        GenSource::Range {
            lo,
            hi,
            kind,
            ring_bits,
        } => format!(
            "{} {:?} {}{}",
            expr(p, r, lo),
            kind,
            expr(p, r, hi),
            ring_bits.map_or(String::new(), |n| format!(" in Mod<{n}>"))
        ),
    }
}

fn builtin_name(p: &Program, f: &BuiltinFn) -> String {
    match f {
        BuiltinFn::Prio { site } => format!(
            "$prio<{}>",
            p.sites.get(*site).map_or("<?>".into(), |s| s.stable.to_string())
        ),
        BuiltinFn::RandPrio { site } => format!(
            "$rprio<{}>",
            p.sites.get(*site).map_or("<?>".into(), |s| s.stable.to_string())
        ),
        BuiltinFn::Rand => "$rand".into(),
        BuiltinFn::RandFloat => "$rand_float".into(),
        BuiltinFn::RandRange => "$rand_range".into(),
        BuiltinFn::Error { .. } => "$error".into(),
        BuiltinFn::Route { role } => format!(
            "$route<{}>",
            p.roles.get(*role).map_or("<?>".into(), |r| r.name.to_string())
        ),
        BuiltinFn::Majority { domain } => format!(
            "$majority<{}>",
            match domain {
                MajorityDomain::Role(id) => p.roles.get(*id).map_or("<?>".into(), |r| r.name.to_string()),
                MajorityDomain::Relation(id) => rel(p, *id),
            }
        ),
        BuiltinFn::ClusterVersionAtLeast(n) => format!("$cluster_version_at_least<{n}>"),
        BuiltinFn::ZWeight { rel: id } => format!("$zweight<{}>", rel(p, *id)),
        BuiltinFn::ZDelta { rel: id } => format!("$zdelta<{}>", rel(p, *id)),
        BuiltinFn::Unwrap { rel: id } => format!("$unwrap<{}>", rel(p, *id)),
        BuiltinFn::Entries => "$entries".into(),
        BuiltinFn::PrincipalOf => "$principal_of".into(),
        BuiltinFn::RoleOf => "$role_of".into(),
        BuiltinFn::Size { role } => format!(
            "$size<{}>",
            p.roles.get(*role).map_or("<?>".into(), |r| r.name.to_string())
        ),
        BuiltinFn::Len => "$len".into(),
        BuiltinFn::IntCast(t) => format!("$as_{}", t.name()),
        BuiltinFn::Lib(f) => format!("$lib_{f:?}"),
        BuiltinFn::Concat => "$concat".into(),
        BuiltinFn::Contains => "$contains".into(),
        BuiltinFn::Keys => "$keys".into(),
        BuiltinFn::Values => "$values".into(),
        BuiltinFn::ToString { .. } => "$to_string".into(),
        BuiltinFn::Hash64 => "$hash64".into(),
        BuiltinFn::Fingerprint => "$fingerprint".into(),
        BuiltinFn::FloatCast => "$as_f64".into(),
    }
}
fn agg_name(p: &Program, f: &AggFunc) -> String {
    match f {
        AggFunc::Uda(id) => p
            .udas
            .get(*id)
            .and_then(|u| p.fns.get(u.finish))
            .map_or("<uda>".into(), |f| f.name.to_string()),
        _ => format!("{f:?}"),
    }
}

fn rel(p: &Program, id: RelId) -> String {
    p.rels
        .get(id)
        .map_or(format!("<rel:{}>", id.raw()), |r| r.name.to_string())
}
fn var(r: &Rule, id: VarId) -> String {
    r.body
        .vars
        .get(id)
        .map_or(format!("<var:{}>", id.raw()), |v| v.name.as_str().to_string())
}
fn term(p: &Program, r: &Rule, t: &Term) -> String {
    match t {
        Term::Var(id) => var(r, *id),
        Term::Const(id) => p
            .consts
            .get(*id)
            .map_or(format!("<const:{}>", id.raw()), |v| format!("{v:?}")),
        Term::Wild => "_".into(),
    }
}
fn pat(p: &Program, r: &Rule, x: &Pattern) -> String {
    match x {
        Pattern::Var(v) => var(r, *v),
        Pattern::Wild => "_".into(),
        Pattern::Const(c) => term(p, r, &Term::Const(*c)),
        Pattern::Tuple(v) => format!("({})", v.iter().map(|x| pat(p, r, x)).collect::<Vec<_>>().join(", ")),
        Pattern::Variant { ty, number, fields } => format!(
            "Variant<{}>#{number}({})",
            type_name(p, *ty),
            fields.iter().map(|x| pat(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Pattern::Struct { ty, fields } => format!(
            "Struct<{}>{{{}}}",
            type_name(p, *ty),
            fields
                .iter()
                .map(|(i, x)| format!("{i}: {}", pat(p, r, x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn expr(p: &Program, r: &Rule, e: &Expr) -> String {
    match e {
        Expr::Term(t) => term(p, r, t),
        Expr::Param(id) => p
            .params
            .get(*id)
            .map_or(format!("<param:{}>", id.raw()), |d| d.name.to_string()),
        Expr::Scalar(s) => format!("${s:?}"),
        Expr::Unary { op, arg } => format!("({op:?} {})", expr(p, r, arg)),
        Expr::Binary { op, lhs, rhs } => format!("({} {op:?} {})", expr(p, r, lhs), expr(p, r, rhs)),
        Expr::Call { f, args } => format!(
            "{}({})",
            match f {
                FnRef::Fn(id) => p
                    .fns
                    .get(*id)
                    .map_or(format!("<fn:{}>", id.raw()), |f| f.name.to_string()),
                FnRef::Builtin(f) => builtin_name(p, f),
            },
            args.iter().map(|x| expr(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Expr::Construct { ty, variant, fields } => format!(
            "{}{}({})",
            type_name(p, *ty),
            variant.map_or(String::new(), |n| format!("#{n}")),
            fields.iter().map(|x| expr(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Expr::Field { base, index } => format!("{}.{index}", expr(p, r, base)),
        Expr::If { cond, then, els } => format!(
            "if {} then {} else {}",
            expr(p, r, cond),
            expr(p, r, then),
            expr(p, r, els)
        ),
        Expr::Match { scrut, arms } => format!(
            "match {} {{{}}}",
            expr(p, r, scrut),
            arms.iter()
                .map(|(pat, guard, body)| format!(
                    "{}{} => {}",
                    pat_as_text(p, r, pat),
                    guard
                        .as_ref()
                        .map_or(String::new(), |g| format!(" if {}", expr(p, r, g))),
                    expr(p, r, body)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Collection { kind, elems } => format!(
            "{kind:?}{{{}}}",
            elems.iter().map(|x| expr(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Expr::Lattice { op, args } => format!(
            "{}.{}({})",
            p.lattices.get(op.lattice).map_or("<?>".into(), |l| l.name.to_string()),
            op.op,
            args.iter().map(|x| expr(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Expr::Let {
            pat: pattern,
            value,
            body,
        } => format!(
            "let {} = {}; {}",
            pat(p, r, pattern),
            expr(p, r, value),
            expr(p, r, body)
        ),
        Expr::Closure { params, body } => format!(
            "|{}| {}",
            params.iter().map(|x| var(r, *x)).collect::<Vec<_>>().join(", "),
            expr(p, r, body)
        ),
        Expr::Typed { ty, expr: e } => format!("({}: {})", expr(p, r, e), type_name(p, *ty)),
    }
}
fn pat_as_text(p: &Program, r: &Rule, x: &Pattern) -> String {
    pat(p, r, x)
}
fn atom(p: &Program, r: &Rule, a: &Atom) -> String {
    let channel = p
        .rels
        .get(a.rel)
        .is_some_and(|d| matches!(d.class, RelClass::Channel(_)));
    let args = if channel {
        a.args.get(1..).unwrap_or(&[])
    } else {
        &a.args
    };
    let mut out = format!(
        "{}({})",
        rel(p, a.rel),
        args.iter().map(|t| term(p, r, t)).collect::<Vec<_>>().join(", ")
    );
    if a.sender.is_some() || a.principal.is_some() {
        out.pop();
        out.push_str(" | ");
        if let Some(s) = &a.sender {
            out.push_str(&term(p, r, s));
        }
        if let Some(pr) = &a.principal {
            if a.sender.is_some() {
                out.push_str(", ");
            }
            out.push_str(&term(p, r, pr));
        }
        out.push(')');
    }
    if let Some(w) = &a.weight {
        write!(out, " weight {}", term(p, r, w)).ok();
    }
    out
}
fn literal(p: &Program, r: &Rule, l: &Literal) -> String {
    match l {
        Literal::Pos(a) => atom(p, r, a),
        Literal::Neg(a) => format!("notin {}", atom(p, r, a)),
        Literal::Bind { pat: x, expr: e } => format!("{} := {}", pat(p, r, x), expr(p, r, e)),
        Literal::Guard(e) => expr(p, r, e),
        Literal::Lookup { var: v, rel: id, key } => format!(
            "{} = {}[{}]",
            var(r, *v),
            rel(p, *id),
            key.iter().map(|x| term(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
        Literal::Gen { pat: x, src } => format!("{} in {}", pat(p, r, x), generator(p, r, src)),
    }
}
fn head_arg(p: &Program, r: &Rule, a: &HeadArg) -> String {
    match a {
        HeadArg::Term(t) => term(p, r, t),
        HeadArg::Agg(a) => format!(
            "{}<{}>",
            agg_name(p, &a.func),
            a.args.iter().map(|x| term(p, r, x)).collect::<Vec<_>>().join(", ")
        ),
    }
}
fn rule(p: &Program, r: &Rule) -> String {
    let dest = if r.kind == RuleKind::Async {
        r.head.args.first().map(|a| format!("@{}", head_arg(p, r, a)))
    } else {
        None
    };
    let args = if dest.is_some() {
        r.head.args.get(1..).unwrap_or(&[])
    } else {
        &r.head.args
    };
    let mut out = format!(
        "{}({}{})",
        rel(p, r.head.rel),
        dest.unwrap_or_default(),
        if args.is_empty() {
            String::new()
        } else {
            format!(
                "{}{}",
                if r.kind == RuleKind::Async { ", " } else { "" },
                args.iter().map(|a| head_arg(p, r, a)).collect::<Vec<_>>().join(", ")
            )
        }
    );
    match r.kind {
        RuleKind::Deductive => {}
        RuleKind::Inductive => out.push_str("@next"),
        RuleKind::Async => out.push_str("@async"),
    };
    if let HeadMode::ZAdd { weight } = &r.head.mode {
        write!(out, " += {}", term(p, r, weight)).ok();
    }
    if !r.body.lits.is_empty() {
        out.push_str(" :- ");
        out.push_str(
            &r.body
                .lits
                .iter()
                .map(|l| literal(p, r, l))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    out.push('.');
    out
}
/// Prints declarations and rules in stable name and label order, with construct markers.
// FEATURE: LANG-001
/// One rule in the textual form: `head :- body.` (`blossom trace why`/`whynot` show the rules they explain).
pub fn rule_text(p: &Program, r: &Rule) -> String {
    rule(p, r)
}

/// One body literal of `r` in the textual form.
pub fn literal_text(p: &Program, r: &Rule, l: &Literal) -> String {
    literal(p, r, l)
}

/// A variable of `r` by its source name.
pub fn var_text(r: &Rule, v: VarId) -> String {
    var(r, v)
}

pub fn print(p: &Program) -> String {
    let mut out = String::new();
    let mut rels = p.rels.iter().collect::<Vec<_>>();
    rels.sort_by(|a, b| a.name.cmp(&b.name));
    for r in rels {
        let class = match r.class {
            RelClass::Idb => "table",
            RelClass::Static => "static",
            RelClass::Event(_) => "event",
            RelClass::Channel(_) => "channel",
            RelClass::Weighted(WeightKind::ZSet) => "zset",
            RelClass::Weighted(WeightKind::Bag) => "bag",
            RelClass::HostTable => "host table",
            RelClass::HostOut(_) => "host out",
        };
        let col_name = |i: ColIdx| {
            r.schema.cols.get(i.index()).map_or(format!("<col:{}>", i.raw()), |c| {
                if c.hidden_dest {
                    format!("@{}", c.name)
                } else {
                    c.name.to_string()
                }
            })
        };
        let ordinary = r
            .schema
            .cols
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                !(r.schema.lattice.iter().any(|(col, _)| col.index() == *i)
                    || matches!(
                        &r.class,
                        RelClass::Channel(ChannelDecl {
                            form: ChannelForm::Direction { .. },
                            ..
                        })
                    ) && *i == 0)
            })
            .map(|(_, c)| format!("{}: {}", c.name, type_name(p, c.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let lattice = r
            .schema
            .lattice
            .iter()
            .map(|(col, ty)| {
                let name = col_name(*col);
                let lattice = p
                    .lattices
                    .get(*ty)
                    .map_or(format!("<lattice:{}>", ty.raw()), |l| l.name.to_string());
                format!("{name}: {lattice}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let columns = if lattice.is_empty() {
            ordinary
        } else {
            format!("{ordinary}; {lattice}")
        };
        let mut flags = Vec::new();
        if !r.schema.key.is_empty() {
            flags.push(format!(
                "key({})",
                r.schema.key.iter().map(|i| col_name(*i)).collect::<Vec<_>>().join(", ")
            ));
        }
        if !r.schema.payload.is_empty() {
            flags.push(format!(
                "payload({})",
                r.schema
                    .payload
                    .iter()
                    .map(|i| col_name(*i))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let RelClass::Channel(ch) = &r.class {
            if let ChannelForm::Direction { src, dst } = ch.form {
                let role = |id: RoleId| {
                    p.roles
                        .get(id)
                        .map_or(format!("<role:{}>", id.raw()), |r| r.name.to_string())
                };
                flags.push(format!("dir({} -> {})", role(src), role(dst)));
            }
            flags.push(format!(
                "fault({})",
                match ch.fault {
                    FaultModel::Lossy => "lossy",
                    FaultModel::LossyDelayed => "lossy_delayed",
                    FaultModel::Reliable => "reliable",
                    FaultModel::ReliableOrdered => "reliable_ordered",
                }
            ));
        }
        if r.durable {
            flags.push("durable".into());
        }
        writeln!(
            out,
            "decl {class} {}({columns}){}",
            r.name,
            if flags.is_empty() {
                String::new()
            } else {
                format!(" {}", flags.join(" "))
            }
        )
        .ok();
    }
    let mut rules = p.rules.iter().collect::<Vec<_>>();
    rules.sort_by(|a, b| a.label.text.cmp(&b.label.text));
    for r in rules {
        if let Some(c) = r.construct.and_then(|id| p.constructs.get(id)) {
            writeln!(
                out,
                "// construct {}: {}{}",
                c.surface.label.map_or(String::new(), |s| s.as_str().to_string()),
                c.kind.name(),
                c.surface.stmt.as_ref().map_or(String::new(), |x| format!(" — {x}"))
            )
            .ok();
        }
        writeln!(out, "// {}", r.label.text).ok();
        writeln!(out, "{}", rule(p, r)).ok();
    }
    out
}

// ---------------------------------------------------------------- values

/// `x.to_string()` for a value of type `ty` with no `to_string` of its own in the library (Appendix B): the value in
/// source syntax ([`value_text`]), its nodes by the names the deployment gave them (`node#3` for one it named none).
pub fn to_string_text(program: &Program, v: &Value, ty: TypeId, node_names: &[std::sync::Arc<str>]) -> String {
    value_text(Some(program), v, Some(ty), &|n| {
        node_names
            .get(n.0 as usize)
            .map_or_else(|| format!("node#{}", n.0), |s| s.to_string())
    })
}

/// A value of type `ty` in `program`'s type table, in source syntax: enum variants and struct fields by name,
/// lattice values by their elements (`⊥` for bottom), durations and instants in seconds. Every value has a text; one
/// whose type is not given (or not in the table) is written by its shape (`#2(…)` for an enum variant, `struct(…)`).
pub fn value_text(program: Option<&Program>, v: &Value, ty: Option<TypeId>, node: &dyn Fn(NodeId) -> String) -> String {
    Texts { program, node }.value(v, ty)
}

/// The program values are written against, and how nodes are named.
struct Texts<'a> {
    program: Option<&'a Program>,
    node: &'a dyn Fn(NodeId) -> String,
}

fn join(xs: impl Iterator<Item = String>) -> String {
    xs.collect::<Vec<_>>().join(", ")
}

impl<'a> Texts<'a> {
    fn def(&self, ty: Option<TypeId>) -> Option<&'a TypeDef> {
        ty.and_then(|t| self.program.and_then(|p| p.types.get(t)))
    }

    fn lattice_ctor(&self, id: LatticeTypeId) -> Option<&'a LatticeCtor> {
        self.program.and_then(|p| p.lattices.get(id)).map(|d| &d.ctor)
    }

    fn group_ctor(&self, id: GroupTypeId) -> Option<&'a GroupCtor> {
        self.program.and_then(|p| p.groups.get(id)).map(|d| &d.ctor)
    }

    fn value(&self, v: &Value, ty: Option<TypeId>) -> String {
        let def = self.def(ty);
        match v {
            Value::Node(n) => (self.node)(*n),
            Value::Str(s) => format!("{s:?}"),
            Value::Bool(b) => b.to_string(),
            Value::Unit => "()".to_owned(),
            Value::Int(i) => i.to_string(),
            // As a literal reads (`1.0`, `2.5e-7`; `NaN`, `inf` have none).
            Value::F64(f) => format!("{f:?}"),
            Value::Duration(d) => seconds(d.as_nanos()),
            Value::Instant(t) => format!("@{}", seconds(t.0)),
            Value::Mod(m) => m.to_string(),
            // The content address and the length.
            Value::Blob(b) => format!("blob#{}:{}", b.hex(), b.len),
            Value::Session(s) => format!("session {}", s.0),
            Value::Conn(c) => format!("conn#{}", c.0),
            // Bytes as a byte string: printable ASCII as is, the rest escaped.
            Value::Bytes(b) => format!("b\"{}\"", b.escape_ascii()),
            Value::Principal(p) => format!("principal {p:?}"),
            Value::Option(None) => "None".to_owned(),
            Value::Option(Some(x)) => {
                let t = match def {
                    Some(TypeDef::Option(t)) => Some(*t),
                    _ => None,
                };
                format!("Some({})", self.value(x, t))
            }
            Value::Tuple(xs) => {
                let ts = match def {
                    Some(TypeDef::Tuple(ts)) => Some(ts),
                    _ => None,
                };
                let elem = |i: usize| ts.and_then(|ts| ts.get(i)).copied();
                format!("({})", join(xs.iter().enumerate().map(|(i, x)| self.value(x, elem(i)))))
            }
            Value::Struct(xs) => match def {
                Some(TypeDef::Struct(s)) => format!(
                    "{} {{ {} }}",
                    s.name,
                    join(
                        xs.iter()
                            .zip(&s.fields)
                            .map(|(x, f)| format!("{}: {}", f.name, self.value(x, Some(f.ty))))
                    )
                ),
                _ => format!("struct({})", join(xs.iter().map(|x| self.value(x, None)))),
            },
            Value::Enum { variant, fields } => {
                let var = match def {
                    Some(TypeDef::Enum(e)) => e.variants.iter().find(|x| x.number == *variant),
                    _ => None,
                };
                let name = var.map_or_else(|| format!("#{variant}"), |x| x.name.to_string());
                if fields.is_empty() {
                    name
                } else {
                    let elem = |i: usize| var.and_then(|v| v.payload.get(i)).map(|f| f.ty);
                    format!(
                        "{name}({})",
                        join(fields.iter().enumerate().map(|(i, x)| self.value(x, elem(i))))
                    )
                }
            }
            // A newer version's variant, kept as it arrived: the local `#[unknown]` variant, the wire number and the
            // payload's size.
            Value::UnknownVariant {
                variant,
                wire_number,
                payload,
            } => {
                let name = match def {
                    Some(TypeDef::Enum(e)) => e
                        .variants
                        .iter()
                        .find(|x| x.number == *variant)
                        .map(|x| x.name.to_string()),
                    _ => None,
                };
                let name = name.unwrap_or_else(|| format!("#{variant}"));
                format!("{name}(#{wire_number}, {} bytes)", payload.len())
            }
            Value::Vec(xs) => {
                let t = match def {
                    Some(TypeDef::Vec(t)) => Some(*t),
                    _ => None,
                };
                format!("[{}]", join(xs.iter().map(|x| self.value(x, t))))
            }
            Value::Set(xs) => {
                let t = match def {
                    Some(TypeDef::Set(t)) => Some(*t),
                    _ => None,
                };
                format!("set[{}]", join(xs.iter().map(|x| self.value(x, t))))
            }
            Value::Map(m) => {
                let (k, t) = match def {
                    Some(TypeDef::Map(k, t)) => (Some(*k), Some(*t)),
                    _ => (None, None),
                };
                format!(
                    "map[{}]",
                    join(
                        m.iter()
                            .map(|(a, b)| format!("{} => {}", self.value(a, k), self.value(b, t)))
                    )
                )
            }
            Value::Lattice(l) => {
                let ctor = match def {
                    Some(TypeDef::Lattice(id)) => self.lattice_ctor(*id),
                    _ => None,
                };
                self.lattice(l, ctor)
            }
            Value::Group(g) => {
                let ctor = match def {
                    Some(TypeDef::Group(id)) => self.group_ctor(*id),
                    _ => None,
                };
                self.group(g, ctor)
            }
            // An opaque host value: its type (or codec) and its encoded bytes.
            Value::Extern { codec, bytes } => {
                let name = match def {
                    Some(TypeDef::Extern(e)) => e.name.to_string(),
                    _ => codec.0.to_string(),
                };
                format!("{name}(b\"{}\")", bytes.escape_ascii())
            }
        }
    }

    fn lattice(&self, l: &LatValue, ctor: Option<&'a LatticeCtor>) -> String {
        use LatticeCtor as C;
        // An adjoined ⊥ or ⊤ is written as such; every other value as the inner lattice writes it.
        if let Some(C::WithBot(inner) | C::WithTop(inner)) = ctor
            && !matches!(l, LatValue::Bottom | LatValue::Top)
        {
            return self.lattice(l, self.lattice_ctor(*inner));
        }
        match l {
            LatValue::Bottom => "⊥".to_owned(),
            LatValue::Top => "⊤".to_owned(),
            LatValue::Bool(b) => b.to_string(),
            LatValue::Elem(x) => {
                let t = match ctor {
                    Some(C::Max(t) | C::Min(t) | C::Point(t) | C::Conflict(t)) => Some(*t),
                    _ => None,
                };
                self.value(x, t)
            }
            LatValue::Set(xs) => {
                let t = match ctor {
                    Some(C::Set(t) | C::PSet(t) | C::UnionFind(t)) => Some(*t),
                    _ => None,
                };
                format!("{{{}}}", join(xs.iter().map(|x| self.value(x, t))))
            }
            LatValue::Map(m) => {
                let (k, inner) = match ctor {
                    Some(C::Map(k, inner)) => (Some(*k), self.lattice_ctor(*inner)),
                    _ => (None, None),
                };
                format!(
                    "{{{}}}",
                    join(
                        m.iter()
                            .map(|(a, b)| format!("{}: {}", self.value(a, k), self.lattice(b, inner)))
                    )
                )
            }
            // Each element with its multiplicity.
            LatValue::Bag(m) => {
                let t = match ctor {
                    Some(C::Bag(t)) => Some(*t),
                    _ => None,
                };
                format!(
                    "{{{}}}",
                    join(m.iter().map(|(x, n)| format!("{} × {n}", self.value(x, t))))
                )
            }
            LatValue::Seq(xs) => {
                let part = |i: usize| -> Option<&'a LatticeCtor> {
                    match ctor {
                        Some(C::Pair(a, b)) => [a, b].get(i).and_then(|id| self.lattice_ctor(**id)),
                        Some(C::Lex { chain, inner }) => [chain, inner].get(i).and_then(|id| self.lattice_ctor(**id)),
                        Some(C::Product(fs)) => fs.get(i).and_then(|(_, id)| self.lattice_ctor(*id)),
                        Some(C::VecUnion(id)) => self.lattice_ctor(*id),
                        _ => None,
                    }
                };
                let field = |i: usize| match ctor {
                    Some(C::Product(fs)) => fs.get(i).map(|(name, _)| format!("{name}: ")),
                    _ => None,
                };
                let items = xs
                    .iter()
                    .enumerate()
                    .map(|(i, x)| format!("{}{}", field(i).unwrap_or_default(), self.lattice(x, part(i))));
                match ctor {
                    Some(C::VecUnion(_)) => format!("[{}]", join(items)),
                    _ => format!("({})", join(items)),
                }
            }
            // An `extern lattice` value: its Rust type (or codec) and its encoded bytes.
            LatValue::Extern { codec, bytes } => {
                let name = match ctor {
                    Some(C::Extern(r)) => r.path.to_string(),
                    _ => codec.0.to_string(),
                };
                format!("{name}(b\"{}\")", bytes.escape_ascii())
            }
        }
    }

    fn group(&self, g: &GroupValue, ctor: Option<&'a GroupCtor>) -> String {
        match g {
            GroupValue::Z(n) => n.to_string(),
            GroupValue::Zn(n) => n.to_string(),
            // Each element with its weight.
            GroupValue::ZSet(m) => {
                let t = match ctor {
                    Some(GroupCtor::ZSet(t)) => Some(*t),
                    _ => None,
                };
                format!(
                    "zset[{}]",
                    join(m.iter().map(|(x, w)| format!("{} => {w}", self.value(x, t))))
                )
            }
            GroupValue::Tuple(gs) => {
                let part = |i: usize| match ctor {
                    Some(GroupCtor::Tuple(cs)) => cs.get(i).and_then(|c| self.group_ctor(*c)),
                    _ => None,
                };
                format!("({})", join(gs.iter().enumerate().map(|(i, x)| self.group(x, part(i)))))
            }
            GroupValue::Map(m) => {
                let (k, inner) = match ctor {
                    Some(GroupCtor::Map(k, c)) => (Some(*k), self.group_ctor(*c)),
                    _ => (None, None),
                };
                format!(
                    "map[{}]",
                    join(
                        m.iter()
                            .map(|(a, b)| format!("{} => {}", self.value(a, k), self.group(b, inner)))
                    )
                )
            }
            // A user group's value is its carrier's: the type its `zero` returns.
            GroupValue::User(x) => {
                let t = match ctor {
                    Some(GroupCtor::User { zero, .. }) => self.program.and_then(|p| p.fns.get(*zero)).map(|f| f.ret),
                    _ => None,
                };
                self.value(x, t)
            }
        }
    }
}

/// Nanoseconds as seconds: `1s`, `1.5s`, `-2s`.
fn seconds(nanos: i64) -> String {
    // On the magnitude, so a negative duration under a second keeps its sign (`-0.007s`).
    let sign = if nanos < 0 { "-" } else { "" };
    let magnitude = nanos.unsigned_abs();
    let whole = magnitude / 1_000_000_000;
    let frac = magnitude % 1_000_000_000;
    if frac == 0 {
        format!("{sign}{whole}s")
    } else {
        let digits = format!("{frac:09}");
        format!("{sign}{whole}.{}s", digits.trim_end_matches('0'))
    }
}
