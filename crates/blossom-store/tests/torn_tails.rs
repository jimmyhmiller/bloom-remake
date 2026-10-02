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
fn a_batch_number_gap_is_refused() {
    // Recovery reads a batch's end from the next batch's number (its marker leads that batch), in both
    // certifications.
    for certification in [Certification::Strict, Certification::Crc] {
        let (fs, dir) = sim();
        let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
            .unwrap()
            .certified(certification);
        wal.append(&rec(1, 1, b"acked")).unwrap();
        wal.sync().unwrap();
        assert!(wal.append(&rec(3, 2, b"gap")).is_err(), "{certification:?}");
    }
}

#[test]
fn consecutive_batches_torn_out_of_order_are_a_torn_tail() {
    // After batch 1's sync, batch 2 is three unsynced writes: batch 1's leading marker and two records. Whichever of
    // them survive a crash, batch 1 (acknowledged) is recovered and the rest is a torn tail.
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0)).unwrap();
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(2, 2, &[2u8; 700])).unwrap();
    wal.append(&rec(2, 3, &[3u8; 700])).unwrap();
    assert_eq!(fs.unsynced_writes().unwrap().len(), 3);
    for mask in 0..8u32 {
        let fates: Vec<WriteFate> = (0..3)
            .map(|i| {
                if mask & (1 << i) != 0 {
                    WriteFate::Survive
                } else {
                    WriteFate::Lost
                }
            })
            .collect();
        let mut image = fs.fork().unwrap();
        image.crash_with_fates(&fates).unwrap();
        let recovered = WalScan::scan(&image, &dir, [7; 16], true)
            .unwrap_or_else(|e| panic!("{fates:?}: {e}"))
            .records()
            .count();
        assert!(recovered >= 1, "{fates:?}: the acknowledged batch was lost");
    }
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
fn a_group_commit_syncs_once_with_crc_certification_and_three_times_strict() {
    // Strict: the batch, then its receipt (the temporary file and the directory); its marker leads the next batch.
    for (certification, want) in [(Certification::Crc, 1), (Certification::Strict, 3)] {
        let (fs, dir) = sim();
        let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
            .unwrap()
            .certified(certification);
        // The segment's first commit (crc writes the segment's receipt then, once).
        wal.append(&rec(1, 1, b"a")).unwrap();
        wal.sync().unwrap();
        wal.append(&rec(2, 2, b"b")).unwrap();
        wal.append(&rec(2, 3, b"c")).unwrap();
        let before = syncs(&fs);
        wal.sync().unwrap();
        assert_eq!(syncs(&fs) - before, want, "{certification:?}");
    }
}

#[test]
fn with_crc_certification_damage_in_an_earlier_synced_batch_is_corruption() {
    // Batch 1's marker leads batch 2, and is durable with batch 2's sync: damage to batch 1 is then told from a torn
    // tail even though batch 1 was the last before it.
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, &[1u8; 300])).unwrap();
    wal.sync().unwrap();
    wal.append(&rec(2, 2, &[2u8; 300])).unwrap();
    wal.sync().unwrap();
    drop(wal);
    let first = WalScan::scan(&*fs, &dir, [7; 16], false)
        .unwrap()
        .records()
        .next()
        .unwrap()
        .0
        .0 as usize;
    fs.corrupt(&dir.join(format!("{:020}.seg", 0)), first + 100).unwrap();
    assert!(WalScan::scan(&*fs, &dir, [7; 16], false).is_err());
}

#[test]
fn with_crc_certification_a_damaged_header_of_an_acknowledged_segment_is_corruption() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    drop(wal);
    fs.corrupt(&dir.join(format!("{:020}.seg", 0)), 6).unwrap();
    assert!(WalScan::scan(&*fs, &dir, [7; 16], true).is_err());
}

#[test]
fn with_crc_certification_batches_count_up_by_one() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, b"a")).unwrap();
    wal.sync().unwrap();
    assert!(wal.append(&rec(3, 2, b"b")).is_err());
}

#[test]
fn with_crc_certification_a_torn_start_of_the_next_batch_is_a_torn_tail() {
    let (fs, dir) = sim();
    let mut wal = FileWal::create(fs.clone(), &dir, header(0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc);
    wal.append(&rec(1, 1, b"acked")).unwrap();
    wal.sync().unwrap();
    // Batch 2 is batch 1's marker, then two records, all unsynced. Losing the marker (followed by the batch after the
    // last whole record's) or the first record (after the marker) is a torn tail.
    wal.append(&rec(2, 2, &[2u8; 700])).unwrap();
    wal.append(&rec(2, 3, &[3u8; 700])).unwrap();
    for fates in [
        [WriteFate::Lost, WriteFate::Survive, WriteFate::Survive],
        [WriteFate::Survive, WriteFate::Lost, WriteFate::Survive],
    ] {
        let mut image = fs.fork().unwrap();
        image.crash_with_fates(&fates).unwrap();
        let scan = WalScan::scan(&image, &dir, [7; 16], true).unwrap();
        assert_eq!(scan.records().count(), 1, "{fates:?}");
        assert!(scan.records().all(|(_, r)| r.tick == 1));
    }
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
    let first = WalScan::scan(&*fs, &dir, [7; 16], false)
        .unwrap()
        .records()
        .next()
        .unwrap()
        .0
        .0 as usize;
    fs.corrupt(&dir.join(format!("{:020}.seg", 0)), first + 100).unwrap();
    assert!(WalScan::scan(&*fs, &dir, [7; 16], false).is_err());
}
