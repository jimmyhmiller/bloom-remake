//! The blob store (FOREIGN-PROTOCOLS §5): a node's durable blobs, appended to pack files under `<store>/blobs/`
//! (docs/plan/notes/SCALE.md). A blob is durable before the WAL record that references it syncs, so recovery never
//! finds a row whose blob is missing. It gets there one of two ways:
//!
//! - **Put** (`put_all`): appended to the active pack, which is then synced (and the directory, for a new pack).
//! - **Logged** (`write_logged`): its bytes travel in the WAL record itself, which makes them durable with the
//!   record's one sync; it is appended to the active pack without a sync (readable at once) and is *pending* until
//!   `sync_logged_below` syncs the packs, which must happen before the WAL that logs it is truncated. Recovery
//!   appends a logged blob that a crash lost again, from its record (`restore_logged`).
//!
//! A pack (`p<seq>.pack`) is a run of entries, each a header (`PACK_MAGIC`, the length, the hash) and the bytes, so
//! each entry checks itself. One sync makes every entry before it durable: a flush's thousands of blobs cost one
//! `fdatasync` a pack, not one a blob (whose cost, a device flush each, held every WAL sync of the node back). A pack
//! past `pack_bytes` is sealed: never appended again. A sealed pack synced whole gets an index (`p<seq>.idx`: its
//! entries, as a scan would find them, then the hash of them), so opening the store reads the index rather than the
//! pack. Opening scans a
//! pack without one: its entries are checked against their hashes and the pack is cut at the first that fails (a torn
//! tail), then synced and indexed. So every entry the store finds when it opens is durable.
//!
//! Deleting a blob forgets it; a sealed pack with no live entry is removed, and `compact` copies the live entries of a
//! sealed pack that is mostly dead into the active pack, syncs them and removes the old pack. Removals are not synced:
//! a pack or an entry a crash brings back is held by no row, and recovery makes every such blob a candidate for
//! collection.
//!
//! Stores from before packs kept a file a blob, `<hash>-<len>`, holding synced bytes: those are adopted as loose blobs
//! (read and deleted, never written). Their provisional `<hash>-<len>.log` files are removed when the store opens: a
//! surviving record logs the blob again.
//!
//! `sync_logged_below` and `compact` run on their own thread while the committer goes on appending: no file is synced
//! under the store's lock.
//!
//! The bytes of the blobs written or read most recently are also kept in memory (`CACHE_BYTES`): a node reads a blob
//! back soon after writing it (a Raft entry replicated or applied in the next tick), and a blob's bytes are its name.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use blossom_value::BlobRef;

use crate::{Lsn, OpenOpts, StoreError, Vfs, VfsFile, durable_dir, invalid, read_all};

/// A blob and its bytes.
pub type BlobBytes = (BlobRef, Arc<[u8]>);

/// The blob directory of a node directory.
pub fn blob_dir(dir: &Path) -> PathBuf {
    dir.join("blobs")
}

/// The first bytes of each pack entry.
const PACK_MAGIC: [u8; 4] = *b"BLPK";
/// The first bytes of a pack's index.
const INDEX_MAGIC: [u8; 4] = *b"BLIX";
/// A pack entry's header: the magic, the length (`u64`, little-endian) and the hash.
const HEADER: u64 = 4 + 8 + 32;
/// An index entry: the offset of the bytes, their length, the hash.
const INDEX_ENTRY: usize = 8 + 8 + 32;
/// How large a pack grows before it is sealed.
pub const PACK_BYTES: u64 = 64 << 20;

/// How a store lays out its packs.
#[derive(Clone, Copy, Debug)]
pub struct BlobOptions {
    /// A pack is sealed once the next entry would take it past this (a larger entry goes alone in a pack).
    pub pack_bytes: u64,
}

impl Default for BlobOptions {
    fn default() -> BlobOptions {
        BlobOptions { pack_bytes: PACK_BYTES }
    }
}

/// Open handles to read packs through, by sequence number.
type Readers = BTreeMap<u64, Box<dyn VfsFile>>;

/// A node's durable blobs.
pub struct BlobStore {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
    opts: BlobOptions,
    state: Mutex<State>,
    cache: Mutex<Cache>,
    readers: Mutex<Readers>,
    /// Held while packs are indexed or compacted, one thread at a time.
    upkeep: Mutex<()>,
}

/// Where a blob's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    /// In pack `seq`, its bytes at `off`.
    Pack { seq: u64, off: u64 },
    /// In a file of its own (a store from before packs).
    Loose,
}

/// A blob the store holds.
#[derive(Clone, Copy, Debug)]
struct Entry {
    loc: Loc,
    /// While pending: the position of the first WAL record that logs it (its entry is not known synced).
    logged: Option<Lsn>,
}

/// A pack's bookkeeping.
#[derive(Default)]
struct Pack {
    /// The bytes appended: entries end here.
    len: u64,
    /// The bytes known synced, from the start.
    synced: u64,
    /// Whether its directory entry is durable.
    named: bool,
    /// No more appends: full, or found when the store opened.
    sealed: bool,
    /// Whether its index is written (it is sealed and was synced whole).
    indexed: bool,
    /// The entries the store holds here, by the offset of their bytes.
    live: BTreeMap<u64, BlobRef>,
    /// Their headers and bytes.
    live_bytes: u64,
    /// Logged entries not known synced, by where they end.
    unsynced: BTreeMap<u64, BlobRef>,
    /// Every entry appended (the offset of its bytes, the blob), live or not, until the pack is indexed: what its
    /// index lists, as a scan would find them.
    entries: Vec<(u64, BlobRef)>,
    /// Entries appended by a put or a compaction that the store does not hold yet (it will once they are synced):
    /// the pack is not removed meanwhile, though nothing live may be in it.
    adopting: u64,
}

impl Pack {
    /// The bytes of entries the store no longer holds here.
    fn dead(&self) -> u64 {
        self.len.saturating_sub(self.live_bytes)
    }

    /// Whether the pack can go: sealed, nothing in it held or about to be.
    fn removable(&self) -> bool {
        self.sealed && self.live.is_empty() && self.adopting == 0
    }
}

/// What the store holds, and its packs.
#[derive(Default)]
struct State {
    index: BTreeMap<BlobRef, Entry>,
    packs: BTreeMap<u64, Pack>,
    /// The pack appended to, and its handle.
    writer: Option<(u64, Box<dyn VfsFile>)>,
    next_seq: u64,
    /// The positions of the records that log the pending blobs, with how many each.
    pending_at: BTreeMap<Lsn, u64>,
}

