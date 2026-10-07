//! Guard elimination and transitive reachability for a single runtime role.
use crate::{
    IrError, ValidatedProgram,
    core::*,
    visit::{Mapper, Remap},
};
use blossom_base::{IndexVec, idx::*};
use blossom_value::TypeTable;
use std::collections::BTreeSet;
#[derive(Default)]
struct Needed {
    types: BTreeSet<TypeId>,
    lattices: BTreeSet<LatticeTypeId>,
    groups: BTreeSet<GroupTypeId>,
    consts: BTreeSet<ConstId>,
    params: BTreeSet<ParamId>,
    fns: BTreeSet<FnId>,
    udas: BTreeSet<UdaId>,
    services: BTreeSet<ServiceId>,
    roles: BTreeSet<RoleId>,
    rels: BTreeSet<RelId>,
    rules: BTreeSet<RuleId>,
    constructs: BTreeSet<ConstructId>,
    sites: BTreeSet<SiteId>,
    invariants: BTreeSet<InvariantId>,
}
macro_rules! needed {($($method:ident:$field:ident:$ty:ident),*)=>{$(fn $method(&mut self,id:$ty)->$ty{self.$field.insert(id);id})*};}
impl Mapper for Needed {
    needed!(typeid:types:TypeId,latticetypeid:lattices:LatticeTypeId,grouptypeid:groups:GroupTypeId,constid:consts:ConstId,paramid:params:ParamId,fnid:fns:FnId,udaid:udas:UdaId,serviceid:services:ServiceId,roleid:roles:RoleId,relid:rels:RelId,ruleid:rules:RuleId,constructid:constructs:ConstructId,siteid:sites:SiteId,invariantid:invariants:InvariantId);
}
struct Numbering {
    types: Vec<Option<TypeId>>,
    lattices: Vec<Option<LatticeTypeId>>,
    groups: Vec<Option<GroupTypeId>>,
    consts: Vec<Option<ConstId>>,
    params: Vec<Option<ParamId>>,
    fns: Vec<Option<FnId>>,
    udas: Vec<Option<UdaId>>,
    services: Vec<Option<ServiceId>>,
    roles: Vec<Option<RoleId>>,
    rels: Vec<Option<RelId>>,
    rules: Vec<Option<RuleId>>,
    constructs: Vec<Option<ConstructId>>,
    sites: Vec<Option<SiteId>>,
    invariants: Vec<Option<InvariantId>>,
}
macro_rules! number {($($method:ident:$field:ident:$ty:ident),*)=>{$(fn $method(&mut self,id:$ty)->$ty{self.$field.get(id.index()).and_then(|x|*x).unwrap_or(id)})*};}
impl Mapper for Numbering {
    number!(typeid:types:TypeId,latticetypeid:lattices:LatticeTypeId,grouptypeid:groups:GroupTypeId,constid:consts:ConstId,paramid:params:ParamId,fnid:fns:FnId,udaid:udas:UdaId,serviceid:services:ServiceId,roleid:roles:RoleId,relid:rels:RelId,ruleid:rules:RuleId,constructid:constructs:ConstructId,siteid:sites:SiteId,invariantid:invariants:InvariantId);
}
fn renumber<I: Idx>(len: usize, selected: &BTreeSet<I>) -> Vec<Option<I>> {
    let mut out = vec![None; len];
    for (n, id) in selected.iter().enumerate() {
        if let Some(slot) = out.get_mut(id.index()) {
            *slot = Some(I::from_raw(n as u32));
        }
    }
    out
}
fn select<I: Idx, T: Clone>(table: &IndexVec<I, T>, ids: &BTreeSet<I>) -> Result<IndexVec<I, T>, IrError> {
    IndexVec::try_from_iter(ids.iter().filter_map(|id| table.get(*id).cloned()))
        .map_err(|e| IrError::builder(e.to_string()))
}
impl Needed {
    fn size(&self) -> usize {
        self.types.len()
            + self.lattices.len()
            + self.groups.len()
            + self.consts.len()
            + self.params.len()
            + self.fns.len()
            + self.udas.len()
            + self.services.len()
            + self.roles.len()
            + self.rels.len()
            + self.rules.len()
            + self.constructs.len()
            + self.sites.len()
            + self.invariants.len()
    }
}
/// Projects a valid choreographic program to the data and rules a role executes.
pub(crate) fn project(valid: &ValidatedProgram, role: RoleId) -> Result<ValidatedProgram, IrError> {
    let p = valid.get();
    if !p.migrations.is_empty() {
        return Err(blossom_base::unimplemented_error!(
            "LANG-262",
            "projecting migration rules requires the M6.5 migration lowering"
        )
        .into());
    }
    if !p.translations.is_empty() {
        return Err(blossom_base::unimplemented_error!(
            "LANG-263",
            "projecting channel translations requires the M6.5 translation lowering"
        )
        .into());
    }
    if p.roles.get(role).is_none() {
        return Err(IrError::builder(format!("unknown projection role {role:?}")));
    }
    let mut want = Needed::default();
    want.roles.insert(role);
    // Channel endpoints exist even when no local handler mentions the channel.
    // The runtime still needs the source's egress and destination's ingress schema.
    for (id, rel) in p.rels.iter_enumerated() {
        if let RelClass::Channel(ch) = &rel.class
            && let ChannelForm::Direction { src, dst } = ch.form
            && (src == role || dst == role)
        {
            want.rels.insert(id);
        }
    }
    for (id, r) in p.rules.iter_enumerated() {
        if r.role == Some(role) {
            want.rules.insert(id);
        }
    }
    // The selected program is the least closed set of typed references reachable from guarded rules.
    loop {
        let old = want.size();
        for id in want.rules.clone() {
            let r = p
                .rules
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing rule"))?;
            if r.role != Some(role) {
                return Err(IrError::builder(format!(
                    "construct crosses the projection role at {}",
                    r.label.text
                )));
            }
            r.remap(&mut want);
        }
        for id in want.rels.clone() {
            p.rels
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing relation"))?
                .remap(&mut want);
        }
        for id in want.constructs.clone() {
            let mut construct = p
                .constructs
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing construct"))?
                .clone();
            construct.rules.retain(|id| want.rules.contains(id));
            construct.remap(&mut want);
        }
        for id in want.sites.clone() {
            p.sites
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing site"))?
                .remap(&mut want);
        }
        for id in want.types.clone() {
            p.types
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing type"))?
                .remap(&mut want);
        }
        for id in want.lattices.clone() {
            p.lattices
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing lattice"))?
                .remap(&mut want);
        }
        for id in want.groups.clone() {
            p.groups
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing group"))?
                .remap(&mut want);
        }
        for id in want.params.clone() {
            p.params
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing param"))?
                .remap(&mut want);
        }
        for id in want.fns.clone() {
            p.fns
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing function"))?
                .remap(&mut want);
        }
        for id in want.udas.clone() {
            p.udas
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing aggregate"))?
                .remap(&mut want);
        }
        for id in want.services.clone() {
            p.services
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing service"))?
                .remap(&mut want);
        }
        for id in want.invariants.clone() {
            p.invariants
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing invariant"))?
                .remap(&mut want);
        }
        for id in want.rels.clone() {
            for f in p.facts.iter().filter(|f| f.rel == id) {
                f.remap(&mut want);
            }
        }
        for id in want.roles.clone() {
            p.roles
                .get(id)
                .ok_or_else(|| IrError::builder("projection references missing role"))?
                .remap(&mut want);
        }
        for (id, r) in p.rels.iter_enumerated() {
            if r.name.to_string().ends_with("$members")
                && want.roles.iter().any(|role| {
                    p.roles
                        .get(*role)
                        .is_some_and(|rd| r.name.to_string().starts_with(&rd.name.to_string()))
                })
            {
                want.rels.insert(id);
            }
        }
        if old == want.size() {
            break;
        }
    }
    // The validator derives the types of expressions structurally (a node, a tuple of kept types, …) and looks them
    // up, so every anonymous type over what is kept stays too. Named types (structs, enums, lattices, groups, extern
    // types) stay only where something kept names them.
    loop {
        let mut grew = false;
        for (id, def) in p.types.iter() {
            if want.types.contains(&id) {
                continue;
            }
            let kept = |t: &TypeId| want.types.contains(t);
            let anonymous = match def {
                blossom_value::TypeDef::Node(Some(r)) => want.roles.contains(r),
                blossom_value::TypeDef::Tuple(ts) => ts.iter().all(kept),
                blossom_value::TypeDef::Vec(t) | blossom_value::TypeDef::Set(t) | blossom_value::TypeDef::Option(t) => {
                    kept(t)
                }
                blossom_value::TypeDef::Map(k, v) => kept(k) && kept(v),
                blossom_value::TypeDef::Struct(_)
                | blossom_value::TypeDef::Enum(_)
                | blossom_value::TypeDef::Lattice(_)
                | blossom_value::TypeDef::Group(_)
                | blossom_value::TypeDef::Extern(_) => false,
                _ => true,
            };
            if anonymous {
                want.types.insert(id);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
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
    out.roles = select(&p.roles, &want.roles)?.remap(&mut n);
    out.rels = select(&p.rels, &want.rels)?.remap(&mut n);
    out.rules = select(&p.rules, &want.rules)?.remap(&mut n);
    let mut constructs = select(&p.constructs, &want.constructs)?;
    for (_, construct) in constructs.iter_enumerated_mut() {
        construct.rules.retain(|id| want.rules.contains(id));
    }
    out.constructs = constructs.remap(&mut n);
    out.sites = select(&p.sites, &want.sites)?.remap(&mut n);
    out.lattices = select(&p.lattices, &want.lattices)?.remap(&mut n);
    out.groups = select(&p.groups, &want.groups)?.remap(&mut n);
    out.consts = select(&p.consts, &want.consts)?;
    out.params = select(&p.params, &want.params)?.remap(&mut n);
    out.fns = select(&p.fns, &want.fns)?.remap(&mut n);
    out.udas = select(&p.udas, &want.udas)?.remap(&mut n);
    out.services = select(&p.services, &want.services)?.remap(&mut n);
    out.invariants = select(&p.invariants, &want.invariants)?.remap(&mut n);
    let mut types = TypeTable::new();
    for (id, def) in p.types.iter() {
        if want.types.contains(&id) {
            types
                .insert(def.remap(&mut n))
                .map_err(|e| IrError::builder(e.to_string()))?;
        }
    }
    out.types = types;
    for (_, r) in out.rules.iter_enumerated_mut() {
        r.role = None;
    }
    for (_, r) in out.rels.iter_enumerated_mut() {
        if matches!(r.placement, Placement::Role(_)) {
            r.placement = Placement::Shared;
        }
    }
    out.facts = p
        .facts
        .iter()
        .filter(|f| want.rels.contains(&f.rel))
        .cloned()
        .collect::<Vec<_>>()
        .remap(&mut n);
    out.migrations = p.migrations.remap(&mut n);
    out.translations = p.translations.remap(&mut n);
    ValidatedProgram::validate(out).map_err(|mut errs| errs.remove(0))
}
