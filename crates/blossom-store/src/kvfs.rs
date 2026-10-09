//! A filesystem over a key-value store (docs/design/DURABLE-OBJECTS.md, way A): the store's files as an inode table and
//! fixed-size chunks, so the WAL and the database run unchanged where the only storage is keys and values (a
//! Durable Object's).
//!
//! Layout, all keys UTF-8:
//!
//! - `n/<path>`: a name, its value the inode number (8 bytes, big-endian);
//! - `d/<path>`: a directory (empty value);
//! - `i/<inode>`: an inode's length in bytes (8 bytes, big-endian), inode numbers 16 hex digits;
//! - `c/<inode>/<index>`: a chunk of [`CHUNK`] bytes (the last one shorter), indexes 16 hex digits;
//! - `next`: the next inode number.
//!
//! A rename moves a name, not the bytes. A file removed while open keeps its inode until its last handle closes; an
//! inode no name holds when the filesystem is opened (a crash with a removed file open) is deleted then.
//!
//! **Durability.** [`KvFs`] makes no write durable by itself: `sync_data` and `sync_dir` do nothing. It is correct
//! over a [`KvStore`] whose writes, made while the caller handles one event, commit together before anything the
//! caller sends leaves (a Durable Object's output gate). Over [`MemKv`] it is a test double: nothing survives the
//! process.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{StoreError, Vfs, VfsFile, VfsLock, invalid};
use crate::vfs::OpenOpts;

/// The size of a chunk: big enough that a WAL record or an SSTable block takes few, small enough for any value limit
/// (a Durable Object's is 2 MB).
pub const CHUNK: usize = 64 * 1024;

/// Keys and values, synchronously: the storage a [`KvFs`] is laid over.
pub trait KvStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn put(&self, key: &str, value: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, key: &str) -> Result<(), StoreError>;
    /// The keys that start with `prefix`, in order.
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError>;
}

/// A [`KvStore`] in memory, for tests.
#[derive(Default)]
pub struct MemKv {
    map: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MemKv {
    fn map(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>>, StoreError> {
        self.map.lock().map_err(|_| invalid("key-value mutex poisoned"))
    }

    /// How many keys it holds.
    pub fn len(&self) -> Result<usize, StoreError> {
        Ok(self.map()?.len())
    }

    pub fn is_empty(&self) -> Result<bool, StoreError> {
        Ok(self.len()? == 0)
    }
}

impl KvStore for MemKv {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.map()?.get(key).cloned())
    }
    fn put(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.map()?.insert(key.to_owned(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.map()?.remove(key);
        Ok(())
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .map()?
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect())
    }
}

/// A [`KvStore`] in memory that journals its writes, for a host whose storage the program cannot call (a Durable
/// Object's, from WebAssembly): the host loads every key at start ([`JournalKv::load`]) and, after each call, takes
/// the writes ([`JournalKv::take_writes`]) and applies them to its storage before anything the call sent leaves. The
/// whole store is in memory.
#[derive(Default)]
pub struct JournalKv {
    map: Mutex<BTreeMap<String, Vec<u8>>>,
    /// The keys written since the last [`JournalKv::take_writes`]: their values, or `None` for a deletion.
    writes: Mutex<BTreeMap<String, Option<Vec<u8>>>>,
}

impl JournalKv {
    /// A store holding `entries` (what the host's storage holds), with nothing journaled.
    pub fn load(entries: impl IntoIterator<Item = (String, Vec<u8>)>) -> JournalKv {
        JournalKv {
            map: Mutex::new(entries.into_iter().collect()),
            writes: Mutex::new(BTreeMap::new()),
        }
    }

    /// The writes since the last call, each key's last: its value, or `None` for a deletion.
    pub fn take_writes(&self) -> Result<Vec<(String, Option<Vec<u8>>)>, StoreError> {
        let mut writes = self.writes.lock().map_err(|_| invalid("journal mutex poisoned"))?;
        Ok(std::mem::take(&mut *writes).into_iter().collect())
    }

