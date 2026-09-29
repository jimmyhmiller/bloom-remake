use crate::{
    Lsn, OpenOpts, StoreError, SyncedTick, TruncateToken, Vfs, invalid,
    vfs::{atomic_write, read_path},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
/// Relation files encoded by the higher-level store codec.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DurableSnapshot {
    pub relations: BTreeMap<u32, Vec<u8>>,
    pub catalog: Vec<u8>,
}
/// Checkpoint identity tied to a successfully synced WAL frontier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointId {
    pub tick: u64,
    pub lsn: Lsn,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: CheckpointId,
    catalog: Vec<u8>,
    files: Vec<(u32, [u8; 32])>,
}
/// Checkpoint writing and installation have separate owners from the WAL.
pub trait CheckpointWriter: Send {
    fn write(&mut self, snap: DurableSnapshot, covers: SyncedTick) -> Result<CheckpointId, StoreError>;
    fn install(&mut self, id: CheckpointId) -> Result<TruncateToken, StoreError>;
}
/// Filesystem checkpoint store. Checksums cover MANIFEST as well as each opaque relation file.
pub struct FileCheckpoints {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
}
impl FileCheckpoints {
    /// Read an existing checkpoint namespace without changing the crash image.
    pub fn from_existing(fs: Arc<dyn Vfs>, dir: &Path) -> Self {
        Self { fs, dir: dir.into() }
    }
    /// Create the checkpoint namespace and durably publish its directory entry.
    pub fn new(fs: Arc<dyn Vfs>, dir: &Path) -> Result<Self, StoreError> {
        crate::vfs::durable_dir(&*fs, &dir.join("ckpt"))?;
        Ok(Self { fs, dir: dir.into() })
    }
    fn path(&self, tick: u64) -> PathBuf {
        self.dir.join("ckpt").join(tick.to_string())
    }
    fn manifest(&self, id: CheckpointId) -> Result<Manifest, StoreError> {
        let path = self.path(id.tick).join("MANIFEST");
        let bytes = read_path(&*self.fs, &path)?;
        let body = bytes.get(32..).ok_or_else(|| invalid("MANIFEST truncated"))?;
        if bytes.get(..32) != Some(blake3::hash(body).as_bytes().as_slice()) {
            return Err(StoreError::Corruption {
                path,
                offset: 0,
                reason: "MANIFEST checksum".into(),
            });
        }
        let m: Manifest = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
        if m.id != id {
            return Err(invalid("checkpoint identity mismatch"));
        }
        Ok(m)
    }
    /// Read a checkpoint, verifying every relation checksum and rejecting duplicate relation ids.
    pub fn read(&self, id: CheckpointId) -> Result<DurableSnapshot, StoreError> {
        let manifest = self.manifest(id)?;
        let mut snap = DurableSnapshot {
            catalog: manifest.catalog,
            relations: BTreeMap::new(),
        };
        for (rel, digest) in manifest.files {
            let path = self.path(id.tick).join(format!("rel-{rel}.dat"));
            let body = read_path(&*self.fs, &path)?;
            if blake3::hash(&body).as_bytes() != &digest {
                return Err(StoreError::Corruption {
                    path,
                    offset: 0,
                    reason: "relation checksum".into(),
                });
            }
            if snap.relations.insert(rel, body).is_some() {
                return Err(invalid("duplicate checkpoint relation"));
            }
        }
        Ok(snap)
    }
    /// Removes every checkpoint directory other than the installed one (older checkpoints, and partial ones a crash
    /// left behind). Call it after `install`: `CURRENT` no longer names any of them, so no crash can need them.
    pub fn prune(&self) -> Result<usize, StoreError> {
        let Some(current) = self.current()? else {
            return Ok(0);
        };
        let root = self.dir.join("ckpt");
        let mut removed = 0;
        for dir in self.fs.list(&root)? {
            let tick = dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<u64>().ok());
            if tick == Some(current.tick) {
                continue;
            }
            for f in self.fs.list(&dir)? {
                self.fs.remove(&f)?;
            }
            self.fs.remove_dir(&dir)?;
            removed += 1;
        }
        if removed > 0 {
            self.fs.sync_dir(&root)?;
        }
        Ok(removed)
    }
    /// Read the atomic CURRENT pointer. A missing pointer means no checkpoint was installed.
    pub fn current(&self) -> Result<Option<CheckpointId>, StoreError> {
        let bytes = match read_path(&*self.fs, &self.dir.join("CURRENT")) {
            Ok(b) => b,
            Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let body = bytes.get(32..).ok_or_else(|| invalid("CURRENT truncated"))?;
        if bytes.get(..32) != Some(blake3::hash(body).as_bytes().as_slice()) {
            return Err(invalid("CURRENT checksum"));
        }
        serde_json::from_slice(body)
            .map(Some)
            .map_err(|e| invalid(e.to_string()))
    }
}
impl CheckpointWriter for FileCheckpoints {
    fn write(&mut self, snap: DurableSnapshot, covers: SyncedTick) -> Result<CheckpointId, StoreError> {
        let id = CheckpointId {
            tick: covers.tick,
            lsn: covers.lsn,
        };
        let dir = self.path(id.tick);
        crate::vfs::durable_dir(&*self.fs, &dir)?;
        let mut files = Vec::new();
        for (rel, bytes) in snap.relations {
            let mut f = self.fs.open(
                &dir.join(format!("rel-{rel}.dat")),
                OpenOpts {
                    create_new: true,
                    ..OpenOpts::default()
                },
            )?;
            f.append(&bytes)?;
            f.sync_data()?;
            files.push((rel, *blake3::hash(&bytes).as_bytes()));
        }
        self.fs.sync_dir(&dir)?;
        let body = serde_json::to_vec(&Manifest {
            id,
            catalog: snap.catalog,
            files,
        })
        .map_err(|e| invalid(e.to_string()))?;
        let mut bytes = blake3::hash(&body).as_bytes().to_vec();
        bytes.extend(body);
        let mut f = self.fs.open(
            &dir.join("MANIFEST"),
            OpenOpts {
                create_new: true,
                ..OpenOpts::default()
            },
        )?;
        f.append(&bytes)?;
        f.sync_data()?;
        self.fs.sync_dir(&dir)?;
        Ok(id)
    }
    fn install(&mut self, id: CheckpointId) -> Result<TruncateToken, StoreError> {
        self.read(id)?;
        if self
            .current()?
            .is_some_and(|current| current.tick > id.tick || current.lsn > id.lsn)
        {
            return Err(invalid("checkpoint installation cannot move backwards"));
        }
        let body = serde_json::to_vec(&id).map_err(|e| invalid(e.to_string()))?;
        let mut bytes = blake3::hash(&body).as_bytes().to_vec();
        bytes.extend(body);
        atomic_write(&*self.fs, &self.dir.join("CURRENT"), &bytes)?;
        Ok(TruncateToken { lsn: id.lsn })
    }
}
