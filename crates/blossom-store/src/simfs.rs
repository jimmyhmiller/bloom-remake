use crate::{OpenOpts, StoreError, Vfs, VfsFile, VfsLock, invalid};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// An unflushed file modification offered to the crash oracle in deterministic order.
#[derive(Clone, Debug)]
pub struct UnsyncedWrite {
    pub path: PathBuf,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub truncate_to: Option<u64>,
}
/// A caller-selected crash fate. Torn writes retain a prefix ending on a 512-byte sector boundary.
#[derive(Clone, Copy, Debug)]
pub enum WriteFate {
    Survive,
    Lost,
    Torn { sectors: usize },
}
/// Failure consumed by the next matching filesystem operation.
#[derive(Clone, Copy, Debug)]
pub enum FsFault {
    SyncEio,
    AppendEnospc,
    ShortWrite(usize),
}
#[derive(Clone, Default)]
struct Inode {
    live: Vec<u8>,
    stable: Vec<u8>,
    writes: Vec<UnsyncedWrite>,
}
#[derive(Clone, Default)]
struct StateImage {
    names: BTreeMap<PathBuf, u64>,
    stable_names: BTreeMap<PathBuf, u64>,
    dirs: BTreeSet<PathBuf>,
    stable_dirs: BTreeSet<PathBuf>,
    inodes: BTreeMap<u64, Inode>,
    next_inode: u64,
    generation: u64,
}
#[derive(Clone, Default)]
struct State {
    names: BTreeMap<PathBuf, u64>,
    stable_names: BTreeMap<PathBuf, u64>,
    dirs: BTreeSet<PathBuf>,
    stable_dirs: BTreeSet<PathBuf>,
    inodes: BTreeMap<u64, Inode>,
    next_inode: u64,
    locks: BTreeSet<PathBuf>,
    fault: Option<FsFault>,
    trace: Vec<String>,
    generation: u64,
    recording: bool,
    cuts: Vec<StateImage>,
}
impl State {
    fn image(&self) -> StateImage {
        StateImage {
            names: self.names.clone(),
            stable_names: self.stable_names.clone(),
            dirs: self.dirs.clone(),
            stable_dirs: self.stable_dirs.clone(),
            inodes: self.inodes.clone(),
            next_inode: self.next_inode,
            generation: self.generation,
        }
    }
    fn from_image(image: StateImage) -> Self {
        Self {
            names: image.names,
            stable_names: image.stable_names,
            dirs: image.dirs,
            stable_dirs: image.stable_dirs,
            inodes: image.inodes,
            next_inode: image.next_inode,
            generation: image.generation,
            ..Self::default()
        }
    }
    fn record(&mut self, label: String) {
        self.trace.push(label);
        if self.recording {
            self.cuts.push(self.image());
        }
    }
}
/// Shared deterministic filesystem; open handles refer to inodes rather than mutable path names.
#[derive(Clone, Default)]
pub struct SimFs {
    state: Arc<Mutex<State>>,
}
impl SimFs {
    fn state(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        self.state.lock().map_err(|_| invalid("simfs mutex poisoned"))
    }
    /// Inject one failure, consumed only by a matching operation.
    pub fn inject(&self, fault: FsFault) -> Result<(), StoreError> {
        self.state()?.fault = Some(fault);
        Ok(())
    }
    /// Durable syscall trace, useful for verifying ordering and enumerating crash cuts.
    pub fn trace(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.state()?.trace.clone())
    }
    /// Record a deep filesystem image after each mutating Vfs operation.
    pub fn enable_crash_recording(&self) -> Result<(), StoreError> {
        let mut state = self.state()?;
        state.recording = true;
        state.cuts.clear();
        Ok(())
    }
    /// Number of captured syscall cuts.
    pub fn cut_count(&self) -> Result<usize, StoreError> {
        Ok(self.state()?.cuts.len())
    }
    /// Independent filesystem states at every captured syscall cut.
    pub fn recorded_cuts(&self) -> Result<Vec<Self>, StoreError> {
        Ok(self
            .state()?
            .cuts
            .iter()
            .cloned()
            .map(|image| Self {
                state: Arc::new(Mutex::new(State::from_image(image))),
            })
            .collect())
    }
    /// Pending writes in the same stable inode order used when applying crash fates.
    pub fn unsynced_writes(&self) -> Result<Vec<UnsyncedWrite>, StoreError> {
        Ok(self
            .state()?
            .inodes
            .values()
            .flat_map(|inode| inode.writes.iter().cloned())
            .collect())
    }
    /// Deep copy for independent crash branches. Locks and open handles are not copied.
    pub fn fork(&self) -> Result<Self, StoreError> {
        let mut state = self.state()?.clone();
        state.locks.clear();
        state.recording = false;
        state.cuts.clear();
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }
    /// Crash discards volatile namespace changes. Data fates are applied to stable inode contents.
    pub fn crash(&mut self, fate: &mut dyn FnMut(&UnsyncedWrite) -> WriteFate) -> Result<(), StoreError> {
        self.crash_result(&mut |write| Ok(fate(write)))
    }
    /// Apply exactly one fate per unsynced write, rejecting missing or extra choices.
    pub fn crash_with_fates(&mut self, fates: &[WriteFate]) -> Result<(), StoreError> {
        let count = self
            .state()?
            .inodes
            .values()
            .map(|inode| inode.writes.len())
            .sum::<usize>();
        if count != fates.len() {
            return Err(invalid("crash fate count does not match unsynced writes"));
        }
        let mut fates = fates.iter().copied();
        self.crash_result(&mut |_| fates.next().ok_or_else(|| invalid("crash fate missing")))
    }
    fn crash_result(
        &mut self,
        fate: &mut dyn FnMut(&UnsyncedWrite) -> Result<WriteFate, StoreError>,
    ) -> Result<(), StoreError> {
        let mut state = self.state()?;
        for inode in state.inodes.values_mut() {
            for write in &inode.writes {
                match fate(write)? {
                    WriteFate::Lost => {}
                    WriteFate::Survive => apply(&mut inode.stable, write, write.bytes.len())?,
                    WriteFate::Torn { sectors } => {
                        let first = (write.offset / 512).saturating_add(1).saturating_mul(512);
                        let end = first.saturating_add(sectors.saturating_sub(1) as u64 * 512);
                        let count = end.saturating_sub(write.offset).min(write.bytes.len() as u64) as usize;
                        apply(&mut inode.stable, write, count)?;
                    }
                }
            }
            inode.live = inode.stable.clone();
            inode.writes.clear();
        }
        state.names = state.stable_names.clone();
        state.dirs = state.stable_dirs.clone();
        state.locks.clear();
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("generation overflow"))?;
        Ok(())
    }
    /// Inject a media fault into synced data; distinct from allowed crash write fates.
    pub fn corrupt(&self, path: &Path, offset: usize) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let id = *s.names.get(path).ok_or_else(|| not_found(path))?;
        let inode = s.inodes.get_mut(&id).ok_or_else(|| invalid("missing inode"))?;
        *inode
            .live
            .get_mut(offset)
            .ok_or_else(|| invalid("media fault outside file"))? ^= 1;
        *inode
            .stable
            .get_mut(offset)
            .ok_or_else(|| invalid("media fault outside synced file"))? ^= 1;
        Ok(())
    }
}
fn apply(bytes: &mut Vec<u8>, write: &UnsyncedWrite, count: usize) -> Result<(), StoreError> {
    if let Some(len) = write.truncate_to {
        bytes.resize(usize::try_from(len).map_err(|_| invalid("truncate too large"))?, 0);
        return Ok(());
    }
    let off = usize::try_from(write.offset).map_err(|_| invalid("write offset too large"))?;
    let end = off.checked_add(count).ok_or_else(|| invalid("write overflow"))?;
    if count == 0 {
        return Ok(());
    }
    bytes.resize(bytes.len().max(end), 0);
    bytes
        .get_mut(off..end)
        .ok_or_else(|| invalid("write range"))?
        .copy_from_slice(write.bytes.get(..count).ok_or_else(|| invalid("write prefix"))?);
    Ok(())
}
fn not_found(path: &Path) -> StoreError {
    io::Error::new(io::ErrorKind::NotFound, path.display().to_string()).into()
}
struct SimFile {
    fs: SimFs,
    id: u64,
    path: PathBuf,
    generation: u64,
}
impl SimFile {
    fn state(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        let s = self.fs.state()?;
        if self.generation != s.generation {
            return Err(invalid("file handle invalidated by crash"));
        }
        Ok(s)
    }
}
impl VfsFile for SimFile {
    fn pread(&self, off: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        let s = self.state()?;
        let inode = s.inodes.get(&self.id).ok_or_else(|| invalid("missing inode"))?;
        let off = usize::try_from(off).map_err(|_| invalid("read offset too large"))?;
        let tail = inode.live.get(off..).unwrap_or_default();
        let n = tail.len().min(buf.len());
        buf.get_mut(..n)
            .ok_or_else(|| invalid("read buffer"))?
            .copy_from_slice(tail.get(..n).ok_or_else(|| invalid("read tail"))?);
        Ok(n)
    }
    fn append(&mut self, data: &[u8]) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let count = match s.fault {
            Some(FsFault::AppendEnospc) => {
                s.fault = None;
                s.record(format!("append_failed {}", self.path.display()));
                return Err(io::Error::from_raw_os_error(28).into());
            }
            Some(FsFault::ShortWrite(n)) => {
                s.fault = None;
                Some(n.min(data.len()))
            }
            _ => None,
        };
        let inode = s.inodes.get_mut(&self.id).ok_or_else(|| invalid("missing inode"))?;
        let write = UnsyncedWrite {
            path: self.path.clone(),
            offset: inode.live.len() as u64,
            bytes: data
                .get(..count.unwrap_or(data.len()))
                .ok_or_else(|| invalid("append range"))?
                .to_vec(),
            truncate_to: None,
        };
        apply(&mut inode.live, &write, write.bytes.len())?;
        inode.writes.push(write);
        s.record(format!("append {}", self.path.display()));
        if count.is_some() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "injected short write").into());
        }
        Ok(())
    }
    fn sync_data(&mut self) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let fail = matches!(s.fault, Some(FsFault::SyncEio));
        if fail {
            s.fault = None;
        }
        let inode = s.inodes.get_mut(&self.id).ok_or_else(|| invalid("missing inode"))?;
        if fail {
            inode.live = inode.stable.clone();
            inode.writes.clear();
            s.record(format!("sync_data_failed {}", self.path.display()));
            return Err(io::Error::from_raw_os_error(5).into());
        }
        inode.stable = inode.live.clone();
        inode.writes.clear();
        s.record(format!("sync_data {}", self.path.display()));
        Ok(())
    }
    fn len(&self) -> Result<u64, StoreError> {
        Ok(self
            .state()?
            .inodes
            .get(&self.id)
            .ok_or_else(|| invalid("missing inode"))?
            .live
            .len() as u64)
    }
    fn truncate(&mut self, len: u64) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let inode = s.inodes.get_mut(&self.id).ok_or_else(|| invalid("missing inode"))?;
        let write = UnsyncedWrite {
            path: self.path.clone(),
            offset: len,
            bytes: Vec::new(),
            truncate_to: Some(len),
        };
        apply(&mut inode.live, &write, 0)?;
        inode.writes.push(write);
        s.record(format!("truncate {}", self.path.display()));
        Ok(())
    }
}
struct SimLock {
    fs: SimFs,
    path: PathBuf,
    generation: u64,
}
impl VfsLock for SimLock {}
impl Drop for SimLock {
    fn drop(&mut self) {
        if let Ok(mut s) = self.fs.state()
            && s.generation == self.generation
        {
            s.locks.remove(&self.path);
        }
    }
}
impl Vfs for SimFs {
    fn open(&self, path: &Path, opts: OpenOpts) -> Result<Box<dyn VfsFile>, StoreError> {
        let mut s = self.state()?;
        let id = if let Some(id) = s.names.get(path).copied() {
            if opts.create_new {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file exists").into());
            }
            id
        } else {
            if !opts.create && !opts.create_new {
                return Err(not_found(path));
            }
            if let Some(parent) = path.parent()
                && !s.dirs.contains(parent)
            {
                return Err(not_found(parent));
            }
            let id = s.next_inode;
            s.next_inode = id.checked_add(1).ok_or_else(|| invalid("inode overflow"))?;
            s.inodes.insert(id, Inode::default());
            s.names.insert(path.into(), id);
            s.record(format!("create {}", path.display()));
            id
        };
        let generation = s.generation;
        drop(s);
        let mut f = SimFile {
            fs: self.clone(),
            id,
            path: path.into(),
            generation,
        };
        if opts.truncate {
            f.truncate(0)?;
        }
        Ok(Box::new(f))
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), StoreError> {
        let mut s = self.state()?;
        let id = s.names.remove(from).ok_or_else(|| not_found(from))?;
        s.names.insert(to.into(), id);
        s.record(format!("rename {} {}", from.display(), to.display()));
        Ok(())
    }
    fn remove(&self, path: &Path) -> Result<(), StoreError> {
        let mut s = self.state()?;
        s.names.remove(path).ok_or_else(|| not_found(path))?;
        s.record(format!("remove {}", path.display()));
        Ok(())
    }
    fn remove_dir(&self, path: &Path) -> Result<(), StoreError> {
        let mut s = self.state()?;
        if !s.dirs.contains(path) {
            return Err(not_found(path));
        }
        if s.names.keys().chain(s.dirs.iter()).any(|p| p.parent() == Some(path)) {
            return Err(io::Error::from_raw_os_error(66).into()); // ENOTEMPTY
        }
        s.dirs.remove(path);
        s.record(format!("rmdir {}", path.display()));
        Ok(())
    }
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
        let s = self.state()?;
        if !s.dirs.contains(dir) {
            return Err(not_found(dir));
        }
        let mut names = BTreeSet::new();
        for p in s.names.keys().chain(s.dirs.iter()) {
            if p.parent() == Some(dir) {
                names.insert(p.clone());
            }
        }
        Ok(names.into_iter().collect())
    }
    fn sync_dir(&self, dir: &Path) -> Result<(), StoreError> {
        let mut s = self.state()?;
        if !s.dirs.contains(dir) {
            return Err(not_found(dir));
        }
        if matches!(s.fault, Some(FsFault::SyncEio)) {
            s.fault = None;
            s.record(format!("sync_dir_failed {}", dir.display()));
            return Err(io::Error::from_raw_os_error(5).into());
        }
        s.stable_names.retain(|p, _| p.parent() != Some(dir));
        let names = s
            .names
            .iter()
            .filter(|(p, _)| p.parent() == Some(dir))
            .map(|(p, id)| (p.clone(), *id))
            .collect::<Vec<_>>();
        s.stable_names.extend(names);
        s.stable_dirs.retain(|p| p.parent() != Some(dir));
        let dirs = s
            .dirs
            .iter()
            .filter(|p| p.parent() == Some(dir))
            .cloned()
            .collect::<Vec<_>>();
        s.stable_dirs.extend(dirs);
        s.record(format!("sync_dir {}", dir.display()));
        Ok(())
    }
    fn create_dir_all(&self, path: &Path) -> Result<(), StoreError> {
        let mut s = self.state()?;
        for p in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if s.dirs.insert(p.into()) {
                s.record(format!("mkdir {}", p.display()));
            }
        }
        Ok(())
    }
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn VfsLock>, StoreError> {
        let mut s = self.state()?;
        if !s.locks.insert(path.into()) {
            return Err(StoreError::Locked {
                path: path.into(),
                pid: std::process::id().to_string(),
            });
        }
        Ok(Box::new(SimLock {
            fs: self.clone(),
            path: path.into(),
            generation: s.generation,
        }))
    }
}