impl State {
    /// Holds `b` at `e` (in place of where it was).
    fn insert(&mut self, b: BlobRef, e: Entry) {
        if let Some(old) = self.index.insert(b, e) {
            self.unlive(&b, old);
        }
        if let Loc::Pack { seq, off } = e.loc
            && let Some(p) = self.packs.get_mut(&seq)
            && p.live.insert(off, b).is_none()
        {
            p.live_bytes += HEADER + b.len;
        }
        if let Some(at) = e.logged {
            *self.pending_at.entry(at).or_default() += 1;
        }
    }

    /// Forgets `b`.
    fn remove(&mut self, b: &BlobRef) -> Option<Entry> {
        let e = self.index.remove(b)?;
        self.unlive(b, e);
        Some(e)
    }

    fn unlive(&mut self, b: &BlobRef, e: Entry) {
        if let Loc::Pack { seq, off } = e.loc
            && let Some(p) = self.packs.get_mut(&seq)
            && p.live.remove(&off).is_some()
        {
            p.live_bytes = p.live_bytes.saturating_sub(HEADER + b.len);
        }
        if let Some(at) = e.logged {
            self.unpend(at);
        }
    }

    fn unpend(&mut self, at: Lsn) {
        if let Some(n) = self.pending_at.get_mut(&at) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.pending_at.remove(&at);
            }
        }
    }

    /// The pending blob `b`'s entry in pack `seq`, ending at `end`, is synced: it is durable, if the store still
    /// holds it there.
    fn settle(&mut self, b: &BlobRef, seq: u64, end: u64) -> bool {
        let Some(e) = self.index.get_mut(b) else {
            return false;
        };
        let here = Loc::Pack {
            seq,
            off: end.saturating_sub(b.len),
        };
        let Some(at) = e.logged.filter(|_| e.loc == here) else {
            return false;
        };
        e.logged = None;
        self.unpend(at);
        true
    }
}

/// How many bytes of recently written or read blobs the store keeps in memory.
const CACHE_BYTES: u64 = 64 << 20;

/// Recently written or read blobs' bytes, the oldest dropped first past `CACHE_BYTES`.
#[derive(Default)]
struct Cache {
    bytes: BTreeMap<BlobRef, Arc<[u8]>>,
    order: VecDeque<BlobRef>,
    total: u64,
}

impl Cache {
    fn get(&self, b: &BlobRef) -> Option<Arc<[u8]>> {
        self.bytes.get(b).cloned()
    }

    fn insert(&mut self, b: BlobRef, bytes: Arc<[u8]>) {
        if self.bytes.contains_key(&b) || b.len > CACHE_BYTES {
            return;
        }
        self.total += b.len;
        self.bytes.insert(b, bytes);
        self.order.push_back(b);
        while self.total > CACHE_BYTES {
            let Some(old) = self.order.pop_front() else { break };
            if self.bytes.remove(&old).is_some() {
                self.total -= old.len;
            }
        }
    }

    /// Forgets `b`. Its place in `order` stays: dropping it later finds it gone (or, cached again since, drops it
    /// early), and `total` counts only what `bytes` holds.
    fn remove(&mut self, b: &BlobRef) {
        if self.bytes.remove(b).is_some() {
            self.total -= b.len;
        }
    }
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// A loose blob's file name (stores from before packs).
fn name(b: &BlobRef) -> String {
    format!("{}-{}", b.hex(), b.len)
}

/// The blob a loose file name names, if it names one.
fn parse(name: &str) -> Option<BlobRef> {
    let (hex, len) = name.split_once('-')?;
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, slot) in hash.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(BlobRef {
        hash,
        len: len.parse().ok()?,
    })
}

/// The sequence number a pack's or an index's file name names, and whether it is the index.
fn parse_pack(name: &str) -> Option<(u64, bool)> {
    let rest = name.strip_prefix('p')?;
    let (seq, ext) = rest.split_once('.')?;
    if seq.len() != 16 {
        return None;
    }
    let seq = u64::from_str_radix(seq, 16).ok()?;
    match ext {
        "pack" => Some((seq, false)),
        "idx" => Some((seq, true)),
        _ => None,
    }
}

