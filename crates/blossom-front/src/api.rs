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
    let roles = roles.iter().map(|r| r.map(|r| RoleId::from_raw(r.0))).collect();
    let halt = hir
        .rels
        .iter()
        .position(|r| r.kind == crate::hir::HRelKind::Halt)
        .and_then(|i| lowered.rels.get(i).copied());
    Ok((
        BlsArtifact {
            nodes: names,
            roles,
            program: lowered.program,
            surface: lowered.surface.into_iter().collect(),
            halt,
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
