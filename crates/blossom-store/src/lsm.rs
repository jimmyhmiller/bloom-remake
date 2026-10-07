//! A versioned LSM tree of byte keys over the [`Vfs`] (docs/design/DATABASE.md §2): the store behind a node's
//! database.
//!
//! An entry is `(key, version, op)`; entries order by key, then version descending. A read as of version `v` takes,
//! per key, the newest entry at or below `v`: the key is present when it is a put. New entries go to the memtable,
//! always at a version above every entry before (versions are released ticks). A flush writes the memtable as an
//! SSTable and then the manifest naming it; a compaction merges SSTables of a similar size into one, keeping every
//! version above the history horizon and, per key, the newest at or below it.
//!
//! Files under the tree's directory: `MANIFEST` (BLAKE3 over a JSON body, written atomically) and `sst/<id>.sst`.
//! An SSTable is data blocks of entries, each followed by its CRC32C; an index of every block's first and last key,
//! offset and length, followed by its CRC32C; and a footer (`FOOTER` bytes) with the index's place, the entry count,
//! the version range and its own CRC32C. An SSTable no manifest names (a flush or compaction a crash cut short) is
//! deleted when the tree opens.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::vfs::{atomic_write, read_path};
use crate::{OpenOpts, StoreError, Vfs, VfsFile, invalid};

/// What an entry does to its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Put,
    Del,
}

impl Op {
    fn byte(self) -> u8 {
        match self {
            Op::Put => 1,
            Op::Del => 2,
        }
    }

    fn of(b: u8) -> Option<Op> {
        match b {
            1 => Some(Op::Put),
            2 => Some(Op::Del),
            _ => None,
        }
    }
}

/// How a tree flushes, writes and compacts.
#[derive(Clone, Copy, Debug)]
pub struct LsmOptions {
    /// The memtable size past which [`Lsm::needs_flush`] says to flush.
    pub memtable_bytes: usize,
    /// The size an SSTable's data blocks are cut at.
    pub block_bytes: usize,
    /// How many SSTables within a factor of two of each other in size merge.
    pub tier: usize,
    /// The most SSTables before all of them merge.
    pub max_tables: usize,
    /// How many versions back from the newest an as-of read may go: compaction keeps every version above
    /// `newest - history` and, per key, the newest at or below it.
    pub history: u64,
}

impl Default for LsmOptions {
    fn default() -> Self {
        LsmOptions {
            memtable_bytes: 4 << 20,
            block_bytes: 16 << 10,
            tier: 4,
            max_tables: 12,
            history: 65_536,
        }
    }
}

const MAGIC: &[u8; 8] = b"BLSSST01";
/// The footer: magic, index offset (u64), index length (u32), entries (u64), lowest and highest version (u64), CRC32C.
const FOOTER: usize = 8 + 8 + 4 + 8 + 8 + 8 + 4;
const MANIFEST_FORMAT: u16 = 1;

fn corrupt(path: &Path, offset: u64, reason: impl Into<String>) -> StoreError {
    StoreError::Corruption {
        path: path.to_path_buf(),
        offset,
        reason: reason.into(),
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(input: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = input.split_first()?;
        *input = rest;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = input.split_at_checked(n)?;
    *input = rest;
    Some(head)
}

fn get_u64(input: &mut &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(take(input, 8)?.try_into().ok()?))
}

fn get_u32(input: &mut &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(take(input, 4)?.try_into().ok()?))
}

/// Bytes followed by their CRC32C: the bytes, if it holds.
fn checked(bytes: &[u8]) -> Option<&[u8]> {
    let (body, crc) = bytes.split_at_checked(bytes.len().checked_sub(4)?)?;
    (u32::from_le_bytes(crc.try_into().ok()?) == crc32c::crc32c(body)).then_some(body)
}

