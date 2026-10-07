//! A query over a node's database (docs/design/DATABASE.md §5): from a program compiled with the query as a view, the
//! least program that computes the view from the durable relations it reads.
//!
//! The view's rules are kept, and the rules of every view they read, transitively; whatever else they read must be
//! a durable relation (the database) or a static one (the deployment's facts). Each durable relation becomes an
//! input of the query program, so one tick of the oracle, given the durable rows as of a tick as that tick's events,
//! computes the view.

use std::collections::BTreeSet;

use blossom_base::idx::*;
use blossom_value::TypeTable;

use crate::projection::{Needed, Numbering, renumber, select};
use crate::visit::Remap;
use crate::{IrError, ValidatedProgram, core::*};

/// The query program for the view `view` as a node of `role` computes it (its rules placed there or nowhere), and the
/// durable relations it reads (by name, as its inputs).
pub(crate) fn query(
    valid: &ValidatedProgram,
    view: RelId,
    role: Option<RoleId>,
) -> Result<(ValidatedProgram, Vec<String>), IrError> {
    let p = valid.get();
    if p.rels.get(view).is_none() {
        return Err(IrError::builder(format!("no relation {view:?} to query")));
    }
    // Derived in the tick, by rules alone: what the query computes rather than reads.
    let derived =
        |r: &RelDecl| matches!(r.class, RelClass::Idb) && matches!(r.persistence, Persistence::None) && !r.durable;
    let mut want = Needed::default();
    want.rels.insert(view);
    loop {
        let old = want.size();
        for id in want.rels.clone() {
            let r = p
                .rels
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing relation"))?;
            if derived(r) {
                for (rule, rd) in p.rules.iter_enumerated() {
                    if rd.head.rel == id && rd.role.is_none_or(|r| Some(r) == role) {
                        want.rules.insert(rule);
                    }
                }
                r.remap(&mut want);
            } else {
                // A relation read, not computed: only its columns' types come along.
                for c in &r.schema.cols {
                    want.types.insert(c.ty);
                }
            }
        }
        for id in want.rules.clone() {
            p.rules
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing rule"))?
                .remap(&mut want);
        }
        for id in want.constructs.clone() {
            let mut construct = p
                .constructs
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing construct"))?
                .clone();
            construct.rules.retain(|id| want.rules.contains(id));
            construct.rels.retain(|id| want.rels.contains(id));
            construct.remap(&mut want);
        }
        for id in want.sites.clone() {
            p.sites
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing site"))?
                .remap(&mut want);
        }
        for id in want.types.clone() {
            p.types
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing type"))?
                .remap(&mut want);
        }
        for id in want.lattices.clone() {
            p.lattices
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing lattice"))?
                .remap(&mut want);
        }
        for id in want.groups.clone() {
            p.groups
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing group"))?
                .remap(&mut want);
        }
        for id in want.params.clone() {
            p.params
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing param"))?
                .remap(&mut want);
        }
        for id in want.fns.clone() {
            p.fns
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing function"))?
                .remap(&mut want);
        }
        for id in want.udas.clone() {
            p.udas
                .get(id)
                .ok_or_else(|| IrError::builder("a query references a missing aggregate"))?
                .remap(&mut want);
        }
        for id in want.rels.clone() {
            for f in p.facts.iter().filter(|f| f.rel == id) {
                f.remap(&mut want);
            }
        }
        if old == want.size() {
            break;
        }
    }
    // A query is one tick: a rule that derives at the next tick, or sends, cannot contribute to it.
    for id in &want.rules {
        if let Some(r) = p.rules.get(*id)
            && r.kind != RuleKind::Deductive
        {
            return Err(IrError::builder(format!(
                "the query's views use `{}`, which derives at a later tick: a query is answered within one tick",
                r.label.text
            )));
        }
    }
    // What the query reads must be in the database (or the deployment's facts).
    let mut inputs = Vec::new();
    let mut refused = Vec::new();
    for id in &want.rels {
        let Some(r) = p.rels.get(*id) else { continue };
        if derived(r) {
            continue;
        }
        match (&r.class, r.durable) {
            (RelClass::Static, _) => {}
            (_, true) => inputs.push(r.name.to_string()),
            _ => refused.push(r.name.to_string()),
        }
    }
    if !refused.is_empty() {
        return Err(IrError::builder(format!(
            "the query reads {}, which {} not durable: only durable relations are in the database",
            refused.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", "),
            if refused.len() == 1 { "is" } else { "are" }
        )));
    }
    // Roles stay where types name them (`Node<R>`); no rule of the query is placed at one.
    for id in want.roles.clone() {
        p.roles
            .get(id)
            .ok_or_else(|| IrError::builder("a query references a missing role"))?
            .remap(&mut want);
    }
    crate::projection::keep_anonymous_types(p, &mut want);
    want.invariants = BTreeSet::new();
    let mut n = Numbering {
        types: renumber(p.types.len(), &want.types),
        lattices: renumber(p.lattices.len(), &want.lattices),
        groups: renumber(p.groups.len(), &want.groups),
        consts: renumber(p.consts.len(), &want.consts),
        params: renumber(p.params.len(), &want.params),
        fns: renumber(p.fns.len(), &want.fns),
        udas: renumber(p.udas.len(), &want.udas),
        services: renumber(p.services.len(), &want.services),
        roles: renumber(p.roles.len(), &want.roles),
        rels: renumber(p.rels.len(), &want.rels),
        rules: renumber(p.rules.len(), &want.rules),
        constructs: renumber(p.constructs.len(), &want.constructs),
        sites: renumber(p.sites.len(), &want.sites),
        invariants: renumber(p.invariants.len(), &want.invariants),
    };
    let mut out = Program::new(p.meta.clone());
    // The durable relations read become inputs: rows given as the query tick's events.
    let mut rels = select(&p.rels, &want.rels)?;
    for (_, r) in rels.iter_enumerated_mut() {
        if !derived(r) && r.durable {
            r.class = RelClass::Event(EventSource::Input);
            r.persistence = Persistence::None;
            r.durable = false;
            r.interface = None;
            r.origin = Origin::User(r.span);
        }
        r.placement = Placement::Shared;
    }
    out.rels = rels.remap(&mut n);
    let mut rules = select(&p.rules, &want.rules)?;
    for (_, r) in rules.iter_enumerated_mut() {
        r.role = None;
    }
    out.rules = rules.remap(&mut n);
    let mut constructs = select(&p.constructs, &want.constructs)?;
    for (_, c) in constructs.iter_enumerated_mut() {
        c.rules.retain(|id| want.rules.contains(id));
        c.rels.retain(|id| want.rels.contains(id));
    }
    out.constructs = constructs.remap(&mut n);
    out.sites = select(&p.sites, &want.sites)?.remap(&mut n);
    out.lattices = select(&p.lattices, &want.lattices)?.remap(&mut n);
    out.groups = select(&p.groups, &want.groups)?.remap(&mut n);
    out.consts = select(&p.consts, &want.consts)?;
    out.params = select(&p.params, &want.params)?.remap(&mut n);
    out.fns = select(&p.fns, &want.fns)?.remap(&mut n);
    out.udas = select(&p.udas, &want.udas)?.remap(&mut n);
    out.roles = select(&p.roles, &want.roles)?.remap(&mut n);
    out.services = select(&p.services, &want.services)?.remap(&mut n);
    let mut types = TypeTable::new();
    for (id, def) in p.types.iter() {
        if want.types.contains(&id) {
            types
                .insert(def.remap(&mut n))
                .map_err(|e| IrError::builder(e.to_string()))?;
        }
    }
    out.types = types;
    out.facts = p
        .facts
        .iter()
        .filter(|f| want.rels.contains(&f.rel))
        .cloned()
        .collect::<Vec<_>>()
        .remap(&mut n);
    let program = ValidatedProgram::validate(out).map_err(|mut errs| errs.remove(0))?;
    Ok((program, inputs))
}

/// Counts the occurrences of one relation in what it maps.
struct CountRel {
    rel: RelId,
    seen: usize,
}

impl crate::visit::Mapper for CountRel {
    fn relid(&mut self, id: RelId) -> RelId {
        if id == self.rel {
            self.seen += 1;
        }
        id
    }
}

/// See `ValidatedProgram::read_only_by_atoms`.
pub(crate) fn read_only_by_atoms(p: &Program, rel: RelId) -> bool {
    let mut count = CountRel { rel, seen: 0 };
    let mut atoms = 0;
    for r in p.rules.iter() {
        r.remap(&mut count);
        if r.head.rel == rel {
            atoms += 1;
        }
        for l in &r.body.lits {
            if let Literal::Pos(a) | Literal::Neg(a) = l
                && a.rel == rel
            {
                atoms += 1;
            }
        }
    }
    // Heads are counted with the atoms: a head writes the relation, it does not read it.
    count.seen == atoms
}
