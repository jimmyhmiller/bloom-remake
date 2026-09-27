//! Reusable filesystem and WAL conformance suites; usable by future backends without testkit dependencies.
use crate::*;
use std::path::Path;
/// Validate append, positional reads, rename, truncation and durable namespace operations.
pub fn vfs_suite(fs: &dyn Vfs, dir: &Path) -> Result<(), StoreError> {
    fs.create_dir_all(dir)?;
    let a = dir.join("a");
    let b = dir.join("b");
    let mut f = fs.open(
        &a,
        OpenOpts {
            create_new: true,
            ..OpenOpts::default()
        },
    )?;
    f.append(b"abc")?;
    f.append(b"def")?;
    f.sync_data()?;
    fs.sync_dir(dir)?;
    let mut buf = [0; 3];
    if f.pread(2, &mut buf)? != 3 || buf != *b"cde" {
        return Err(invalid("positional read conformance"));
    }
    fs.rename(&a, &b)?;
    fs.sync_dir(dir)?;
    if crate::vfs::read_path(fs, &b)? != b"abcdef" {
        return Err(invalid("rename conformance"));
    }
    f.truncate(3)?;
    f.sync_data()?;
    if crate::vfs::read_path(fs, &b)? != b"abc" {
        return Err(invalid("truncate conformance"));
    }
    fs.remove(&b)?;
    fs.sync_dir(dir)?;
    Ok(())
}
/// Validate Invariant B and the synced tick watermark on a fresh WAL.
pub fn wal_suite(wal: &mut dyn WalWriter) -> Result<(), StoreError> {
    let first = WalRecordBuf {
        batch: 1,
        tick: 1,
        now: 1,
        kind: 0,
        payload: b"one".to_vec(),
    };
    wal.append(&first)?;
    let second = WalRecordBuf {
        batch: 2,
        tick: 2,
        now: 2,
        kind: 0,
        payload: b"two".to_vec(),
    };
    if wal.append(&second).is_ok() {
        return Err(invalid("Invariant B was not enforced"));
    }
    let a = wal.sync()?;
    if a.synced_tick().map(SyncedTick::tick) != Some(1) {
        return Err(invalid("sync watermark"));
    }
    let pos = wal.append(&second)?;
    let b = wal.sync()?;
    if b.synced_tick().map(SyncedTick::tick) != Some(2) || b.lsn() <= a.lsn() || pos < a.lsn() {
        return Err(invalid("monotone WAL conformance"));
    }
    Ok(())
}