fn with_crc(mut bytes: Vec<u8>) -> Vec<u8> {
    let crc = crc32c::crc32c(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

/// An entry as SSTables and the memtable keep it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: Vec<u8>,
    pub version: u64,
    pub op: Op,
}

fn encode_entry(out: &mut Vec<u8>, key: &[u8], version: u64, op: Op) {
    put_varint(out, key.len() as u64);
    out.extend_from_slice(key);
    out.extend_from_slice(&version.to_le_bytes());
    out.push(op.byte());
}

fn decode_block(path: &Path, offset: u64, mut body: &[u8]) -> Result<Vec<Entry>, StoreError> {
    let bad = || corrupt(path, offset, "a malformed block");
    let n = get_varint(&mut body).ok_or_else(bad)?;
    let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0).min(body.len()));
    for _ in 0..n {
        let len = usize::try_from(get_varint(&mut body).ok_or_else(bad)?).map_err(|_| bad())?;
        let key = take(&mut body, len).ok_or_else(bad)?.to_vec();
        let version = get_u64(&mut body).ok_or_else(bad)?;
        let op = take(&mut body, 1)
            .and_then(|b| b.first().copied())
            .and_then(Op::of)
            .ok_or_else(bad)?;
        out.push(Entry { key, version, op });
    }
    if !body.is_empty() {
        return Err(bad());
    }
    Ok(out)
}

/// One data block as the index knows it.
#[derive(Clone, Debug)]
struct BlockRef {
    first: Vec<u8>,
    last: Vec<u8>,
    offset: u64,
    len: u32,
}

/// What a manifest records of an SSTable.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct TableMeta {
    id: u64,
    bytes: u64,
    entries: u64,
    min_version: u64,
    max_version: u64,
}

/// An open SSTable.
struct Sst {
    meta: TableMeta,
    path: PathBuf,
    /// Shared by readers on several threads; a read holds it for one block.
    file: Mutex<Box<dyn VfsFile>>,
    index: Vec<BlockRef>,
}

impl Sst {
    fn open(fs: &dyn Vfs, path: PathBuf, meta: TableMeta) -> Result<Sst, StoreError> {
        let file = fs.open(&path, OpenOpts::default())?;
        let len = file.len()?;
        if len != meta.bytes {
            return Err(corrupt(
                &path,
                len,
                format!("{len} bytes, the manifest says {}", meta.bytes),
            ));
        }
        let footer_at = len
            .checked_sub(FOOTER as u64)
            .ok_or_else(|| corrupt(&path, 0, "shorter than its footer"))?;
        let mut footer = vec![0u8; FOOTER];
        read_exact(&*file, footer_at, &mut footer)?;
        let body = checked(&footer).ok_or_else(|| corrupt(&path, footer_at, "footer checksum"))?;
        let mut f = body;
        let bad = || corrupt(&path, footer_at, "a malformed footer");
        if take(&mut f, 8) != Some(MAGIC.as_slice()) {
            return Err(corrupt(&path, footer_at, "not an SSTable"));
        }
        let index_at = get_u64(&mut f).ok_or_else(bad)?;
        let index_len = get_u32(&mut f).ok_or_else(bad)?;
        let entries = get_u64(&mut f).ok_or_else(bad)?;
        let (min_version, max_version) = (get_u64(&mut f).ok_or_else(bad)?, get_u64(&mut f).ok_or_else(bad)?);
        if (entries, min_version, max_version) != (meta.entries, meta.min_version, meta.max_version) {
            return Err(corrupt(&path, footer_at, "the footer disagrees with the manifest"));
        }
        let mut raw = vec![0u8; index_len as usize];
        read_exact(&*file, index_at, &mut raw)?;
        let mut ix = checked(&raw).ok_or_else(|| corrupt(&path, index_at, "index checksum"))?;
        let bad = || corrupt(&path, index_at, "a malformed index");
        let n = get_varint(&mut ix).ok_or_else(bad)?;
        let mut index = Vec::new();
        for _ in 0..n {
            let fl = usize::try_from(get_varint(&mut ix).ok_or_else(bad)?).map_err(|_| bad())?;
            let first = take(&mut ix, fl).ok_or_else(bad)?.to_vec();
            let ll = usize::try_from(get_varint(&mut ix).ok_or_else(bad)?).map_err(|_| bad())?;
            let last = take(&mut ix, ll).ok_or_else(bad)?.to_vec();
            let offset = get_u64(&mut ix).ok_or_else(bad)?;
            let len = get_u32(&mut ix).ok_or_else(bad)?;
            index.push(BlockRef {
                first,
                last,
                offset,
                len,
            });
        }
        Ok(Sst {
            meta,
            path,
            file: Mutex::new(file),
            index,
        })
    }

