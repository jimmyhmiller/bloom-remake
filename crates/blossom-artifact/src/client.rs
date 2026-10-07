//! What a client member's page runs (docs/design/CLIENTS.md §8): the program projected onto one client role, for one
//! deployment. Only the role's rules, the relations they read and write, the channels the role is an end of (as
//! schemas), the link events and member relations of the roles it talks to, and the types, functions and constants
//! those need. No rule placed at another role, no relation only another role holds, and no source text of either.
//!
//! The server builds it from the deployment's [`BlsArtifact`] ([`ClientArtifact::project`]) and serves it encoded
//! ([`ClientArtifact::encode`]); the page decodes it ([`ClientArtifact::decode`]), which re-validates the program, and
//! runs it with no compiler. [`ClientArtifact::leaks`] checks a projection against the whole program.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{RelId, RoleId, Symbol};
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{ChannelForm, ConstructKind, EventSource, Placement, Program, RelClass, RoleKind};
use serde::{Deserialize, Serialize};

use crate::bls::BlsArtifact;

/// The first bytes of an encoded client artifact, and its format.
const MAGIC: &[u8; 4] = b"BLSC";
const FORMAT: u16 = 1;
/// The largest encoded artifact a page accepts.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// What can go wrong building or reading a client artifact.
#[derive(Debug, thiserror::Error)]
pub enum ClientArtifactError {
    #[error("`{0}` is not a client role of the program")]
    NotAClient(String),
    #[error("projecting the program onto `{role}`: {detail}")]
    Projection { role: String, detail: String },
    #[error("a client artifact: {0}")]
    Malformed(String),
    #[error("the client artifact's program is not valid: {0}")]
    Invalid(String),
}

/// The program as one client role's members run it.
#[derive(Clone)]
pub struct ClientArtifact {
    /// The projected program, its relations and roles renumbered; its rules carry no role guard.
    pub program: ValidatedProgram,
    /// The client role, in `program`'s numbering, and its name.
    pub role: RoleId,
    pub role_name: String,
    /// The deployment's nodes (`NodeId(i)` is `nodes[i]`) and each one's role in `program`'s numbering (`None` for a
    /// node whose role the page never deals with).
    pub nodes: Vec<Symbol>,
    pub roles: Vec<Option<RoleId>>,
    /// For every surface relation kept: the IR column of each declared column.
    pub surface: BTreeMap<RelId, Vec<usize>>,
}

/// The encoded form: the program as data, and the rest.
#[derive(Serialize, Deserialize)]
struct Encoded {
    program: Program,
    /// The projected program's digest, checked on decoding.
    digest: [u8; 32],
    role: RoleId,
    role_name: String,
    nodes: Vec<Symbol>,
    roles: Vec<Option<RoleId>>,
    surface: BTreeMap<RelId, Vec<usize>>,
}

fn by_name(p: &Program) -> BTreeMap<String, RelId> {
    p.rels
        .iter_enumerated()
        .map(|(id, r)| (r.name.to_string(), id))
        .collect()
}

