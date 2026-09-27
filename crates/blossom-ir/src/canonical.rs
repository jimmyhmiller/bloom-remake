//! Source-order-independent canonical relabeling before BLAKE3 hashing.
use crate::{
    IrError,
    core::*,
    visit::{Mapper, Remap},
};
use blossom_base::{IndexVec, Span, idx::*};
use blossom_value::{TypeDef, TypeTable};
use std::collections::BTreeSet;
struct Map {
    types: Vec<Option<TypeId>>,
    lattices: Vec<LatticeTypeId>,
    groups: Vec<GroupTypeId>,
    consts: Vec<Option<ConstId>>,
    params: Vec<ParamId>,
    fns: Vec<FnId>,
    udas: Vec<UdaId>,
    services: Vec<ServiceId>,
    roles: Vec<RoleId>,
    rels: Vec<RelId>,
    rules: Vec<RuleId>,
    sites: Vec<SiteId>,
    constructs: Vec<ConstructId>,
    invariants: Vec<InvariantId>,
    vars: Vec<VarId>,
}
fn get<I: Idx>(v: &[I], id: I) -> I {
    v.get(id.index()).copied().unwrap_or(id)
}
macro_rules! map {($($f:ident:$method:ident:$t:ident),*)=>{$(fn $method(&mut self,id:$t)->$t{get(&self.$f,id)})*};}
impl Mapper for Map {
    map!(lattices:latticetypeid:LatticeTypeId,groups:grouptypeid:GroupTypeId,params:paramid:ParamId,fns:fnid:FnId,udas:udaid:UdaId,services:serviceid:ServiceId,roles:roleid:RoleId,rels:relid:RelId,rules:ruleid:RuleId,sites:siteid:SiteId,constructs:constructid:ConstructId,invariants:invariantid:InvariantId,vars:varid:VarId);
    fn typeid(&mut self, id: TypeId) -> TypeId {
        self.types.get(id.index()).and_then(|x| *x).unwrap_or(id)
    }
    fn constid(&mut self, id: ConstId) -> ConstId {
        self.consts.get(id.index()).and_then(|x| *x).unwrap_or(id)
    }
    fn span(&mut self, _span: Span) -> Span {
        Span::new(FileId::from_raw(0), 0, 0)
    }
}
fn order<I: Idx, T>(table: &IndexVec<I, T>, mut key: impl FnMut(&T) -> String) -> Vec<I> {
    let mut indices = table.iter_enumerated().map(|(id, v)| (id, key(v))).collect::<Vec<_>>();
    indices.sort_by(|a, b| a.1.cmp(&b.1));
    indices.into_iter().map(|(id, _)| id).collect()
}
fn inverse<I: Idx>(order: &[I]) -> Vec<I> {
    let mut map = vec![I::from_raw(0); order.len()];
    for (i, id) in order.iter().enumerate() {
        if let Some(slot) = map.get_mut(id.index()) {
            *slot = I::from_raw(i as u32)
        }
    }
    map
}
fn reorder<I: Idx, T: Clone>(table: &IndexVec<I, T>, order: &[I]) -> Result<IndexVec<I, T>, IrError> {
    IndexVec::try_from_iter(order.iter().filter_map(|id| table.get(*id).cloned()))
        .map_err(|e| IrError::builder(e.to_string()))
}
fn children(def: &TypeDef) -> Vec<TypeId> {
    match def {
        TypeDef::Tuple(v) => v.clone(),
        TypeDef::Struct(s) => s.fields.iter().map(|f| f.ty).collect(),
        TypeDef::Enum(e) => e.variants.iter().flat_map(|v| v.payload.iter().map(|f| f.ty)).collect(),
        TypeDef::Vec(t) | TypeDef::Set(t) | TypeDef::Option(t) => vec![*t],
        TypeDef::Map(k, v) => vec![*k, *v],
        _ => Vec::new(),
    }
}
fn group_key(p: &Program, id: GroupTypeId) -> String {
    let Some(g) = p.groups.get(id) else {
        return String::new();
    };
    let c = match &g.ctor {
        GroupCtor::Z => "Z".into(),
        GroupCtor::Zn(n) => format!("Zn({n})"),
        GroupCtor::ZSet(t) => format!("ZSet({})", type_key(p, *t)),
        GroupCtor::Tuple(items) => format!(
            "Tuple({:?})",
            items.iter().map(|x| group_key(p, *x)).collect::<Vec<_>>()
        ),
        GroupCtor::Map(k, v) => format!("Map({},{})", type_key(p, *k), group_key(p, *v)),
        GroupCtor::User {
            name,
            zero,
            add,
            neg,
            mul,
        } => format!(
            "User({name},{:?},{:?},{:?},{:?})",
            p.fns.get(*zero).map(|x| &x.name),
            p.fns.get(*add).map(|x| &x.name),
            p.fns.get(*neg).map(|x| &x.name),
            mul.and_then(|x| p.fns.get(x)).map(|x| &x.name)
        ),
    };
    format!("{c}|{}", g.ring)
}
fn type_key(p: &Program, id: TypeId) -> String {
    let Some(d) = p.types.get(id) else { return String::new() };
    let keys = children(d).iter().map(|t| type_key(p, *t)).collect::<Vec<_>>();
    match d {
        TypeDef::Node(role) => format!(
            "Node({})",
            role.and_then(|id| p.roles.get(id))
                .map_or(String::new(), |r| r.name.to_string())
        ),
        TypeDef::Lattice(id) => format!(
            "Lattice({})",
            p.lattices.get(*id).map_or(String::new(), |l| l.name.to_string())
        ),
        TypeDef::Group(id) => format!("Group({})", group_key(p, *id)),
        TypeDef::Struct(s) => format!(
            "Struct({},{:?},{:?},{keys:?})",
            s.name,
            s.reserved,
            s.fields
                .iter()
                .map(|f| (f.name, f.field_no, &f.default, f.since, f.deprecated, f.renamed_from))
                .collect::<Vec<_>>()
        ),
        TypeDef::Enum(e) => format!(
            "Enum({},{:?},{:?},{:?},{keys:?})",
            e.name,
            e.unknown,
            e.reserved,
            e.variants
                .iter()
                .map(|v| (
                    v.name,
                    v.number,
                    v.since,
                    v.payload
                        .iter()
                        .map(|f| (f.name, f.field_no, &f.default, f.since, f.deprecated, f.renamed_from))
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
        ),
        TypeDef::Tuple(_) => format!("Tuple({keys:?})"),
        TypeDef::Vec(_) => format!("Vec({keys:?})"),
        TypeDef::Set(_) => format!("Set({keys:?})"),
        TypeDef::Map(..) => format!("Map({keys:?})"),
        TypeDef::Option(_) => format!("Option({keys:?})"),
        _ => format!("{d:?}"),
    }
}
fn intern_type(p: &Program, id: TypeId, map: &mut Map, out: &mut TypeTable) -> Result<(), IrError> {
    if map.types.get(id.index()).and_then(|x| *x).is_some() {
        return Ok(());
    }
    let def = p
        .types
        .get(id)
        .ok_or_else(|| IrError::builder("unknown type during canonicalization"))?;
    for child in children(def) {
        intern_type(p, child, map, out)?
    }
    let mapped = def.remap(map);
    let new = out.insert(mapped).map_err(|e| IrError::builder(e.to_string()))?;
    if let Some(x) = map.types.get_mut(id.index()) {
        *x = Some(new)
    }
    Ok(())
}
struct ConstUse<'a> {
    map: &'a mut Vec<Option<ConstId>>,
    next: u32,
}
impl Mapper for ConstUse<'_> {
    fn constid(&mut self, id: ConstId) -> ConstId {
        if let Some(slot) = self.map.get_mut(id.index()) {
            if slot.is_none() {
                *slot = Some(ConstId::from_raw(self.next));
                self.next += 1;
            }
            slot.unwrap_or(id)
        } else {
            id
        }
    }
}
fn canonical_vars(rule: &Rule) -> Vec<VarId> {
    struct First {
        seen: BTreeSet<VarId>,
        order: Vec<VarId>,
    }
    impl Mapper for First {
        fn varid(&mut self, id: VarId) -> VarId {
            if self.seen.insert(id) {
                self.order.push(id)
            }
            id
        }
    }
    let mut f = First {
        seen: BTreeSet::new(),
        order: Vec::new(),
    };
    rule.head.remap(&mut f);
    rule.body.lits.remap(&mut f);
    for (id, _) in rule.body.vars.iter_enumerated() {
        if f.seen.insert(id) {
            f.order.push(id)
        }
    }
    f.order
}