    fn block(&self, b: &BlockRef) -> Result<Vec<Entry>, StoreError> {
        let mut raw = vec![0u8; b.len as usize];
        {
            let file = self.file.lock().map_err(|_| invalid("an SSTable's lock is poisoned"))?;
            read_exact(&**file, b.offset, &mut raw)?;
        }
        let body = checked(&raw).ok_or_else(|| corrupt(&self.path, b.offset, "block checksum"))?;
        decode_block(&self.path, b.offset, body)
    }

    /// The entries whose key starts with `prefix`, at or below `as_of`, in order.
    fn scan(&self, prefix: &[u8], as_of: u64, out: &mut Vec<Entry>) -> Result<(), StoreError> {
        let start = self.index.partition_point(|b| b.last.as_slice() < prefix);
        for b in self.index.iter().skip(start) {
            if b.first.as_slice() > prefix && !b.first.starts_with(prefix) {
                break;
            }
            for e in self.block(b)? {
                if e.key.starts_with(prefix) && e.version <= as_of {
                    out.push(e);
                }
            }
        }
        Ok(())
    }
}

fn read_exact(file: &dyn VfsFile, mut off: u64, mut buf: &mut [u8]) -> Result<(), StoreError> {
    while !buf.is_empty() {
        let n = file.pread(off, buf)?;
        if n == 0 {
            return Err(invalid("a read past the end of a file"));
        }
        buf = buf.get_mut(n..).ok_or_else(|| invalid("read overflow"))?;
        off += n as u64;
    }
    Ok(())
}

/// Writes the sorted `entries` as the SSTable `path` (to a temporary name, synced, then renamed and the directory
/// synced): what a manifest records of it.
fn write_table(
    fs: &dyn Vfs,
    path: &Path,
    id: u64,
    block_bytes: usize,
    entries: impl Iterator<Item = Result<Entry, StoreError>>,
) -> Result<Option<TableMeta>, StoreError> {
    let tmp = path.with_extension("tmp");
    let mut file = fs.open(
        &tmp,
        OpenOpts {
            create: true,
            truncate: true,
            ..OpenOpts::default()
        },
    )?;
    let mut offset = 0u64;
    let mut index: Vec<BlockRef> = Vec::new();
    let mut block: Vec<u8> = Vec::new();
    let mut in_block = 0u64;
    let mut first: Option<Vec<u8>> = None;
    let mut last: Vec<u8> = Vec::new();
    let (mut count, mut min_v, mut max_v) = (0u64, u64::MAX, 0u64);
    let mut flush_block = |file: &mut Box<dyn VfsFile>,
                           block: &mut Vec<u8>,
                           in_block: &mut u64,
                           first: &mut Option<Vec<u8>>,
                           last: &[u8]|
     -> Result<(), StoreError> {
        let Some(f) = first.take() else { return Ok(()) };
        let mut body = Vec::with_capacity(block.len() + 10);
        put_varint(&mut body, *in_block);
        body.append(block);
        let bytes = with_crc(body);
        file.append(&bytes)?;
        let len = u32::try_from(bytes.len()).map_err(|_| invalid("a block over 4 GiB"))?;
        index.push(BlockRef {
            first: f,
            last: last.to_vec(),
            offset,
            len,
        });
        offset += bytes.len() as u64;
        *in_block = 0;
        Ok(())
    };
    for e in entries {
        let e = e?;
        if first.is_none() {
            first = Some(e.key.clone());
        }
        encode_entry(&mut block, &e.key, e.version, e.op);
        in_block += 1;
        count += 1;
        min_v = min_v.min(e.version);
        max_v = max_v.max(e.version);
        last = e.key;
        if block.len() >= block_bytes {
            flush_block(&mut file, &mut block, &mut in_block, &mut first, &last)?;
        }
    }
    flush_block(&mut file, &mut block, &mut in_block, &mut first, &last)?;
    if count == 0 {
        drop(file);
        fs.remove(&tmp)?;
        return Ok(None);
    }
    let mut ix = Vec::new();
    put_varint(&mut ix, index.len() as u64);
    for b in &index {
        put_varint(&mut ix, b.first.len() as u64);
        ix.extend_from_slice(&b.first);
        put_varint(&mut ix, b.last.len() as u64);
        ix.extend_from_slice(&b.last);
        ix.extend_from_slice(&b.offset.to_le_bytes());
        ix.extend_from_slice(&b.len.to_le_bytes());
    }
    let ix = with_crc(ix);
    let index_at = offset;
    file.append(&ix)?;
    let mut footer = Vec::with_capacity(FOOTER);
    footer.extend_from_slice(MAGIC);
    footer.extend_from_slice(&index_at.to_le_bytes());
    footer.extend_from_slice(
        &u32::try_from(ix.len())
            .map_err(|_| invalid("an index over 4 GiB"))?
            .to_le_bytes(),
    );
    footer.extend_from_slice(&count.to_le_bytes());
    footer.extend_from_slice(&min_v.to_le_bytes());
    footer.extend_from_slice(&max_v.to_le_bytes());
    let footer = with_crc(footer);
    file.append(&footer)?;
    file.sync_data()?;
    let bytes = file.len()?;
    drop(file);
    fs.rename(&tmp, path)?;
    fs.sync_dir(path.parent().unwrap_or(Path::new(".")))?;
    Ok(Some(TableMeta {
        id,
        bytes,
        entries: count,
        min_version: min_v,
        max_version: max_v,
    }))
}

