//! The blob store (FOREIGN-PROTOCOLS §5): a node's durable blobs, one file each under `<store>/blobs/`, named by the
//! blob's hash and length. A blob is durable before the WAL record that references it syncs, so recovery never finds
//! a row whose blob is missing. It gets there one of two ways:
//!
//! - **Put** (`put_all`): written to a temporary file, synced, then renamed to its name; the directory is synced once
//!   for a batch of blobs.
//! - **Logged** (`write_logged`): its bytes travel in the WAL record itself, which makes them durable with the
//!   record's one sync; the file is written without a sync (so it can be read at once) and is *pending* until
//!   `sync_logged_below` syncs it, which must happen before the WAL that logs it is truncated. Recovery restores a
//!   logged blob whose file a crash lost or tore from the WAL (`restore_logged`).
//!
//! So a name may hold content that is not durable, or (after a crash) torn: a name this process did not write is
//! never trusted, and reading one checks its hash.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use blossom_value::BlobRef;

use crate::{Lsn, OpenOpts, StoreError, Vfs, durable_dir, invalid, read_all};

/// A blob and its bytes.
pub type BlobBytes = (BlobRef, Arc<[u8]>);

/// The blob directory of a node directory.
pub fn blob_dir(dir: &Path) -> PathBuf {
    dir.join("blobs")
}

/// A node's durable blobs.
pub struct BlobStore {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
    known: Mutex<Known>,
}

/// The blobs this process wrote: writing them again is skipped.
#[derive(Default)]
struct Known {
    /// Durable (synced, and their directory entry too).
    durable: BTreeSet<BlobRef>,
    /// Logged: durable in the WAL record at this position, their file written but not synced.
    pending: BTreeMap<BlobRef, Lsn>,
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn name(b: &BlobRef) -> String {
    format!("{}-{}", b.hex(), b.len)
}

/// The blob a file name names, if it names one.
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

impl BlobStore {
    /// Opens (creating it if needed) the blob directory of the node directory `dir`, and removes temporary files a
    /// crash left behind.
    pub fn open(fs: Arc<dyn Vfs>, dir: &Path) -> Result<BlobStore, StoreError> {
        let dir = blob_dir(dir);
        durable_dir(&*fs, &dir)?;
        for p in fs.list(&dir)? {
            if p.extension().is_some_and(|e| e == "tmp") {
                fs.remove(&p)?;
            }
        }
        Ok(BlobStore {
            fs,
            dir,
            known: Mutex::new(Known::default()),
        })
    }

    fn path(&self, b: &BlobRef) -> PathBuf {
        self.dir.join(name(b))
    }

    fn known(&self) -> Result<std::sync::MutexGuard<'_, Known>, StoreError> {
        self.known
            .lock()
            .map_err(|_| invalid("the blob store's lock is poisoned"))
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

    /// Writes `bytes` to a temporary file, synced or not, and renames it to `b`'s name.
    fn write_file(&self, b: &BlobRef, bytes: &[u8], sync: bool) -> Result<(), StoreError> {
        let path = self.path(b);
        let tmp = path.with_extension("tmp");
        let mut f = self.fs.open(
            &tmp,
            OpenOpts {
                create: true,
                truncate: true,
                ..OpenOpts::default()
            },
        )?;
        f.append(bytes)?;
        if sync {
            f.sync_data()?;
        }
        self.fs.rename(&tmp, &path)
    }

    /// The blobs of `blobs` this process has neither made durable nor logged, each once: those a WAL record must
    /// log (or `put_all` make durable) before a record referencing them syncs.
    pub fn unwritten(&self, blobs: &[BlobBytes]) -> Result<Vec<BlobBytes>, StoreError> {
        let known = self.known()?;
        let mut seen = BTreeSet::new();
        Ok(blobs
            .iter()
            .filter(|(b, _)| !known.durable.contains(b) && !known.pending.contains_key(b) && seen.insert(*b))
            .cloned()
            .collect())
    }

    /// Records that `blobs` are logged in the WAL record at `at` (appended, so durable once it syncs) and writes
    /// their files without a sync, so they can be read at once. They stay pending until `sync_logged_below`.
    pub fn write_logged(&self, blobs: &[BlobBytes], at: Lsn) -> Result<(), StoreError> {
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            self.write_file(b, bytes, false)?;
            let mut known = self.known()?;
            if !known.durable.contains(b) {
                let e = known.pending.entry(*b).or_insert(at);
                *e = (*e).min(at);
            }
        }
        Ok(())
    }

