//! The frontend entry points used by the driver (ARCHITECTURE §13.1).
//!
//! [`compile`] runs the phases in order: module loading and parsing ([`crate::modules`]), resolution and
//! instantiation ([`crate::resolve`]), type checking ([`crate::typeck`]), event classification and the handler rules
//! ([`crate::classify`]), and lowering for a deployment ([`crate::lower`]). Each phase stops the pipeline when it
//! reports an error.

// FEATURE: LANG-001

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{Diagnostic, Diagnostics, InternalError, RoleId, SourceDb, Symbol, code};

use crate::hir::HRoleId;
use crate::modules::{Loader, ModuleTree};

/// Why a `.bls` program was not compiled.
#[derive(Debug, thiserror::Error)]
pub enum BlsError {
    /// The program is rejected; the diagnostics say why.
    #[error("the program has {} error(s)", .0.error_count())]
    Rejected(Diagnostics),
    /// A frontend bug: lowering produced a program the IR validator rejects.
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// A node of the deployment and the role it is assigned (`None` in a role-free program).
#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub name: String,
    pub role: Option<String>,
}

/// A deployment's value for a deploy-time parameter (LANG-010), typed as the deployment spec writes it: an integer,
/// a bool, or text (a string, or a duration such as `"150ms"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamBinding {
    Int(i128),
    Bool(bool),
    Text(String),
}

/// A duration written like a Blossom literal: `500ms`, `1s`, `2m`, `1h`, `10us`, `5ns`.
pub fn parse_duration(text: &str) -> Option<blossom_value::time::Duration> {
    let split = text.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = text.split_at(split);
    // A duration binding is written like a duration literal: a non-negative count and a unit.
    let n: i64 = num.trim().parse::<u64>().ok().and_then(|n| i64::try_from(n).ok())?;
    let scale: i64 = match unit {
        "ns" => 1,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        _ => return None,
    };
    n.checked_mul(scale).map(blossom_value::time::Duration::from_nanos)
}

/// Compiles the program rooted at `root` for a deployment of `nodes`. Warnings are returned with the artifact.
pub fn compile(
    root: &str,
    nodes: &[NodeSpec],
    loader: &mut dyn Loader,
    sources: &mut SourceDb,
) -> Result<(BlsArtifact, Diagnostics), BlsError> {
    compile_with(root, nodes, &std::collections::BTreeMap::new(), loader, sources)
}

/// [`compile`] with the deployment's values of deploy-time parameters.
pub fn compile_with(
    root: &str,
    nodes: &[NodeSpec],
    params: &std::collections::BTreeMap<String, ParamBinding>,
    loader: &mut dyn Loader,
    sources: &mut SourceDb,
) -> Result<(BlsArtifact, Diagnostics), BlsError> {
    compile_for(root, Some(nodes), params, loader, sources)
}

/// Compiles a program for checking, with no deployment given: a role-free program on one node, a program with roles
/// on one node per role that holds nodes ([`checking_nodes`]).
pub fn compile_checking(
    root: &str,
    params: &std::collections::BTreeMap<String, ParamBinding>,
    loader: &mut dyn Loader,
    sources: &mut SourceDb,
) -> Result<(BlsArtifact, Diagnostics), BlsError> {
    compile_for(root, None, params, loader, sources)
}

/// The deployment a program is checked on: one node `n1` when it has no roles, else one node per role but an
/// external one (`server1` for `Server`).
pub(crate) fn checking_nodes(hir: &crate::hir::Hir) -> Vec<NodeSpec> {
    if hir.roles.is_empty() {
        return vec![NodeSpec {
            name: "n1".to_owned(),
            role: None,
        }];
    }
    hir.roles
        .iter()
        .filter(|r| r.name.segments().len() == 1 && r.kind != crate::hir::RoleKind::External)
        .map(|r| NodeSpec {
            name: format!("{}1", r.name.to_string().to_lowercase()),
            role: Some(r.name.to_string()),
        })
        .collect()
}

