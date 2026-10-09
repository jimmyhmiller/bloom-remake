//! Keyed members by the node ids a host gave them (docs/design/KEYED.md §3).
//!
//! A keyed member's identity is a value, [`Value::Member`]: its role and its key. Inside a process, a host routes by
//! [`NodeId`]s, so it gives each member it runs or sends to one, and tells the evaluators which ([`Members`]): `self`
//! and a delivery's sender become the member's value, and a send to a member goes to its id. The ids never reach a
//! value, so they need not agree across hosts.

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

use blossom_value::Value;
use blossom_value::time::{MemberRef, NodeId};

use crate::core::Program;

/// The keyed members a host knows, by node id. A closed table (simulation's) knows only the members it was given; an
/// open one (a running node's) gives a member it has not seen the next id of [`NodeId::MEMBERS`] when it is asked
/// for one, so a node can send to any member.
#[derive(Debug, Default)]
pub struct Members {
    table: RwLock<Table>,
    open: bool,
}

#[derive(Debug, Default)]
struct Table {
    by_id: BTreeMap<NodeId, MemberRef>,
    by_ref: BTreeMap<MemberRef, NodeId>,
    next: u32,
}

impl Members {
    /// A table that gives ids on demand.
    pub fn open() -> Members {
        Members {
            table: RwLock::default(),
            open: true,
        }
    }

    // The table is only ever written whole (both maps under one lock), so one a panicking thread held is consistent.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Table> {
        self.table.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Table> {
        self.table.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives member `m` the id `id`; refused when either is already given.
    pub fn insert(&self, id: NodeId, m: MemberRef) -> Result<(), String> {
        let mut t = self.write();
        if let Some(had) = t.by_id.get(&id) {
            return Err(format!("node {} is already the member {had:?}", id.0));
        }
        if let Some(had) = t.by_ref.get(&m) {
            return Err(format!("the member {m:?} already has node {}", had.0));
        }
        t.by_id.insert(id, m.clone());
        t.by_ref.insert(m, id);
        Ok(())
    }

    /// The member node `id` is, if it is one.
    pub fn get(&self, id: NodeId) -> Option<MemberRef> {
        self.read().by_id.get(&id).cloned()
    }

    /// The node id of member `m`: the one the host gave it, or, in an open table, a new one; `None` when a closed
    /// table does not know it, or an open one has given every id of the range.
    pub fn id(&self, m: &MemberRef) -> Option<NodeId> {
        if let Some(id) = self.read().by_ref.get(m) {
            return Some(*id);
        }
        if !self.open {
            return None;
        }
        let mut t = self.write();
        if let Some(id) = t.by_ref.get(m) {
            return Some(*id);
        }
        let id = NodeId::member(t.next)?;
        t.next += 1;
        t.by_id.insert(id, m.clone());
        t.by_ref.insert(m.clone(), id);
        Some(id)
    }

    /// Node `id` as a value: its member, or the node id.
    pub fn value(&self, id: NodeId) -> Value {
        match self.read().by_id.get(&id) {
            Some(m) => Value::Member(m.clone()),
            None => Value::Node(id),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.read().by_id.is_empty()
    }

    /// Every member the table knows, by id.
    pub fn all(&self) -> Vec<(NodeId, MemberRef)> {
        self.read().by_id.iter().map(|(id, m)| (*id, m.clone())).collect()
    }
}

/// A deployment's keyed members: a node of a keyed role is the member its name keys (`--nodes game-1=Game` is
/// `Game.named("game-1")`), so a simulation names the members it runs. `names[i]` and `roles[i]` are node `i`'s.
pub fn of_deployment<N: AsRef<str>>(
    program: &Program,
    names: &[N],
    roles: &[Option<blossom_base::RoleId>],
) -> Result<Members, String> {
    let out = Members::default();
    for (i, (name, role)) in names.iter().zip(roles).enumerate() {
        if let Some(r) = role
            && program.is_keyed(*r)
        {
            let id = NodeId(u32::try_from(i).map_err(|_| "too many nodes".to_string())?);
            out.insert(id, program.member(*r, name.as_ref()))?;
        }
    }
    Ok(out)
}

/// A member as source writes it, `Game:"game-17"`: its name everywhere (a member's seed σn derives from it, so it is
/// the same on every host and in simulation).
pub fn member_name(m: &MemberRef) -> String {
    format!("{}:{:?}", m.role_name, m.key)
}