fn not_found(e: &StoreError) -> bool {
    matches!(e, StoreError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Removes `path`; one already gone is fine.
fn remove_if_there(fs: &dyn Vfs, path: &Path) -> Result<(), StoreError> {
    match fs.remove(path) {
        Err(e) if !not_found(&e) => Err(e),
        _ => Ok(()),
    }
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn hash_at(bytes: &[u8], at: usize) -> Option<[u8; 32]> {
    bytes.get(at..at.checked_add(32)?)?.try_into().ok()
}

/// The entry of a pack's bytes at `pos` if it checks: the offset of its bytes, the blob, and where it ends.
fn entry_at(bytes: &[u8], pos: usize) -> Option<(u64, BlobRef, usize)> {
    if bytes.get(pos..pos.checked_add(4)?)? != PACK_MAGIC {
        return None;
    }
    let b = BlobRef {
        len: u64_at(bytes, pos + 4)?,
        hash: hash_at(bytes, pos + 12)?,
    };
    let start = pos.checked_add(HEADER as usize)?;
    let end = start.checked_add(usize::try_from(b.len).ok()?)?;
    (BlobRef::of(bytes.get(start..end)?) == b).then_some((start as u64, b, end))
}

/// The entries of a pack's bytes that check (the offset of their bytes, the blob), up to the first that does not,
/// and where that is: what a crash may have torn starts there.
fn scan(bytes: &[u8]) -> (Vec<(u64, BlobRef)>, u64) {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while let Some((off, b, end)) = entry_at(bytes, pos) {
        entries.push((off, b));
        pos = end;
    }
    (entries, pos as u64)
}

/// An index's bytes: its entries (the offset of their bytes, the blob), if it is whole and they fit a pack of `len`.
fn parse_index(bytes: &[u8], len: u64) -> Option<Vec<(u64, BlobRef)>> {
    let body_end = bytes.len().checked_sub(32)?;
    let body = bytes.get(..body_end)?;
    if BlobRef::of(body).hash != hash_at(bytes, body_end)? || body.get(..4)? != INDEX_MAGIC {
        return None;
    }
    let n = usize::try_from(u64_at(body, 4)?).ok()?;
    if body.len() != n.checked_mul(INDEX_ENTRY)?.checked_add(12)? {
        return None;
    }
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let at = 12 + i * INDEX_ENTRY;
        let off = u64_at(body, at)?;
        let b = BlobRef {
            len: u64_at(body, at + 8)?,
            hash: hash_at(body, at + 16)?,
        };
        if off < HEADER || off.checked_add(b.len)? > len {
            return None;
        }
        entries.push((off, b));
    }
    Some(entries)
}

impl BlobStore {
    /// Opens (creating it if needed) the blob directory of the node directory `dir`.
    pub fn open(fs: Arc<dyn Vfs>, dir: &Path) -> Result<BlobStore, StoreError> {
        BlobStore::open_with(fs, dir, BlobOptions::default())
    }

    /// Opens the blob directory of the node directory `dir` with `opts`: loads each pack's index, or scans a pack that
    /// has none (cutting a torn tail), syncs and indexes it; adopts loose blobs; removes temporary and provisional
    /// files. Every pack it finds is sealed: appends go to a new one.
    pub fn open_with(fs: Arc<dyn Vfs>, dir: &Path, opts: BlobOptions) -> Result<BlobStore, StoreError> {
        let dir = blob_dir(dir);
        durable_dir(&*fs, &dir)?;
        let store = BlobStore {
            fs,
            dir,
            opts,
            state: Mutex::new(State::default()),
            cache: Mutex::new(Cache::default()),
            readers: Mutex::new(BTreeMap::new()),
            upkeep: Mutex::new(()),
        };
        let mut state = State::default();
        let (mut packs, mut indexes) = (BTreeSet::new(), BTreeSet::new());
        let mut changed = false;
        for p in store.fs.list(&store.dir)? {
            let Some(n) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if n.ends_with(".tmp") || n.ends_with(".log") {
                store.fs.remove(&p)?;
                changed = true;
            } else if let Some((seq, index)) = parse_pack(n) {
                if index {
                    indexes.insert(seq);
                } else {
                    packs.insert(seq);
                }
            } else if let Some(b) = parse(n) {
                state.insert(
                    b,
                    Entry {
                        loc: Loc::Loose,
                        logged: None,
                    },
                );
            }
        }
        for seq in indexes.difference(&packs) {
            store.fs.remove(&store.index_path(*seq))?;
            changed = true;
        }
        for seq in packs {
            let mut f = store.fs.open(&store.pack_path(seq), OpenOpts::default())?;
            let file_len = f.len()?;
            let indexed = if indexes.contains(&seq) {
                let bytes = read_all(&*store.fs.open(&store.index_path(seq), OpenOpts::default())?)?;
                parse_index(&bytes, file_len)
            } else {
                None
            };
            let (entries, len, is_indexed) = match indexed {
                Some(entries) => (entries, file_len, true),
                None => {
                    // No index, or one that does not check (never torn: written whole, then renamed): the pack is
                    // read entry by entry.
                    let (entries, valid) = scan(&read_all(&*f)?);
                    if valid < file_len {
                        f.truncate(valid)?;
                    }
                    f.sync_data()?;
                    changed = true;
                    (entries, valid, false)
                }
            };
            state.next_seq = seq.saturating_add(1);
            if entries.is_empty() {
                remove_if_there(&*store.fs, &store.index_path(seq))?;
                store.fs.remove(&store.pack_path(seq))?;
                changed = true;
                continue;
            }
            state.packs.insert(
                seq,
                Pack {
                    len,
                    synced: len,
                    named: true,
                    sealed: true,
                    indexed: is_indexed,
                    entries: if is_indexed { Vec::new() } else { entries.clone() },
                    ..Pack::default()
                },
            );
            for (off, b) in entries {
                // A blob in two packs (a compaction's copy and its source, both kept by a crash) is held once.
                if !state.index.contains_key(&b) {
                    state.insert(
                        b,
                        Entry {
                            loc: Loc::Pack { seq, off },
                            logged: None,
                        },
                    );
                }
            }
        }
        if changed {
            store.fs.sync_dir(&store.dir)?;
        }
        *store.state()? = state;
        store.index_sealed()?;
        Ok(store)
    }

    fn pack_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("p{seq:016x}.pack"))
    }

    fn index_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("p{seq:016x}.idx"))
    }

    fn loose_path(&self, b: &BlobRef) -> PathBuf {
        self.dir.join(name(b))
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        self.state
            .lock()
            .map_err(|_| invalid("the blob store's lock is poisoned"))
    }

    fn cache(&self) -> Result<MutexGuard<'_, Cache>, StoreError> {
        self.cache
            .lock()
            .map_err(|_| invalid("the blob store's cache lock is poisoned"))
    }

    fn readers(&self) -> Result<MutexGuard<'_, Readers>, StoreError> {
        self.readers
            .lock()
            .map_err(|_| invalid("the blob store's readers lock is poisoned"))
    }

    fn upkeep(&self) -> Result<MutexGuard<'_, ()>, StoreError> {
        self.upkeep
            .lock()
            .map_err(|_| invalid("the blob store's upkeep lock is poisoned"))
    }

    fn check(b: &BlobRef, bytes: &[u8]) -> Result<(), StoreError> {
        if BlobRef::of(bytes) != *b {
            return Err(invalid(format!(
                "the bytes given for blob {} are not its bytes",
                b.hex()
            )));
        }
        Ok(())
    }

    /// Appends `b` to the active pack (a new one first if there is none, or the entry would take it past
    /// `pack_bytes`), without a sync. Returns where its bytes are; the caller decides what the store holds.
    fn append(&self, s: &mut State, b: &BlobRef, bytes: &[u8]) -> Result<Loc, StoreError> {
        let entry = HEADER + b.len;
        if let Some((seq, _)) = &s.writer
            && s.packs
                .get(seq)
                .is_some_and(|p| p.len > 0 && p.len.saturating_add(entry) > self.opts.pack_bytes)
        {
            Self::seal(s);
        }
        if s.writer.is_none() {
            let seq = s.next_seq;
            s.next_seq = seq
                .checked_add(1)
                .ok_or_else(|| invalid("the pack sequence overflows"))?;
            let f = self.fs.open(
                &self.pack_path(seq),
                OpenOpts {
                    create_new: true,
                    ..OpenOpts::default()
                },
            )?;
            s.packs.insert(seq, Pack::default());
            s.writer = Some((seq, f));
        }
        let Some((seq, f)) = s.writer.as_mut() else {
            return Err(invalid("no pack to append to"));
        };
        let seq = *seq;
        let mut rec = Vec::with_capacity(bytes.len() + HEADER as usize);
        rec.extend_from_slice(&PACK_MAGIC);
        rec.extend_from_slice(&b.len.to_le_bytes());
        rec.extend_from_slice(&b.hash);
        rec.extend_from_slice(bytes);
        if let Err(e) = f.append(&rec) {
            // How much reached the file is unknown: the pack takes no more (opening cuts a partial entry).
            Self::seal(s);
            return Err(e);
        }
        let p = s
            .packs
            .get_mut(&seq)
            .ok_or_else(|| invalid("the active pack has no bookkeeping"))?;
        let off = p.len + HEADER;
        p.len += entry;
        p.entries.push((off, *b));
        Ok(Loc::Pack { seq, off })
    }

    fn seal(s: &mut State) {
        if let Some((seq, _)) = s.writer.take()
            && let Some(p) = s.packs.get_mut(&seq)
        {
            p.sealed = true;
        }
    }

    /// Appends the logged blob `b` (logged in the record at `at`) and holds it, pending.
    fn append_logged(&self, s: &mut State, b: &BlobRef, bytes: &[u8], at: Lsn) -> Result<(), StoreError> {
        let loc = self.append(s, b, bytes)?;
        if let Loc::Pack { seq, off } = loc
            && let Some(p) = s.packs.get_mut(&seq)
        {
            p.unsynced.insert(off + b.len, *b);
        }
        s.insert(*b, Entry { loc, logged: Some(at) });
        Ok(())
    }

    /// Appends `b` for a put or a compaction: the store holds it there once it is synced (`adopt`).
    fn append_adopting(&self, s: &mut State, b: &BlobRef, bytes: &[u8]) -> Result<Loc, StoreError> {
        let loc = self.append(s, b, bytes)?;
        if let Loc::Pack { seq, .. } = loc
            && let Some(p) = s.packs.get_mut(&seq)
        {
            p.adopting += 1;
        }
        Ok(loc)
    }

    /// An entry `append_adopting` made is synced: the store holds `b` there if `holds` (else it is dead).
    fn adopt(s: &mut State, b: BlobRef, loc: Loc, holds: bool) {
        if let Loc::Pack { seq, .. } = loc
            && let Some(p) = s.packs.get_mut(&seq)
        {
            p.adopting = p.adopting.saturating_sub(1);
        }
        if holds {
            s.insert(b, Entry { loc, logged: None });
        }
    }

    /// Syncs every pack with bytes not known synced (and the directory, for a pack whose name is not durable yet);
    /// the pending blobs those syncs cover are durable. No lock is held while syncing. Returns how many it made
    /// durable.
    fn sync_packs(&self) -> Result<usize, StoreError> {
        let due: Vec<(u64, u64, bool)> = self
            .state()?
            .packs
            .iter()
            .filter(|(_, p)| p.synced < p.len || !p.named)
            .map(|(seq, p)| (*seq, p.len, p.named))
            .collect();
        let mut synced = Vec::new();
        for (seq, len, named) in due {
            match self.fs.open(&self.pack_path(seq), OpenOpts::default()) {
                Ok(mut f) => {
                    f.sync_data()?;
                    synced.push((seq, len, named));
                }
                // Removed meanwhile: nothing it held is held any more.
                Err(e) if not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        if synced.iter().any(|(_, _, named)| !named) {
            self.fs.sync_dir(&self.dir)?;
        }
        let mut guard = self.state()?;
        let s = &mut *guard;
        let mut made = 0;
        for (seq, len, _) in synced {
            let Some(p) = s.packs.get_mut(&seq) else {
                continue;
            };
            p.synced = p.synced.max(len);
            p.named = true;
            let later = p.unsynced.split_off(&(len + 1));
            let done = std::mem::replace(&mut p.unsynced, later);
            for (end, b) in done {
                if s.settle(&b, seq, end) {
                    made += 1;
                }
            }
        }
        Ok(made)
    }

    /// Writes the index of each sealed pack that is synced whole and has none (its entries, live or not, then the hash
    /// of them), then syncs the directory once.
    fn index_sealed(&self) -> Result<(), StoreError> {
        let _turn = self.upkeep()?;
        let due: Vec<(u64, Vec<(u64, BlobRef)>)> = self
            .state()?
            .packs
            .iter()
            .filter(|(_, p)| p.sealed && !p.indexed && p.named && p.synced >= p.len)
            .map(|(seq, p)| (*seq, p.entries.clone()))
            .collect();
        if due.is_empty() {
            return Ok(());
        }
        let mut written = Vec::new();
        for (seq, entries) in due {
            let mut body = Vec::with_capacity(12 + entries.len() * INDEX_ENTRY + 32);
            body.extend_from_slice(&INDEX_MAGIC);
            body.extend_from_slice(&(entries.len() as u64).to_le_bytes());
            for (off, b) in &entries {
                body.extend_from_slice(&off.to_le_bytes());
                body.extend_from_slice(&b.len.to_le_bytes());
                body.extend_from_slice(&b.hash);
            }
            let check = BlobRef::of(&body).hash;
            body.extend_from_slice(&check);
            let path = self.index_path(seq);
            let tmp = path.with_extension("idx.tmp");
            let mut f = self.fs.open(
                &tmp,
                OpenOpts {
                    create: true,
                    truncate: true,
                    ..OpenOpts::default()
                },
            )?;
            f.append(&body)?;
            f.sync_data()?;
            self.fs.rename(&tmp, &path)?;
            written.push(seq);
        }
        self.fs.sync_dir(&self.dir)?;
        let mut s = self.state()?;
        for seq in written {
            if let Some(p) = s.packs.get_mut(&seq) {
                p.indexed = true;
                p.entries = Vec::new();
            }
        }
        Ok(())
    }

    /// The blobs of `blobs` the store does not hold (durable or logged), each once: those a WAL record must log (or
    /// `put_all` make durable) before a record referencing them syncs.
    pub fn unwritten(&self, blobs: &[BlobBytes]) -> Result<Vec<BlobBytes>, StoreError> {
        let s = self.state()?;
        let mut seen = BTreeSet::new();
        Ok(blobs
            .iter()
            .filter(|(b, _)| !s.index.contains_key(b) && seen.insert(*b))
            .cloned()
            .collect())
    }

    /// Records that `blobs` are logged in the WAL record at `at` (appended, so durable once it syncs) and appends
    /// them to the active pack without a sync, so they can be read at once. They stay pending until
    /// `sync_logged_below`. A blob the store already holds keeps its entry (durable, or pending on its earlier
    /// record).
    pub fn write_logged(&self, blobs: &[BlobBytes], at: Lsn) -> Result<(), StoreError> {
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            self.cache()?.insert(*b, bytes.clone());
            let mut s = self.state()?;
            if !s.index.contains_key(b) {
                self.append_logged(&mut s, b, bytes, at)?;
            }
        }
        Ok(())
    }

    /// Recovery: `blobs` are logged in the WAL record at `at`, which survived the crash. One the store found when it
    /// opened is durable; one it lacks (its entry lost or torn with the crash, or never written) is appended again,
    /// pending, so it is synced before that record's WAL goes.
    pub fn restore_logged(&self, blobs: &[BlobBytes], at: Lsn) -> Result<(), StoreError> {
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            let mut s = self.state()?;
            match s.index.get(b).copied() {
                Some(Entry {
                    loc,
                    logged: Some(first),
                }) if at < first => s.insert(*b, Entry { loc, logged: Some(at) }),
                Some(_) => {}
                None => self.append_logged(&mut s, b, bytes, at)?,
            }
        }
        Ok(())
    }

    /// The blobs pending (logged in records not truncated yet, their entries not known synced).
    pub fn pending_blobs(&self) -> Result<Vec<BlobRef>, StoreError> {
        Ok(self
            .state()?
            .index
            .iter()
            .filter(|(_, e)| e.logged.is_some())
            .map(|(b, _)| *b)
            .collect())
    }

    /// How many logged blobs are pending.
    pub fn pending(&self) -> Result<usize, StoreError> {
        let n: u64 = self.state()?.pending_at.values().sum();
        usize::try_from(n).map_err(|_| invalid("too many pending blobs to count"))
    }

    /// Makes durable the pending blobs logged before `lsn` (the WAL below it is about to be truncated): if there are
    /// any, syncs every pack with unsynced bytes (each once, and the directory once for new packs), which makes every
    /// pending blob in them durable; then indexes the packs that are sealed and synced whole. Returns how many blobs
    /// it made durable. Holds no lock while it syncs, so the committer keeps logging.
    pub fn sync_logged_below(&self, lsn: Lsn) -> Result<usize, StoreError> {
        let due = self
            .state()?
            .pending_at
            .first_key_value()
            .is_some_and(|(first, _)| *first < lsn);
        let made = if due { self.sync_packs()? } else { 0 };
        self.index_sealed()?;
        Ok(made)
    }

    /// Makes `blobs` durable: each the store does not hold is appended, then the packs are synced (which makes the
    /// pending ones among them durable too).
    pub fn put_all(&self, blobs: &[BlobBytes]) -> Result<(), StoreError> {
        if blobs.is_empty() {
            return Ok(());
        }
        let mut appended: Vec<(BlobRef, Loc)> = Vec::new();
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            self.cache()?.insert(*b, bytes.clone());
            let mut s = self.state()?;
            if s.index.contains_key(b) || appended.iter().any(|(x, _)| x == b) {
                continue;
            }
            let loc = self.append_adopting(&mut s, b, bytes)?;
            appended.push((*b, loc));
        }
        self.sync_packs()?;
        let mut s = self.state()?;
        for (b, loc) in appended {
            let holds = !s.index.contains_key(&b);
            Self::adopt(&mut s, b, loc, holds);
        }
        Ok(())
    }

    /// The bytes of `b`, checked against its hash; `None` if the store does not hold it.
    pub fn read(&self, b: &BlobRef) -> Result<Option<Arc<[u8]>>, StoreError> {
        if let Some(bytes) = self.cache()?.get(b) {
            return Ok(Some(bytes));
        }
        // A compaction moves a blob before it removes the pack it was in: a read that finds the pack gone looks again.
        for _ in 0..3 {
            let loc = match self.state()?.index.get(b) {
                Some(e) => e.loc,
                None => return Ok(None),
            };
            let bytes = match loc {
                Loc::Loose => self.read_loose(b)?,
                Loc::Pack { seq, off } => match self.read_at(seq, off, b)? {
                    Some(bytes) => bytes,
                    None => continue,
                },
            };
            self.cache()?.insert(*b, bytes.clone());
            return Ok(Some(bytes));
        }
        Err(invalid(format!("blob {} kept moving while it was read", b.hex())))
    }

    fn read_loose(&self, b: &BlobRef) -> Result<Arc<[u8]>, StoreError> {
        let path = self.loose_path(b);
        let bytes = read_all(&*self.fs.open(&path, OpenOpts::default())?)?;
        if BlobRef::of(&bytes) != *b {
            return Err(StoreError::Corruption {
                path,
                offset: 0,
                reason: format!("blob {} does not hold its bytes", b.hex()),
            });
        }
        Ok(Arc::from(bytes))
    }

    /// The bytes of `b` at `off` in pack `seq`, checked; `None` if the pack is gone.
    fn read_at(&self, seq: u64, off: u64, b: &BlobRef) -> Result<Option<Arc<[u8]>>, StoreError> {
        let len = usize::try_from(b.len).map_err(|_| invalid("a blob too large to read"))?;
        let mut buf = vec![0u8; len];
        {
            let mut readers = self.readers()?;
            let f = match readers.entry(seq) {
                std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::btree_map::Entry::Vacant(v) => {
                    match self.fs.open(&self.pack_path(seq), OpenOpts::default()) {
                        Ok(f) => v.insert(f),
                        Err(e) if not_found(&e) => return Ok(None),
                        Err(e) => return Err(e),
                    }
                }
            };
            let mut pos = 0;
            while pos < len {
                let n = f.pread(
                    off + pos as u64,
                    buf.get_mut(pos..).ok_or_else(|| invalid("read offset"))?,
                )?;
                if n == 0 {
                    return Err(StoreError::Corruption {
                        path: self.pack_path(seq),
                        offset: off,
                        reason: format!("the pack ends inside blob {}", b.hex()),
                    });
                }
                pos += n;
            }
        }
        if BlobRef::of(&buf) != *b {
            return Err(StoreError::Corruption {
                path: self.pack_path(seq),
                offset: off,
                reason: format!("blob {} does not hold its bytes", b.hex()),
            });
        }
        Ok(Some(Arc::from(buf)))
    }

    /// The blobs the store holds durably (not the pending ones).
    pub fn list(&self) -> Result<Vec<BlobRef>, StoreError> {
        Ok(self
            .state()?
            .index
            .iter()
            .filter(|(_, e)| e.logged.is_none())
            .map(|(b, _)| *b)
            .collect())
    }

    /// Deletes the blobs `gone` (after a flush: blobs no durable row can reference any more; the node decides which).
    /// A blob the store does not hold is skipped. A sealed pack left with no live entry is removed (not synced: see
    /// the module's comment). Returns how many it deleted.
    pub fn delete(&self, gone: &[BlobRef]) -> Result<usize, StoreError> {
        if gone.is_empty() {
            return Ok(0);
        }
        let mut deleted = 0;
        let mut loose = Vec::new();
        let empty: Vec<u64> = {
            let mut s = self.state()?;
            for b in gone {
                if let Some(e) = s.remove(b) {
                    deleted += 1;
                    if e.loc == Loc::Loose {
                        loose.push(*b);
                    }
                }
            }
            let empty: Vec<u64> = s
                .packs
                .iter()
                .filter(|(_, p)| p.removable())
                .map(|(seq, _)| *seq)
                .collect();
            for seq in &empty {
                s.packs.remove(seq);
            }
            empty
        };
        {
            let mut cache = self.cache()?;
            for b in gone {
                cache.remove(b);
            }
        }
        for b in loose {
            remove_if_there(&*self.fs, &self.loose_path(&b))?;
        }
        for seq in empty {
            self.remove_pack(seq)?;
        }
        Ok(deleted)
    }

    fn remove_pack(&self, seq: u64) -> Result<(), StoreError> {
        self.readers()?.remove(&seq);
        remove_if_there(&*self.fs, &self.index_path(seq))?;
        remove_if_there(&*self.fs, &self.pack_path(seq))
    }

    /// Compacts the sealed, indexed pack with the most dead bytes, if they are at least half of it: its live entries
    /// are appended to the active pack and synced, the store holds them there, and the pack is removed. Returns how
    /// many packs it removed (0 or 1).
    pub fn compact(&self) -> Result<usize, StoreError> {
        let _turn = self.upkeep()?;
        let target = self
            .state()?
            .packs
            .iter()
            .filter(|(_, p)| p.sealed && p.indexed && p.dead().saturating_mul(2) >= p.len)
            .max_by_key(|(_, p)| p.dead())
            .map(|(seq, p)| (*seq, p.live.iter().map(|(o, b)| (*o, *b)).collect::<Vec<_>>()));
        let Some((seq, live)) = target else {
            return Ok(0);
        };
        let mut moved = Vec::new();
        for (off, b) in live {
            let Some(bytes) = self.read_at(seq, off, &b)? else {
                // Removed meanwhile: nothing it held is held any more (the copies made so far are dead).
                break;
            };
            let mut s = self.state()?;
            if s.index.get(&b).map(|e| e.loc) != Some(Loc::Pack { seq, off }) {
                continue;
            }
            let loc = self.append_adopting(&mut s, &b, &bytes)?;
            moved.push((b, off, loc));
        }
        // The copies (and the name of a pack they started) are durable before the store holds them there.
        self.sync_packs()?;
        let empty = {
            let mut s = self.state()?;
            for (b, off, loc) in moved {
                let holds = s.index.get(&b).map(|e| e.loc) == Some(Loc::Pack { seq, off });
                Self::adopt(&mut s, b, loc, holds);
            }
            let empty = s.packs.get(&seq).is_some_and(|p| p.removable());
            if empty {
                s.packs.remove(&seq);
            }
            empty
        };
        if !empty {
            return Ok(0);
        }
        self.remove_pack(seq)?;
        Ok(1)
    }
}