    fn map(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>>, StoreError> {
        self.map.lock().map_err(|_| invalid("key-value mutex poisoned"))
    }

    fn journal(&self, key: &str, value: Option<&[u8]>) -> Result<(), StoreError> {
        let mut writes = self.writes.lock().map_err(|_| invalid("journal mutex poisoned"))?;
        writes.insert(key.to_owned(), value.map(<[u8]>::to_vec));
        Ok(())
    }
}

impl KvStore for JournalKv {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.map()?.get(key).cloned())
    }
    fn put(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.map()?.insert(key.to_owned(), value.to_vec());
        self.journal(key, Some(value))
    }
    fn delete(&self, key: &str) -> Result<(), StoreError> {
        if self.map()?.remove(key).is_some() {
            self.journal(key, None)?;
        }
        Ok(())
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .map()?
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect())
    }
}

/// A filesystem over a [`KvStore`].
#[derive(Clone)]
pub struct KvFs {
    inner: Arc<Inner>,
}

struct Inner {
    kv: Arc<dyn KvStore>,
    /// Per inode, how many handles are open (in this process), and whether its name is gone.
    open: Mutex<BTreeMap<u64, (usize, bool)>>,
    locks: Mutex<BTreeSet<PathBuf>>,
}

fn path_key(prefix: &str, path: &Path) -> Result<String, StoreError> {
    let s = path
        .to_str()
        .ok_or_else(|| invalid(format!("a path that is not UTF-8: {}", path.display())))?;
    Ok(format!("{prefix}{s}"))
}

fn inode_key(id: u64) -> String {
    format!("i/{id:016x}")
}

fn chunk_key(id: u64, index: u64) -> String {
    format!("c/{id:016x}/{index:016x}")
}

fn u64_of(bytes: &[u8], what: &str) -> Result<u64, StoreError> {
    let b: [u8; 8] = bytes
        .try_into()
        .map_err(|_| invalid(format!("{what}: {} bytes, not 8", bytes.len())))?;
    Ok(u64::from_be_bytes(b))
}

fn not_found(path: &Path) -> StoreError {
    io::Error::new(io::ErrorKind::NotFound, path.display().to_string()).into()
}

impl KvFs {
    /// The filesystem `kv` holds, its orphaned inodes deleted (see the module's documentation).
    pub fn open(kv: Arc<dyn KvStore>) -> Result<KvFs, StoreError> {
        let fs = KvFs {
            inner: Arc::new(Inner {
                kv,
                open: Mutex::new(BTreeMap::new()),
                locks: Mutex::new(BTreeSet::new()),
            }),
        };
        fs.collect_orphans()?;
        Ok(fs)
    }

    fn kv(&self) -> &dyn KvStore {
        &*self.inner.kv
    }

    fn collect_orphans(&self) -> Result<(), StoreError> {
        let mut named = BTreeSet::new();
        for k in self.kv().list("n/")? {
            let v = self.kv().get(&k)?.ok_or_else(|| invalid(format!("{k} vanished")))?;
            named.insert(u64_of(&v, &k)?);
        }
        for k in self.kv().list("i/")? {
            let id = u64::from_str_radix(&k[2..], 16).map_err(|_| invalid(format!("a malformed inode key {k}")))?;
            if !named.contains(&id) {
                self.drop_inode(id)?;
            }
        }
        Ok(())
    }

    fn inode_of(&self, path: &Path) -> Result<Option<u64>, StoreError> {
        match self.kv().get(&path_key("n/", path)?)? {
            Some(v) => Ok(Some(u64_of(&v, "a name's inode")?)),
            None => Ok(None),
        }
    }

    fn is_dir(&self, path: &Path) -> Result<bool, StoreError> {
        // The root of the namespace (the empty path, `/`) always exists.
        if path.as_os_str().is_empty() || path == Path::new("/") {
            return Ok(true);
        }
        Ok(self.kv().get(&path_key("d/", path)?)?.is_some())
    }