fn compile_for(
    root: &str,
    nodes: Option<&[NodeSpec]>,
    params: &std::collections::BTreeMap<String, ParamBinding>,
    loader: &mut dyn Loader,
    sources: &mut SourceDb,
) -> Result<(BlsArtifact, Diagnostics), BlsError> {
    let mut diags = Diagnostics::new();
    let Some(tree) = ModuleTree::load(root, loader, sources, &mut diags) else {
        return Err(BlsError::Rejected(diags));
    };
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    let Some(mut hir) = crate::resolve::resolve(&tree, sources, &mut diags, params)? else {
        return Err(BlsError::Rejected(diags));
    };
    crate::typeck::check(&mut hir, &mut diags)?;
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    crate::classify::check(&hir, &mut diags)?;
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    let derived;
    let nodes = match nodes {
        Some(n) => n,
        None => {
            derived = checking_nodes(&hir);
            &derived
        }
    };
    let (names, roles) = deployment(&hir, nodes, &mut diags);
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    let lowered = crate::lower::lower(
        &hir,
        &crate::lower::Deployment {
            nodes: &names,
            roles: &roles,
        },
    )?;
    // A timer's guard depends on carried state only (LANGUAGE §15.2).
    for (span, why) in unsteady_guards(lowered.program.get()) {
        diags.push(
            Diagnostic::new(
                code!("BLS0412"),
                format!(
                    "a timer's `while` guard must depend only on carried state (tables, statics and views of them): \
                     it {why}, which the node would see only when it next ticks"
                ),
            )
            .with_primary(span),
        );
    }
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    // No evaluation may be deeper than the stack a tick runs on (LANGUAGE §16.1).
    if let Some(d) = too_deep(lowered.program.get(), |id| {
        lowered
            .fn_origins
            .get(id.index())
            .and_then(|h| hir.fns.get(h.index()))
            .map(|f| (format!("function `{}`", f.name), f.span))
    }) {
        diags.push(d);
        return Err(BlsError::Rejected(diags));
    }
    let roles = roles.iter().map(|r| r.map(|r| RoleId::from_raw(r.0))).collect();
    let halt = hir
        .rels
        .iter()
        .position(|r| r.kind == crate::hir::HRelKind::Halt)
        .and_then(|i| lowered.rels.get(i).copied());
    let methods = lowered
        .fn_origins
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let m = hir.methods.iter().find(|m| m.f == *h)?;
            Some((blossom_base::FnId::from_raw(u32::try_from(i).ok()?), m.span))
        })
        .collect();
    Ok((
        BlsArtifact {
            nodes: names,
            roles,
            program: lowered.program,
            surface: lowered.surface.into_iter().collect(),
            halt,
            methods,
        },
        diags,
    ))
}