impl ClientArtifact {
    /// Projects `full` onto its client role `role` (by name).
    pub fn project(full: &BlsArtifact, role: &str) -> Result<ClientArtifact, ClientArtifactError> {
        let p = full.program.get();
        let Some((full_role, _)) = p
            .roles
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == role && r.kind == RoleKind::Client)
        else {
            return Err(ClientArtifactError::NotAClient(role.to_owned()));
        };
        let program = full
            .program
            .project(full_role)
            .map_err(|e| ClientArtifactError::Projection {
                role: role.to_owned(),
                detail: e.to_string(),
            })?;
        let q = program.get();
        // The projection renumbers; roles and relations keep their names.
        let role_of = |r: RoleId| -> Option<RoleId> {
            let name = p.roles.get(r)?.name.to_string();
            q.roles
                .iter_enumerated()
                .find(|(_, d)| d.name.to_string() == name)
                .map(|(id, _)| id)
        };
        let me = role_of(full_role).ok_or_else(|| ClientArtifactError::Projection {
            role: role.to_owned(),
            detail: "the projection lost its own role".into(),
        })?;
        let rels = by_name(q);
        let surface = full
            .surface
            .iter()
            .filter_map(|(rel, cols)| {
                let name = p.rels.get(*rel)?.name.to_string();
                rels.get(&name).map(|id| (*id, cols.clone()))
            })
            .collect();
        Ok(ClientArtifact {
            role: me,
            role_name: role.to_owned(),
            nodes: full.nodes.clone(),
            roles: full.roles.iter().map(|r| r.and_then(role_of)).collect(),
            surface,
            program,
        })
    }

    /// The artifact as the page's [`BlsArtifact`]: its nodes, their roles and the projected program.
    pub fn artifact(&self) -> BlsArtifact {
        BlsArtifact {
            nodes: self.nodes.clone(),
            roles: self.roles.clone(),
            program: self.program.clone(),
            surface: self.surface.clone(),
            halt: None,
            methods: BTreeMap::new(),
        }
    }

    /// The bytes a server serves.
    pub fn encode(&self) -> Result<Vec<u8>, ClientArtifactError> {
        let body = postcard::to_allocvec(&Encoded {
            program: self.program.get().clone(),
            digest: self.program.digest().0,
            role: self.role,
            role_name: self.role_name.clone(),
            nodes: self.nodes.clone(),
            roles: self.roles.clone(),
            surface: self.surface.clone(),
        })
        .map_err(|e| ClientArtifactError::Malformed(e.to_string()))?;
        let mut out = Vec::with_capacity(body.len() + 6);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Reads an encoded artifact: its program is validated again, and must have the digest it was encoded with.
    pub fn decode(bytes: &[u8]) -> Result<ClientArtifact, ClientArtifactError> {
        let bad = |m: &str| ClientArtifactError::Malformed(m.to_owned());
        if bytes.len() > MAX_BYTES {
            return Err(bad("over 64 MiB"));
        }
        let (magic, rest) = bytes.split_at_checked(4).ok_or_else(|| bad("too short"))?;
        if magic != MAGIC {
            return Err(bad("not a client artifact (no BLSC header)"));
        }
        let (format, body) = rest.split_at_checked(2).ok_or_else(|| bad("too short"))?;
        let format = u16::from_le_bytes(<[u8; 2]>::try_from(format).map_err(|_| bad("too short"))?);
        if format != FORMAT {
            return Err(ClientArtifactError::Malformed(format!(
                "format {format} (this build reads {FORMAT})"
            )));
        }
        let e: Encoded = postcard::from_bytes(body).map_err(|e| ClientArtifactError::Malformed(e.to_string()))?;
        let program = ValidatedProgram::validate(e.program).map_err(|errs| {
            ClientArtifactError::Invalid(errs.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
        })?;
        if program.digest().0 != e.digest {
            return Err(bad("its program does not have the digest it was built with"));
        }
        if program
            .get()
            .roles
            .get(e.role)
            .is_none_or(|r| r.kind != RoleKind::Client)
        {
            return Err(bad("its role is not a client role of its program"));
        }
        if e.roles.len() != e.nodes.len() || e.roles.iter().flatten().any(|r| program.get().roles.get(*r).is_none()) {
            return Err(bad("its node roles do not match its program"));
        }
        Ok(ClientArtifact {
            program,
            role: e.role,
            role_name: e.role_name,
            nodes: e.nodes,
            roles: e.roles,
            surface: e.surface,
        })
    }

    /// The first 16 bytes of the projected program's digest: what a member presents on its link, so a server refuses a
    /// page built from another program.
    pub fn part(&self) -> [u8; 16] {
        part_of(&self.program)
    }

    /// What of `full` the projection holds that its role's members must not see: each a description. Empty for a
    /// projection that holds only the role's rules and relations, the channels it is an end of, and the link events
    /// and member relations of the roles it talks to.
    pub fn leaks(&self, full: &BlsArtifact) -> Vec<String> {
        let p = full.program.get();
        let q = self.program.get();
        let mut out = Vec::new();
        let Some(me) = p
            .roles
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == self.role_name)
            .map(|(id, _)| id)
        else {
            return vec![format!("the program has no role `{}`", self.role_name)];
        };
        let full_rels = by_name(p);
        // The roles the client deals with: its own, and the other ends of its channels.
        let mut peers = BTreeSet::from([me]);
        for r in p.rels.iter() {
            if let RelClass::Channel(ch) = &r.class
                && let ChannelForm::Direction { src, dst } = ch.form
            {
                if src == me {
                    peers.insert(dst);
                }
                if dst == me {
                    peers.insert(src);
                }
            }
        }
        for r in q.rels.iter() {
            let name = r.name.to_string();
            let Some(orig) = full_rels.get(&name).and_then(|id| p.rels.get(*id)) else {
                out.push(format!("relation `{name}` is not in the program"));
                continue;
            };
            let allowed = match (&orig.class, &orig.placement) {
                (RelClass::Channel(ch), _) => {
                    matches!(ch.form, ChannelForm::Direction { src, dst } if src == me || dst == me)
                }
                (RelClass::Event(EventSource::Link { peer, .. }), Placement::Role(at)) => {
                    *at == me && peers.contains(peer)
                }
                (_, Placement::Shared) => true,
                (_, Placement::Role(at)) => *at == me,
            };
            // A role's member relation and the node directory are the deployment's, which the page is given anyway.
            let deployment = matches!(&orig.origin, blossom_ir::core::Origin::Generated { construct }
                if matches!(p.constructs.get(*construct).map(|c| &c.kind), Some(ConstructKind::Members { .. })));
            if !allowed && !deployment {
                out.push(format!("relation `{name}`, placed at another role"));
            }
        }
        // A label may repeat across roles: a projected rule is fine when the program has it at the client role.
        let mut full_rules: BTreeMap<String, BTreeSet<Option<RoleId>>> = BTreeMap::new();
        for r in p.rules.iter() {
            full_rules.entry(r.label.text.to_string()).or_default().insert(r.role);
        }
        for r in q.rules.iter() {
            let label = r.label.text.to_string();
            match full_rules.get(&label) {
                Some(at) if at.contains(&Some(me)) || at.contains(&None) => {}
                Some(_) => out.push(format!("rule `{label}`, placed at another role")),
                None => out.push(format!("rule `{label}` is not in the program")),
            }
        }
        out
    }
}

/// The first 16 bytes of a projected program's digest (see [`ClientArtifact::part`]).
pub fn part_of(program: &ValidatedProgram) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (o, b) in out.iter_mut().zip(program.digest().0) {
        *o = b;
    }
    out
}
