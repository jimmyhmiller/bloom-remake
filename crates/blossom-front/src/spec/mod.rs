//! Specs (ARCHITECTURE §13.10, LANGUAGE §17): a `spec` item compiled with its target into a [`SimArtifact`] that the
//! simulator runs and LDFI checks.
//!
//! - The **target** (`for M<T…>(K = v, …)`) is a module or choreography of the spec's file (or of a module it uses),
//!   compiled as a program root for the spec's scenario.
//! - The **scenario**: `nodes A, B, C;` (SCREAMING_CASE constants) and `assign Role = [A, B];` give the deployment;
//!   `fact r(…) @ n;` asserts a row of the static relation `r` at node `n` (every node without `@`), and
//!   `fact r(…) @ n at tick k;` an event of the input `r` at node `n`'s tick `k` (every node without `@`).
//! - The spec's **views** are compiled to a second program over trace relations: an atom `r(…) @ n` of a target
//!   relation reads `r` at the evaluation point (EOT for `pre` and `post`), `r(…) @ n at tick k` reads tick `k`, and
//!   `crashed(n)` is the crash oracle (LANGUAGE §17.3). Each trace relation has the node as column 0.
//! - `faults { eot, eff, crashes, model, round }` is the failure spec ⟨EOT, EFF, maxCrashes⟩ (TEST-020) of the
//!   synchronous model; `round` maps physical time to rounds (ODD-16) and is required when the target observes time.
//! - `check ldfi [expect holds | fails];` needs `pre` and `post` with one schema (BLS0900, CR-30).
//!
//! `include S;` merges the members of the spec `S`. Members this build does not implement (`liveness`, `prove`,
//! `expect`, invariants) are reported when present.

use std::collections::{BTreeMap, BTreeSet};

use blossom_artifact::sim::{
    EdgeTime, IngressFact, InputFact, LogicalEdge, LogicalIdx, LogicalKind, LogicalRel, NodeStatic, OutcomeSpec,
    Profile, SimArtifact, SpecFeed,
};
use blossom_base::{Diagnostic, Diagnostics, RelId, RoleId, SourceDb, Span, Symbol, code, internal_error};
use blossom_ir::core::{BuiltinScalar, EventSource, Expr, Literal, Program, RelClass, RuleKind};
use blossom_value::time::{Duration, NodeId, Tick};
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

use crate::api::{BlsError, NodeSpec};
use crate::ast::{self, Arg, ExprKind, Ident, ItemKind, LitValue, PrefixOp, SpecMember};
use crate::hir::{HRelKind, Hir};
use crate::modules::{Loader, ModuleTree};
use crate::resolve::SpecMode;

/// A `check` member.
#[derive(Clone, Debug)]
pub struct Check {
    /// `ldfi`, `bmc`, `smt`, `sim`, …
    pub tool: Symbol,
    /// `expect holds` (`Some(true)`), `expect fails` (`Some(false)`), or only reported.
    pub expect: Option<bool>,
    pub span: Span,
}

/// The failure spec of `faults { … }` (TEST-020).
#[derive(Clone, Copy, Debug)]
pub struct Faults {
    pub eot: u64,
    pub eff: u64,
    pub crashes: u32,
}

/// A spec compiled with its target.
pub struct CompiledSpec {
    pub name: Symbol,
    pub artifact: SimArtifact,
    pub faults: Option<Faults>,
    pub checks: Vec<Check>,
    /// Members and checks that need tools this build does not have: reported as not run, with their feature.
    pub not_run: Vec<(String, Span)>,
}

/// Compiles the spec named `name` of the file `root`.
pub fn compile_spec(
    root: &str,
    name: &str,
    loader: &mut dyn Loader,
    sources: &mut SourceDb,
) -> Result<(CompiledSpec, Diagnostics), BlsError> {
    let mut diags = Diagnostics::new();
    let Some(tree) = ModuleTree::load(root, loader, sources, &mut diags) else {
        return Err(BlsError::Rejected(diags));
    };
    if diags.has_errors() {
        return Err(BlsError::Rejected(diags));
    }
    let compiled = compile(&tree, name, sources, &mut diags)?;
    match compiled {
        Some(c) if !diags.has_errors() => Ok((c, diags)),
        _ => Err(BlsError::Rejected(diags)),
    }
}

