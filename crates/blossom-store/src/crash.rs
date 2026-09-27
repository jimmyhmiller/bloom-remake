//! Deterministic bounded exploration of all volatile file-write fates at scripted syscall cuts.
use crate::*;
/// One explicitly captured crash cut in a workload; captures actual filesystem state after a syscall.
pub struct CrashPoint {
    pub fs: SimFs,
    pub acknowledged: Vec<WalRecordBuf>,
    pub wal_dir: std::path::PathBuf,
    pub uuid: [u8; 16],
}
/// Explore every lost/surviving/sector-torn write combination at each workload cut.
/// The workload must capture cuts after each syscall it wishes to explore; `max_images` is a hard bound,
/// never a silent sampling limit. Returns the number of checked images.
pub fn enumerate_crash_points(
    workload: impl FnOnce() -> Result<Vec<CrashPoint>, StoreError>,
    max_images: usize,
) -> Result<usize, StoreError> {
    let mut images = 0usize;
    for point in workload()? {
        let mut probe = point.fs.fork()?;
        let mut writes = Vec::new();
        probe.crash(&mut |w| {
            writes.push(w.clone());
            WriteFate::Lost
        })?;
        let choices = writes
            .iter()
            .map(|w| {
                let mut c = vec![WriteFate::Lost, WriteFate::Survive];
                let end = w.offset.saturating_add(w.bytes.len() as u64);
                let crossed = end.saturating_sub(1) / 512 - w.offset / 512;
                for sectors in 1..=crossed as usize {
                    c.push(WriteFate::Torn { sectors });
                }
                c
            })
            .collect::<Vec<_>>();
        let count = choices.iter().try_fold(1usize, |n, c| {
            n.checked_mul(c.len())
                .ok_or_else(|| invalid("crash combinations overflow"))
        })?;
        if images.checked_add(count).is_none_or(|n| n > max_images) {
            return Err(invalid("crash exploration exceeds explicit bound"));
        }
        for mut index in 0..count {
            let mut fs = point.fs.fork()?;
            let mut cursor = 0;
            fs.crash(&mut |_| {
                let fate = choices
                    .get(cursor)
                    .and_then(|c| c.get(index % c.len()))
                    .copied()
                    .unwrap_or(WriteFate::Lost);
                if let Some(c) = choices.get(cursor) {
                    index /= c.len();
                }
                cursor += 1;
                fate
            })?;
            let scan = WalScan::scan(&fs, &point.wal_dir, point.uuid, true)?;
            let recovered = scan.records().map(|(_, r)| r).collect::<Vec<_>>();
            if recovered.len() < point.acknowledged.len()
                || !recovered.iter().zip(&point.acknowledged).all(|(a, b)| *a == b)
            {
                return Err(invalid("crash lost an acknowledged WAL prefix"));
            }
            images += 1;
        }
    }
    Ok(images)
}
