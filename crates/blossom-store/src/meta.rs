use crate::{
    StoreError, Vfs, VfsLock, invalid,
    vfs::{atomic_write, read_path},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
/// Persistent node identity, independent of dense runtime identifiers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreIdentity {
    pub store_uuid: [u8; 16],
    pub deployment_id: [u8; 16],
    pub program_id: [u8; 16],
    pub node_name: Arc<str>,
    pub principal: Arc<str>,
    pub format: u16,
    pub directory_digest: [u8; 16],
}
/// Explicit initialization policy; startup must never silently create a fresh identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenMode {
    #[default]
    Existing,
    InitFresh,
}
/// Opaque metadata; codecs and identity validation are added by M5.4.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetaRecord {
    pub identity: StoreIdentity,
    pub node_id_map: Vec<u8>,
    pub restarts: u64,
    pub reserved_tick: u64,
    pub last_now: i64,
    pub understood_version: u64,
    pub poison_deny_list: Vec<u8>,
    pub clean_shutdown: bool,
}
/// Atomic checksummed META persistence.
pub struct MetaStore {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
}
impl MetaStore {
    /// Metadata writer rooted at an existing node directory.
    pub fn new(fs: Arc<dyn Vfs>, dir: &Path) -> Self {
        Self { fs, dir: dir.into() }
    }
    /// Write tmp, sync file, rename, then sync directory.
    pub fn write(&self, record: &MetaRecord) -> Result<(), StoreError> {
        let body = serde_json::to_vec(record).map_err(|e| invalid(e.to_string()))?;
        let mut bytes = b"BLSM".to_vec();
        bytes.extend(blake3::hash(&body).as_bytes());
        bytes.extend(body);
        atomic_write(&*self.fs, &self.dir.join("META"), &bytes)
    }
    /// Read and verify the complete metadata record.
    pub fn read(&self) -> Result<MetaRecord, StoreError> {
        let path = self.dir.join("META");
        let bytes = read_path(&*self.fs, &path)?;
        let body = bytes.get(36..).ok_or_else(|| invalid("META truncated"))?;
        if bytes.get(..4) != Some(b"BLSM") || bytes.get(4..36) != Some(blake3::hash(body).as_bytes().as_slice()) {
            return Err(StoreError::Corruption {
                path,
                offset: 0,
                reason: "META checksum".into(),
            });
        }
        serde_json::from_slice(body).map_err(|e| StoreError::Corruption {
            path,
            offset: 36,
            reason: e.to_string(),
        })
    }
}
/// Hold the exclusive directory lock for the process lifetime.
pub struct StoreLock {
    _guard: Box<dyn VfsLock>,
}
impl StoreLock {
    /// Acquire LOCK, reporting the holder's pid on refusal.
    pub fn acquire(fs: &dyn Vfs, dir: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            _guard: fs.lock_exclusive(&dir.join("LOCK"))?,
        })
    }
}