/// A spec's members, its includes merged.
#[derive(Default)]
struct Members<'t> {
    target: Option<(&'t ast::SpecItem, &'t [Ident])>,
    nodes: Vec<Ident>,
    assign: Vec<(Ident, &'t [Ident])>,
    faults: Option<(&'t [(Ident, ast::Expr)], Span)>,
    facts: Vec<&'t ast::Fact>,
    views: Vec<&'t ast::ViewDecl>,
    checks: Vec<(Ident, Option<Ident>, Span)>,
    not_run: Vec<(String, Span)>,
}

fn find_spec(items: &[ast::Item], name: Symbol) -> Option<&ast::SpecItem> {
    items.iter().find_map(|i| match &i.kind {
        ItemKind::Spec(s) if s.name.is_some_and(|n| n.name == name) => Some(s),
        _ => None,
    })
}

fn collect<'t>(
    tree: &'t ModuleTree,
    spec: &'t ast::SpecItem,
    out: &mut Members<'t>,
    seen: &mut BTreeSet<Symbol>,
    diags: &mut Diagnostics,
) {
    if let Some(n) = spec.name
        && !seen.insert(n.name)
    {
        diags.push(
            Diagnostic::new(
                code!("BLS0201"),
                format!("spec `{}` is included twice (or in a cycle)", n.as_str()),
            )
            .with_primary(n.span),
        );
        return;
    }
    if let Some(t) = &spec.target {
        if out.target.is_some() {
            diags.push(Diagnostic::new(code!("BLS0201"), "a spec has one target").with_primary(spec.span));
        } else {
            out.target = Some((spec, t.as_slice()));
        }
    }
    for m in &spec.members {
        match m {
            SpecMember::Nodes(ns) => out.nodes.extend(ns.iter().copied()),
            SpecMember::Assign { role, nodes, .. } => out.assign.push((*role, nodes.as_slice())),
            SpecMember::Faults(opts, span) => {
                if out.faults.is_some() {
                    diags.push(Diagnostic::new(code!("BLS0201"), "`faults` is given twice").with_primary(*span));
                }
                out.faults = Some((opts.as_slice(), *span));
            }
            SpecMember::Include(path, span) => {
                let target = match path.as_slice() {
                    [n] => find_spec(&tree.root.items, n.name),
                    [m, n] => tree.modules.get(m.as_str()).and_then(|f| find_spec(&f.items, n.name)),
                    _ => None,
                };
                match target {
                    Some(s) => collect(tree, s, out, seen, diags),
                    None => diags.push(Diagnostic::new(code!("BLS0204"), "unknown spec").with_primary(*span)),
                }
            }
            SpecMember::Check { kind, expect, span, .. } => out.checks.push((*kind, *expect, *span)),
            SpecMember::Fact(f) => out.facts.push(f),
            SpecMember::View(v) => out.views.push(v),
            SpecMember::Invariant(inv) => out.not_run.push((
                format!(
                    "invariant `{}` (checked by bmc, sim and smt: VER-002, TEST-001)",
                    inv.name.as_str()
                ),
                inv.span,
            )),
            SpecMember::Const { name, .. } => diags.push(
                Diagnostic::not_implemented(
                    blossom_base::FeatureId("LANG-010"),
                    "constants in a spec",
                    "the Blossom frontend (slice 2)",
                )
                .with_primary(name.span),
            ),
            SpecMember::Unsupported { what, span } => {
                let feature = match *what {
                    "liveness" => "TEST-021",
                    "prove" => "VER-003",
                    _ => "TEST-020",
                };
                out.not_run.push((format!("`{what}` ({feature})"), *span));
            }
        }
    }
}