    fn len_of(&self, id: u64) -> Result<u64, StoreError> {
        match self.kv().get(&inode_key(id))? {
            Some(v) => u64_of(&v, "an inode's length"),
            None => Err(invalid(format!("inode {id:016x} is gone (its file was removed)"))),
        }
    }

    fn set_len(&self, id: u64, len: u64) -> Result<(), StoreError> {
        self.kv().put(&inode_key(id), &len.to_be_bytes())
    }

    fn drop_inode(&self, id: u64) -> Result<(), StoreError> {
        for k in self.kv().list(&format!("c/{id:016x}/"))? {
            self.kv().delete(&k)?;
        }
        self.kv().delete(&inode_key(id))
    }

    /// A name's removal: the inode goes now if no handle has it open, else when its last handle closes.
    fn unlink(&self, id: u64) -> Result<(), StoreError> {
        let mut open = self.inner.open.lock().map_err(|_| invalid("open-files mutex poisoned"))?;
        match open.get_mut(&id) {
            Some((n, gone)) if *n > 0 => {
                *gone = true;
                Ok(())
            }
            _ => {
                drop(open);
                self.drop_inode(id)
            }
        }
    }

    fn opened(&self, id: u64) -> Result<(), StoreError> {
        let mut open = self.inner.open.lock().map_err(|_| invalid("open-files mutex poisoned"))?;
        open.entry(id).or_insert((0, false)).0 += 1;
        Ok(())
    }

    fn closed(&self, id: u64) -> Result<(), StoreError> {
        let mut open = self.inner.open.lock().map_err(|_| invalid("open-files mutex poisoned"))?;
        let drop_it = match open.get_mut(&id) {
            Some((n, gone)) => {
                *n -= 1;
                let last = *n == 0;
                let gone = *gone;
                if last {
                    open.remove(&id);
                }
                last && gone
            }
            None => false,
        };
        drop(open);
        if drop_it { self.drop_inode(id) } else { Ok(()) }
    }

    fn new_inode(&self) -> Result<u64, StoreError> {
        let id = match self.kv().get("next")? {
            Some(v) => u64_of(&v, "the next inode")?,
            None => 1,
        };
        let next = id.checked_add(1).ok_or_else(|| invalid("inode numbers exhausted"))?;
        self.kv().put("next", &next.to_be_bytes())?;
        self.set_len(id, 0)?;
        Ok(id)
    }

    /// Writes `data` at `off` (at most the length: no holes).
    fn write_at(&self, id: u64, off: u64, data: &[u8]) -> Result<(), StoreError> {
        let len = self.len_of(id)?;
        if off > len {
            return Err(invalid("a write past the end of a file"));
        }
        let mut pos = off;
        let mut rest = data;
        while !rest.is_empty() {
            let index = pos / CHUNK as u64;
            let within = (pos % CHUNK as u64) as usize;
            let key = chunk_key(id, index);
            let mut chunk = self.kv().get(&key)?.unwrap_or_default();
            let take = rest.len().min(CHUNK - within);
            if chunk.len() < within + take {
                chunk.resize(within + take, 0);
            }
            chunk[within..within + take].copy_from_slice(&rest[..take]);
            self.kv().put(&key, &chunk)?;
            pos += take as u64;
            rest = &rest[take..];
        }
        if pos > len {
            self.set_len(id, pos)?;
        }
        Ok(())
    }
}

struct KvFile {
    fs: KvFs,
    id: u64,
}

impl Drop for KvFile {
    fn drop(&mut self) {
        // A failure here leaves the inode for the next open's collection.
        let _ = self.fs.closed(self.id);
    }
}