/// The deployment of `nodes` for `hir`: names sorted, each node's role checked against the program's roles (a
/// process role holds one node, a cluster one or more, an external role none), and the node names that facts write
/// as strings checked (LANGUAGE §2.4). Problems go to `diags`.
pub(crate) fn deployment(
    hir: &crate::hir::Hir,
    nodes: &[NodeSpec],
    diags: &mut Diagnostics,
) -> (Vec<Symbol>, Vec<Option<HRoleId>>) {
    let mut sorted: Vec<&NodeSpec> = nodes.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names = Vec::new();
    let mut roles: Vec<Option<HRoleId>> = Vec::new();
    for (i, n) in sorted.iter().enumerate() {
        if sorted.get(i + 1).is_some_and(|m| m.name == n.name) {
            diags.push(Diagnostic::new(
                code!("BLS0200"),
                format!("node `{}` is listed twice", n.name),
            ));
            continue;
        }
        let role = match (&n.role, hir.roles.is_empty()) {
            // A single-location program's only role is `Node` (LANGUAGE §7.7).
            (None, true) => None,
            (Some(r), true) if r == "Node" => None,
            (Some(r), false) => {
                match hir
                    .roles
                    .iter()
                    .position(|x| x.name.segments().len() == 1 && x.name.to_string() == *r)
                {
                    Some(i)
                        if hir
                            .roles
                            .get(i)
                            .is_some_and(|x| x.kind == crate::hir::RoleKind::External) =>
                    {
                        diags.push(Diagnostic::new(
                            code!("BLS0200"),
                            format!(
                                "node `{}` is assigned the external role `{r}`, which holds no nodes",
                                n.name
                            ),
                        ));
                        None
                    }
                    Some(i) => Some(HRoleId(i as u32)),
                    None => {
                        diags.push(Diagnostic::new(
                            code!("BLS0200"),
                            format!("node `{}` is assigned the unknown role `{r}`", n.name),
                        ));
                        None
                    }
                }
            }
            (None, false) => {
                diags.push(Diagnostic::new(
                    code!("BLS0200"),
                    format!("the program has roles, so node `{}` needs one", n.name),
                ));
                None
            }
            (Some(r), true) => {
                diags.push(Diagnostic::new(
                    code!("BLS0200"),
                    format!(
                        "node `{}` is assigned role `{r}`, but the program declares no roles",
                        n.name
                    ),
                ));
                None
            }
        };
        names.push(Symbol::intern(&n.name));
        roles.push(role);
    }
    for (i, r) in hir.roles.iter().enumerate() {
        let count = roles.iter().filter(|x| **x == Some(HRoleId(i as u32))).count();
        let ok = match r.kind {
            crate::hir::RoleKind::Process => count == 1,
            crate::hir::RoleKind::Cluster => count >= 1,
            crate::hir::RoleKind::External => true,
        };
        if !ok {
            diags.push(Diagnostic::new(
                code!("BLS0200"),
                format!(
                    "role `{}` is a {} role but the deployment assigns it {count} node(s)",
                    r.name,
                    if r.kind == crate::hir::RoleKind::Process {
                        "process (one node)"
                    } else {
                        "cluster (one or more nodes)"
                    }
                ),
            ));
        }
    }
    if names.is_empty() {
        diags.push(Diagnostic::new(code!("BLS0200"), "the deployment has no nodes"));
    }
    // Node names written as strings in facts must name nodes of the deployment (LANGUAGE §2.4).
    for f in &hir.facts {
        for e in &f.row {
            if let crate::hir::HExprKind::Value(blossom_value::Value::Str(name), _) = &e.kind
                && matches!(
                    e.ty.and_then(|t| hir.types.get(t)),
                    Some(blossom_value::TypeDef::Node(_))
                )
                && !names.iter().any(|n| n.as_str() == &**name)
            {
                diags.push(
                    Diagnostic::new(code!("BLS0200"), format!("`{name}` is not a node of the deployment"))
                        .with_primary(e.span),
                );
            }
        }
    }
    (names, roles)
}

/// BLS0217 (LANGUAGE §16.1) when `program`'s deepest evaluation is past the bound the evaluators' stack is sized for.
/// `function` names a declared function and gives its span.
/// The guarded timers whose guard does not depend on carried state alone, with why: an event, an input, a message
/// or a stream reads (or `now()`, `tick()` or `rand`) would make a guard that held at the end of a tick false at the
/// next without the node knowing, so the node and the synchronous world would fire differently (LANGUAGE §15.2).
/// The lints of a program built to be deployed: a logical timer (`every n ticks`) keeps its node ticking, so it is
/// meant for simulation and LDFI (BLS1006, LANGUAGE §15.2).
pub fn deployed_lints(p: &blossom_ir::core::Program) -> Diagnostics {
    use blossom_ir::core::{EventSource, RelClass, TimerClock};
    let mut out = Diagnostics::new();
    for r in p.rels.iter() {
        if let RelClass::Event(EventSource::Timer(t)) = &r.class
            && t.clock == TimerClock::Logical
        {
            out.push(
                Diagnostic::new(
                    code!("BLS1006"),
                    format!(
                        "`{}` is a logical timer: it keeps its node ticking without pause, so it is meant for \
                         simulation and LDFI; a deployment uses a physical one (`every DURATION`)",
                        r.name
                    ),
                )
                .with_primary(r.span),
            );
        }
    }
    out
}

