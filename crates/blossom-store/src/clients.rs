//! The client registry (docs/design/CLIENTS.md §2): the client members a node admitted, so a member keeps its
//! identity across reconnects and restarts of either end.
//!
//! `CLIENTS` in the node directory holds, for each admitted member, its serial (its id is minted from it), its role
//! and the BLAKE3 hash of the secret half of the token the member was given. It is written atomically and checksummed
//! like META, once per admission (rare: a reconnect reads it, never writes).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::vfs::{atomic_write, read_path};
use crate::{StoreError, Vfs, invalid};

/// The secret half of a member's token.
pub const SECRET_LEN: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// The next serial to give out.
    next: u32,
    /// Each admitted member: its role and its secret's hash (hex).
    members: BTreeMap<u32, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    role: String,
    secret_hash: String,
}

/// A node's client registry.
pub struct ClientRegistry {
    fs: Arc<dyn Vfs>,
    path: PathBuf,
    record: Record,
}

fn hash(secret: &[u8; SECRET_LEN]) -> String {
    blake3::hash(secret).to_hex().to_string()
}

impl ClientRegistry {
    /// The registry of the node directory `dir` (empty when it has none yet).
    pub fn open(fs: Arc<dyn Vfs>, dir: &Path) -> Result<ClientRegistry, StoreError> {
        let path = dir.join("CLIENTS");
        let present = fs.list(dir)?.iter().any(|p| p.file_name() == path.file_name());
        let record = if present {
            let bytes = read_path(&*fs, &path)?;
            let body = bytes.get(36..).ok_or_else(|| invalid("CLIENTS truncated"))?;
            if bytes.get(..4) != Some(b"BLSC") || bytes.get(4..36) != Some(blake3::hash(body).as_bytes().as_slice()) {
                return Err(StoreError::Corruption {
                    path,
                    offset: 0,
                    reason: "CLIENTS checksum".into(),
                });
            }
            serde_json::from_slice(body).map_err(|e| StoreError::Corruption {
                path: path.clone(),
                offset: 36,
                reason: e.to_string(),
            })?
        } else {
            Record::default()
        };
        Ok(ClientRegistry { fs, path, record })
    }

    /// Admits a new member of `role` whose token's secret is `secret`: its serial, after the registry is durable.
    /// `None` when `limit` serials are given out.
    pub fn admit(&mut self, role: &str, secret: &[u8; SECRET_LEN], limit: u32) -> Result<Option<u32>, StoreError> {
        let serial = self.record.next;
        if serial >= limit {
            return Ok(None);
        }
        let mut next = self.record.clone();
        next.next = serial + 1;
        next.members.insert(
            serial,
            Entry {
                role: role.to_owned(),
                secret_hash: hash(secret),
            },
        );
        let body = serde_json::to_vec(&next).map_err(|e| invalid(e.to_string()))?;
        let mut bytes = b"BLSC".to_vec();
        bytes.extend(blake3::hash(&body).as_bytes());
        bytes.extend(body);
        atomic_write(&*self.fs, &self.path, &bytes)?;
        self.record = next;
        Ok(Some(serial))
    }

    /// The role of member `serial`, if `secret` is its token's.
    pub fn check(&self, serial: u32, secret: &[u8; SECRET_LEN]) -> Option<&str> {
        self.record
            .members
            .get(&serial)
            .filter(|e| e.secret_hash == hash(secret))
            .map(|e| e.role.as_str())
    }

    /// The number of members admitted.
    pub fn len(&self) -> usize {
        self.record.members.len()
    }

    /// Whether no member was admitted.
    pub fn is_empty(&self) -> bool {
        self.record.members.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SimFs;

    #[test]
    fn members_survive_a_reopen_and_need_their_secret() {
        let fs: Arc<dyn Vfs> = Arc::new(SimFs::default());
        let dir = Path::new("/n1");
        fs.create_dir_all(dir).unwrap();
        let mut r = ClientRegistry::open(fs.clone(), dir).unwrap();
        assert!(r.is_empty());
        let a = r.admit("Browser", &[1; SECRET_LEN], 8).unwrap().unwrap();
        let b = r.admit("Browser", &[2; SECRET_LEN], 8).unwrap().unwrap();
        assert_eq!((a, b), (0, 1));
        let r = ClientRegistry::open(fs, dir).unwrap();
        assert_eq!(r.check(0, &[1; SECRET_LEN]), Some("Browser"));
        assert_eq!(r.check(1, &[1; SECRET_LEN]), None, "another member's secret");
        assert_eq!(r.check(7, &[1; SECRET_LEN]), None, "a serial never given out");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn serials_stop_at_the_limit() {
        let fs: Arc<dyn Vfs> = Arc::new(SimFs::default());
        let dir = Path::new("/n1");
        fs.create_dir_all(dir).unwrap();
        let mut r = ClientRegistry::open(fs, dir).unwrap();
        assert!(r.admit("B", &[1; SECRET_LEN], 1).unwrap().is_some());
        assert!(r.admit("B", &[2; SECRET_LEN], 1).unwrap().is_none());
    }
}