/// Writes `m` as `dir`'s manifest, atomically: BLAKE3 over the body, then the body.
fn write_manifest(fs: &dyn Vfs, dir: &Path, m: &Manifest) -> Result<(), StoreError> {
    let body = serde_json::to_vec(m).map_err(|e| invalid(e.to_string()))?;
    let mut bytes = blake3::hash(&body).as_bytes().to_vec();
    bytes.extend_from_slice(&body);
    atomic_write(fs, &dir.join("MANIFEST"), &bytes)
}

/// The manifest's body.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u16,
    /// Live SSTables, newest first.
    tables: Vec<TableMeta>,
    /// The version (tick) and mark the flushed entries cover: every entry at or below the version is in the
    /// tables.
    flushed_version: u64,
    flushed_mark: u64,
    /// The oldest version an as-of read may ask for: compaction merged away what is below it.
    floor: u64,
    next_id: u64,
}

impl Manifest {
    fn empty() -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            tables: Vec::new(),
            flushed_version: 0,
            flushed_mark: 0,
            floor: 0,
            next_id: 1,
        }
    }
}

type Mem = BTreeMap<(Vec<u8>, Reverse<u64>), Op>;

/// What the tree holds now.
struct State {
    mem: Mem,
    mem_bytes: usize,
    /// A memtable being flushed (still read), with the version and mark it covers.
    frozen: Option<(Arc<Mem>, u64, u64)>,
    tables: Vec<Arc<Sst>>,
    manifest: Manifest,
    /// The newest version applied, and the caller's mark with it.
    applied: u64,
    applied_mark: u64,
}

/// What a flush or compaction left durable: the version and mark the tables cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flushed {
    pub version: u64,
    pub mark: u64,
}

/// A versioned LSM tree.
pub struct Lsm {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
    opts: LsmOptions,
    state: RwLock<State>,
    /// Flushes and compactions run one at a time.
    work: Mutex<()>,
    /// Opened by a reader: nothing it applies is written.
    read_only: bool,
}