impl VfsFile for KvFile {
    fn pread(&self, off: u64, buf: &mut [u8]) -> Result<usize, StoreError> {
        let len = self.fs.len_of(self.id)?;
        if off >= len || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((len - off) as usize);
        let mut done = 0;
        while done < want {
            let pos = off + done as u64;
            let index = pos / CHUNK as u64;
            let within = (pos % CHUNK as u64) as usize;
            let chunk = self
                .fs
                .kv()
                .get(&chunk_key(self.id, index))?
                .ok_or_else(|| invalid(format!("chunk {index} of inode {:016x} is missing", self.id)))?;
            let take = (want - done).min(chunk.len().saturating_sub(within));
            if take == 0 {
                return Err(invalid(format!("chunk {index} of inode {:016x} is short", self.id)));
            }
            buf[done..done + take].copy_from_slice(&chunk[within..within + take]);
            done += take;
        }
        Ok(done)
    }

    fn append(&mut self, data: &[u8]) -> Result<(), StoreError> {
        let len = self.fs.len_of(self.id)?;
        self.fs.write_at(self.id, len, data)
    }

    /// Nothing to do: the store's writes commit with the caller's event (see the module's documentation).
    fn sync_data(&mut self) -> Result<(), StoreError> {
        Ok(())
    }

    fn len(&self) -> Result<u64, StoreError> {
        self.fs.len_of(self.id)
    }

    fn truncate(&mut self, len: u64) -> Result<(), StoreError> {
        let old = self.fs.len_of(self.id)?;
        if len > old {
            return self.fs.write_at(self.id, old, &vec![0; (len - old) as usize]);
        }
        let keep_chunks = len.div_ceil(CHUNK as u64);
        for k in self.fs.kv().list(&format!("c/{:016x}/", self.id))? {
            let index = u64::from_str_radix(k.rsplit('/').next().unwrap_or(""), 16)
                .map_err(|_| invalid(format!("a malformed chunk key {k}")))?;
            if index >= keep_chunks {
                self.fs.kv().delete(&k)?;
            }
        }
        let tail = (len % CHUNK as u64) as usize;
        if tail != 0 {
            let key = chunk_key(self.id, keep_chunks - 1);
            if let Some(mut chunk) = self.fs.kv().get(&key)? {
                chunk.truncate(tail);
                self.fs.kv().put(&key, &chunk)?;
            }
        }
        self.fs.set_len(self.id, len)
    }
}

struct KvLock {
    fs: KvFs,
    path: PathBuf,
}

impl VfsLock for KvLock {}

impl Drop for KvLock {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.fs.inner.locks.lock() {
            locks.remove(&self.path);
        }
    }
}