    /// Recovery: `blobs` are logged in the WAL record at `at`, which survived the crash. A file the crash lost or
    /// tore (or never wrote) is written again from the record; every one is pending, so it is synced before that
    /// record's WAL goes (a file that reads back whole may still be in the page cache only).
    pub fn restore_logged(&self, blobs: &[BlobBytes], at: Lsn) -> Result<(), StoreError> {
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            if !matches!(self.read(b), Ok(Some(_))) {
                self.write_file(b, bytes, false)?;
            }
            let mut known = self.known()?;
            let e = known.pending.entry(*b).or_insert(at);
            *e = (*e).min(at);
        }
        Ok(())
    }

    /// Makes durable the pending blobs logged before `lsn` (the WAL below it is about to be truncated): syncs each
    /// one's file, then the directory once. A blob deleted meanwhile is skipped; one logged again meanwhile (deleted
    /// and written anew) stays pending under its new position. Returns how many it synced. Holds no lock while it
    /// syncs, so the committer keeps logging.
    pub fn sync_logged_below(&self, lsn: Lsn) -> Result<usize, StoreError> {
        let due: Vec<(BlobRef, Lsn)> = self
            .known()?
            .pending
            .iter()
            .filter(|(_, at)| **at < lsn)
            .map(|(b, at)| (*b, *at))
            .collect();
        let mut synced = Vec::new();
        for (b, at) in due {
            match self.fs.open(&self.path(&b), OpenOpts::default()) {
                Ok(mut f) => f.sync_data()?,
                Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            synced.push((b, at));
        }
        if synced.is_empty() {
            return Ok(0);
        }
        self.fs.sync_dir(&self.dir)?;
        let mut known = self.known()?;
        let mut made = 0;
        for (b, at) in synced {
            if known.pending.get(&b) == Some(&at) {
                known.pending.remove(&b);
                known.durable.insert(b);
                made += 1;
            }
        }
        Ok(made)
    }

    /// How many logged blobs are pending (their files not yet synced).
    pub fn pending(&self) -> Result<usize, StoreError> {
        Ok(self.known()?.pending.len())
    }

    /// Makes `blobs` durable: each is written (unless this process already made it durable) and synced, then the
    /// directory once. A pending blob's file is synced in place. A name this process did not write is written again:
    /// a crash may have left it torn, or in no more than the page cache.
    pub fn put_all(&self, blobs: &[BlobBytes]) -> Result<(), StoreError> {
        if blobs.is_empty() {
            return Ok(());
        }
        let (durable, pending): (BTreeSet<BlobRef>, BTreeSet<BlobRef>) = {
            let known = self.known()?;
            (known.durable.clone(), known.pending.keys().copied().collect())
        };
        let mut wrote = Vec::new();
        for (b, bytes) in blobs {
            Self::check(b, bytes)?;
            if durable.contains(b) || wrote.contains(b) {
                continue;
            }
            if pending.contains(b) {
                match self.fs.open(&self.path(b), OpenOpts::default()) {
                    Ok(mut f) => f.sync_data()?,
                    // Deleted meanwhile: written anew.
                    Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                        self.write_file(b, bytes, true)?
                    }
                    Err(e) => return Err(e),
                }
            } else {
                self.write_file(b, bytes, true)?;
            }
            wrote.push(*b);
        }
        if wrote.is_empty() {
            return Ok(());
        }
        self.fs.sync_dir(&self.dir)?;
        let mut known = self.known()?;
        for b in wrote {
            known.pending.remove(&b);
            known.durable.insert(b);
        }
        Ok(())
    }

    /// The bytes of `b`, checked against its hash; `None` if the store does not hold it.
    pub fn read(&self, b: &BlobRef) -> Result<Option<Arc<[u8]>>, StoreError> {
        let path = self.path(b);
        let f = match self.fs.open(&path, OpenOpts::default()) {
            Ok(f) => f,
            Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let bytes = read_all(&*f)?;
        if BlobRef::of(&bytes) != *b {
            return Err(StoreError::Corruption {
                path,
                offset: 0,
                reason: format!("blob {} does not hold its bytes", b.hex()),
            });
        }
        Ok(Some(Arc::from(bytes)))
    }

    /// The blobs the store holds.
    pub fn list(&self) -> Result<Vec<BlobRef>, StoreError> {
        Ok(self
            .fs
            .list(&self.dir)?
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).and_then(parse))
            .collect())
    }

    /// Deletes the blobs `gone` (after a checkpoint: blobs no durable row can reference any more; the node decides
    /// which, from the change, so this does no listing). A blob the store does not hold is skipped. Returns how
    /// many were deleted.
    pub fn delete(&self, gone: &[BlobRef]) -> Result<usize, StoreError> {
        if gone.is_empty() {
            return Ok(0);
        }
        // The sets must forget them first, or a `put_all` or a record racing this would skip writing one a row
        // needs again.
        {
            let mut known = self.known()?;
            for b in gone {
                known.durable.remove(b);
                known.pending.remove(b);
            }
        }
        let mut deleted = 0;
        for b in gone {
            match self.fs.remove(&self.path(b)) {
                Ok(()) => deleted += 1,
                Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        if deleted > 0 {
            self.fs.sync_dir(&self.dir)?;
        }
        Ok(deleted)
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
    use crate::SimFs;

    #[test]
    fn blobs_round_trip_and_names_parse() {
        let fs: Arc<dyn Vfs> = Arc::new(SimFs::default());
        let store = BlobStore::open(fs, Path::new("/n")).unwrap();
        let a: Arc<[u8]> = Arc::from(&b"first blob"[..]);
        let b: Arc<[u8]> = Arc::from(&b"second"[..]);
        let (ra, rb) = (BlobRef::of(&a), BlobRef::of(&b));
        store.put_all(&[(ra, a.clone()), (rb, b.clone())]).unwrap();
        assert_eq!(store.read(&ra).unwrap().as_deref(), Some(&a[..]));
        assert_eq!(store.read(&BlobRef::of(b"absent")).unwrap(), None);
        let mut listed = store.list().unwrap();
        listed.sort();
        let mut want = vec![ra, rb];
        want.sort();
        assert_eq!(listed, want);
        assert_eq!(parse(&name(&ra)), Some(ra));
        assert_eq!(store.delete(&[rb, BlobRef::of(b"absent")]).unwrap(), 1);
        assert_eq!(store.list().unwrap(), vec![ra]);
        // A deleted blob is written again when it is put again.
        store.put_all(&[(rb, b.clone())]).unwrap();
        assert_eq!(store.read(&rb).unwrap().as_deref(), Some(&b[..]));
        // Bytes that are not the blob's are refused.
        assert!(store.put_all(&[(ra, b.clone())]).is_err());
    }

    /// A crash on `fs` that loses every unsynced write, and the store opened on what is left.
    fn crashed(fs: &SimFs) -> (SimFs, BlobStore) {
        let mut after = fs.fork().unwrap();
        after.crash(&mut |_| crate::WriteFate::Lost).unwrap();
        let store = BlobStore::open(Arc::new(after.clone()), Path::new("/n")).unwrap();
        (after, store)
    }

    #[test]
    fn a_logged_blob_is_pending_until_synced_below_its_record() {
        let fs = SimFs::default();
        let store = BlobStore::open(Arc::new(fs.clone()), Path::new("/n")).unwrap();
        let a: Arc<[u8]> = Arc::from(&b"logged"[..]);
        let ra = BlobRef::of(&a);
        store.write_logged(&[(ra, a.clone())], Lsn(10)).unwrap();
        // Readable at once, but not durable: a crash loses it.
        assert_eq!(store.read(&ra).unwrap().as_deref(), Some(&a[..]));
        assert_eq!(store.pending().unwrap(), 1);
        assert_eq!(store.unwritten(&[(ra, a.clone())]).unwrap(), vec![]);
        let (lost, recovered) = crashed(&fs);
        assert_eq!(recovered.read(&ra).unwrap(), None);
        // Recovery writes it again from its record, still pending.
        recovered.restore_logged(&[(ra, a.clone())], Lsn(10)).unwrap();
        assert_eq!(recovered.read(&ra).unwrap().as_deref(), Some(&a[..]));
        assert_eq!(recovered.pending().unwrap(), 1);
        // Only a truncation past its record syncs it.
        assert_eq!(recovered.sync_logged_below(Lsn(10)).unwrap(), 0);
        assert_eq!(recovered.sync_logged_below(Lsn(11)).unwrap(), 1);
        assert_eq!(recovered.pending().unwrap(), 0);
        let (_, again) = crashed(&lost);
        assert_eq!(again.read(&ra).unwrap().as_deref(), Some(&a[..]));
    }

    #[test]
    fn a_deleted_logged_blob_is_forgotten_and_a_put_syncs_a_pending_one() {
        let fs = SimFs::default();
        let store = BlobStore::open(Arc::new(fs.clone()), Path::new("/n")).unwrap();
        let (a, b): (Arc<[u8]>, Arc<[u8]>) = (Arc::from(&b"gone"[..]), Arc::from(&b"put"[..]));
        let (ra, rb) = (BlobRef::of(&a), BlobRef::of(&b));
        store.write_logged(&[(ra, a.clone()), (rb, b.clone())], Lsn(5)).unwrap();
        assert_eq!(store.delete(&[ra]).unwrap(), 1);
        assert_eq!(store.unwritten(&[(ra, a.clone())]).unwrap(), vec![(ra, a.clone())]);
        store.put_all(&[(rb, b.clone())]).unwrap();
        assert_eq!(store.pending().unwrap(), 0);
        assert_eq!(store.sync_logged_below(Lsn(100)).unwrap(), 0);
        let (_, recovered) = crashed(&fs);
        assert_eq!(recovered.read(&rb).unwrap().as_deref(), Some(&b[..]));
        assert_eq!(recovered.read(&ra).unwrap(), None);
        // A name the process did not write is written again by a put (a crash may have torn it).
        recovered.put_all(&[(rb, b.clone())]).unwrap();
        assert_eq!(recovered.read(&rb).unwrap().as_deref(), Some(&b[..]));
        // Bytes that are not the blob's are refused on every path.
        assert!(store.write_logged(&[(ra, b.clone())], Lsn(1)).is_err());
        assert!(store.restore_logged(&[(ra, b.clone())], Lsn(1)).is_err());
    }
}