impl blossom_value::BlobSource for BlobStore {
    /// A blob the store cannot read (missing, or corrupt) is `None`: the evaluator reports it as a host bug.
    fn get(&self, b: &BlobRef) -> Option<Arc<[u8]>> {
        self.read(b).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SimFs, WriteFate};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A blob of `size` bytes that starts with `tag`.
    fn blob(tag: &str, size: usize) -> BlobBytes {
        let mut bytes = tag.as_bytes().to_vec();
        bytes.resize(size.max(tag.len()), b'.');
        let bytes: Arc<[u8]> = Arc::from(bytes);
        (BlobRef::of(&bytes), bytes)
    }

    /// Packs of 4 KiB: a few entries each.
    fn small() -> BlobOptions {
        BlobOptions { pack_bytes: 4096 }
    }

    fn open(fs: &SimFs, opts: BlobOptions) -> BlobStore {
        BlobStore::open_with(Arc::new(fs.clone()), Path::new("/n"), opts).unwrap()
    }

    /// A crash on `fs` that loses every unsynced write, and the store opened on what is left.
    fn crashed(fs: &SimFs, opts: BlobOptions) -> (SimFs, BlobStore) {
        let mut after = fs.fork().unwrap();
        after.crash(&mut |_| WriteFate::Lost).unwrap();
        let store = open(&after, opts);
        (after, store)
    }

