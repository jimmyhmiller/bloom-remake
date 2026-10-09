#![deny(unsafe_op_in_unsafe_fn)]
//! Durable opaque bytes, deterministic filesystem simulation, WAL and checkpoints (M2.6).
// FEATURE: DIST-020
mod blob;
mod ckpt;
mod clients;
mod kvfs;
pub mod lsm;
pub mod conformance;
pub mod crash;
mod meta;
mod simfs;
mod vfs;
mod wal;
pub use blob::*;
pub use ckpt::*;
pub use clients::*;
pub use kvfs::*;
pub use meta::*;
pub use simfs::*;
pub use vfs::*;
pub use wal::*;

/// Errors from storage operations; corruption always identifies its location.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt storage at {path}:{offset}: {reason}")]
    Corruption {
        path: std::path::PathBuf,
        offset: u64,
        reason: String,
    },
    #[error("WAL writer is poisoned")]
    Poisoned,
    #[error("storage lock {path} held by pid {pid}")]
    Locked { path: std::path::PathBuf, pid: String },
    #[error("invalid storage operation: {0}")]
    Invalid(String),
    #[error(transparent)]
    Unimplemented(#[from] blossom_base::Unimplemented),
    #[error(transparent)]
    Internal(#[from] blossom_base::InternalError),
}

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}
pub(crate) fn read_all(file: &dyn VfsFile) -> Result<Vec<u8>, StoreError> {
    let len = usize::try_from(file.len()?).map_err(|_| invalid("file too large"))?;
    let mut bytes = vec![0; len];
    let mut pos = 0;
    while pos < len {
        let n = file.pread(pos as u64, bytes.get_mut(pos..).ok_or_else(|| invalid("read offset"))?)?;
        if n == 0 {
            return Err(invalid("file shortened during read"));
        }
        pos = pos.checked_add(n).ok_or_else(|| invalid("read overflow"))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "blossom-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn sim() -> (Arc<SimFs>, PathBuf) {
        let fs = Arc::new(SimFs::default());
        let root = PathBuf::from("/store");
        fs.create_dir_all(&root).unwrap();
        fs.sync_dir(Path::new("/")).unwrap();
        (fs, root)
    }
    fn header() -> SegmentHeader {
        SegmentHeader {
            format: 1,
            store_uuid: [7; 16],
            segment_seq: 1,
            restarts: 1,
            boot_nonce: 3,
            lsn_base: Lsn(0),
            catalog: b"opaque catalog".to_vec(),
        }
    }
    fn rec(batch: u64, tick: u64, payload: &[u8]) -> WalRecordBuf {
        WalRecordBuf {
            batch,
            tick,
            now: tick as i64,
            kind: 1,
            payload: payload.to_vec(),
        }
    }
    fn real() -> (Arc<RealFs>, Temp) {
        (Arc::new(RealFs), Temp::new())
    }
    #[test]
    fn vfs_suite_realfs() {
        let (fs, t) = real();
        conformance::vfs_suite(&*fs, &t.0).unwrap();
    }
    #[test]
    fn vfs_suite_simfs() {
        let (fs, dir) = sim();
        conformance::vfs_suite(&*fs, &dir).unwrap();
        let mut fs2 = fs.fork().unwrap();
        fs2.crash(&mut |_| WriteFate::Lost).unwrap();
        assert!(fs2.list(&dir).unwrap().is_empty());
    }
    #[test]
    fn wal_suite_file_realfs() {
        let (fs, t) = real();
        let dir = t.0.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        conformance::wal_suite(&mut wal).unwrap();
        let scan = WalScan::scan(&*fs, &dir, [7; 16], false).unwrap();
        assert_eq!(scan.records().count(), 2);
    }
    #[test]
    fn wal_suite_file_simfs() {
        let (fs, root) = sim();
        let dir = root.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        conformance::wal_suite(&mut wal).unwrap();
        let mut fs2 = fs.fork().unwrap();
        fs2.crash(&mut |_| WriteFate::Lost).unwrap();
        assert_eq!(WalScan::scan(&fs2, &dir, [7; 16], false).unwrap().records().count(), 2);
    }
    #[test]
    fn wal_suite_mem() {
        conformance::wal_suite(&mut MemDurability::default()).unwrap();
    }
    #[test]
    fn torn_tail_truncated() {
        let (fs, root) = sim();
        let dir = root.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"stable")).unwrap();
        wal.sync().unwrap();
        wal.append(&rec(2, 2, &vec![4; 600])).unwrap();
        let mut fs2 = fs.fork().unwrap();
        fs2.crash(&mut |w| {
            if w.bytes.len() > 512 {
                WriteFate::Torn { sectors: 1 }
            } else {
                WriteFate::Lost
            }
        })
        .unwrap();
        let scan = WalScan::scan(&fs2, &dir, [7; 16], true).unwrap();
        assert_eq!(scan.records().count(), 1);
        assert_eq!(
            fs2.open(&dir.join("00000000000000000001.seg"), OpenOpts::default())
                .unwrap()
                .len()
                .unwrap(),
            scan.end.0
        );
    }
    #[test]
    fn corruption_refused_with_offset() {
        let (fs, root) = sim();
        let dir = root.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"a")).unwrap();
        wal.sync().unwrap();
        wal.append(&rec(2, 2, b"b")).unwrap();
        wal.sync().unwrap();
        let path = dir.join("00000000000000000001.seg");
        let header_len = super::wal::test_header_len(&header());
        fs.corrupt(&path, header_len + 10).unwrap();
        match WalScan::scan(&*fs, &dir, [7; 16], true) {
            Err(StoreError::Corruption { path: p, offset, .. }) => {
                assert_eq!(p, path);
                assert_eq!(offset, header_len as u64);
            }
            r => panic!("unexpected: {r:?}"),
        }
    }
    #[test]
    fn corruption_of_last_synced_data_refused() {
        let (fs, root) = sim();
        let dir = root.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"acknowledged")).unwrap();
        wal.sync().unwrap();
        let path = dir.join("00000000000000000001.seg");
        fs.corrupt(&path, super::wal::test_header_len(&header()) + 41).unwrap();
        assert!(matches!(
            WalScan::scan(&*fs, &dir, [7; 16], true),
            Err(StoreError::Corruption { .. })
        ));
    }
    #[test]
    fn synced_marker_and_receipt_media_fault_refused() {
        let (fs, root) = sim();
        let dir = root.join("wal");
        let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"data")).unwrap();
        wal.sync().unwrap();
        // Batch 1's marker leads batch 2, durable with its sync.
        wal.append(&rec(2, 2, b"next")).unwrap();
        wal.sync().unwrap();
        let segment = dir.join("00000000000000000001.seg");
        let receipt = dir.join("00000000000000000001.ack");
        let marker_offset = super::wal::test_header_len(&header()) + 41 + 4;
        let marker_fault = fs.fork().unwrap();
        marker_fault.corrupt(&segment, marker_offset + 10).unwrap();
        assert!(matches!(
            WalScan::scan(&marker_fault, &dir, [7; 16], true),
            Err(StoreError::Corruption { .. })
        ));
        let receipt_fault = fs.fork().unwrap();
        receipt_fault.corrupt(&receipt, 40).unwrap();
        assert!(matches!(
            WalScan::scan(&receipt_fault, &dir, [7; 16], true),
            Err(StoreError::Corruption { .. })
        ));
    }
    #[test]
    fn failed_sync_poisons() {
        let (fs, root) = sim();
        let mut wal = FileWal::create(fs.clone(), &root.join("wal"), header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"a")).unwrap();
        fs.inject(FsFault::SyncEio).unwrap();
        assert!(wal.sync().is_err());
        assert!(matches!(wal.sync(), Err(StoreError::Poisoned)));
        assert!(matches!(wal.append(&rec(2, 2, b"b")), Err(StoreError::Poisoned)));
    }
    #[test]
    fn checkpoint_fsync_order() {
        let (fs, root) = sim();
        let mut wal = FileWal::create(fs.clone(), &root.join("wal"), header(), Lsn(0)).unwrap();
        wal.append(&rec(1, 1, b"a")).unwrap();
        let synced = wal.sync().unwrap().synced_tick().unwrap();
        let mut ckpt = FileCheckpoints::new(fs.clone(), &root).unwrap();
        let mut snap = DurableSnapshot::default();
        snap.relations.insert(3, b"data".to_vec());
        let id = ckpt.write(snap.clone(), synced).unwrap();
        ckpt.install(id).unwrap();
        let trace = fs.trace().unwrap().join("\n");
        let rel = trace.find("sync_data /store/ckpt/1/rel-3.dat").unwrap();
        let dir1 = trace.get(rel..).unwrap().find("sync_dir /store/ckpt/1").unwrap() + rel;
        let manifest = trace.find("sync_data /store/ckpt/1/MANIFEST").unwrap();
        let dir2 = trace.get(manifest..).unwrap().find("sync_dir /store/ckpt/1").unwrap() + manifest;
        let current = trace.find("sync_data /store/CURRENT.tmp").unwrap();
        let rename = trace.find("rename /store/CURRENT.tmp /store/CURRENT").unwrap();
        let final_dir = trace.get(rename..).unwrap().find("sync_dir /store").unwrap() + rename;
        assert!(
            rel < dir1
                && dir1 < manifest
                && manifest < dir2
                && dir2 < current
                && current < rename
                && rename < final_dir
        );
        let mut fs2 = fs.fork().unwrap();
        fs2.crash(&mut |_| WriteFate::Lost).unwrap();
        assert_eq!(
            FileCheckpoints::new(Arc::new(fs2), &root).unwrap().read(id).unwrap(),
            snap
        );
    }
    fn identity() -> StoreIdentity {
        StoreIdentity {
            store_uuid: [7; 16],
            deployment_id: [1; 16],
            program_id: [2; 16],
            node_name: "node".into(),
            principal: "principal".into(),
            format: 1,
            directory_digest: [3; 16],
        }
    }
    #[test]
    fn meta_atomic_write() {
        let (fs, root) = sim();
        let meta = MetaStore::new(fs.clone(), &root);
        let record = MetaRecord {
            identity: identity(),
            node_id_map: vec![1],
            restarts: 0,
            reserved_tick: 65536,
            last_now: 7,
            understood_version: 1,
            poison_deny_list: vec![],
            clean_shutdown: false,
            certification: Certification::default(),
        };
        meta.write(&record).unwrap();
        let mut fs2 = fs.fork().unwrap();
        fs2.crash(&mut |_| WriteFate::Lost).unwrap();
        assert_eq!(MetaStore::new(Arc::new(fs2), &root).read().unwrap(), record);
    }
    #[test]
    fn lock_refuses_second_holder() {
        let (fs, root) = sim();
        let a = StoreLock::acquire(&*fs, &root).unwrap();
        assert!(matches!(
            StoreLock::acquire(&*fs, &root),
            Err(StoreError::Locked { .. })
        ));
        drop(a);
        StoreLock::acquire(&*fs, &root).unwrap();
    }
    #[test]
    fn crashcheck_store_workload() {
        let checked = crash::enumerate_crash_points(
            |fs| {
                let root = PathBuf::from("/store");
                fs.create_dir_all(&root)?;
                let dir = root.join("wal");
                let mut workload = crash::CrashWorkload::new(dir.clone(), [7; 16]);
                let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0))?;
                let first = rec(1, 1, b"data");
                wal.append(&first)?;
                workload.appended(first);
                let synced = wal.sync()?;
                workload.acknowledged(&fs, synced)?;
                let mut checkpoints = FileCheckpoints::new(fs.clone(), &root)?;
                let mut snapshot = DurableSnapshot::default();
                snapshot.relations.insert(3, b"checkpoint".to_vec());
                let id = checkpoints.write(snapshot.clone(), synced.synced_tick().unwrap())?;
                checkpoints.install(id)?;
                workload.checkpoint = Some((root, snapshot));
                Ok(workload)
            },
            10000,
        )
        .unwrap();
        assert!(
            checked > 5000,
            "every mutating Vfs call and synced byte must be explored"
        );
    }
    #[test]
    fn simfs_captures_each_mutation_cut() {
        let fs = SimFs::default();
        fs.enable_crash_recording().unwrap();
        fs.create_dir_all(Path::new("/store")).unwrap();
        let mut file = fs
            .open(
                Path::new("/store/a"),
                OpenOpts {
                    create_new: true,
                    ..OpenOpts::default()
                },
            )
            .unwrap();
        file.append(b"data").unwrap();
        file.sync_data().unwrap();
        file.truncate(2).unwrap();
        fs.rename(Path::new("/store/a"), Path::new("/store/b")).unwrap();
        fs.sync_dir(Path::new("/store")).unwrap();
        fs.remove(Path::new("/store/b")).unwrap();
        assert_eq!(fs.cut_count().unwrap(), fs.trace().unwrap().len());
        assert_eq!(fs.recorded_cuts().unwrap().len(), fs.cut_count().unwrap());
    }
    #[test]
    fn crash_fate_count_mismatch_is_error() {
        let (fs, root) = sim();
        let mut file = fs
            .open(
                &root.join("pending"),
                OpenOpts {
                    create_new: true,
                    ..OpenOpts::default()
                },
            )
            .unwrap();
        file.append(b"unsynced").unwrap();
        let mut copy = fs.fork().unwrap();
        assert!(copy.crash_with_fates(&[]).is_err());
    }
    #[test]
    fn wal_recovery_fuzz_mirror() {
        for seed in 0..128u64 {
            let (fs, root) = sim();
            let dir = root.join("wal");
            let mut wal = FileWal::create(fs.clone(), &dir, header(), Lsn(0)).unwrap();
            let payload = (0..(seed as usize % 73))
                .map(|i| (seed as u8).wrapping_add(i as u8))
                .collect::<Vec<_>>();
            wal.append(&rec(1, 1, &payload)).unwrap();
            wal.sync().unwrap();
            let scan = WalScan::scan(&*fs, &dir, [7; 16], false).unwrap();
            assert_eq!(scan.records().next().unwrap().1.payload, payload);
            let path = dir.join("00000000000000000001.seg");
            let offset = (seed as usize) % (fs.open(&path, OpenOpts::default()).unwrap().len().unwrap() as usize);
            fs.corrupt(&path, offset).unwrap();
            let _ = WalScan::scan(&*fs, &dir, [7; 16], false);
        }
    }
}
