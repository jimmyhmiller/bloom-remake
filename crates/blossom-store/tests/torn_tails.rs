//! Torn tails versus corruption (regressions from the S3 review): the unsynced last batch may be lost or torn in any
//! order, including the first batch of a segment and a batch whose number skips ahead; a newest segment whose header
//! never became durable is an aborted creation; damage inside synced data is still refused.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_store::*;

#[cfg(test)]
fn sim() -> (Arc<SimFs>, PathBuf) {
    let fs = Arc::new(SimFs::default());
    let root = PathBuf::from("/store");
    fs.create_dir_all(&root).unwrap();
    fs.sync_dir(Path::new("/")).unwrap();
    (fs, root.join("wal"))
}

#[cfg(test)]
fn header(seq: u64) -> SegmentHeader {
    SegmentHeader {
        format: 1,
        store_uuid: [7; 16],
        segment_seq: seq,
        restarts: 1,
        boot_nonce: 3,
        lsn_base: Lsn(0),
        catalog: b"opaque catalog".to_vec(),
    }
}

#[cfg(test)]
fn rec(batch: u64, tick: u64, payload: &[u8]) -> WalRecordBuf {
    WalRecordBuf {
        batch,
        tick,
        now: tick as i64,
        kind: 1,
        payload: payload.to_vec(),
    }
}

/// Loses the first of the two unsynced writes and keeps the second, then scans; the number of records recovered.
#[cfg(test)]
fn lose_first_keep_second(fs: &SimFs, dir: &Path) -> usize {
    let mut image = fs.fork().unwrap();
    assert_eq!(image.unsynced_writes().unwrap().len(), 2);
    image.crash_with_fates(&[WriteFate::Lost, WriteFate::Survive]).unwrap();
    WalScan::scan(&image, dir, [7; 16], true).unwrap().records().count()
}

#[test]
fn the_first_batch_of_a_segment_torn_out_of_order_is_a_torn_tail() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, &[1u8; 700])).unwrap();
    wal.append(&rec(1, 2, &[2u8; 700])).unwrap();
    assert_eq!(lose_first_keep_second(&fs, &dir), 0);
}

#[test]
fn a_batch_number_gap_is_still_a_torn_tail() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(3, 2, &[2u8; 700])).unwrap();
    wal.append(&rec(3, 3, &[3u8; 700])).unwrap();
    assert_eq!(lose_first_keep_second(&fs, &dir), 1);
}

#[test]
fn consecutive_batches_torn_out_of_order_are_a_torn_tail() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(2, 2, &[2u8; 700])).unwrap();
    wal.append(&rec(2, 3, &[3u8; 700])).unwrap();
    assert_eq!(lose_first_keep_second(&fs, &dir), 1);
}

#[test]
fn an_empty_newest_segment_is_an_aborted_creation() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    drop(wal);
    // What a real filesystem can leave after a power loss inside `create`: the entry without the header.
    let path = dir.join(format!("{:020}.seg", 1));
    drop(
        fs.open(
            &path,
            OpenOpts {
                create_new: true,
                ..OpenOpts::default()
            },
        )
        .unwrap(),
    );
    fs.sync_dir(&dir).unwrap();
    let scan = WalScan::scan(&*fs, &dir, [7; 16], true).unwrap();
    assert_eq!(scan.records().count(), 1);
    assert!(
        fs.list(&dir).unwrap().iter().all(|p| *p != path),
        "the aborted segment is removed"
    );
}

#[test]
fn damage_in_a_synced_batch_before_a_later_batch_is_corruption() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, &[1u8; 300])).unwrap();
    wal.append(&rec(1, 2, &[2u8; 300])).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(2, 3, &[3u8; 300])).unwrap();
    wal.sync().unwrap();
    drop(wal);
    let seg = dir.join(format!("{:020}.seg", 0));
    // Flip a byte inside the first record's payload (well past the header).
    let header_len = {
        let scan = WalScan::scan(&*fs, &dir, [7; 16], false).unwrap();
        scan.records().next().unwrap().0.0 as usize
    };
    fs.corrupt(&seg, header_len + 100).unwrap();
    // Remove the receipt, so only the batch structure can tell the damage is not a torn tail.
    fs.remove(&dir.join(format!("{:020}.ack", 0))).unwrap();
    assert!(WalScan::scan(&*fs, &dir, [7; 16], false).is_err());
}

#[test]
fn a_damaged_header_of_an_acknowledged_newest_segment_is_corruption() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    drop(wal);
    fs.corrupt(&dir.join(format!("{:020}.seg", 0)), 6).unwrap();
    assert!(WalScan::scan(&*fs, &dir, [7; 16], true).is_err());
}

/// The syncs (of data and of directories) in the filesystem's trace so far.
#[cfg(test)]
fn syncs(fs: &SimFs) -> usize {
    fs.trace().unwrap().iter().filter(|l| l.starts_with("sync_")).count()
}

#[test]
fn a_group_commit_syncs_once_with_crc_certification_and_four_times_strict() {
    for (certification, want) in [(Certification::Crc, 1), (Certification::Strict, 4)] {
        let (fs, dir) = sim();
        let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
            .unwrap()
            .certified(certification);
        wal.append(&rec(1, 1, b"a")).unwrap();
        wal.append(&rec(1, 2, b"b")).unwrap();
        let before = syncs(&fs);
        wal.sync().unwrap();
        assert_eq!(syncs(&fs) - before, want, "{certification:?}");
    }
}

#[test]
fn with_crc_certification_a_torn_start_of_the_next_batch_is_a_torn_tail() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    // Batch 2's first record is lost and its second survives: without markers, the damage is followed by the batch
    // after the last whole record's.
    wal.append(&rec(2, 2, &[2u8; 700])).unwrap();
    wal.append(&rec(2, 3, &[3u8; 700])).unwrap();
    let mut image = fs.fork().unwrap();
    image.crash_with_fates(&[WriteFate::Lost, WriteFate::Survive]).unwrap();
    let scan = WalScan::scan_certified(&image, &dir, [7; 16], true, Certification::Crc).unwrap();
    assert_eq!(scan.records().count(), 1);
    assert!(scan.records().all(|(_, r)| r.tick == 1));
}

#[test]
fn with_crc_certification_damage_before_two_later_batches_is_corruption() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, &[1u8; 300])).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(2, 2, &[2u8; 300])).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(3, 3, &[3u8; 300])).unwrap();
    wal.sync().unwrap();
    drop(wal);
    let first = WalScan::scan_certified(&*fs, &dir, [7; 16], false, Certification::Crc)
        .unwrap()
        .records()
        .next()
        .unwrap()
        .0
        .0 as usize;
    fs.corrupt(&dir.join(format!("{:020}.seg", 0)), first + 100).unwrap();
    assert!(WalScan::scan_certified(&*fs, &dir, [7; 16], false, Certification::Crc).is_err());
}
