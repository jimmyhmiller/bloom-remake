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
    /// The bytes of decoded blocks the tree keeps for reads.
    pub cache_bytes: usize,
    /// The owner's format of its keys, recorded in the manifest: a tree written in another one is refused
    /// ([`manifest_format`] tells it beforehand).
    pub format: u32,
}

impl Default for LsmOptions {
    fn default() -> Self {
        LsmOptions {
            memtable_bytes: 4 << 20,
            block_bytes: 16 << 10,
            tier: 4,
            max_tables: 12,
            history: 65_536,
            cache_bytes: 32 << 20,
            format: 0,
        }
    }
}

const MAGIC: &[u8; 8] = b"BLSSST01";
/// Format 2 adds a Bloom filter of the table's keys: its place follows the version range in the footer.
const MAGIC2: &[u8; 8] = b"BLSSST02";
const FOOTER2: usize = FOOTER + 8 + 4;
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

/// A Bloom filter of a table's keys: `bits` bits, `probes` positions per key (FNV-1a, double hashing).
#[derive(Clone, Debug)]
struct Bloom {
    bits: Vec<u8>,
    probes: u32,
}

impl Bloom {
    const BITS_PER_KEY: usize = 10;
    const PROBES: u32 = 7;

    fn hashes(key: &[u8]) -> (u64, u64) {
        let fnv = |basis: u64| {
            key.iter()
                .fold(basis, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
        };
        (fnv(0xcbf2_9ce4_8422_2325), fnv(0x8422_2325_cbf2_9ce4) | 1)
    }

    fn of(hashes: &[(u64, u64)]) -> Bloom {
        let nbits = (hashes.len() * Bloom::BITS_PER_KEY).max(64);
        let mut bits = vec![0u8; nbits.div_ceil(8)];
        let m = (bits.len() * 8) as u64;
        for (h1, h2) in hashes {
            for i in 0..u64::from(Bloom::PROBES) {
                let bit = h1.wrapping_add(i.wrapping_mul(*h2)) % m;
                if let Some(byte) = bits.get_mut((bit / 8) as usize) {
                    *byte |= 1 << (bit % 8);
                }
            }
        }
        Bloom {
            bits,
            probes: Bloom::PROBES,
        }
    }

    /// Whether the key may be in the table (`false`: it is not).
    fn may_contain(&self, key: &[u8]) -> bool {
        let (h1, h2) = Bloom::hashes(key);
        let m = (self.bits.len() * 8) as u64;
        if m == 0 {
            return true;
        }
        (0..u64::from(self.probes)).all(|i| {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % m;
            self.bits
                .get((bit / 8) as usize)
                .is_some_and(|b| b & (1 << (bit % 8)) != 0)
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bits.len() + 8);
        out.extend_from_slice(&self.probes.to_le_bytes());
        out.extend_from_slice(&self.bits);
        with_crc(out)
    }

    fn decode(path: &Path, offset: u64, raw: &[u8]) -> Result<Bloom, StoreError> {
        let mut body = checked(raw).ok_or_else(|| corrupt(path, offset, "filter checksum"))?;
        let probes = get_u32(&mut body).ok_or_else(|| corrupt(path, offset, "a malformed filter"))?;
        if probes == 0 || probes > 32 {
            return Err(corrupt(path, offset, format!("a filter of {probes} probes")));
        }
        Ok(Bloom {
            bits: body.to_vec(),
            probes,
        })
    }
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

/// Decoded blocks, by table and offset, in a bounded least-recently-used cache shared by a tree's tables.
pub(crate) struct BlockCache {
    capacity: usize,
    inner: Mutex<CacheInner>,
}

/// A cached block: its entries, its last use and its size.
struct Slot {
    block: Arc<Vec<Entry>>,
    used: u64,
    bytes: usize,
}

#[derive(Default)]
struct CacheInner {
    blocks: BTreeMap<(u64, u64), Slot>,
    /// Each block's last use, oldest first.
    uses: BTreeMap<u64, (u64, u64)>,
    clock: u64,
    bytes: usize,
}

impl BlockCache {
    fn new(capacity: usize) -> BlockCache {
        BlockCache {
            capacity,
            inner: Mutex::new(CacheInner::default()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, CacheInner>, StoreError> {
        self.inner
            .lock()
            .map_err(|_| invalid("the block cache's lock is poisoned"))
    }

    fn get(&self, key: (u64, u64)) -> Result<Option<Arc<Vec<Entry>>>, StoreError> {
        let mut c = self.lock()?;
        c.clock += 1;
        let now = c.clock;
        let Some(slot) = c.blocks.get_mut(&key) else {
            return Ok(None);
        };
        let (block, old) = (slot.block.clone(), std::mem::replace(&mut slot.used, now));
        c.uses.remove(&old);
        c.uses.insert(now, key);
        Ok(Some(block))
    }

    fn put(&self, key: (u64, u64), block: Arc<Vec<Entry>>, bytes: usize) -> Result<(), StoreError> {
        let mut c = self.lock()?;
        if bytes > self.capacity {
            return Ok(());
        }
        c.clock += 1;
        let now = c.clock;
        if let Some(old) = c.blocks.insert(
            key,
            Slot {
                block,
                used: now,
                bytes,
            },
        ) {
            c.uses.remove(&old.used);
            c.bytes -= old.bytes;
        }
        c.uses.insert(now, key);
        c.bytes += bytes;
        while c.bytes > self.capacity {
            let Some((_, oldest)) = c.uses.pop_first() else { break };
            if let Some(old) = c.blocks.remove(&oldest) {
                c.bytes -= old.bytes;
            }
        }
        Ok(())
    }

    /// Drops a removed table's blocks.
    fn forget(&self, table: u64) -> Result<(), StoreError> {
        let mut c = self.lock()?;
        let gone: Vec<((u64, u64), u64, usize)> = c
            .blocks
            .range((table, 0)..=(table, u64::MAX))
            .map(|(k, slot)| (*k, slot.used, slot.bytes))
            .collect();
        for (k, used, b) in gone {
            c.blocks.remove(&k);
            c.uses.remove(&used);
            c.bytes -= b;
        }
        Ok(())
    }
}

/// An open SSTable.
struct Sst {
    meta: TableMeta,
    path: PathBuf,
    /// Shared by readers on several threads; a read holds it for one block.
    file: Mutex<Box<dyn VfsFile>>,
    index: Vec<BlockRef>,
    cache: Arc<BlockCache>,
    /// The filter of its keys (format 2; a format-1 table has none).
    filter: Option<Bloom>,
}

impl Sst {
    fn open(fs: &dyn Vfs, path: PathBuf, meta: TableMeta, cache: Arc<BlockCache>) -> Result<Sst, StoreError> {
        let file = fs.open(&path, OpenOpts::default())?;
        let len = file.len()?;
        if len != meta.bytes {
            return Err(corrupt(
                &path,
                len,
                format!("{len} bytes, the manifest says {}", meta.bytes),
            ));
        }
        // A format-2 footer is longer; a table shorter than one is format 1 or nothing.
        let footer_at2 = len.checked_sub(FOOTER2 as u64);
        let mut footer2 = vec![0u8; FOOTER2];
        let v2 = match footer_at2 {
            Some(at) => {
                read_exact(&*file, at, &mut footer2)?;
                footer2.get(..8) == Some(MAGIC2.as_slice())
            }
            None => false,
        };
        let (footer_at, footer) = match footer_at2 {
            Some(at) if v2 => (at, footer2),
            _ => {
                let at = len
                    .checked_sub(FOOTER as u64)
                    .ok_or_else(|| corrupt(&path, 0, "shorter than its footer"))?;
                let mut footer = vec![0u8; FOOTER];
                read_exact(&*file, at, &mut footer)?;
                (at, footer)
            }
        };
        let body = checked(&footer).ok_or_else(|| corrupt(&path, footer_at, "footer checksum"))?;
        let mut f = body;
        let bad = || corrupt(&path, footer_at, "a malformed footer");
        let magic = take(&mut f, 8);
        if magic != Some(MAGIC.as_slice()) && magic != Some(MAGIC2.as_slice()) {
            return Err(corrupt(&path, footer_at, "not an SSTable"));
        }
        let index_at = get_u64(&mut f).ok_or_else(bad)?;
        let index_len = get_u32(&mut f).ok_or_else(bad)?;
        let entries = get_u64(&mut f).ok_or_else(bad)?;
        let (min_version, max_version) = (get_u64(&mut f).ok_or_else(bad)?, get_u64(&mut f).ok_or_else(bad)?);
        let filter = if v2 {
            let at = get_u64(&mut f).ok_or_else(bad)?;
            let flen = get_u32(&mut f).ok_or_else(bad)?;
            let mut raw = vec![0u8; flen as usize];
            read_exact(&*file, at, &mut raw)?;
            Some(Bloom::decode(&path, at, &raw)?)
        } else {
            None
        };
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
            cache,
            filter,
        })
    }

    /// A block, from the cache or read (and cached).
    fn block(&self, b: &BlockRef) -> Result<Arc<Vec<Entry>>, StoreError> {
        let key = (self.meta.id, b.offset);
        if let Some(block) = self.cache.get(key)? {
            return Ok(block);
        }
        let block = Arc::new(self.read_block(b)?);
        self.cache.put(key, block.clone(), b.len as usize)?;
        Ok(block)
    }

    /// A block read from the file, bypassing the cache (a compaction reads each block once).
    fn read_block(&self, b: &BlockRef) -> Result<Vec<Entry>, StoreError> {
        let mut raw = vec![0u8; b.len as usize];
        {
            let file = self.file.lock().map_err(|_| invalid("an SSTable's lock is poisoned"))?;
            read_exact(&**file, b.offset, &mut raw)?;
        }
        let body = checked(&raw).ok_or_else(|| corrupt(&self.path, b.offset, "block checksum"))?;
        decode_block(&self.path, b.offset, body)
    }

    /// The entries with keys from `start` (inclusive) to `end` (exclusive; `None`: no end), at or below `as_of`, in
    /// order.
    fn scan(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, out: &mut Vec<Entry>) -> Result<(), StoreError> {
        self.scan_keys(start, end, as_of, usize::MAX, out).map(|_| ())
    }

    /// The entries at or below `as_of` of the first `keys` distinct keys from `start` to `end` (every version of
    /// each), into `out`. Returns the last key taken when the range holds more keys than that (`None`: none left).
    fn scan_keys(
        &self,
        start: &[u8],
        end: Option<&[u8]>,
        as_of: u64,
        keys: usize,
        out: &mut Vec<Entry>,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let before_end = |k: &[u8]| end.is_none_or(|e| k < e);
        let first = self.index.partition_point(|b| b.last.as_slice() < start);
        let mut taken = 0usize;
        let mut last: Option<Vec<u8>> = None;
        for b in self.index.iter().skip(first) {
            if !before_end(&b.first) {
                break;
            }
            for e in self.block(b)?.iter() {
                if e.key.as_slice() < start || !before_end(&e.key) {
                    continue;
                }
                if last.as_deref() != Some(e.key.as_slice()) {
                    if taken == keys {
                        return Ok(last);
                    }
                    taken += 1;
                    last = Some(e.key.clone());
                }
                if e.version <= as_of {
                    out.push(e.clone());
                }
            }
        }
        Ok(None)
    }

    /// The newest entry of `key` at or below `as_of`: its version and op.
    fn get(&self, key: &[u8], as_of: u64) -> Result<Option<(u64, Op)>, StoreError> {
        if self.filter.as_ref().is_some_and(|f| !f.may_contain(key)) {
            return Ok(None);
        }
        let start = self.index.partition_point(|b| b.last.as_slice() < key);
        for b in self.index.iter().skip(start) {
            if b.first.as_slice() > key {
                break;
            }
            // Versions of a key are newest first: the first at or below `as_of` is the one.
            if let Some(e) = self.block(b)?.iter().find(|e| e.key == key && e.version <= as_of) {
                return Ok(Some((e.version, e.op)));
            }
        }
        Ok(None)
    }
}

/// The smallest byte string greater than every one starting with `prefix` (`None`: there is none).
pub(crate) fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(last) = out.pop() {
        if last < 0xff {
            out.push(last + 1);
            return Some(out);
        }
    }
    None
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
    // Each distinct key's hashes, for the filter (a key's versions are adjacent).
    let mut hashes: Vec<(u64, u64)> = Vec::new();
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
        if count == 0 || e.key != last {
            hashes.push(Bloom::hashes(&e.key));
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
    let filter = Bloom::of(&hashes).encode();
    let filter_at = index_at + ix.len() as u64;
    file.append(&filter)?;
    let mut footer = Vec::with_capacity(FOOTER2);
    footer.extend_from_slice(MAGIC2);
    footer.extend_from_slice(&index_at.to_le_bytes());
    footer.extend_from_slice(
        &u32::try_from(ix.len())
            .map_err(|_| invalid("an index over 4 GiB"))?
            .to_le_bytes(),
    );
    footer.extend_from_slice(&count.to_le_bytes());
    footer.extend_from_slice(&min_v.to_le_bytes());
    footer.extend_from_slice(&max_v.to_le_bytes());
    footer.extend_from_slice(&filter_at.to_le_bytes());
    footer.extend_from_slice(
        &u32::try_from(filter.len())
            .map_err(|_| invalid("a filter over 4 GiB"))?
            .to_le_bytes(),
    );
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

/// `dir`'s manifest, if it has one.
fn read_manifest(fs: &dyn Vfs, dir: &Path) -> Result<Option<Manifest>, StoreError> {
    let path = dir.join("MANIFEST");
    if !fs.list(dir)?.iter().any(|p| p == &path) {
        return Ok(None);
    }
    let bytes = read_path(fs, &path)?;
    let body = bytes.get(32..).ok_or_else(|| corrupt(&path, 0, "MANIFEST truncated"))?;
    if bytes.get(..32) != Some(blake3::hash(body).as_bytes().as_slice()) {
        return Err(corrupt(&path, 0, "MANIFEST checksum"));
    }
    let m: Manifest = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
    if m.format != MANIFEST_FORMAT {
        return Err(invalid(format!(
            "database format {} (this build reads {MANIFEST_FORMAT})",
            m.format
        )));
    }
    Ok(Some(m))
}

/// The key format the tree under `dir` was written in (`None`: there is no tree there).
pub fn manifest_format(fs: &dyn Vfs, dir: &Path) -> Result<Option<u32>, StoreError> {
    match fs.list(dir) {
        Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(_) => Ok(read_manifest(fs, dir)?.map(|m| m.key_format)),
    }
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
    flushed_version: Option<u64>,
    flushed_mark: u64,
    /// The oldest version an as-of read may ask for: compaction merged away what is below it.
    floor: u64,
    next_id: u64,
    /// The owner's format of its keys ([`LsmOptions::format`]).
    #[serde(default)]
    key_format: u32,
}

impl Manifest {
    fn empty(key_format: u32) -> Manifest {
        Manifest {
            key_format,
            format: MANIFEST_FORMAT,
            tables: Vec::new(),
            flushed_version: None,
            flushed_mark: 0,
            floor: 0,
            next_id: 1,
        }
    }
}

type Mem = BTreeMap<(Vec<u8>, Reverse<u64>), Op>;

/// A page of a scan ([`Lsm::scan_page`]): the keys present, in order, and where the next page starts (`None`: the
/// range is done).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub keys: Vec<Vec<u8>>,
    pub next: Option<Vec<u8>>,
}

/// What the tree holds now.
struct State {
    mem: Mem,
    mem_bytes: usize,
    /// A memtable being flushed (still read), with the version and mark it covers.
    frozen: Option<(Arc<Mem>, u64, u64)>,
    tables: Vec<Arc<Sst>>,
    manifest: Manifest,
    /// The newest version applied (`None`: none yet), and the caller's mark with it.
    applied: Option<u64>,
    applied_mark: u64,
}

/// What a flush or compaction left durable: the version and mark the tables cover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flushed {
    version: Option<u64>,
    mark: u64,
}

impl Flushed {
    /// What a tree other than this one reports covering (`crate::tree::KeyTree`).
    pub fn at(version: Option<u64>, mark: u64) -> Flushed {
        Flushed { version, mark }
    }

    /// Every version at or below this one is in the tables (`None`: no version is).
    pub fn version(&self) -> Option<u64> {
        self.version
    }

    /// The caller's mark that came with the newest version flushed.
    pub fn mark(&self) -> u64 {
        self.mark
    }
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
    cache: Arc<BlockCache>,
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
        let manifest = if let Some(m) = read_manifest(&*fs, dir)? {
            if m.key_format != opts.format {
                return Err(invalid(format!(
                    "the database's keys are in format {}, this build writes format {}",
                    m.key_format, opts.format
                )));
            }
            m
        } else {
            // A new tree gets its manifest with its first flush: until then a reader finds no database, and its owner
            // starts it again after a crash.
            Manifest::empty(opts.format)
        };
        let cache = Arc::new(BlockCache::new(opts.cache_bytes));
        let mut tables = Vec::new();
        for t in &manifest.tables {
            tables.push(Arc::new(Sst::open(
                &*fs,
                sst_dir.join(format!("{}.sst", t.id)),
                t.clone(),
                cache.clone(),
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
            cache,
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

    /// The newest version applied (`None`: none yet).
    pub fn applied(&self) -> Result<Option<u64>, StoreError> {
        Ok(self.read()?.applied)
    }

    /// Raises the oldest version an as-of read may ask for to `version` (versions below it are not what they were:
    /// a tree started from a snapshot holds nothing of its past). Recorded with the next flush.
    pub fn raise_floor(&self, version: u64) -> Result<(), StoreError> {
        // Not while a flush or compaction writes a manifest copied before it.
        let _work = self
            .work
            .lock()
            .map_err(|_| invalid("the database's work lock is poisoned"))?;
        let mut s = self.write()?;
        s.manifest.floor = s.manifest.floor.max(version);
        Ok(())
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
        if let Some(a) = s.applied
            && version <= a
        {
            return Err(invalid(format!("version {version} applied after version {a}")));
        }
        for (key, op) in changes {
            s.mem_bytes += key.len() + 16;
            s.mem.insert((key, Reverse(version)), op);
        }
        s.applied = Some(version);
        s.applied_mark = mark;
        Ok(())
    }

    /// Adds `changes` to the newest applied version, which stays the newest (no version is applied): keys a caller
    /// derives from the rows already there, between two versions (a keyspace built from a table, docs/design/
    /// DATABASE.md §7). No entry of the newest version may hold one of the keys. Refused before any version.
    pub fn amend(&self, changes: impl IntoIterator<Item = (Vec<u8>, Op)>) -> Result<(), StoreError> {
        let mut s = self.write()?;
        let version = s
            .applied
            .ok_or_else(|| invalid("an amendment of a tree with no version applied"))?;
        for (key, op) in changes {
            s.mem_bytes += key.len() + 16;
            s.mem.insert((key, Reverse(version)), op);
        }
        Ok(())
    }

    /// Whether the memtable has grown past its size.
    pub fn needs_flush(&self) -> Result<bool, StoreError> {
        Ok(self.read()?.mem_bytes >= self.opts.memtable_bytes)
    }

    /// Writes the memtable as an SSTable, then the manifest naming it and the version the tables now cover (every one
    /// applied). The manifest is written even when the memtable is empty: the first flush of a new tree creates it.
    pub fn flush(&self) -> Result<Flushed, StoreError> {
        if self.read_only {
            return Err(invalid("a database opened read-only is not flushed or compacted"));
        }
        let _work = self
            .work
            .lock()
            .map_err(|_| invalid("the database's work lock is poisoned"))?;
        let (frozen, version, mark, id, mut manifest) = {
            let mut s = self.write()?;
            // A flush that failed left its memtable frozen: it goes out with this one (its versions are older).
            if let Some((old, _, _)) = s.frozen.take() {
                for (k, op) in old.iter() {
                    s.mem.entry(k.clone()).or_insert(*op);
                    s.mem_bytes += k.0.len() + 16;
                }
            }
            let mem = Arc::new(std::mem::take(&mut s.mem));
            s.mem_bytes = 0;
            let (version, mark) = (s.applied, s.applied_mark);
            let id = s.manifest.next_id;
            if !mem.is_empty() {
                s.frozen = Some((mem.clone(), version.unwrap_or_default(), mark));
                s.manifest.next_id += 1;
            }
            (mem, version, mark, id, s.manifest.clone())
        };
        let table = if frozen.is_empty() {
            None
        } else {
            let path = self.dir.join("sst").join(format!("{id}.sst"));
            let entries = frozen.iter().map(|((key, Reverse(version)), op)| {
                Ok(Entry {
                    key: key.clone(),
                    version: *version,
                    op: *op,
                })
            });
            match write_table(&*self.fs, &path, id, self.opts.block_bytes, entries)? {
                Some(m) => Some(Arc::new(Sst::open(&*self.fs, path, m, self.cache.clone())?)),
                None => None,
            }
        };
        if let Some(t) = &table {
            manifest.tables.insert(0, t.meta.clone());
        }
        manifest.flushed_version = version;
        manifest.flushed_mark = mark;
        // Only flushes and compactions change the manifest, and they run one at a time: written outside the state's
        // lock, so applies and reads go on meanwhile.
        self.write_manifest(&manifest)?;
        let mut s = self.write()?;
        if let Some(t) = table {
            s.tables.insert(0, t);
        }
        // A floor raised since the manifest was copied stays raised.
        manifest.floor = manifest.floor.max(s.manifest.floor);
        s.manifest = manifest;
        s.frozen = None;
        Ok(Flushed { version, mark })
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
            let horizon = s.applied.map_or(0, |a| a.saturating_sub(self.opts.history));
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
            Some(m) => Some(Arc::new(Sst::open(&*self.fs, path, m, self.cache.clone())?)),
            None => None,
        };
        // Only flushes and compactions change the manifest, and they run one at a time: written outside the state's
        // lock, so applies and reads go on meanwhile.
        let mut manifest = self.read()?.manifest.clone();
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
        let mut s = self.write()?;
        manifest.floor = manifest.floor.max(s.manifest.floor);
        s.tables.retain(|t| !gone.contains(&t.meta.id));
        if let Some(t) = table {
            let at = at.min(s.tables.len());
            s.tables.insert(at, t);
        }
        s.manifest = manifest;
        drop(s);
        for t in &chosen {
            self.fs.remove(&t.path)?;
            self.cache.forget(t.meta.id)?;
        }
        self.fs.sync_dir(&self.dir.join("sst"))?;
        Ok(true)
    }

    /// The keys starting with `prefix` present as of version `as_of`, in order. A version below the floor (merged
    /// away) or above the newest applied is refused.
    pub fn scan(&self, prefix: &[u8], as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        self.scan_range(prefix, successor(prefix).as_deref(), as_of)
    }

    /// The keys from `start` (inclusive) to `end` (exclusive; `None`: no end) present as of version `as_of`, in order
    /// (the same refusals as [`Lsm::scan`]).
    pub fn scan_range(&self, start: &[u8], end: Option<&[u8]>, as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        let before_end = |k: &[u8]| end.is_none_or(|e| k < e);
        let (mut newest, tables) = {
            let s = self.read()?;
            if as_of < s.manifest.floor {
                return Err(invalid(format!(
                    "version {as_of} is past the history kept (from version {})",
                    s.manifest.floor
                )));
            }
            if let Some(a) = s.applied
                && as_of > a
            {
                return Err(invalid(format!(
                    "version {as_of} is newer than the newest applied ({a})"
                )));
            }
            // The newest entry at or below `as_of` per key, from the memtables (newer than every table).
            let mut newest: BTreeMap<Vec<u8>, (u64, Op)> = BTreeMap::new();
            let mems = std::iter::once(&s.mem).chain(s.frozen.as_ref().map(|(m, _, _)| &**m));
            for mem in mems {
                for ((key, Reverse(version)), op) in mem.range((start.to_vec(), Reverse(u64::MAX))..) {
                    if !before_end(key) {
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
            t.scan(start, end, as_of, &mut found)?;
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

    /// A page of [`Lsm::scan_range`]: the keys present as of `as_of` among the next `keys` distinct keys any
    /// memtable or table holds from `start` to `end` (live or deleted, so a page may hold fewer, even none), in
    /// order, and where the next page starts (`None`: the range is done). A scan holds one page at a time, however
    /// large the range.
    pub fn scan_page(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, keys: usize) -> Result<Page, StoreError> {
        if keys == 0 {
            return Err(invalid("a scan page of no keys"));
        }
        let before_end = |k: &[u8]| end.is_none_or(|e| k < e);
        // Each source gives the entries of its next `keys` keys and the last one if it has more: every source is
        // complete up to the smallest such key, which bounds the page.
        let mut bound: Option<Vec<u8>> = None;
        let mut cut = |last: Option<Vec<u8>>| {
            if let Some(l) = last
                && bound.as_ref().is_none_or(|b| l < *b)
            {
                bound = Some(l);
            }
        };
        let mut entries: Vec<(Vec<u8>, u64, Op)> = Vec::new();
        let tables = {
            let s = self.read()?;
            if as_of < s.manifest.floor || s.applied.is_some_and(|a| as_of > a) {
                return Err(invalid(format!(
                    "version {as_of} is outside the history kept (from version {})",
                    s.manifest.floor
                )));
            }
            let mems = std::iter::once(&s.mem).chain(s.frozen.as_ref().map(|(m, _, _)| &**m));
            for mem in mems {
                let mut taken = 0usize;
                let mut last: Option<&Vec<u8>> = None;
                let mut more = None;
                for ((key, Reverse(version)), op) in mem.range((start.to_vec(), Reverse(u64::MAX))..) {
                    if !before_end(key) {
                        break;
                    }
                    if last != Some(key) {
                        if taken == keys {
                            more = last.cloned();
                            break;
                        }
                        taken += 1;
                        last = Some(key);
                    }
                    if *version <= as_of {
                        entries.push((key.clone(), *version, *op));
                    }
                }
                cut(more);
            }
            s.tables.clone()
        };
        let mut found = Vec::new();
        for t in &tables {
            found.clear();
            let more = t.scan_keys(start, end, as_of, keys, &mut found)?;
            entries.extend(found.drain(..).map(|e| (e.key, e.version, e.op)));
            cut(more);
        }
        let mut newest: BTreeMap<Vec<u8>, (u64, Op)> = BTreeMap::new();
        for (key, version, op) in entries {
            if bound.as_ref().is_some_and(|b| key > *b) {
                continue;
            }
            let slot = newest.entry(key).or_insert((version, op));
            if version > slot.0 {
                *slot = (version, op);
            }
        }
        // The sources together may hold more keys below the bound than a page: it ends at the page's last.
        if newest.len() > keys {
            let rest = newest.keys().nth(keys).cloned();
            if let Some(r) = rest {
                newest.split_off(&r);
            }
            bound = newest.keys().next_back().cloned();
        }
        let keys = newest
            .into_iter()
            .filter(|(_, (_, op))| *op == Op::Put)
            .map(|(k, _)| k)
            .collect();
        // The next page starts just past the bound: the smallest key greater than it.
        let next = bound.map(|mut b| {
            b.push(0);
            b
        });
        Ok(Page { keys, next })
    }

    /// Whether `key` is present as of version `as_of` (the same refusals as [`Lsm::scan`]).
    pub fn get(&self, key: &[u8], as_of: u64) -> Result<bool, StoreError> {
        let (mut newest, tables) = {
            let s = self.read()?;
            if as_of < s.manifest.floor || s.applied.is_some_and(|a| as_of > a) {
                return Err(invalid(format!(
                    "version {as_of} is outside the history kept (from version {})",
                    s.manifest.floor
                )));
            }
            let mut newest: Option<(u64, Op)> = None;
            let mems = std::iter::once(&s.mem).chain(s.frozen.as_ref().map(|(m, _, _)| &**m));
            for mem in mems {
                let found = mem
                    .range((key.to_vec(), Reverse(as_of))..)
                    .next()
                    .filter(|((k, _), _)| k.as_slice() == key)
                    .map(|((_, Reverse(v)), op)| (*v, *op));
                if let Some(f) = found
                    && newest.is_none_or(|n| f.0 > n.0)
                {
                    newest = Some(f);
                }
            }
            (newest, s.tables.clone())
        };
        // A merged table may hold versions on either side of another's: every table is asked.
        for t in &tables {
            if let Some(f) = t.get(key, as_of)?
                && newest.is_none_or(|n| f.0 > n.0)
            {
                newest = Some(f);
            }
        }
        Ok(newest.is_some_and(|(_, op)| op == Op::Put))
    }

    /// The SSTables now live: their ids and sizes, newest first (for tests and tooling).
    pub fn tables(&self) -> Result<Vec<(u64, u64)>, StoreError> {
        Ok(self.read()?.tables.iter().map(|t| (t.meta.id, t.meta.bytes)).collect())
    }

    /// What the tree holds, as its manifest and tables say.
    pub fn info(&self) -> Result<TreeInfo, StoreError> {
        let s = self.read()?;
        Ok(TreeInfo {
            tables: s
                .tables
                .iter()
                .map(|t| TableInfo {
                    id: t.meta.id,
                    bytes: t.meta.bytes,
                    entries: t.meta.entries,
                    min_version: t.meta.min_version,
                    max_version: t.meta.max_version,
                    blocks: t.index.len(),
                    filtered: t.filter.is_some(),
                })
                .collect(),
            flushed: s.manifest.flushed_version,
            applied: s.applied,
            floor: s.manifest.floor,
            key_format: s.manifest.key_format,
            memtable_entries: s.mem.len(),
        })
    }

    /// Reads every block of every table, checking what a read would not: entries strictly in order (key, then version
    /// descending), each block's first and last keys as its index says, the entry count and version range as the
    /// manifest says, and every key in the table's filter. The entries checked.
    pub fn verify(&self) -> Result<u64, StoreError> {
        let tables = self.read()?.tables.clone();
        let mut checked = 0u64;
        for t in &tables {
            let mut count = 0u64;
            let mut prev: Option<(Vec<u8>, u64)> = None;
            for b in &t.index {
                let entries = t.read_block(b)?;
                let at = |reason: &str| corrupt(&t.path, b.offset, reason.to_owned());
                if entries.first().map(|e| &e.key) != Some(&b.first) || entries.last().map(|e| &e.key) != Some(&b.last)
                {
                    return Err(at("a block's keys disagree with the index"));
                }
                for e in entries.iter() {
                    if let Some((pk, pv)) = &prev {
                        let in_order = pk.as_slice() < e.key.as_slice() || (pk == &e.key && *pv > e.version);
                        if !in_order {
                            return Err(at("entries out of order"));
                        }
                    }
                    if e.version < t.meta.min_version || e.version > t.meta.max_version {
                        return Err(at("an entry outside the table's version range"));
                    }
                    if t.filter.as_ref().is_some_and(|f| !f.may_contain(&e.key)) {
                        return Err(at("a key its filter does not hold"));
                    }
                    prev = Some((e.key.clone(), e.version));
                    count += 1;
                }
            }
            if count != t.meta.entries {
                return Err(corrupt(
                    &t.path,
                    0,
                    format!("{count} entries, the manifest says {}", t.meta.entries),
                ));
            }
            checked += count;
        }
        Ok(checked)
    }
}

/// A table as [`Lsm::info`] describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableInfo {
    pub id: u64,
    pub bytes: u64,
    pub entries: u64,
    pub min_version: u64,
    pub max_version: u64,
    pub blocks: usize,
    /// Whether it has a filter (format 2).
    pub filtered: bool,
}

/// A tree as [`Lsm::info`] describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeInfo {
    /// Live tables, newest first.
    pub tables: Vec<TableInfo>,
    /// The version the tables cover, the newest applied (the memtable's too), and the oldest an as-of read may ask.
    pub flushed: Option<u64>,
    pub applied: Option<u64>,
    pub floor: u64,
    pub key_format: u32,
    pub memtable_entries: usize,
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
            *entries = t.read_block(b)?.into_iter();
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

impl crate::tree::KeyTree for Lsm {
    fn key_format(&self) -> Result<Option<u32>, StoreError> {
        manifest_format(&*self.fs, &self.dir)
    }
    fn applied(&self) -> Result<Option<u64>, StoreError> {
        Lsm::applied(self)
    }
    fn apply(&self, version: u64, mark: u64, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        Lsm::apply(self, version, mark, changes)
    }
    fn amend(&self, changes: Vec<(Vec<u8>, Op)>) -> Result<(), StoreError> {
        Lsm::amend(self, changes)
    }
    fn needs_flush(&self) -> Result<bool, StoreError> {
        Lsm::needs_flush(self)
    }
    fn flush(&self) -> Result<Flushed, StoreError> {
        Lsm::flush(self)
    }
    fn flushed(&self) -> Result<Flushed, StoreError> {
        Lsm::flushed(self)
    }
    fn compact(&self) -> Result<bool, StoreError> {
        Lsm::compact(self)
    }
    fn floor(&self) -> Result<u64, StoreError> {
        Lsm::floor(self)
    }
    fn raise_floor(&self, version: u64) -> Result<(), StoreError> {
        Lsm::raise_floor(self, version)
    }
    fn get(&self, key: &[u8], as_of: u64) -> Result<bool, StoreError> {
        Lsm::get(self, key, as_of)
    }
    fn scan(&self, prefix: &[u8], as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        Lsm::scan(self, prefix, as_of)
    }
    fn scan_range(&self, start: &[u8], end: Option<&[u8]>, as_of: u64) -> Result<Vec<Vec<u8>>, StoreError> {
        Lsm::scan_range(self, start, end, as_of)
    }
    fn scan_page(&self, start: &[u8], end: Option<&[u8]>, as_of: u64, keys: usize) -> Result<Page, StoreError> {
        Lsm::scan_page(self, start, end, as_of, keys)
    }
    fn info(&self) -> Result<TreeInfo, StoreError> {
        Lsm::info(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_has_no_false_negatives_and_few_false_positives() {
        let keys: Vec<Vec<u8>> = (0..2000u32).map(|i| format!("key-{i}").into_bytes()).collect();
        let hashes: Vec<(u64, u64)> = keys.iter().map(|k| Bloom::hashes(k)).collect();
        let f = Bloom::decode(Path::new("t"), 0, &Bloom::of(&hashes).encode()).unwrap();
        assert!(keys.iter().all(|k| f.may_contain(k)));
        let false_positives = (0..2000u32)
            .filter(|i| f.may_contain(format!("absent-{i}").as_bytes()))
            .count();
        // About 1% at 10 bits a key and 7 probes.
        assert!(false_positives < 60, "{false_positives} false positives in 2000");
    }
}
