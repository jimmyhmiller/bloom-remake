//! S24: the WAL truncates behind a database flush (docs/design/DATABASE.md §7). Only older segments whose every
//! record's tick the flush covers go, in order; the segment being written never does; and the proof is a `Flushed`
//! the tree returned, which nothing else can make.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_store::lsm::{Flushed, Lsm, LsmOptions, Op};
use blossom_store::*;

#[cfg(test)]
fn header(seq: u64, base: Lsn) -> SegmentHeader {
    SegmentHeader {
        format: 1,
        store_uuid: [7; 16],
        segment_seq: seq,
        restarts: seq + 1,
        boot_nonce: 3,
        lsn_base: base,
        catalog: b"opaque".to_vec(),
    }
}

#[cfg(test)]
fn rec(batch: u64, tick: u64) -> WalRecordBuf {
    WalRecordBuf {
        batch,
        tick,
        now: tick as i64,
        kind: 1,
        payload: vec![tick as u8; 64],
    }
}

/// A tree flushed through `version`: the proof a truncation needs.
#[cfg(test)]
fn flushed(fs: &Arc<SimFs>, version: u64) -> Flushed {
    let dir = PathBuf::from(format!("/db{version}"));
    let lsm = Lsm::open(fs.clone() as Arc<dyn Vfs>, &dir, LsmOptions::default()).unwrap();
    lsm.apply(version, 0, vec![(b"k".to_vec(), Op::Put)]).unwrap();
    lsm.flush().unwrap()
}

#[test]
fn a_flush_truncates_the_older_segments_it_covers() {
    let fs = Arc::new(SimFs::default());
    let dir = PathBuf::from("/store/wal");
    fs.create_dir_all(Path::new("/store")).unwrap();
    // Three incarnations: ticks 1–3, 4–6, and 7 in the segment being written.
    let mut base = Lsn(0);
    for (seq, ticks) in [(0u64, 1..=3u64), (1, 4..=6)] {
        let mut wal = FileWal::create(fs.clone(), &dir, header(seq, base), base).unwrap();
        for t in ticks {
            wal.append(&rec(t, t)).unwrap();
            wal.sync().unwrap();
        }
        drop(wal);
        base = WalScan::scan(&*fs, &dir, [7; 16], false).unwrap().end;
    }
    let mut wal = FileWal::create(fs.clone(), &dir, header(2, base), base).unwrap();
    wal.append(&rec(7, 7)).unwrap();
    wal.sync().unwrap();
    let segments = || WalScan::scan(&*fs, &dir, [7; 16], false).unwrap().segments.len();
    // Through tick 2: not even the first segment.
    assert!(wal.truncation(&flushed(&fs, 2)).unwrap().is_none());
    // Through tick 5: the first segment only (the second holds tick 6).
    let t5 = wal.truncation(&flushed(&fs, 5)).unwrap().unwrap();
    wal.truncate_through(t5).unwrap();
    assert_eq!(segments(), 2);
    // Through tick 9: the second too, never the one being written.
    let t9 = wal.truncation(&flushed(&fs, 9)).unwrap().unwrap();
    wal.truncate_through(t9).unwrap();
    assert_eq!(segments(), 1);
    assert_eq!(
        WalScan::scan(&*fs, &dir, [7; 16], false)
            .unwrap()
            .records()
            .map(|(_, r)| r.tick)
            .collect::<Vec<_>>(),
        [7]
    );
}