    fn reads(store: &BlobStore, b: &BlobBytes) -> bool {
        store.read(&b.0).unwrap().as_deref() == Some(&b.1[..])
    }

    fn files(fs: &SimFs, ext: &str) -> usize {
        fs.list(&blob_dir(Path::new("/n")))
            .unwrap()
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .count()
    }

    #[test]
    fn blobs_round_trip_and_names_parse() {
        let fs = SimFs::default();
        let store = open(&fs, BlobOptions::default());
        let (a, b) = (blob("first blob", 10), blob("second", 6));
        store.put_all(&[a.clone(), b.clone()]).unwrap();
        assert!(reads(&store, &a));
        assert_eq!(store.read(&BlobRef::of(b"absent")).unwrap(), None);
        let mut listed = store.list().unwrap();
        listed.sort();
        let mut want = vec![a.0, b.0];
        want.sort();
        assert_eq!(listed, want);
        assert_eq!(parse(&name(&a.0)), Some(a.0));
        assert_eq!(parse_pack("p000000000000002a.idx"), Some((42, true)));
        assert_eq!(parse_pack("p2a.pack"), None);
        assert_eq!(store.delete(&[b.0, BlobRef::of(b"absent")]).unwrap(), 1);
        assert_eq!(store.list().unwrap(), vec![a.0]);
        // A deleted blob is written again when it is put again.
        store.put_all(std::slice::from_ref(&b)).unwrap();
        assert!(reads(&store, &b));
        // Bytes that are not the blob's are refused on every path.
        assert!(store.put_all(&[(a.0, b.1.clone())]).is_err());
        assert!(store.write_logged(&[(a.0, b.1.clone())], Lsn(1)).is_err());
        assert!(store.restore_logged(&[(a.0, b.1.clone())], Lsn(1)).is_err());
        // A put is durable.
        let (_, recovered) = crashed(&fs, BlobOptions::default());
        assert!(reads(&recovered, &a) && reads(&recovered, &b));
    }

