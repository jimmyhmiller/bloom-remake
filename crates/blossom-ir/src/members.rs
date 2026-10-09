//! Keyed members by the node ids a host gave them (docs/design/KEYED.md §3).
//!
//! A keyed member's identity is a value, [`Value::Member`]: its role and its key. Inside a process, a host routes by
//! [`NodeId`]s, so it gives each member it runs or sends to one, and tells the evaluators which ([`Members`]): `self`
//! and a delivery's sender become the member's value, and a send to a member goes to its id. The ids never reach a
//! value, so they need not agree across hosts.

use std::collections::BTreeMap;

use blossom_value::Value;
use blossom_value::time::{MemberRef, NodeId};

use crate::core::Program;

/// The keyed members a host knows, by node id.
#[derive(Clone, Debug, Default)]
pub struct Members {
    by_id: BTreeMap<NodeId, MemberRef>,
    by_ref: BTreeMap<MemberRef, NodeId>,
}

impl Members {
    /// Gives member `m` the id `id`; refused when either is already given.
    pub fn insert(&mut self, id: NodeId, m: MemberRef) -> Result<(), String> {
        if let Some(had) = self.by_id.get(&id) {
            return Err(format!("node {} is already the member {had:?}", id.0));
        }
        if let Some(had) = self.by_ref.get(&m) {
            return Err(format!("the member {m:?} already has node {}", had.0));
        }
        self.by_id.insert(id, m.clone());
        self.by_ref.insert(m, id);
        Ok(())
    }

    /// The member node `id` is, if it is one.
    pub fn get(&self, id: NodeId) -> Option<&MemberRef> {
        self.by_id.get(&id)
    }

    /// The node id of member `m`, if the host gave it one.
    pub fn id(&self, m: &MemberRef) -> Option<NodeId> {
        self.by_ref.get(m).copied()
    }

    /// Node `id` as a value: its member, or the node id.
    pub fn value(&self, id: NodeId) -> Value {
        match self.by_id.get(&id) {
            Some(m) => Value::Member(m.clone()),
            None => Value::Node(id),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &MemberRef)> {
        self.by_id.iter().map(|(id, m)| (*id, m))
    }
}

/// A deployment's keyed members: a node of a keyed role is the member its name keys (`--nodes game-1=Game` is
/// `Game.named("game-1")`), so a simulation names the members it runs. `names[i]` and `roles[i]` are node `i`'s.
pub fn of_deployment<N: AsRef<str>>(
    program: &Program,
    names: &[N],
    roles: &[Option<blossom_base::RoleId>],
) -> Result<Members, String> {
    let mut out = Members::default();
    for (i, (name, role)) in names.iter().zip(roles).enumerate() {
        if let Some(r) = role
            && program.is_keyed(*r)
        {
            let id = NodeId(u32::try_from(i).map_err(|_| "too many nodes".to_string())?);
            out.insert(
                id,
                MemberRef {
                    role: *r,
                    key: std::sync::Arc::from(name.as_ref()),
                },
            )?;
        }
    }
    Ok(out)
}

/// A member as source writes it, `Game:"game-17"`: its name everywhere (a member's seed σn derives from it, so it is
/// the same on every host and in simulation).
pub fn member_name(program: &Program, m: &MemberRef) -> String {
    let role = program
        .roles
        .get(m.role)
        .map_or_else(|| format!("role#{}", m.role.raw()), |r| r.name.to_string());
    format!("{role}:{:?}", m.key)
}