fn canonical_embedded_rule(rule: &Rule, map: &mut Map) -> Result<Rule, IrError> {
    let order = canonical_vars(rule);
    map.vars = inverse(&order);
    let mut out = rule.remap(map);
    out.body.vars = reorder(&rule.body.vars, &order)?.remap(map);
    map.vars.clear();
    Ok(out)
}

/// Produces a deterministic representation of a validated core program.
pub(crate) fn canonical(p: &Program) -> Result<Program, IrError> {
    let mut c = p.clone();
    let rel_order = order(&p.rels, |r| r.name.to_string());
    let rule_order = order(&p.rules, |r| r.label.text.to_string());
    let role_order = order(&p.roles, |r| r.name.to_string());
    let lattice_order = order(&p.lattices, |l| l.name.to_string());
    let fn_order = order(&p.fns, |f| f.name.to_string());
    let site_order = order(&p.sites, |s| s.stable.to_string());
    let inv_order = order(&p.invariants, |i| i.name.to_string());
    let construct_order = order(&p.constructs, |c| {
        let first = c
            .rules
            .iter()
            .filter_map(|id| p.rules.get(*id))
            .map(|r| r.label.text.as_ref())
            .min()
            .unwrap_or("");
        let mut owned_rels = c
            .rels
            .iter()
            .filter_map(|id| p.rels.get(*id))
            .map(|r| r.name.to_string())
            .collect::<Vec<_>>();
        owned_rels.sort();
        let mut owned_rules = c
            .rules
            .iter()
            .filter_map(|id| p.rules.get(*id))
            .map(|r| r.label.text.to_string())
            .collect::<Vec<_>>();
        owned_rules.sort();
        format!(
            "{:?}|{}|{}|{first}|{:?}|{:?}",
            c.kind.name(),
            c.surface.label.map_or(String::new(), |s| s.as_str().to_string()),
            c.surface.module,
            owned_rels,
            owned_rules
        )
    });
    let group_order = order(&p.groups, |g| group_key(p, g.id));
    let param_order = order(&p.params, |x| x.name.to_string());
    let uda_order = order(&p.udas, |x| {
        p.fns.get(x.finish).map_or(String::new(), |f| f.name.to_string())
    });
    let service_order = order(&p.services, |x| x.name.to_string());
    let mut map = Map {
        types: vec![None; p.types.len()],
        lattices: inverse(&lattice_order),
        groups: inverse(&group_order),
        consts: vec![None; p.consts.len()],
        params: inverse(&param_order),
        fns: inverse(&fn_order),
        udas: inverse(&uda_order),
        services: inverse(&service_order),
        roles: inverse(&role_order),
        rels: inverse(&rel_order),
        rules: inverse(&rule_order),
        sites: inverse(&site_order),
        constructs: inverse(&construct_order),
        invariants: inverse(&inv_order),
        vars: Vec::new(),
    };
    let mut types = TypeTable::new();
    for id in &rel_order {
        if let Some(r) = p.rels.get(*id) {
            for col in &r.schema.cols {
                intern_type(p, col.ty, &mut map, &mut types)?
            }
        }
    }
    for id in &fn_order {
        if let Some(f) = p.fns.get(*id) {
            for (_, t) in &f.params {
                intern_type(p, *t, &mut map, &mut types)?
            }
            intern_type(p, f.ret, &mut map, &mut types)?
        }
    }
    let mut remaining = p.types.iter().map(|(id, _)| id).collect::<Vec<_>>();
    remaining.sort_by_cached_key(|id| type_key(p, *id));
    for id in remaining {
        intern_type(p, id, &mut map, &mut types)?
    }
    {
        let mut uses = ConstUse {
            map: &mut map.consts,
            next: 0,
        };
        for id in &rel_order {
            if let Some(r) = p.rels.get(*id) {
                r.remap(&mut uses);
            }
        }
        for id in &fn_order {
            if let Some(f) = p.fns.get(*id) {
                f.remap(&mut uses);
            }
        }
        for id in &rule_order {
            if let Some(r) = p.rules.get(*id) {
                r.remap(&mut uses);
            }
        }
        let mut facts = p.facts.iter().collect::<Vec<_>>();
        facts.sort_by(|a, b| {
            p.rels
                .get(a.rel)
                .map(|r| r.name.clone())
                .cmp(&p.rels.get(b.rel).map(|r| r.name.clone()))
                .then_with(|| {
                    a.row
                        .iter()
                        .filter_map(|id| p.consts.get(*id))
                        .cmp(b.row.iter().filter_map(|id| p.consts.get(*id)))
                })
        });
        for f in facts {
            f.remap(&mut uses);
        }
        for id in &construct_order {
            if let Some(x) = p.constructs.get(*id) {
                x.remap(&mut uses);
            }
        }
    }
    let mut unused = p
        .consts
        .iter_enumerated()
        .filter(|(id, _)| map.consts.get(id.index()).is_some_and(|x| x.is_none()))
        .map(|(id, v)| (id, v.clone()))
        .collect::<Vec<_>>();
    unused.sort_by(|a, b| a.1.cmp(&b.1));
    let mut next = map.consts.iter().flatten().count() as u32;
    for (id, _) in unused {
        if let Some(x) = map.consts.get_mut(id.index()) {
            *x = Some(ConstId::from_raw(next));
            next += 1;
        }
    }
    c.roles = reorder(&p.roles, &role_order)?;
    c.rels = reorder(&p.rels, &rel_order)?;
    c.rules = reorder(&p.rules, &rule_order)?;
    c.lattices = reorder(&p.lattices, &lattice_order)?;
    c.groups = reorder(&p.groups, &group_order)?;
    c.fns = reorder(&p.fns, &fn_order)?;
    c.sites = reorder(&p.sites, &site_order)?;
    c.constructs = reorder(&p.constructs, &construct_order)?;
    c.invariants = reorder(&p.invariants, &inv_order)?;
    c.params = reorder(&p.params, &param_order)?;
    c.udas = reorder(&p.udas, &uda_order)?;
    c.services = reorder(&p.services, &service_order)?;
    let mut const_order = p.consts.iter_enumerated().map(|(id, _)| id).collect::<Vec<_>>();
    const_order.sort_by_key(|id| map.consts.get(id.index()).and_then(|x| *x));
    c.consts = reorder(&p.consts, &const_order)?;
    c.types = types;
    // Variables have a rule-local id space; each rule gets its own first-occurrence numbering.
    let mut mapped = c.clone();
    for (id, rule) in c.rules.iter_enumerated() {
        let old = p
            .rules
            .get(
                *rule_order
                    .get(id.index())
                    .ok_or_else(|| IrError::builder("missing ordered rule"))?,
            )
            .ok_or_else(|| IrError::builder("missing rule"))?;
        let order = canonical_vars(old);
        map.vars = inverse(&order);
        let mut transformed = rule.remap(&mut map);
        transformed.body.vars = reorder(&rule.body.vars, &order)?.remap(&mut map);
        if let Some(dst) = mapped.rules.get_mut(id) {
            *dst = transformed
        }
    }
    map.vars.clear();
    mapped.meta = mapped.meta.remap(&mut map);
    mapped.lattices = c.lattices.remap(&mut map);
    mapped.groups = c.groups.remap(&mut map);
    mapped.params = c.params.remap(&mut map);
    mapped.fns = c.fns.remap(&mut map);
    mapped.udas = c.udas.remap(&mut map);
    mapped.services = c.services.remap(&mut map);
    mapped.roles = c.roles.remap(&mut map);
    mapped.rels = c.rels.remap(&mut map);
    mapped.facts = c.facts.remap(&mut map);
    mapped.constructs = c.constructs.remap(&mut map);
    mapped.sites = c.sites.remap(&mut map);
    mapped.invariants = c.invariants.remap(&mut map);
    mapped.migrations = Vec::new();
    for migration in &c.migrations {
        let mut item = migration.remap(&mut map);
        item.rules = Vec::new();
        let mut ordered = migration.rules.iter().collect::<Vec<_>>();
        ordered.sort_by(|a, b| a.label.text.cmp(&b.label.text));
        for rule in ordered {
            item.rules.push(canonical_embedded_rule(rule, &mut map)?)
        }
        mapped.migrations.push(item);
    }
    mapped.migrations.sort_by_key(|m| m.from);
    mapped.translations = Vec::new();
    for translation in &c.translations {
        let mut item = translation.remap(&mut map);
        item.rules = Vec::new();
        let mut ordered = translation.rules.iter().collect::<Vec<_>>();
        ordered.sort_by(|a, b| a.label.text.cmp(&b.label.text));
        for rule in ordered {
            item.rules.push(canonical_embedded_rule(rule, &mut map)?)
        }
        mapped.translations.push(item);
    }
    mapped.translations.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| format!("{:?}", a.direction).cmp(&format!("{:?}", b.direction)))
    });
    mapped
        .facts
        .sort_by(|a, b| a.rel.cmp(&b.rel).then_with(|| a.row.cmp(&b.row)));
    for (_, c) in mapped.constructs.iter_enumerated_mut() {
        c.rules.sort();
        c.rels.sort();
    }
    Ok(mapped)
}