impl Vfs for KvFs {
    fn open(&self, path: &Path, opts: OpenOpts) -> Result<Box<dyn VfsFile>, StoreError> {
        let id = match self.inode_of(path)? {
            Some(_) if opts.create_new => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file exists").into());
            }
            Some(id) => id,
            None => {
                if !opts.create && !opts.create_new {
                    return Err(not_found(path));
                }
                if let Some(parent) = path.parent()
                    && !self.is_dir(parent)?
                {
                    return Err(not_found(parent));
                }
                let id = self.new_inode()?;
                self.kv().put(&path_key("n/", path)?, &id.to_be_bytes())?;
                id
            }
        };
        self.opened(id)?;
        let mut f = KvFile { fs: self.clone(), id };
        if opts.truncate {
            f.truncate(0)?;
        }
        Ok(Box::new(f))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), StoreError> {
        let id = self.inode_of(from)?.ok_or_else(|| not_found(from))?;
        if let Some(old) = self.inode_of(to)?
            && old != id
        {
            self.unlink(old)?;
        }
        self.kv().put(&path_key("n/", to)?, &id.to_be_bytes())?;
        self.kv().delete(&path_key("n/", from)?)
    }

    fn remove(&self, path: &Path) -> Result<(), StoreError> {
        let id = self.inode_of(path)?.ok_or_else(|| not_found(path))?;
        self.kv().delete(&path_key("n/", path)?)?;
        self.unlink(id)
    }

    fn remove_dir(&self, path: &Path) -> Result<(), StoreError> {
        if !self.is_dir(path)? {
            return Err(not_found(path));
        }
        if !self.list(path)?.is_empty() {
            return Err(io::Error::other(format!("{} is not empty", path.display())).into());
        }
        self.kv().delete(&path_key("d/", path)?)
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
        if !self.is_dir(dir)? {
            return Err(not_found(dir));
        }
        let mut out = BTreeSet::new();
        for prefix in ["n/", "d/"] {
            let start = path_key(prefix, dir)?;
            let start = if start.ends_with('/') { start } else { format!("{start}/") };
            for k in self.kv().list(&start)? {
                let p = PathBuf::from(&k[prefix.len()..]);
                if p.parent() == Some(dir) {
                    out.insert(p);
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Nothing to do: the store's writes commit with the caller's event (see the module's documentation).
    fn sync_dir(&self, dir: &Path) -> Result<(), StoreError> {
        if !self.is_dir(dir)? {
            return Err(not_found(dir));
        }
        Ok(())
    }

    /// One lock per path in this process: an object runs one node at a time, so no other process shares its store.
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn VfsLock>, StoreError> {
        let mut locks = self.inner.locks.lock().map_err(|_| invalid("lock mutex poisoned"))?;
        if !locks.insert(path.into()) {
            return Err(StoreError::Locked {
                path: path.into(),
                pid: "this process".to_owned(),
            });
        }
        Ok(Box::new(KvLock {
            fs: self.clone(),
            path: path.into(),
        }))
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), StoreError> {
        for p in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !p.as_os_str().is_empty() && p != Path::new("/") {
                self.kv().put(&path_key("d/", p)?, &[])?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::read_path;

    fn fs() -> (Arc<MemKv>, KvFs) {
        let kv = Arc::new(MemKv::default());
        let fs = KvFs::open(kv.clone()).unwrap();
        (kv, fs)
    }

    #[test]
    fn passes_the_vfs_conformance_suite() {
        let (_, fs) = fs();
        crate::conformance::vfs_suite(&fs, Path::new("/store/x")).unwrap();
    }

    #[test]
    fn files_span_chunks() {
        let (_, fs) = fs();
        fs.create_dir_all(Path::new("/d")).unwrap();
        let data: Vec<u8> = (0..(3 * CHUNK + 17)).map(|i| (i % 251) as u8).collect();
        let mut f = fs.open(Path::new("/d/f"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
        // Appends that straddle chunk boundaries.
        for piece in data.chunks(CHUNK / 3 + 5) {
            f.append(piece).unwrap();
        }
        assert_eq!(read_path(&fs, Path::new("/d/f")).unwrap(), data);
        let mut buf = vec![0; 100];
        assert_eq!(f.pread(CHUNK as u64 - 50, &mut buf).unwrap(), 100);
        assert_eq!(buf, data[CHUNK - 50..CHUNK + 50]);
        f.truncate(CHUNK as u64 + 3).unwrap();
        assert_eq!(read_path(&fs, Path::new("/d/f")).unwrap(), data[..CHUNK + 3]);
        assert_eq!(f.pread(CHUNK as u64 + 3, &mut buf).unwrap(), 0);
    }

    #[test]
    fn a_rename_moves_the_name_and_replaces_the_target() {
        let (kv, fs) = fs();
        fs.create_dir_all(Path::new("/d")).unwrap();
        let mut a = fs.open(Path::new("/d/a"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
        a.append(b"new").unwrap();
        let mut b = fs.open(Path::new("/d/b"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
        b.append(b"old").unwrap();
        drop(b);
        fs.rename(Path::new("/d/a"), Path::new("/d/b")).unwrap();
        assert_eq!(read_path(&fs, Path::new("/d/b")).unwrap(), b"new");
        assert_eq!(fs.list(Path::new("/d")).unwrap(), vec![PathBuf::from("/d/b")]);
        drop(a);
        // The replaced file's inode and chunks are gone: a name, its inode, one chunk, the directories, `next`.
        assert_eq!(kv.list("i/").unwrap().len(), 1);
        assert_eq!(kv.list("c/").unwrap().len(), 1);
    }

    #[test]
    fn a_removed_file_open_elsewhere_lives_until_its_handle_closes() {
        let (kv, fs) = fs();
        fs.create_dir_all(Path::new("/d")).unwrap();
        let mut f = fs.open(Path::new("/d/f"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
        f.append(b"still here").unwrap();
        fs.remove(Path::new("/d/f")).unwrap();
        let mut buf = [0; 10];
        assert_eq!(f.pread(0, &mut buf).unwrap(), 10);
        assert_eq!(&buf, b"still here");
        drop(f);
        assert!(kv.list("i/").unwrap().is_empty());
        assert!(kv.list("c/").unwrap().is_empty());
    }

    #[test]
    fn opening_collects_inodes_no_name_holds() {
        let kv = Arc::new(MemKv::default());
        {
            let fs = KvFs::open(kv.clone()).unwrap();
            fs.create_dir_all(Path::new("/d")).unwrap();
            let mut f = fs.open(Path::new("/d/f"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
            f.append(b"x").unwrap();
            fs.remove(Path::new("/d/f")).unwrap();
            // The process "crashes" with the handle open: nothing drops it.
            std::mem::forget(f);
        }
        assert_eq!(kv.list("i/").unwrap().len(), 1);
        let _fs = KvFs::open(kv.clone()).unwrap();
        assert!(kv.list("i/").unwrap().is_empty());
        assert!(kv.list("c/").unwrap().is_empty());
    }

    #[test]
    fn a_journal_replayed_into_another_store_is_the_same_filesystem() {
        let journal = Arc::new(JournalKv::default());
        let fs = KvFs::open(journal.clone()).unwrap();
        fs.create_dir_all(Path::new("/d")).unwrap();
        let mut f = fs.open(Path::new("/d/f"), OpenOpts { create: true, ..OpenOpts::default() }).unwrap();
        f.append(&vec![7; CHUNK + 10]).unwrap();
        drop(f);
        fs.rename(Path::new("/d/f"), Path::new("/d/g")).unwrap();
        // The host's storage: everything the journal said, in one go.
        let host = MemKv::default();
        for (k, v) in journal.take_writes().unwrap() {
            match v {
                Some(v) => host.put(&k, &v).unwrap(),
                None => host.delete(&k).unwrap(),
            }
        }
        assert!(journal.take_writes().unwrap().is_empty());
        // A later start loads the host's keys: the same files.
        let entries: Vec<(String, Vec<u8>)> = host
            .list("")
            .unwrap()
            .into_iter()
            .map(|k| {
                let v = host.get(&k).unwrap().unwrap();
                (k, v)
            })
            .collect();
        let again = KvFs::open(Arc::new(JournalKv::load(entries))).unwrap();
        assert_eq!(read_path(&again, Path::new("/d/g")).unwrap(), vec![7; CHUNK + 10]);
        assert!(again.list(Path::new("/d")).unwrap() == vec![PathBuf::from("/d/g")]);
    }

    #[test]
    fn the_wal_runs_over_it() {
        let (_, fs) = fs();
        let fs: Arc<dyn Vfs> = Arc::new(fs);
        let dir = Path::new("/node/wal");
        fs.create_dir_all(dir).unwrap();
        let header = crate::SegmentHeader {
            format: 1,
            store_uuid: [7; 16],
            segment_seq: 1,
            restarts: 1,
            boot_nonce: 3,
            lsn_base: crate::Lsn(0),
            catalog: b"opaque catalog".to_vec(),
        };
        let mut wal = crate::FileWal::create(fs.clone(), dir, header, crate::Lsn(0)).unwrap();
        crate::conformance::wal_suite(&mut wal).unwrap();
        let scan = crate::WalScan::scan(&*fs, dir, [7; 16], false).unwrap();
        assert_eq!(scan.records().count(), 2);
    }
}