    #[test]
    fn a_logged_blob_is_pending_until_synced_below_its_record() {
        let fs = SimFs::default();
        let store = open(&fs, BlobOptions::default());
        let a = blob("logged", 100);
        store.write_logged(std::slice::from_ref(&a), Lsn(10)).unwrap();
        // Readable at once, but not durable: a crash loses it.
        assert!(reads(&store, &a));
        assert_eq!(store.pending().unwrap(), 1);
        assert_eq!(store.pending_blobs().unwrap(), vec![a.0]);
        assert_eq!(store.unwritten(std::slice::from_ref(&a)).unwrap(), vec![]);
        let (lost, recovered) = crashed(&fs, BlobOptions::default());
        assert_eq!(recovered.read(&a.0).unwrap(), None);
        // Recovery appends it again from its record, still pending.
        recovered.restore_logged(std::slice::from_ref(&a), Lsn(10)).unwrap();
        assert!(reads(&recovered, &a));
        assert_eq!(recovered.pending().unwrap(), 1);
        // Only a truncation past its record syncs it.
        assert_eq!(recovered.sync_logged_below(Lsn(10)).unwrap(), 0);
        assert_eq!(recovered.sync_logged_below(Lsn(11)).unwrap(), 1);
        assert_eq!(recovered.pending().unwrap(), 0);
        assert_eq!(recovered.list().unwrap(), vec![a.0]);
        let (_, again) = crashed(&lost, BlobOptions::default());
        assert!(reads(&again, &a));
    }

