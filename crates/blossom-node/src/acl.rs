//! Ingress admission by ACL (LANGUAGE §18.3, ARCHITECTURE §5.8 step 5).
//!
//! A channel accepts a message only from the sources its ACL admits. The inferred ACL (default-deny) admits the
//! roles that have a `send` into the channel: the source role of a `Src -> Dst` channel, and otherwise the role of
//! every rule that sends into it (a rule of a role-free program admits every node). An explicit `#[accept(…)]`
//! narrows it to the listed roles, client sessions when it says `external`, and, with `principal in REL`, to
//! senders whose principal is a row of `REL` on the receiving node at its last committed tick. A rejected message is
//! dropped before the tick: an omission (SEM-090).

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RoleId};
use blossom_ir::core::{AclSpec, ChannelForm, Program, RelClass, RoleKind, RuleKind};

/// Who sent a message.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    /// A node of the deployment, with its role and principal.
    Node { role: Option<RoleId>, principal: &'a str },
    /// A client session, with the principal it authenticated as.
    Session { principal: &'a str },
}

/// Why admission rejected a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The relation is not a channel this node receives.
    NotAChannel,
    /// The channel's ACL does not admit the source.
    Acl,
}

/// One channel's admitted sources.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChannelAcl {
    /// The roles whose nodes may send; `None` admits every node (a role-free program).
    roles: Option<BTreeSet<RoleId>>,
    /// Whether client sessions may send.
    external: bool,
    principal_in: Option<RelId>,
}

/// The ACL of every channel of a program.
#[derive(Clone, Debug, Default)]
pub struct AclTable {
    channels: BTreeMap<RelId, ChannelAcl>,
}

impl AclTable {
    pub fn of(program: &Program) -> AclTable {
        let external = |r: RoleId| program.roles.get(r).is_some_and(|d| d.kind == RoleKind::External);
        let mut channels = BTreeMap::new();
        for (id, rel) in program.rels.iter_enumerated() {
            let RelClass::Channel(ch) = &rel.class else {
                continue;
            };
            let inferred = match ch.form {
                ChannelForm::Direction { src, .. } if external(src) => ChannelAcl {
                    roles: Some(BTreeSet::new()),
                    external: true,
                    principal_in: None,
                },
                ChannelForm::Direction { src, .. } => ChannelAcl {
                    roles: Some(BTreeSet::from([src])),
                    external: false,
                    principal_in: None,
                },
                ChannelForm::Column | ChannelForm::NodeToNode => {
                    let mut roles = Some(BTreeSet::new());
                    for rule in program.rules.iter() {
                        if rule.kind != RuleKind::Async || rule.head.rel != id {
                            continue;
                        }
                        match (rule.role, roles.as_mut()) {
                            (Some(r), Some(set)) => {
                                set.insert(r);
                            }
                            (None, _) => roles = None,
                            (Some(_), None) => {}
                        }
                    }
                    ChannelAcl {
                        roles,
                        external: false,
                        principal_in: None,
                    }
                }
            };
            let acl = match &ch.acl {
                AclSpec::Inferred => inferred,
                AclSpec::Explicit(e) => {
                    let listed: BTreeSet<RoleId> = e.roles.iter().copied().collect();
                    ChannelAcl {
                        roles: Some(match &inferred.roles {
                            Some(r) => r.intersection(&listed).copied().collect(),
                            None => listed,
                        }),
                        external: inferred.external && e.external,
                        principal_in: e.principal_in,
                    }
                }
            };
            channels.insert(id, acl);
        }
        AclTable { channels }
    }

    /// The relation whose rows name the principals `rel` admits, if its ACL says `principal in REL`.
    pub fn principal_relation(&self, rel: RelId) -> Option<RelId> {
        self.channels.get(&rel).and_then(|c| c.principal_in)
    }

    /// Admits or rejects a message on `rel` from `source`. `principal_in(r, p)` says whether principal `p` is a row
    /// of the unary relation `r` at the receiving node's last committed tick.
    pub fn admit(
        &self,
        rel: RelId,
        source: Source<'_>,
        principal_in: &dyn Fn(RelId, &str) -> bool,
    ) -> Result<(), Rejection> {
        let acl = self.channels.get(&rel).ok_or(Rejection::NotAChannel)?;
        let (admitted, principal) = match source {
            Source::Node { role, principal } => (
                match (&acl.roles, role) {
                    (None, _) => true,
                    (Some(set), Some(r)) => set.contains(&r),
                    (Some(_), None) => false,
                },
                principal,
            ),
            Source::Session { principal } => (acl.external, principal),
        };
        if !admitted {
            return Err(Rejection::Acl);
        }
        match acl.principal_in {
            Some(r) if !principal_in(r, principal) => Err(Rejection::Acl),
            _ => Ok(()),
        }
    }
}
