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
/// A checkpoint is a full image (`files`, in the directory of the checkpoint `base`, its own when `None`) followed by
/// delta layers, each the changes since the checkpoint before it, applied in order (FOREIGN-PROTOCOLS §6).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: CheckpointId,
    catalog: Vec<u8>,
    files: Vec<(u32, [u8; 32])>,
    /// The checkpoint whose directory holds the full image; `None` for this one.
    #[serde(default)]
    base: Option<u64>,
    /// The delta layers after the image: each checkpoint directory's `delta.dat`, with its digest and size.
    #[serde(default)]
    layers: Vec<(u64, [u8; 32], u64)>,
}

/// The shape of a checkpoint chain: what compaction decides from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainInfo {
    /// The bytes of the full image.
    pub base_bytes: u64,
    /// The delta layers after it, and their bytes.
    pub layers: usize,
    pub layer_bytes: u64,
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
    /// Read a checkpoint's full image, verifying every relation checksum and rejecting duplicate relation ids. The
    /// delta layers after it are [`FileCheckpoints::read_layers`].
    pub fn read(&self, id: CheckpointId) -> Result<DurableSnapshot, StoreError> {
        let manifest = self.manifest(id)?;
        let base_dir = self.path(manifest.base.unwrap_or(id.tick));
        let base = match manifest.base {
            None => manifest,
            Some(t) => {
                let m = self.manifest_at(t)?;
                Manifest {
                    catalog: manifest.catalog,
                    ..m
                }
            }
        };
        let mut snap = DurableSnapshot {
            catalog: base.catalog,
            relations: BTreeMap::new(),
        };
        for (rel, digest) in base.files {
            let path = base_dir.join(format!("rel-{rel}.dat"));
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
    /// The manifest in the directory of checkpoint `tick`, whatever its LSN (a chain names its base by tick).
    fn manifest_at(&self, tick: u64) -> Result<Manifest, StoreError> {
        let path = self.path(tick).join("MANIFEST");
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
        if m.id.tick != tick || m.base.is_some() {
            return Err(invalid(format!("checkpoint {tick} is not a full image")));
        }
        Ok(m)
    }

    /// A checkpoint's delta layers, oldest first, each verified: the payloads to apply to its image in order.
    pub fn read_layers(&self, id: CheckpointId) -> Result<Vec<Vec<u8>>, StoreError> {
        let manifest = self.manifest(id)?;
        let mut out = Vec::new();
        for (tick, digest, _) in manifest.layers {
            let path = self.path(tick).join("delta.dat");
            let body = read_path(&*self.fs, &path)?;
            if blake3::hash(&body).as_bytes() != &digest {
                return Err(StoreError::Corruption {
                    path,
                    offset: 0,
                    reason: "delta layer checksum".into(),
                });
            }
            out.push(body);
        }
        Ok(out)
    }

    /// The shape of the installed checkpoint's chain (`None` without one).
    pub fn chain(&self) -> Result<Option<ChainInfo>, StoreError> {
        let Some(id) = self.current()? else {
            return Ok(None);
        };
        let m = self.manifest(id)?;
        let base_tick = m.base.unwrap_or(id.tick);
        let base = if m.base.is_some() { self.manifest_at(base_tick)? } else { m.clone() };
        let mut base_bytes = 0u64;
        for (rel, _) in &base.files {
            let f = self.fs.open(&self.path(base_tick).join(format!("rel-{rel}.dat")), OpenOpts::default())?;
            base_bytes = base_bytes.saturating_add(f.len()?);
        }
        Ok(Some(ChainInfo {
            base_bytes,
            layers: m.layers.len(),
            layer_bytes: m.layers.iter().map(|l| l.2).sum(),
        }))
    }

    /// Writes a checkpoint at `covers` that is the installed one plus `delta` (the changes since it, in the WAL's
    /// delta encoding): its cost follows the change, not the state. Install it like any checkpoint.
    pub fn write_layer(&mut self, delta: &[u8], covers: SyncedTick) -> Result<CheckpointId, StoreError> {
        let current = self
            .current()?
            .ok_or_else(|| invalid("a delta layer needs an installed checkpoint to build on"))?;
        let prev = self.manifest(current)?;
        let id = CheckpointId {
            tick: covers.tick,
            lsn: covers.lsn,
        };
        if id.tick <= current.tick {
            return Err(invalid("a delta layer must come after the checkpoint it builds on"));
        }
        let dir = self.path(id.tick);
        crate::vfs::durable_dir(&*self.fs, &dir)?;
        let mut f = self.fs.open(
            &dir.join("delta.dat"),
            OpenOpts {
                create_new: true,
                ..OpenOpts::default()
            },
        )?;
        f.append(delta)?;
        f.sync_data()?;
        let mut layers = prev.layers.clone();
        layers.push((id.tick, *blake3::hash(delta).as_bytes(), delta.len() as u64));
        let manifest = Manifest {
            id,
            catalog: prev.catalog.clone(),
            files: Vec::new(),
            base: Some(prev.base.unwrap_or(current.tick)),
            layers,
        };
        self.write_manifest(&dir, &manifest)?;
        Ok(id)
    }

    fn write_manifest(&self, dir: &Path, manifest: &Manifest) -> Result<(), StoreError> {
        let body = serde_json::to_vec(manifest).map_err(|e| invalid(e.to_string()))?;
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
        self.fs.sync_dir(dir)?;
        Ok(())
    }

    /// Removes every checkpoint directory the installed one does not use (older checkpoints outside its chain, and
    /// partial ones a crash left behind). Call it after `install`: `CURRENT` names none of them, so no crash can need
    /// them.
    pub fn prune(&self) -> Result<usize, StoreError> {
        let Some(current) = self.current()? else {
            return Ok(0);
        };
        let m = self.manifest(current)?;
        let mut used: std::collections::BTreeSet<u64> = m.layers.iter().map(|l| l.0).collect();
        used.insert(current.tick);
        if let Some(b) = m.base {
            used.insert(b);
        }
        let root = self.dir.join("ckpt");
        let mut removed = 0;
        for dir in self.fs.list(&root)? {
            let tick = dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<u64>().ok());
            if tick.is_some_and(|t| used.contains(&t)) {
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
        self.write_manifest(
            &dir,
            &Manifest {
                id,
                catalog: snap.catalog,
                files,
                base: None,
                layers: Vec::new(),
            },
        )?;
        Ok(id)
    }
    fn install(&mut self, id: CheckpointId) -> Result<TruncateToken, StoreError> {
        self.read(id)?;
        self.read_layers(id)?;
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