fn unsteady_guards(p: &blossom_ir::core::Program) -> Vec<(blossom_base::Span, String)> {
    use blossom_ir::core::{EventSource, GenSource, Literal, Persistence, RelClass};
    use std::collections::BTreeMap;
    fn steady(
        p: &blossom_ir::core::Program,
        r: blossom_base::RelId,
        memo: &mut BTreeMap<blossom_base::RelId, Option<String>>,
    ) -> Option<String> {
        if let Some(known) = memo.get(&r) {
            return known.clone();
        }
        // Assumed steady while its definition is checked (recursive views).
        memo.insert(r, None);
        let Some(decl) = p.rels.get(r) else {
            return Some("names a relation the program does not have".into());
        };
        let why = match &decl.class {
            RelClass::Event(_) | RelClass::Channel(_) => Some(format!("reads `{}`, an event", decl.name)),
            RelClass::Static => None,
            RelClass::Idb if decl.persistence != Persistence::None => None,
            _ => p.rules.iter().filter(|rule| rule.head.rel == r).find_map(|rule| {
                let exprs_vary = rule.body.lits.iter().any(|l| match l {
                    Literal::Bind { expr, .. } | Literal::Guard(expr) => expr.time_varying(),
                    Literal::Gen { src, .. } => match src {
                        GenSource::Value(e) | GenSource::Lattice(e) => e.time_varying(),
                        GenSource::Range { lo, hi, .. } => lo.time_varying() || hi.time_varying(),
                        GenSource::TableFn { .. } => false,
                    },
                    _ => false,
                });
                if exprs_vary {
                    return Some(format!("reads the clock, the tick or `rand` (in `{}`)", decl.name));
                }
                rule.body.lits.iter().find_map(|l| match l {
                    Literal::Pos(a) | Literal::Neg(a) => steady(p, a.rel, memo),
                    Literal::Lookup { rel, .. } => steady(p, *rel, memo),
                    _ => None,
                })
            }),
        };
        memo.insert(r, why.clone());
        why
    }
    let mut memo = BTreeMap::new();
    let mut out = Vec::new();
    for r in p.rels.iter() {
        if let RelClass::Event(EventSource::Timer(t)) = &r.class
            && let Some(g) = t.guard
            && let Some(why) = steady(p, g, &mut memo)
        {
            out.push((r.span, why));
        }
    }
    out
}

pub(crate) fn too_deep(
    program: &blossom_ir::core::Program,
    function: impl Fn(blossom_base::FnId) -> Option<(String, blossom_base::Span)>,
) -> Option<Diagnostic> {
    use blossom_ir::depth::{DepthSite, MAX_EVAL_DEPTH, deepest};
    let (depth, site) = deepest(program)?;
    if depth <= MAX_EVAL_DEPTH {
        return None;
    }
    let (what, span) = match site {
        DepthSite::Fn(id) => match function(id) {
            Some((what, span)) => (what, Some(span)),
            None => ("a function".to_owned(), None),
        },
        DepthSite::Rule(id) => {
            let r = program.rules.get(id);
            (
                format!("rule `{}`", r.map(|r| r.label.to_string()).unwrap_or_default()),
                r.map(|r| r.span),
            )
        }
        DepthSite::Partition(id) => {
            let r = program.rels.get(id);
            (
                format!(
                    "the partition key of `{}`",
                    r.map(|r| r.name.to_string()).unwrap_or_default()
                ),
                None,
            )
        }
    };
    let mut d = Diagnostic::new(
        code!("BLS0217"),
        format!(
            "{what} evaluates {depth} levels deep, past the bound of {MAX_EVAL_DEPTH} (§16.1): split its nesting or \
             its chain of calls"
        ),
    );
    if let Some(s) = span {
        d = d.with_primary(s);
    }
    Some(d)
}