impl Lsm {
    /// Opens the tree under `dir` (creating it): the tables its manifest names; the files a crash left unnamed are
    /// removed.
    pub fn open(fs: Arc<dyn Vfs>, dir: &Path, opts: LsmOptions) -> Result<Lsm, StoreError> {
        Lsm::open_with(fs, dir, opts, false)
    }

    /// Opens an existing tree without changing any file (a tool reading a stopped node's database): what it applies
    /// stays in memory, and it is never flushed or compacted. A directory with no manifest is refused.
    pub fn open_read_only(fs: Arc<dyn Vfs>, dir: &Path, opts: LsmOptions) -> Result<Lsm, StoreError> {
        Lsm::open_with(fs, dir, opts, true)
    }

    fn open_with(fs: Arc<dyn Vfs>, dir: &Path, opts: LsmOptions, read_only: bool) -> Result<Lsm, StoreError> {
        let sst_dir = dir.join("sst");
        if read_only {
            let named = fs
                .list(dir)?
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "MANIFEST"));
            if !named {
                return Err(invalid(format!("{} holds no database", dir.display())));
            }
        } else {
            crate::vfs::durable_dir(&*fs, &sst_dir)?;
        }
        let manifest_path = dir.join("MANIFEST");
        let manifest = if fs.list(dir)?.iter().any(|p| p == &manifest_path) {
            let bytes = read_path(&*fs, &manifest_path)?;
            let body = bytes
                .get(32..)
                .ok_or_else(|| corrupt(&manifest_path, 0, "MANIFEST truncated"))?;
            if bytes.get(..32) != Some(blake3::hash(body).as_bytes().as_slice()) {
                return Err(corrupt(&manifest_path, 0, "MANIFEST checksum"));
            }
            let m: Manifest = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
            if m.format != MANIFEST_FORMAT {
                return Err(invalid(format!(
                    "database format {} (this build reads {MANIFEST_FORMAT})",
                    m.format
                )));
            }
            m
        } else {
            // A new tree has a manifest from the start: a reader finds a database, empty as it is.
            let m = Manifest::empty();
            if !read_only {
                write_manifest(&*fs, dir, &m)?;
            }
            m
        };
        let mut tables = Vec::new();
        for t in &manifest.tables {
            tables.push(Arc::new(Sst::open(
                &*fs,
                sst_dir.join(format!("{}.sst", t.id)),
                t.clone(),
            )?));
        }
        // What a crash left: tables no manifest names, and temporary files.
        let named: Vec<PathBuf> = manifest
            .tables
            .iter()
            .map(|t| sst_dir.join(format!("{}.sst", t.id)))
            .collect();
        let mut removed = false;
        if !read_only {
            for p in fs.list(&sst_dir)? {
                if !named.contains(&p) {
                    fs.remove(&p)?;
                    removed = true;
                }
            }
        }
        if removed {
            fs.sync_dir(&sst_dir)?;
        }
        let (applied, applied_mark) = (manifest.flushed_version, manifest.flushed_mark);
        Ok(Lsm {
            fs,
            dir: dir.to_path_buf(),
            opts,
            state: RwLock::new(State {
                mem: Mem::new(),
                mem_bytes: 0,
                frozen: None,
                tables,
                manifest,
                applied,
                applied_mark,
            }),
            work: Mutex::new(()),
            read_only,
        })
    }

    fn read(&self) -> Result<std::sync::RwLockReadGuard<'_, State>, StoreError> {
        self.state
            .read()
            .map_err(|_| invalid("the database's lock is poisoned"))
    }

    fn write(&self) -> Result<std::sync::RwLockWriteGuard<'_, State>, StoreError> {
        self.state
            .write()
            .map_err(|_| invalid("the database's lock is poisoned"))
    }

    /// The version and mark the durable tables cover: a recovery applies what came after.
    pub fn flushed(&self) -> Result<Flushed, StoreError> {
        let s = self.read()?;
        Ok(Flushed {
            version: s.manifest.flushed_version,
            mark: s.manifest.flushed_mark,
        })
    }

    /// The newest version applied.
    pub fn applied(&self) -> Result<u64, StoreError> {
        Ok(self.read()?.applied)
    }

    /// The oldest version an as-of read may ask for.
    pub fn floor(&self) -> Result<u64, StoreError> {
        Ok(self.read()?.manifest.floor)
    }

    /// Applies one version's changes: `version` above every one before, and `mark` a position of the caller's that the
    /// version reaches (what [`Lsm::flushed`] reports back once the version is in the tables).
    pub fn apply(
        &self,
        version: u64,
        mark: u64,
        changes: impl IntoIterator<Item = (Vec<u8>, Op)>,
    ) -> Result<(), StoreError> {
        let mut s = self.write()?;
        if version <= s.applied && s.applied != 0 {
            return Err(invalid(format!(
                "version {version} applied after version {}",
                s.applied
            )));
        }
        for (key, op) in changes {
            s.mem_bytes += key.len() + 16;
            s.mem.insert((key, Reverse(version)), op);
        }
        s.applied = version;
        s.applied_mark = mark;
        Ok(())
    }

    /// Whether the memtable has grown past its size.
    pub fn needs_flush(&self) -> Result<bool, StoreError> {
        Ok(self.read()?.mem_bytes >= self.opts.memtable_bytes)
    }

    /// Writes the memtable as an SSTable and the manifest naming it: what the tables now cover (`None` when the
    /// memtable was empty).
    pub fn flush(&self) -> Result<Option<Flushed>, StoreError> {
        if self.read_only {
            return Err(invalid("a database opened read-only is not flushed or compacted"));
        }
        let _work = self
            .work
            .lock()
            .map_err(|_| invalid("the database's work lock is poisoned"))?;
        let (frozen, version, mark, id) = {
            let mut s = self.write()?;
            // A flush that failed left its memtable frozen: it goes out with this one (its versions are older).
            if let Some((old, _, _)) = s.frozen.take() {
                for (k, op) in old.iter() {
                    s.mem.entry(k.clone()).or_insert(*op);
                    s.mem_bytes += k.0.len() + 16;
                }
            }
            if s.mem.is_empty() {
                return Ok(None);
            }
            let mem = Arc::new(std::mem::take(&mut s.mem));
            s.mem_bytes = 0;
            let (version, mark) = (s.applied, s.applied_mark);
            s.frozen = Some((mem.clone(), version, mark));
            let id = s.manifest.next_id;
            s.manifest.next_id += 1;
            (mem, version, mark, id)
        };
        let path = self.dir.join("sst").join(format!("{id}.sst"));
        let entries = frozen.iter().map(|((key, Reverse(version)), op)| {
            Ok(Entry {
                key: key.clone(),
                version: *version,
                op: *op,
            })
        });
        let meta = write_table(&*self.fs, &path, id, self.opts.block_bytes, entries)?;
        let table = match meta {
            Some(m) => Some(Arc::new(Sst::open(&*self.fs, path, m)?)),
            None => None,
        };
        let mut s = self.write()?;
        let mut manifest = s.manifest.clone();
        if let Some(t) = &table {
            manifest.tables.insert(0, t.meta.clone());
        }
        manifest.flushed_version = version;
        manifest.flushed_mark = mark;
        self.write_manifest(&manifest)?;
        if let Some(t) = table {
            s.tables.insert(0, t);
        }
        s.manifest = manifest;
        s.frozen = None;
        Ok(Some(Flushed { version, mark }))
    }

    fn write_manifest(&self, m: &Manifest) -> Result<(), StoreError> {
        write_manifest(&*self.fs, &self.dir, m)
    }

    /// Merges SSTables when some are due (see [`LsmOptions`]): whether it merged.
    pub fn compact(&self) -> Result<bool, StoreError> {
        if self.read_only {
            return Err(invalid("a database opened read-only is not flushed or compacted"));
        }
        let _work = self
            .work
            .lock()
            .map_err(|_| invalid("the database's work lock is poisoned"))?;
        let (chosen, horizon, bottom, id) = {
            let mut s = self.write()?;
            let Some(chosen) = pick(&s.tables, self.opts) else {
                return Ok(false);
            };
            let horizon = s.applied.saturating_sub(self.opts.history);
            // A delete at or below the horizon may go when no table outside the merge holds a version that old.
            let bottom = s
                .tables
                .iter()
                .filter(|t| !chosen.iter().any(|c| Arc::ptr_eq(c, t)))
                .all(|t| t.meta.min_version > horizon);
            let id = s.manifest.next_id;
            s.manifest.next_id += 1;
            (chosen, horizon, bottom, id)
        };
        let path = self.dir.join("sst").join(format!("{id}.sst"));
        let merged = Merge::new(&chosen)?;
        let kept = Gc {
            inner: merged,
            horizon,
            bottom,
            key: None,
            settled: false,
        };
        let meta = write_table(&*self.fs, &path, id, self.opts.block_bytes, kept)?;
        let table = match meta {
            Some(m) => Some(Arc::new(Sst::open(&*self.fs, path, m)?)),
            None => None,
        };
        let mut s = self.write()?;
        let mut manifest = s.manifest.clone();
        let gone: Vec<u64> = chosen.iter().map(|t| t.meta.id).collect();
        // The merged table takes the place of the newest table it merged, keeping the newest-first order of the rest.
        let at = manifest
            .tables
            .iter()
            .position(|t| gone.contains(&t.id))
            .ok_or_else(|| invalid("the tables compacted are not in the manifest"))?;
        manifest.tables.retain(|t| !gone.contains(&t.id));
        if let Some(t) = &table {
            manifest.tables.insert(at.min(manifest.tables.len()), t.meta.clone());
        }
        manifest.floor = manifest.floor.max(horizon);
        self.write_manifest(&manifest)?;
        s.tables.retain(|t| !gone.contains(&t.meta.id));
        if let Some(t) = table {
            let at = at.min(s.tables.len());
            s.tables.insert(at, t);
        }
        s.manifest = manifest;
        drop(s);
        for t in &chosen {
            self.fs.remove(&t.path)?;
        }
        self.fs.sync_dir(&self.dir.join("sst"))?;
        Ok(true)
    }

    /// The keys starting with `prefix` present as of version `as_of`, in order. A version below the floor (merged
    /// away) or above the newest applied is refused.
    pub fn scan(&self, prefix: &[u8], as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        let (mut newest, tables) = {
            let s = self.read()?;
            if as_of < s.manifest.floor {
                return Err(invalid(format!(
                    "version {as_of} is past the history kept (from version {})",
                    s.manifest.floor
                )));
            }
            if as_of > s.applied {
                return Err(invalid(format!(
                    "version {as_of} is newer than the newest applied ({})",
                    s.applied
                )));
            }
            // The newest entry at or below `as_of` per key, from the memtables (newer than every table).
            let mut newest: BTreeMap<Vec<u8>, (u64, Op)> = BTreeMap::new();
            let mems = std::iter::once(&s.mem).chain(s.frozen.as_ref().map(|(m, _, _)| &**m));
            for mem in mems {
                for ((key, Reverse(version)), op) in mem.range((prefix.to_vec(), Reverse(u64::MAX))..) {
                    if !key.starts_with(prefix) {
                        break;
                    }
                    if *version <= as_of {
                        let slot = newest.entry(key.clone()).or_insert((*version, *op));
                        if *version > slot.0 {
                            *slot = (*version, *op);
                        }
                    }
                }
            }
            (newest, s.tables.clone())
        };
        let mut found = Vec::new();
        for t in &tables {
            found.clear();
            t.scan(prefix, as_of, &mut found)?;
            for e in found.drain(..) {
                let slot = newest.entry(e.key).or_insert((e.version, e.op));
                if e.version > slot.0 {
                    *slot = (e.version, e.op);
                }
            }
        }
        newest.retain(|_, (_, op)| *op == Op::Put);
        Ok(newest.into_keys().collect())
    }

    /// The SSTables now live: their ids and sizes, newest first (for tests and tooling).
    pub fn tables(&self) -> Result<Vec<(u64, u64)>, StoreError> {
        Ok(self.read()?.tables.iter().map(|t| (t.meta.id, t.meta.bytes)).collect())
    }
}