fn compile(
    tree: &ModuleTree,
    name: &str,
    sources: &SourceDb,
    diags: &mut Diagnostics,
) -> Result<Option<CompiledSpec>, BlsError> {
    let Some(spec) = find_spec(&tree.root.items, Symbol::intern(name)) else {
        diags.push(Diagnostic::new(
            code!("BLS0204"),
            format!("no spec `{name}` in the file"),
        ));
        return Ok(None);
    };
    let mut m = Members::default();
    collect(tree, spec, &mut m, &mut BTreeSet::new(), diags);
    if diags.has_errors() {
        return Ok(None);
    }
    let Some((target_spec, target)) = m.target else {
        diags.push(
            Diagnostic::new(code!("BLS0900"), format!("spec `{name}` has no target (`for M`)")).with_primary(spec.span),
        );
        return Ok(None);
    };
    // The deployment.
    let mut node_specs = Vec::new();
    for n in &m.nodes {
        if !is_screaming(n.as_str()) {
            diags.push(
                Diagnostic::new(
                    code!("BLS0211"),
                    format!("spec node name `{}` is not SCREAMING_CASE", n.as_str()),
                )
                .with_primary(n.span),
            );
        }
        let role = m
            .assign
            .iter()
            .find(|(_, ns)| ns.iter().any(|x| x.name == n.name))
            .map(|(r, _)| r.as_str().to_owned());
        node_specs.push(NodeSpec {
            name: n.as_str().to_owned(),
            role,
        });
    }
    for (r, ns) in &m.assign {
        for n in ns.iter() {
            if !m.nodes.iter().any(|x| x.name == n.name) {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0200"),
                        format!(
                            "`{}` is assigned to `{}` but is not a spec node",
                            n.as_str(),
                            r.as_str()
                        ),
                    )
                    .with_primary(n.span),
                );
            }
        }
    }
    if node_specs.is_empty() {
        diags.push(Diagnostic::new(code!("BLS0900"), format!("spec `{name}` has no `nodes`")).with_primary(spec.span));
    }
    if diags.has_errors() {
        return Ok(None);
    }
    // The target, compiled as a program root.
    let Some((mut hir, _)) = crate::resolve::resolve_module_root(tree, sources, diags, target, &[], &[], None)? else {
        return Ok(None);
    };
    crate::typeck::check(&mut hir, diags)?;
    if diags.has_errors() {
        return Ok(None);
    }
    crate::classify::check(&hir, diags)?;
    if diags.has_errors() {
        return Ok(None);
    }
    let (names, roles) = crate::api::deployment(&hir, &node_specs, diags);
    if diags.has_errors() {
        return Ok(None);
    }
    let deployment = crate::lower::Deployment {
        nodes: &names,
        roles: &roles,
    };
    let lowered = crate::lower::lower(&hir, &deployment)?;
    let protocol = lowered.program;
    let node_index: BTreeMap<Symbol, u32> = names.iter().enumerate().map(|(i, n)| (*n, i as u32)).collect();

    // The target's surface relations, for the spec's located atoms and facts.
    // Instances' relations are named by their path, `a.r` (LANGUAGE §17.3).
    let mut targets: BTreeMap<Symbol, usize> = BTreeMap::new();
    for (i, r) in hir.rels.iter().enumerate() {
        let segs = r.name.segments();
        if segs.is_empty() || segs.iter().any(|s| s.as_str().contains('$')) {
            continue;
        }
        let name = segs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(".");
        targets.insert(Symbol::intern(&name), i);
    }

    // Faults.
    let faults = match m.faults {
        None => None,
        Some((opts, span)) => parse_faults(opts, span, diags),
    };
    let observes_time = protocol
        .get()
        .rels
        .iter()
        .any(|r| matches!(r.class, RelClass::Event(EventSource::Timer(_))))
        || reads_now(protocol.get());
    let round = match m
        .faults
        .and_then(|(opts, _)| opts.iter().find(|(k, _)| k.as_str() == "round"))
    {
        Some((_, e)) => match duration(e) {
            Some(d) => Some(d),
            None => {
                diags.push(Diagnostic::new(code!("BLS0900"), "`round` is a positive duration").with_primary(e.span));
                None
            }
        },
        None => None,
    };
    let round = match (round, observes_time) {
        (Some(r), _) => r,
        (None, false) => Duration::from_nanos(1_000_000_000),
        (None, true) => {
            diags.push(
                Diagnostic::new(
                    code!("BLS0900"),
                    "the target reads the clock or has physical timers, so `faults` needs `round` (ODD-16)",
                )
                .with_primary(m.faults.map_or(spec.span, |(_, s)| s)),
            );
            return Ok(None);
        }
    };

    // Scenario facts.
    let mut inputs = Vec::new();
    let mut ingress = Vec::new();
    let mut statics = Vec::new();
    for f in &m.facts {
        // A root relation by name, or an instance's channel from an external role by its path (`tpc.begin`).
        let path: Vec<&str> = f.head.rel.iter().map(Ident::as_str).collect();
        let found = targets.get(&Symbol::intern(&path.join("."))).copied();
        let Some(hi) = found else {
            diags.push(
                Diagnostic::new(
                    code!("BLS0200"),
                    format!(
                        "`{}` is not a relation of the spec's target (or an instance channel from an external role)",
                        path.join(".")
                    ),
                )
                .with_primary(f.head.span),
            );
            continue;
        };
        let Some(hrel) = hir.rels.get(hi) else { continue };
        let Some(&(ir, ref map)) = lowered.surface.get(hi) else {
            return Err(internal_error!("a target relation was not lowered").into());
        };
        let arity = protocol
            .get()
            .rels
            .get(ir)
            .map(|r| r.schema.cols.len())
            .ok_or_else(|| internal_error!("a target relation has no IR declaration"))?;
        let Some(mut row) = fact_row(&hir, hrel, map, arity, &f.head, &node_index, diags) else {
            continue;
        };
        if ingress_channel(&hir, hrel) {
            // A client session's message (LANGUAGE §18.4): `fact c(…) @ n from s at tick k`.
            let (Some(n), Some(k), Some(from)) = (&f.at, &f.tick, &f.from) else {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0405"),
                        "a message from an external role names its node, session and tick: `@ n from s at tick k`",
                    )
                    .with_primary(f.span),
                );
                continue;
            };
            let (Some(node), Some(tick), Some(session)) = (node_const(n, &node_index), tick_const(k), tick_const(from))
            else {
                diags.push(
                    Diagnostic::new(
                        code!("BLS0300"),
                        "`@ n` names a spec node, `from s` a session number and `at tick k` a tick",
                    )
                    .with_primary(f.span),
                );
                continue;
            };
            if let Some(dest) = row.first_mut() {
                *dest = Value::Node(node);
            }
            ingress.push(IngressFact {
                node,
                tick: Tick(tick),
                rel: ir,
                session: blossom_value::value::SessionId(session),
                row,
            });
            continue;
        }
        if let Some(from) = &f.from {
            diags.push(
                Diagnostic::new(
                    code!("BLS0405"),
                    "`from s` names the session of a message from an external role",
                )
                .with_primary(from.span),
            );
            continue;
        }
        let row = row;
        let at: Vec<NodeId> = match &f.at {
            None => (0..names.len() as u32).map(NodeId).collect(),
            Some(e) => match node_const(e, &node_index) {
                Some(n) => vec![n],
                None => {
                    diags.push(Diagnostic::new(code!("BLS0200"), "`@ n` names a spec node").with_primary(e.span));
                    continue;
                }
            },
        };
        match (&hrel.kind, &f.tick) {
            (HRelKind::Static, None) => {
                for node in at {
                    statics.push(NodeStatic {
                        node,
                        rel: ir,
                        row: row.clone(),
                    });
                }
            }
            (HRelKind::Input { root: true }, Some(k)) => {
                let Some(tick) = tick_const(k) else {
                    diags.push(Diagnostic::new(code!("BLS0300"), "`at tick k` needs a tick").with_primary(k.span));
                    continue;
                };
                for node in at {
                    inputs.push(InputFact {
                        node,
                        tick: Tick(tick),
                        rel: ir,
                        row: row.clone(),
                    });
                }
            }
            _ => diags.push(
                Diagnostic::new(
                    code!("BLS0405"),
                    "a spec fact asserts a row of a static relation (`@ n`) or an input event (`@ n at tick k`)",
                )
                .with_primary(f.span),
            ),
        }
    }
    if diags.has_errors() {
        return Ok(None);
    }

    // The spec program over trace relations.
    let spec_program = compile_views(
        tree,
        sources,
        diags,
        &hir,
        &targets,
        &node_index,
        &names,
        &roles,
        &m.views,
        target_spec,
    )?;
    let Some((spec_hir, spec_lowered, mode)) = spec_program else {
        return Ok(None);
    };
    let ldfi = m.checks.iter().any(|(k, _, _)| k.as_str() == "ldfi");
    let find = |n: &str| -> Option<usize> {
        spec_hir
            .rels
            .iter()
            .position(|r| r.name.segments().len() == 1 && r.name.to_string() == n && r.kind == HRelKind::View)
    };
    let outcome = match (find("pre"), find("post")) {
        (Some(pre), Some(post)) => {
            let types = |i: usize| {
                spec_hir
                    .rels
                    .get(i)
                    .map(|r| r.cols.iter().map(|c| c.ty).collect::<Vec<_>>())
            };
            if types(pre) != types(post) {
                diags.push(
                    Diagnostic::new(code!("BLS0900"), "`pre` and `post` have different schemas (CR-30)")
                        .with_primary(spec.span),
                );
                return Ok(None);
            }
            let (Some(pre), Some(post)) = (
                spec_lowered.rels.get(pre).copied(),
                spec_lowered.rels.get(post).copied(),
            ) else {
                return Err(internal_error!("pre or post was not lowered").into());
            };
            Some((pre, post))
        }
        _ if ldfi => {
            diags.push(
                Diagnostic::new(code!("BLS0900"), "`check ldfi` needs views `pre` and `post` (CR-30)")
                    .with_primary(spec.span),
            );
            return Ok(None);
        }
        _ => None,
    };
    if ldfi && faults.is_none() {
        diags.push(Diagnostic::new(code!("BLS0900"), "`check ldfi` needs `faults` (CR-30)").with_primary(spec.span));
        return Ok(None);
    }

    // Logical relations: the protocol's IR relations one for one, then the spec's, then the crash oracle.
    let p = protocol.get();
    let mut rels: Vec<LogicalRel> = p
        .rels
        .iter()
        .map(|r| LogicalRel {
            name: Symbol::intern(&r.name.to_string()),
            arity: r.schema.cols.len(),
            kind: LogicalKind::Protocol,
            protocol: Some(r.id),
            channel: None,
            input: None,
            spec: None,
            spec_at: Vec::new(),
        })
        .collect();
    let mut feeds = Vec::new();
    let mut fed: BTreeMap<RelId, LogicalIdx> = BTreeMap::new();
    for ((tname, time), trace) in &mode.traces {
        let (Some(&hi), Some(spec_rel)) = (targets.get(tname), spec_lowered.rels.get(trace.index()).copied()) else {
            return Err(internal_error!("a trace relation without its target").into());
        };
        let Some(&(ir, _)) = lowered.surface.get(hi) else {
            return Err(internal_error!("a target relation was not lowered").into());
        };
        let logical = LogicalIdx(ir.index() as u32);
        match time {
            None => {
                feeds.push(SpecFeed::AtEot {
                    spec: spec_rel,
                    rel: logical,
                });
                if let Some(r) = rels.get_mut(ir.index()) {
                    r.spec = Some(spec_rel);
                }
            }
            Some(k) => {
                feeds.push(SpecFeed::AtTick {
                    spec: spec_rel,
                    rel: logical,
                    tick: Tick(*k),
                });
                if let Some(r) = rels.get_mut(ir.index()) {
                    r.spec_at.push((Tick(*k), spec_rel));
                }
            }
        }
        fed.insert(spec_rel, logical);
    }
    let crashed_rel = match mode.crashed {
        Some(c) => {
            let Some(spec_rel) = spec_lowered.rels.get(c.index()).copied() else {
                return Err(internal_error!("the crashed oracle was not lowered").into());
            };
            feeds.push(SpecFeed::Crashed { spec: spec_rel });
            let idx = LogicalIdx(rels.len() as u32);
            rels.push(LogicalRel {
                name: Symbol::intern("crashed"),
                arity: 1,
                kind: LogicalKind::Crash,
                protocol: None,
                channel: None,
                input: None,
                spec: Some(spec_rel),
                spec_at: Vec::new(),
            });
            fed.insert(spec_rel, idx);
            Some(spec_rel)
        }
        None => None,
    };
    let sp = spec_lowered.program.get();
    let mut spec_logical: BTreeMap<RelId, LogicalIdx> = fed.clone();
    for r in sp.rels.iter() {
        if fed.contains_key(&r.id) || Some(r.id) == crashed_rel {
            continue;
        }
        let idx = LogicalIdx(rels.len() as u32);
        rels.push(LogicalRel {
            name: Symbol::intern(&r.name.to_string()),
            arity: r.schema.cols.len(),
            kind: LogicalKind::Spec,
            protocol: None,
            channel: None,
            input: None,
            spec: Some(r.id),
            spec_at: Vec::new(),
        });
        spec_logical.insert(r.id, idx);
    }
    // Edges: every body atom of every rule, protocol and spec.
    let mut edges = Vec::new();
    for rule in p.rules.iter() {
        let to = LogicalIdx(rule.head.rel.index() as u32);
        for lit in &rule.body.lits {
            if let Literal::Pos(a) | Literal::Neg(a) = lit {
                edges.push(LogicalEdge {
                    from: LogicalIdx(a.rel.index() as u32),
                    to,
                    time: edge_time(&rule.kind),
                    negated: matches!(lit, Literal::Neg(_)),
                });
            }
        }
    }
    for rule in sp.rules.iter() {
        let Some(&to) = spec_logical.get(&rule.head.rel) else {
            continue;
        };
        for lit in &rule.body.lits {
            if let Literal::Pos(a) | Literal::Neg(a) = lit
                && let Some(&from) = spec_logical.get(&a.rel)
            {
                edges.push(LogicalEdge {
                    from,
                    to,
                    time: EdgeTime::Deductive,
                    negated: matches!(lit, Literal::Neg(_)),
                });
            }
        }
    }
    edges.sort();
    edges.dedup();
    let halt = hir
        .rels
        .iter()
        .position(|r| r.kind == HRelKind::Halt)
        .and_then(|i| lowered.rels.get(i).copied());
    let artifact = SimArtifact {
        nodes: names.clone(),
        roles: roles.iter().map(|r| r.map(|r| RoleId::from_raw(r.0))).collect(),
        profile: Profile::Blossom { round },
        protocol,
        inputs,
        ingress,
        statics,
        halt,
        rels,
        edges,
        spec: outcome.map(|(pre, post)| OutcomeSpec {
            program: spec_lowered.program.clone(),
            pre,
            post,
            feeds,
        }),
        // A spec's runs are seeded with run seed 0; `check sim { seed }` will choose others (TEST-001).
        seed: blossom_value::Seed::from_u64(0),
    };
    let checks = m
        .checks
        .iter()
        .map(|(k, e, span)| Check {
            tool: k.name,
            expect: e.map(|e| e.as_str() == "holds"),
            span: *span,
        })
        .collect();
    for (k, e, span) in &m.checks {
        if let Some(e) = e
            && !matches!(e.as_str(), "holds" | "fails")
        {
            diags.push(Diagnostic::new(code!("BLS0900"), "`expect` is `holds` or `fails`").with_primary(e.span));
        }
        if k.as_str() != "ldfi" {
            let feature = match k.as_str() {
                "bmc" => "VER-002",
                "smt" => "VER-010",
                "sim" => "TEST-001",
                "asp" => "VER-003",
                _ => "TEST-020",
            };
            m.not_run.push((format!("`check {}` ({feature})", k.as_str()), *span));
        }
    }
    Ok(Some(CompiledSpec {
        name: Symbol::intern(name),
        artifact,
        faults,
        checks,
        not_run: m.not_run,
    }))
}