    /// A blob logged again keeps its entry and its first record.
    #[test]
    fn a_blob_logged_again_keeps_its_first_record() {
        let fs = SimFs::default();
        let store = open(&fs, BlobOptions::default());
        let x = blob("logged twice", 100);
        store.write_logged(std::slice::from_ref(&x), Lsn(5)).unwrap();
        store.write_logged(std::slice::from_ref(&x), Lsn(9)).unwrap();
        assert_eq!(files(&fs, "pack"), 1);
        assert_eq!(store.sync_logged_below(Lsn(6)).unwrap(), 1);
        let (_, recovered) = crashed(&fs, BlobOptions::default());
        assert!(reads(&recovered, &x));
    }

    /// A torn entry ends what a pack holds: the entries after it go too, and come back from their records.
    #[test]
    fn a_torn_pack_tail_is_cut_and_restored_from_the_records() {
        let fs = SimFs::default();
        let store = open(&fs, BlobOptions::default());
        let a = blob("synced", 1500);
        store.write_logged(std::slice::from_ref(&a), Lsn(1)).unwrap();
        assert_eq!(store.sync_logged_below(Lsn(2)).unwrap(), 1);
        let (b, c) = (blob("torn", 1500), blob("after it", 1500));
        store.write_logged(std::slice::from_ref(&b), Lsn(3)).unwrap();
        store.write_logged(std::slice::from_ref(&c), Lsn(4)).unwrap();
        let mut after = fs.fork().unwrap();
        let mut first = true;
        after
            .crash(&mut |_| {
                if std::mem::take(&mut first) {
                    WriteFate::Torn { sectors: 1 }
                } else {
                    WriteFate::Survive
                }
            })
            .unwrap();
        let recovered = open(&after, BlobOptions::default());
        assert!(reads(&recovered, &a));
        assert_eq!(recovered.read(&b.0).unwrap(), None);
        assert_eq!(recovered.read(&c.0).unwrap(), None);
        recovered.restore_logged(std::slice::from_ref(&b), Lsn(3)).unwrap();
        recovered.restore_logged(std::slice::from_ref(&c), Lsn(4)).unwrap();
        assert_eq!(recovered.sync_logged_below(Lsn(5)).unwrap(), 2);
        let (_, again) = crashed(&after, BlobOptions::default());
        assert!(reads(&again, &a) && reads(&again, &b) && reads(&again, &c));
    }

    /// A put syncs the pack, and with it the logged blobs before it.
    #[test]
    fn a_put_makes_the_logged_blobs_before_it_durable() {
        let fs = SimFs::default();
        let store = open(&fs, BlobOptions::default());
        let (x, y) = (blob("logged", 300), blob("put", 300));
        store.write_logged(std::slice::from_ref(&x), Lsn(10)).unwrap();
        store.put_all(std::slice::from_ref(&y)).unwrap();
        assert_eq!(store.pending().unwrap(), 0);
        let (_, recovered) = crashed(&fs, BlobOptions::default());
        assert!(reads(&recovered, &x) && reads(&recovered, &y));
    }

    /// A sealed pack synced whole is indexed, and opening reads its index, not the pack: a corrupt byte in it is found
    /// by the read of that blob, not when the store opens.
    #[test]
    fn sealed_packs_are_indexed_and_open_from_their_index() {
        let fs = SimFs::default();
        let store = open(&fs, small());
        let blobs: Vec<BlobBytes> = (0..10).map(|i| blob(&format!("blob {i}"), 1000)).collect();
        for (i, b) in blobs.iter().enumerate() {
            store.write_logged(std::slice::from_ref(b), Lsn(i as u64 + 1)).unwrap();
        }
        assert_eq!(store.sync_logged_below(Lsn(100)).unwrap(), 10);
        // Three entries of 1044 bytes fill a pack of 4 KiB: packs 0-2 are sealed, pack 3 is active.
        assert_eq!(files(&fs, "pack"), 4);
        assert_eq!(files(&fs, "idx"), 3);
        fs.corrupt(&store.pack_path(0), HEADER as usize + 10).unwrap();
        let reopened = open(&fs, small());
        assert!(reopened.read(&blobs[0].0).is_err());
        for b in &blobs[1..] {
            assert!(reads(&reopened, b));
        }
        // The active pack had no index: it was scanned, synced and indexed.
        assert_eq!(files(&fs, "idx"), 4);
    }

    #[test]
    fn deletes_remove_empty_packs_and_compaction_reclaims_mostly_dead_ones() {
        let fs = SimFs::default();
        let store = open(&fs, small());
        let blobs: Vec<BlobBytes> = (0..9).map(|i| blob(&format!("blob {i}"), 1000)).collect();
        store.put_all(&blobs).unwrap();
        // Packs 0 and 1 are sealed and synced: indexed at the next truncation.
        store.sync_logged_below(Lsn(1)).unwrap();
        assert_eq!(files(&fs, "idx"), 2);
        assert_eq!(store.delete(&[blobs[0].0, blobs[1].0, blobs[2].0]).unwrap(), 3);
        assert!(!fs.list(&store.dir).unwrap().contains(&store.pack_path(0)));
        // Two of pack 1's three entries go: more than half of it is dead.
        assert_eq!(store.delete(&[blobs[3].0, blobs[4].0]).unwrap(), 2);
        assert_eq!(store.compact().unwrap(), 1);
        assert!(!fs.list(&store.dir).unwrap().contains(&store.pack_path(1)));
        assert_eq!(store.compact().unwrap(), 0);
        for b in &blobs[5..] {
            assert!(reads(&store, b));
        }
        let (_, recovered) = crashed(&fs, small());
        for b in &blobs[5..] {
            assert!(reads(&recovered, b));
        }
    }

