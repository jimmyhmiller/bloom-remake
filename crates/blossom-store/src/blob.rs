//! The blob store (FOREIGN-PROTOCOLS §5): a node's durable blobs, one file each under `<store>/blobs/`, named by the
//! blob's hash and length. A blob is made durable before the WAL record that references it syncs, so recovery never
//! finds a row whose blob is missing.
//!
//! A blob is written to a temporary file, synced, then renamed to its name; the directory is synced once for a batch
//! of blobs. A name therefore only ever holds complete, synced content, and reading one checks its hash anyway.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use blossom_value::BlobRef;

use crate::{OpenOpts, StoreError, Vfs, durable_dir, invalid, read_all};

/// The blob directory of a node directory.
pub fn blob_dir(dir: &Path) -> PathBuf {
    dir.join("blobs")
}

/// A node's durable blobs.
pub struct BlobStore {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
    /// Blobs this process made durable (their directory entry synced): writing them again is skipped.
    durable: Mutex<BTreeSet<BlobRef>>,
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
            durable: Mutex::new(BTreeSet::new()),
        })
    }

    fn path(&self, b: &BlobRef) -> PathBuf {
        self.dir.join(name(b))
    }

    /// Makes `blobs` durable: each is written (unless it already is) and synced, then the directory once.
    pub fn put_all(&self, blobs: &[(BlobRef, Arc<[u8]>)]) -> Result<(), StoreError> {
        if blobs.is_empty() {
            return Ok(());
        }
        let known = self
            .durable
            .lock()
            .map_err(|_| invalid("the blob store's lock is poisoned"))?
            .clone();
        let mut wrote = Vec::new();
        for (b, bytes) in blobs {
            if BlobRef::of(bytes) != *b {
                return Err(invalid(format!(
                    "the bytes given for blob {} are not its bytes",
                    b.hex()
                )));
            }
            if known.contains(b) {
                continue;
            }
            let path = self.path(b);
            // A name holds only synced content (it is renamed into place after its sync), but its entry may not be
            // durable yet: rewriting is not needed, the directory sync below makes it so.
            let exists = self.fs.open(&path, OpenOpts::default()).is_ok();
            if !exists {
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
                f.sync_data()?;
                self.fs.rename(&tmp, &path)?;
            }
            wrote.push(*b);
        }
        if wrote.is_empty() {
            return Ok(());
        }
        self.fs.sync_dir(&self.dir)?;
        self.durable
            .lock()
            .map_err(|_| invalid("the blob store's lock is poisoned"))?
            .extend(wrote);
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
        // The set must forget them first, or a `put_all` racing this would skip writing one a row needs again.
        {
            let mut durable = self
                .durable
                .lock()
                .map_err(|_| invalid("the blob store's lock is poisoned"))?;
            for b in gone {
                durable.remove(b);
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
}