/// A compiled program root as the simulator runs it on its own: no spec, its IR relations as its logical relations.
pub fn sim_artifact(
    bls: blossom_artifact::bls::BlsArtifact,
    round: Duration,
    seed: blossom_value::Seed,
) -> SimArtifact {
    let p = bls.program.get();
    let rels = p
        .rels
        .iter()
        .map(|r| LogicalRel {
            name: Symbol::intern(&r.name.to_string()),
            arity: r.schema.cols.len(),
            kind: LogicalKind::Protocol,
            protocol: Some(r.id),
            channel: None,
            input: None,
            spec: None,
            spec_at: Vec::new(),
        })
        .collect();
    SimArtifact {
        nodes: bls.nodes.clone(),
        roles: bls.roles.clone(),
        profile: Profile::Blossom { round },
        protocol: bls.program.clone(),
        inputs: Vec::new(),
        ingress: Vec::new(),
        statics: Vec::new(),
        halt: bls.halt,
        rels,
        edges: Vec::new(),
        spec: None,
        seed,
    }
}

type SpecCompiled = Option<(Hir, crate::lower::Lowered, SpecMode)>;

/// The spec's views, resolved in spec mode over the target's relations, type-checked and lowered.
#[allow(clippy::too_many_arguments)]
fn compile_views(
    tree: &ModuleTree,
    sources: &SourceDb,
    diags: &mut Diagnostics,
    hir: &Hir,
    targets: &BTreeMap<Symbol, usize>,
    nodes: &BTreeMap<Symbol, u32>,
    names: &[Symbol],
    roles: &[Option<crate::hir::HRoleId>],
    views: &[&ast::ViewDecl],
    spec: &ast::SpecItem,
) -> Result<SpecCompiled, BlsError> {
    let mode = SpecMode {
        targets: targets
            .iter()
            .filter_map(|(n, i)| hir.rels.get(*i).map(|r| (*n, r.cols.clone())))
            .collect(),
        traces: BTreeMap::new(),
        crashed: None,
        nodes: nodes.clone(),
    };
    let items: Vec<ast::Item> = views
        .iter()
        .map(|v| ast::Item {
            attrs: Vec::new(),
            is_pub: false,
            kind: ItemKind::View((*v).clone()),
            span: v.span,
        })
        .collect();
    let name = spec.name.map_or(Symbol::intern("spec"), |n| n.name);
    let Some((mut spec_hir, mode)) = crate::resolve::resolve_spec_views(tree, sources, diags, name, hir, &items, mode)?
    else {
        return Ok(None);
    };
    crate::typeck::check(&mut spec_hir, diags)?;
    if diags.has_errors() {
        return Ok(None);
    }
    let lowered = crate::lower::lower(&spec_hir, &crate::lower::Deployment { nodes: names, roles })?;
    Ok(Some((spec_hir, lowered, mode)))
}