    /// At every crash point of logging, syncing, deleting, compacting and putting (unsynced writes lost, kept, or
    /// torn), the store opens, holds every blob it had made durable and not deleted, and reads every blob it holds.
    #[test]
    fn every_crash_point_keeps_the_durable_blobs() {
        let fs = SimFs::default();
        let store = open(&fs, small());
        let base: Vec<BlobBytes> = (0..6).map(|i| blob(&format!("base {i}"), 900)).collect();
        store.put_all(&base).unwrap();
        store.sync_logged_below(Lsn(1)).unwrap();
        fs.enable_crash_recording().unwrap();
        let mut durable: BTreeMap<BlobRef, Arc<[u8]>> = base.iter().cloned().collect();
        let mut acks = vec![(fs.cut_count().unwrap(), durable.clone())];
        let logged: Vec<BlobBytes> = (0..5).map(|i| blob(&format!("logged {i}"), 700)).collect();
        for (i, b) in logged.iter().enumerate() {
            store.write_logged(std::slice::from_ref(b), Lsn(10 + i as u64)).unwrap();
        }
        store.sync_logged_below(Lsn(100)).unwrap();
        durable.extend(logged.iter().cloned());
        acks.push((fs.cut_count().unwrap(), durable.clone()));
        let gone: Vec<BlobRef> = base[..4].iter().map(|b| b.0).collect();
        store.delete(&gone).unwrap();
        for b in &gone {
            durable.remove(b);
        }
        acks.push((fs.cut_count().unwrap(), durable.clone()));
        while store.compact().unwrap() > 0 {}
        let late = blob("late", 2000);
        store.put_all(std::slice::from_ref(&late)).unwrap();
        durable.insert(late.0, late.1.clone());
        acks.push((fs.cut_count().unwrap(), durable.clone()));
        let cuts = fs.recorded_cuts().unwrap();
        assert!(cuts.len() > 20, "{} cuts", cuts.len());
        let mut images = 0;
        for (ci, cut) in cuts.into_iter().enumerate() {
            let want = &acks.iter().rev().find(|(at, _)| *at <= ci + 1).unwrap().1;
            let writes = cut.unsynced_writes().unwrap();
            let mut variants = vec![
                vec![WriteFate::Lost; writes.len()],
                vec![WriteFate::Survive; writes.len()],
            ];
            for (i, w) in writes.iter().enumerate() {
                if !w.bytes.is_empty() && (w.offset + w.bytes.len() as u64 - 1) / 512 > w.offset / 512 {
                    let mut v = vec![WriteFate::Survive; writes.len()];
                    v[i] = WriteFate::Torn { sectors: 1 };
                    variants.push(v);
                }
            }
            for fates in variants {
                let mut image = cut.fork().unwrap();
                image.crash_with_fates(&fates).unwrap();
                let reopened = open(&image, small());
                for (b, bytes) in want {
                    assert_eq!(reopened.read(b).unwrap().as_deref(), Some(&bytes[..]), "cut {ci}");
                }
                for b in reopened.list().unwrap() {
                    assert!(reopened.read(&b).unwrap().is_some(), "cut {ci}");
                }
                images += 1;
            }
        }
        assert!(images > 40);
    }

    /// The committer logs (and logs again) while the promoter syncs and compacts, puts make blobs durable and dead
    /// blobs are deleted: nothing fails, and after a power loss every blob made durable reads back whole.
    #[test]
    fn syncs_and_compactions_run_beside_logging_puts_and_deletes() {
        for round in 0..10u64 {
            let fs = SimFs::default();
            let store = Arc::new(open(&fs, small()));
            let keep: Vec<BlobBytes> = (0..150u64).map(|i| blob(&format!("keep {i} {round}"), 300)).collect();
            let dead: Vec<BlobBytes> = (0..150u64).map(|i| blob(&format!("dead {i} {round}"), 300)).collect();
            let logged = Arc::new(AtomicU64::new(0));
            let committer = {
                let (store, keep, dead, logged) = (store.clone(), keep.clone(), dead.clone(), logged.clone());
                std::thread::spawn(move || {
                    for (i, (k, d)) in keep.iter().zip(&dead).enumerate() {
                        let at = Lsn(3 * i as u64 + 1);
                        store.write_logged(std::slice::from_ref(d), at).unwrap();
                        store.write_logged(std::slice::from_ref(k), Lsn(at.0 + 1)).unwrap();
                        // An earlier blob logged again, maybe while it is synced or moved.
                        let again = keep.get(i / 2).unwrap();
                        store.write_logged(std::slice::from_ref(again), Lsn(at.0 + 2)).unwrap();
                        logged.store(at.0 + 3, Ordering::SeqCst);
                    }
                })
            };
            let putter = {
                let (store, keep) = (store.clone(), keep.clone());
                std::thread::spawn(move || {
                    for b in keep.iter().step_by(7) {
                        store.put_all(std::slice::from_ref(b)).unwrap();
                    }
                })
            };
            let deleter = {
                let (store, dead) = (store.clone(), dead.clone());
                std::thread::spawn(move || {
                    for chunk in dead.chunks(10) {
                        let gone: Vec<BlobRef> = chunk.iter().map(|b| b.0).collect();
                        while store.unwritten(chunk).unwrap().len() == chunk.len() {
                            std::thread::yield_now();
                        }
                        store.delete(&gone).unwrap();
                    }
                })
            };
            while !committer.is_finished() {
                store.sync_logged_below(Lsn(logged.load(Ordering::SeqCst))).unwrap();
                store.compact().unwrap();
            }
            committer.join().unwrap();
            putter.join().unwrap();
            deleter.join().unwrap();
            store.sync_logged_below(Lsn(u64::MAX)).unwrap();
            while store.compact().unwrap() > 0 {}
            assert_eq!(store.pending().unwrap(), 0);
            for b in &keep {
                assert!(reads(&store, b), "round {round}");
            }
            let (_, recovered) = crashed(&fs, small());
            for b in &keep {
                assert!(reads(&recovered, b), "round {round}");
            }
        }
    }

    /// A store from before packs: its blob files are held as they are, its provisional files go.
    #[test]
    fn loose_blobs_of_an_older_store_are_adopted() {
        let fs = SimFs::default();
        let dir = blob_dir(Path::new("/n"));
        fs.create_dir_all(&dir).unwrap();
        let (a, p) = (blob("loose", 50), blob("provisional", 50));
        for (path, bytes) in [
            (dir.join(name(&a.0)), &a.1),
            (dir.join(format!("{}.log", name(&p.0))), &p.1),
        ] {
            let mut f = fs
                .open(
                    &path,
                    OpenOpts {
                        create: true,
                        ..OpenOpts::default()
                    },
                )
                .unwrap();
            f.append(bytes).unwrap();
            f.sync_data().unwrap();
        }
        fs.sync_dir(&dir).unwrap();
        let store = open(&fs, BlobOptions::default());
        assert!(reads(&store, &a));
        assert_eq!(store.list().unwrap(), vec![a.0]);
        assert_eq!(store.read(&p.0).unwrap(), None);
        assert_eq!(files(&fs, "log"), 0);
        assert_eq!(store.delete(&[a.0]).unwrap(), 1);
        assert_eq!(store.read(&a.0).unwrap(), None);
        assert!(fs.list(&dir).unwrap().is_empty());
    }
}