/// The tables to merge, if any are due: four or more within a factor of two in size, or all of them past the most.
fn pick(tables: &[Arc<Sst>], opts: LsmOptions) -> Option<Vec<Arc<Sst>>> {
    if tables.len() > opts.max_tables {
        return Some(tables.to_vec());
    }
    let mut by_size: Vec<&Arc<Sst>> = tables.iter().collect();
    by_size.sort_by_key(|t| t.meta.bytes);
    for (i, small) in by_size.iter().enumerate() {
        let group: Vec<Arc<Sst>> = by_size
            .iter()
            .skip(i)
            .take_while(|t| t.meta.bytes <= small.meta.bytes.saturating_mul(2).max(1))
            .map(|t| Arc::clone(t))
            .collect();
        if group.len() >= opts.tier.max(2) {
            return Some(group);
        }
    }
    None
}

/// A merge's next candidate: an entry's key and version, and its source.
type Head = Reverse<(Vec<u8>, Reverse<u64>, usize)>;

/// A k-way merge of tables' entries, in order (key, then version descending), reading a block at a time.
struct Merge {
    sources: Vec<(Arc<Sst>, usize, std::vec::IntoIter<Entry>)>,
    heap: BinaryHeap<Head>,
    heads: Vec<Option<Entry>>,
}

