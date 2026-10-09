//! Keyed members on `blossom run` (docs/design/KEYED.md §3): which host runs a member.
//!
//! A deployment's nodes of a keyed role are that role's **hosts**. A member lives on the host rendezvous hashing over
//! the hosts picks for it: the host whose name scores highest with the member's role and key. Every node computes the
//! same host, so a sender routes a message to a member there, and a receiver accepts a member as a sender only from
//! that host.

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RoleId;
use blossom_ir::core::Program;
use blossom_value::time::{MemberRef, NodeId};

use crate::RuntimeError;
use crate::deploy::DeploymentSpec;

/// A keyed role's name, and its hosts' ids and names.
type Hosts = (Arc<str>, Vec<(NodeId, Arc<str>)>);

/// Each keyed role's hosts, and how a member finds its own.
#[derive(Clone, Debug, Default)]
pub struct Routing {
    roles: BTreeMap<RoleId, Hosts>,
}

/// A host's score for a member: the first eight bytes of BLAKE3 over the role's name, the key and the host's name.
fn score(role: &str, key: &str, host: &str) -> u64 {
    let mut h = blake3::Hasher::new();
    for part in [role, key, host] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    let digest = h.finalize();
    // A BLAKE3 digest is 32 bytes.
    u64::from_be_bytes(digest.as_bytes().first_chunk::<8>().copied().unwrap_or_default())
}

impl Routing {
    /// The hosts of `program`'s keyed roles: the deployment's nodes of each.
    pub fn of(spec: &DeploymentSpec, artifact: &BlsArtifact) -> Result<Routing, RuntimeError> {
        let p = artifact.program.get();
        let mut roles: BTreeMap<RoleId, Hosts> = p
            .keyed_roles()
            .map(|r| (r.id, (Arc::from(r.name.to_string()), Vec::new())))
            .collect();
        for (i, (n, role)) in spec.nodes.iter().zip(&artifact.roles).enumerate() {
            if let Some(r) = role
                && let Some((_, hosts)) = roles.get_mut(r)
            {
                let id = NodeId(u32::try_from(i).map_err(|_| RuntimeError::Config("too many nodes".into()))?);
                hosts.push((id, Arc::from(n.name.as_str())));
            }
        }
        Ok(Routing { roles })
    }

    /// Whether the program has keyed roles.
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    /// Whether node `n` hosts a keyed role.
    pub fn is_host(&self, n: NodeId) -> bool {
        self.roles.values().any(|(_, hosts)| hosts.iter().any(|(h, _)| *h == n))
    }

    /// The host of member `m`; an error when its role is not keyed or the deployment gives it no host.
    pub fn host_of(&self, m: &MemberRef) -> Result<NodeId, RuntimeError> {
        let (role, hosts) = self
            .roles
            .get(&m.role)
            .ok_or_else(|| RuntimeError::Config(format!("role {} is not a keyed role", m.role.raw())))?;
        hosts
            .iter()
            .max_by_key(|(id, name)| (score(role, &m.key, name), std::cmp::Reverse(*id)))
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                RuntimeError::Config(format!(
                    "the keyed role `{role}` has no host: the deployment needs a node of role `{role}` to run its \
                     members"
                ))
            })
    }
}

/// The member a row is addressed to (its column 0), if it is one.
pub fn addressee(row: &[blossom_value::Value]) -> Option<&MemberRef> {
    match row.first() {
        Some(blossom_value::Value::Member(m)) => Some(m),
        _ => None,
    }
}

/// A member, from what a `FromMember` frame names: the role's id (checked keyed in `program`) and the key.
pub fn member_of(program: &Program, role: u32, key: String) -> Result<MemberRef, RuntimeError> {
    let role = RoleId::from_raw(role);
    if !program.is_keyed(role) {
        return Err(RuntimeError::Config(format!(
            "a batch from a member of role {}, which is not keyed",
            role.raw()
        )));
    }
    Ok(program.member(role, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_spreads_members_and_moves_few_when_a_host_joins() {
        let role = |hosts: &[&str]| {
            let mut r = BTreeMap::new();
            r.insert(
                RoleId::from_raw(1),
                (
                    Arc::from("Game"),
                    hosts
                        .iter()
                        .enumerate()
                        .map(|(i, h)| (NodeId(i as u32), Arc::from(*h)))
                        .collect(),
                ),
            );
            Routing { roles: r }
        };
        let m = |k: usize| MemberRef::new(RoleId::from_raw(1), "Game", format!("game-{k}"));
        let two = role(&["h1", "h2"]);
        let three = role(&["h1", "h2", "h3"]);
        let mut on = [0usize; 3];
        let mut moved = 0;
        for k in 0..600 {
            let a = two.host_of(&m(k)).unwrap();
            let b = three.host_of(&m(k)).unwrap();
            on[b.0 as usize] += 1;
            // A member moves only to the new host.
            if a != b {
                assert_eq!(b, NodeId(2));
                moved += 1;
            }
        }
        assert!(on.iter().all(|n| *n > 120), "{on:?}");
        assert!((120..=280).contains(&moved), "{moved}");
        assert!(role(&[]).host_of(&m(0)).is_err());
    }
}