fn edge_time(kind: &RuleKind) -> EdgeTime {
    match kind {
        RuleKind::Deductive => EdgeTime::Deductive,
        RuleKind::Inductive => EdgeTime::Next,
        RuleKind::Async => EdgeTime::Async,
    }
}

fn is_screaming(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Whether some rule reads the clock.
fn reads_now(p: &Program) -> bool {
    fn expr(e: &Expr) -> bool {
        match e {
            Expr::Scalar(BuiltinScalar::Now) => true,
            Expr::Unary { arg, .. } => expr(arg),
            Expr::Binary { lhs, rhs, .. } => expr(lhs) || expr(rhs),
            Expr::If { cond, then, els } => expr(cond) || expr(then) || expr(els),
            Expr::Call { args, .. } | Expr::Construct { fields: args, .. } | Expr::Collection { elems: args, .. } => {
                args.iter().any(expr)
            }
            Expr::Field { base, .. } => expr(base),
            Expr::Match { scrut, arms } => {
                expr(scrut) || arms.iter().any(|(_, g, b)| g.as_ref().is_some_and(expr) || expr(b))
            }
            _ => false,
        }
    }
    p.rules.iter().any(|r| {
        r.body.lits.iter().any(|l| match l {
            Literal::Bind { expr: e, .. } | Literal::Guard(e) => expr(e),
            _ => false,
        })
    })
}

fn parse_faults(opts: &[(Ident, ast::Expr)], span: Span, diags: &mut Diagnostics) -> Option<Faults> {
    let mut eot = None;
    let mut eff = None;
    let mut crashes = None;
    for (k, e) in opts {
        let int = || match &e.kind {
            ExprKind::Lit(LitValue::Int { value, .. }) => u64::try_from(*value).ok(),
            _ => None,
        };
        match k.as_str() {
            "eot" => eot = int(),
            "eff" => eff = int(),
            "crashes" => crashes = int().and_then(|c| u32::try_from(c).ok()),
            "model" => {
                let model = match &e.kind {
                    ExprKind::Path(p, _) if p.len() == 1 => p.first().map(Ident::as_str),
                    _ => None,
                };
                if model != Some("sync") {
                    diags.push(
                        Diagnostic::not_implemented(
                            blossom_base::FeatureId("TEST-001"),
                            "the asynchronous fault model under LDFI",
                            "the Blossom frontend (slice 2)",
                        )
                        .with_primary(e.span),
                    );
                }
            }
            "round" => {}
            "delay" => diags.push(
                Diagnostic::not_implemented(
                    blossom_base::FeatureId("TEST-001"),
                    "`delay`",
                    "the Blossom frontend (slice 2)",
                )
                .with_primary(e.span),
            ),
            other => diags.push(
                Diagnostic::new(code!("BLS0900"), format!("unknown `faults` field `{other}`")).with_primary(k.span),
            ),
        }
    }
    match (eot, eff, crashes) {
        (Some(eot), Some(eff), Some(crashes)) => Some(Faults { eot, eff, crashes }),
        _ => {
            diags.push(
                Diagnostic::new(code!("BLS0900"), "`faults` needs integer `eot`, `eff` and `crashes`")
                    .with_primary(span),
            );
            None
        }
    }
}

fn duration(e: &ast::Expr) -> Option<Duration> {
    match &e.kind {
        ExprKind::Lit(LitValue::Duration(ns)) => i64::try_from(*ns).ok().filter(|n| *n > 0).map(Duration::from_nanos),
        _ => None,
    }
}

fn tick_const(e: &ast::Expr) -> Option<u64> {
    match &e.kind {
        ExprKind::Lit(LitValue::Int { value, .. }) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn node_const(e: &ast::Expr, nodes: &BTreeMap<Symbol, u32>) -> Option<NodeId> {
    match &e.kind {
        ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => {
            p.first().and_then(|i| nodes.get(&i.name)).map(|i| NodeId(*i))
        }
        _ => None,
    }
}

/// A fact's row, in IR column order, decoded by the target relation's column types.
/// Whether `r` is a channel whose source role is `external`: its messages come from client sessions.
fn ingress_channel(hir: &Hir, r: &crate::hir::HRel) -> bool {
    match &r.kind {
        HRelKind::Channel(ch) => ch
            .direction
            .and_then(|(src, _)| hir.roles.get(src.index()))
            .is_some_and(|role| role.kind == crate::hir::RoleKind::External),
        _ => false,
    }
}

/// A fact's row in IR column order (`arity` columns; a column the surface lacks, a channel's destination, is `()`
/// until filled).
fn fact_row(
    hir: &Hir,
    rel: &crate::hir::HRel,
    map: &[usize],
    arity: usize,
    head: &ast::Head,
    nodes: &BTreeMap<Symbol, u32>,
    diags: &mut Diagnostics,
) -> Option<Vec<Value>> {
    let args: Vec<&ast::Expr> = head
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Pos(e) => Some(e),
            _ => None,
        })
        .collect();
    if args.len() != head.args.len() || args.len() != rel.cols.len() {
        diags.push(
            Diagnostic::new(
                code!("BLS0301"),
                format!("`{}` has {} column(s)", rel.name, rel.cols.len()),
            )
            .with_primary(head.span),
        );
        return None;
    }
    let mut row = vec![Value::Unit; arity];
    for ((e, col), ir) in args.iter().zip(&rel.cols).zip(map) {
        let ty = col.ty?;
        let Some(v) = const_of(hir, e, ty, nodes) else {
            diags.push(
                Diagnostic::new(
                    code!("BLS0300"),
                    format!("this value does not fit column `{}`", col.name),
                )
                .with_primary(e.span),
            );
            return None;
        };
        if let Some(slot) = row.get_mut(*ir) {
            *slot = v;
        }
    }
    Some(row)
}

/// A scenario constant of type `ty`: a literal, a spec node name, a tuple of those.
fn const_of(hir: &Hir, e: &ast::Expr, ty: blossom_base::TypeId, nodes: &BTreeMap<Symbol, u32>) -> Option<Value> {
    let def = hir.types.get(ty)?;
    match (&e.kind, def) {
        (ExprKind::Lit(LitValue::Int { value, .. }), TypeDef::Int(t)) => {
            IntValue::from_i128(*t, i128::try_from(*value).ok()?).map(Value::Int)
        }
        (ExprKind::Prefix { op: PrefixOp::Neg, arg }, TypeDef::Int(t)) => match &arg.kind {
            ExprKind::Lit(LitValue::Int { value, .. }) => {
                IntValue::from_i128(*t, -i128::try_from(*value).ok()?).map(Value::Int)
            }
            _ => None,
        },
        (ExprKind::Lit(LitValue::Str(s)), TypeDef::Str) => Some(Value::Str(s.as_str().into())),
        (ExprKind::Lit(LitValue::Str(s)), TypeDef::Node(_)) => {
            nodes.get(&Symbol::intern(s)).map(|i| Value::Node(NodeId(*i)))
        }
        (ExprKind::Lit(LitValue::Bool(b)), TypeDef::Bool) => Some(Value::Bool(*b)),
        (ExprKind::Lit(LitValue::Duration(ns)), TypeDef::Duration) => {
            Some(Value::Duration(Duration::from_nanos(i64::try_from(*ns).ok()?)))
        }
        (ExprKind::Path(..), TypeDef::Node(_)) => node_const(e, nodes).map(Value::Node),
        (ExprKind::Tuple(es), TypeDef::Tuple(ts)) if es.len() == ts.len() => {
            let ts = ts.clone();
            let vs: Option<Vec<Value>> = es.iter().zip(ts).map(|(x, t)| const_of(hir, x, t, nodes)).collect();
            Some(Value::Tuple(vs?.into()))
        }
        _ => None,
    }
}
