//! Crash points of a WAL with one sync per group commit (`Certification::Crc`, from the S5 review): at every durable
//! syscall of multi-record group commits, and of a segment roll in the middle of a batch, under every fate of the
//! unsynced writes (lost, kept, torn at each sector), recovery keeps every acknowledged record, recovers a prefix of
//! what was appended, and a new incarnation's segment scans on top of it.

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
fn header(seq: u64, base: u64) -> SegmentHeader {
    SegmentHeader {
        format: 1,
        store_uuid: [7; 16],
        segment_seq: seq,
        restarts: 1,
        boot_nonce: 3,
        lsn_base: Lsn(base),
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

#[cfg(test)]
fn crc_wal(fs: &Arc<SimFs>, dir: &Path) -> FileWal {
    FileWal::create(fs.clone(), dir, header(0, 0), Lsn(0))
        .unwrap()
        .certified(Certification::Crc)
}

/// Group commits of one to three records, crashed at every cut under every combination of fates.
#[test]
fn crc_multi_record_group_commits_keep_every_acknowledged_record_at_every_crash_point() {
    let (fs, dir) = sim();
    fs.enable_crash_recording().unwrap();
    let mut appended = Vec::new();
    let mut acks: Vec<(usize, usize)> = Vec::new();
    let mut wal = crc_wal(&fs, &dir);
    let batches: &[&[u64]] = &[&[1, 2, 3], &[4], &[5, 6], &[7, 8, 9]];
    let mut tick = 0;
    for (i, b) in batches.iter().enumerate() {
        for &p in *b {
            tick += 1;
            let r = rec(i as u64 + 1, tick, &vec![p as u8; 200 + 150 * p as usize]);
            wal.append(&r).unwrap();
            appended.push(r);
        }
        if i + 1 < batches.len() {
            wal.sync().unwrap();
            acks.push((fs.cut_count().unwrap(), appended.len()));
        }
    }
    drop(wal);
    let cuts = fs.recorded_cuts().unwrap();
    let mut images = 0;
    for (ci, cut) in cuts.into_iter().enumerate() {
        let acked = acks.iter().rev().find(|(at, _)| *at <= ci + 1).map_or(0, |a| a.1);
        let choices: Vec<Vec<WriteFate>> = cut
            .unsynced_writes()
            .unwrap()
            .iter()
            .map(|w| {
                let mut f = vec![WriteFate::Lost, WriteFate::Survive];
                if !w.bytes.is_empty() {
                    let end = w.offset + w.bytes.len() as u64;
                    let crossed = (end - 1) / 512 - w.offset / 512;
                    for s in 1..=crossed as usize {
                        f.push(WriteFate::Torn { sectors: s });
                    }
                }
                f
            })
            .collect();
        let count: usize = choices.iter().map(|c| c.len()).product();
        for mut combo in 0..count {
            let mut fates = Vec::new();
            for c in &choices {
                fates.push(c[combo % c.len()]);
                combo /= c.len();
            }
            let mut image = cut.fork().unwrap();
            image.crash_with_fates(&fates).unwrap();
            let scan = match WalScan::scan_certified(&image, &dir, [7; 16], true, Certification::Crc) {
                Ok(s) => s,
                Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    assert_eq!(acked, 0);
                    continue;
                }
                Err(e) => panic!("cut {ci} fates {fates:?}: recovery refused: {e}"),
            };
            let got: Vec<WalRecordBuf> = scan.records().map(|(_, r)| r.clone()).collect();
            assert!(appended.starts_with(&got), "cut {ci}: non-prefix");
            assert!(got.len() >= acked, "cut {ci} fates {fates:?}: lost acked ({} < {acked})", got.len());
            let again = WalScan::scan_certified(&image, &dir, [7; 16], true, Certification::Crc).unwrap();
            assert_eq!(again.records().count(), got.len());
            // New incarnation on top, as recovery does.
            let seq = scan.segments.last().map_or(0, |s| s.header.segment_seq + 1);
            let base = scan.segments.last().map_or(Lsn(0), |s| s.end);
            let img = Arc::new(image);
            let mut w2 = FileWal::create(img.clone(), &dir, header(seq, base.0), base)
                .unwrap()
                .certified(Certification::Crc);
            w2.append(&rec(1, 1000, b"next")).unwrap();
            w2.sync().unwrap();
            let third = WalScan::scan_certified(&*img, &dir, [7; 16], true, Certification::Crc)
                .unwrap_or_else(|e| panic!("cut {ci}: rescan after new incarnation refused: {e}"));
            assert_eq!(third.records().count(), got.len() + 1);
            images += 1;
        }
    }
    assert!(images > 100, "only {images} crash images");
}

/// A segment roll in the middle of a crc batch (the runtime committer appends a whole group, and `append` rolls
/// when the segment would exceed 64 MiB), crashed at every cut with coarse fates.
#[test]
fn crc_segment_roll_mid_batch_keeps_every_acknowledged_record() {
    let (fs, dir) = sim();
    fs.enable_crash_recording().unwrap();
    let big = vec![9u8; 24 * 1024 * 1024];
    let mut appended = Vec::new();
    let mut acks: Vec<(usize, usize)> = Vec::new();
    let mut wal = crc_wal(&fs, &dir);
    let plan: &[(u64, bool)] = &[(1, false), (1, true), (2, false), (2, true), (3, false), (3, false)];
    for (i, (batch, sync)) in plan.iter().enumerate() {
        let r = rec(*batch, i as u64 + 1, &big);
        wal.append(&r).unwrap();
        appended.push(r);
        if *sync {
            wal.sync().unwrap();
            acks.push((fs.cut_count().unwrap(), appended.len()));
        }
    }
    drop(wal);
    let segs = fs.list(&dir).unwrap().iter().filter(|p| p.extension().is_some_and(|e| e == "seg")).count();
    assert!(segs >= 2, "no roll happened ({segs} segments)");
    let cuts = fs.recorded_cuts().unwrap();
    let mut images = 0;
    for (ci, cut) in cuts.into_iter().enumerate() {
        let acked = acks.iter().rev().find(|(at, _)| *at <= ci + 1).map_or(0, |a| a.1);
        let n = cut.unsynced_writes().unwrap().len();
        let pats: [fn(usize) -> WriteFate; 5] = [
            |_| WriteFate::Lost,
            |_| WriteFate::Survive,
            |_| WriteFate::Torn { sectors: 1 },
            |i| if i % 2 == 0 { WriteFate::Lost } else { WriteFate::Survive },
            |i| if i % 2 == 0 { WriteFate::Survive } else { WriteFate::Torn { sectors: 3 } },
        ];
        for p in pats {
            let fates: Vec<WriteFate> = (0..n).map(p).collect();
            let mut image = cut.fork().unwrap();
            image.crash_with_fates(&fates).unwrap();
            let scan = match WalScan::scan_certified(&image, &dir, [7; 16], true, Certification::Crc) {
                Ok(s) => s,
                Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => panic!("cut {ci}: refused: {e}"),
            };
            let got: Vec<u64> = scan.records().map(|(_, r)| r.tick).collect();
            let want: Vec<u64> = appended.iter().map(|r| r.tick).collect();
            assert!(want.starts_with(&got), "cut {ci}: non-prefix {got:?}");
            assert!(got.len() >= acked, "cut {ci}: lost acked {got:?} < {acked}");
            images += 1;
        }
    }
    assert!(images > 20, "only {images} crash images");
}