impl Merge {
    fn new(tables: &[Arc<Sst>]) -> Result<Merge, StoreError> {
        let mut m = Merge {
            sources: tables.iter().map(|t| (t.clone(), 0, Vec::new().into_iter())).collect(),
            heap: BinaryHeap::new(),
            heads: vec![None; tables.len()],
        };
        for i in 0..tables.len() {
            m.advance(i)?;
        }
        Ok(m)
    }

    /// Loads source `i`'s next entry into the heap.
    fn advance(&mut self, i: usize) -> Result<(), StoreError> {
        let Some((t, next_block, entries)) = self.sources.get_mut(i) else {
            return Ok(());
        };
        let e = loop {
            if let Some(e) = entries.next() {
                break Some(e);
            }
            let Some(b) = t.index.get(*next_block) else {
                break None;
            };
            *entries = t.block(b)?.into_iter();
            *next_block += 1;
        };
        if let Some(e) = &e {
            self.heap.push(Reverse((e.key.clone(), Reverse(e.version), i)));
        }
        if let Some(slot) = self.heads.get_mut(i) {
            *slot = e;
        }
        Ok(())
    }
}

impl Iterator for Merge {
    type Item = Result<Entry, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let Reverse((_, _, i)) = self.heap.pop()?;
        let e = self.heads.get_mut(i).and_then(Option::take)?;
        if let Err(err) = self.advance(i) {
            return Some(Err(err));
        }
        Some(Ok(e))
    }
}

/// Drops what no read can see: per key, every version above the horizon, then the newest at or below it (unless it
/// is a delete and nothing older is left elsewhere), and nothing older.
struct Gc<I> {
    inner: I,
    horizon: u64,
    bottom: bool,
    /// The key being passed, and whether its newest version at or below the horizon was seen.
    key: Option<Vec<u8>>,
    settled: bool,
}

impl<I: Iterator<Item = Result<Entry, StoreError>>> Iterator for Gc<I> {
    type Item = Result<Entry, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let e = match self.inner.next()? {
                Ok(e) => e,
                Err(err) => return Some(Err(err)),
            };
            if self.key.as_deref() != Some(e.key.as_slice()) {
                self.key = Some(e.key.clone());
                self.settled = false;
            }
            if e.version > self.horizon {
                return Some(Ok(e));
            }
            if self.settled {
                continue;
            }
            self.settled = true;
            if e.op == Op::Del && self.bottom {
                continue;
            }
            return Some(Ok(e));
        }
    }
}
