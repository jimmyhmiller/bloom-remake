//! Exhaustive crash cuts, write fates, and synced-byte media faults for scripted storage work.
use crate::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Script output: intended WAL records, acknowledgement frontiers, and an optional checkpoint.
pub struct CrashWorkload {
    /// WAL directory to recover after each cut.
    pub wal_dir: PathBuf,
    /// Expected store UUID for segment validation.
    pub uuid: [u8; 16],
    /// Every record successfully appended by the script, in log order.
    pub records: Vec<WalRecordBuf>,
    /// `(cut_count, acknowledged_record_count, synced_lsn)` recorded after each successful sync.
    pub acknowledgements: Vec<(usize, usize, Lsn)>,
    /// Checkpoint directory and expected relation bytes, if the script installs one.
    pub checkpoint: Option<(PathBuf, DurableSnapshot)>,
}
impl CrashWorkload {
    /// Construct an empty script result. The caller records each successful append and sync.
    pub fn new(wal_dir: PathBuf, uuid: [u8; 16]) -> Self {
        Self {
            wal_dir,
            uuid,
            records: Vec::new(),
            acknowledgements: Vec::new(),
            checkpoint: None,
        }
    }
    /// Record an appended record after `WalWriter::append` returns.
    pub fn appended(&mut self, record: WalRecordBuf) {
        self.records.push(record);
    }
    /// Record the acknowledgement frontier immediately after `WalWriter::sync` returns.
    pub fn acknowledged(&mut self, fs: &SimFs, synced: SyncedUpTo) -> Result<(), StoreError> {
        let last = self
            .records
            .last()
            .ok_or_else(|| invalid("acknowledgement without records"))?;
        if synced.synced_tick().is_none_or(|tick| tick.tick() < last.tick) {
            return Err(invalid("acknowledgement exceeds successful sync"));
        }
        self.acknowledgements
            .push((fs.cut_count()?, self.records.len(), synced.lsn()));
        Ok(())
    }
}
/// Crash every automatically captured mutating Vfs call under every write fate combination.
/// Also flip every acknowledged WAL/receipt byte and every installed checkpoint relation byte;
/// each media fault must cause recovery refusal. The image bound fails closed, never samples.
pub fn enumerate_crash_points(
    workload: impl FnOnce(Arc<SimFs>) -> Result<CrashWorkload, StoreError>,
    max_images: usize,
) -> Result<usize, StoreError> {
    let fs = Arc::new(SimFs::default());
    fs.enable_crash_recording()?;
    let script = workload(fs.clone())?;
    let cuts = fs.recorded_cuts()?;
    if cuts.is_empty() {
        return Err(invalid("crash workload made no mutating Vfs calls"));
    }
    let mut images = 0usize;
    for (cut_index, cut) in cuts.into_iter().enumerate() {
        let choices = cut
            .unsynced_writes()?
            .iter()
            .map(|w| {
                let mut fates = vec![WriteFate::Lost, WriteFate::Survive];
                if !w.bytes.is_empty() {
                    let end = w.offset.saturating_add(w.bytes.len() as u64);
                    let crossed = end.saturating_sub(1) / 512 - w.offset / 512;
                    for sectors in 1..=crossed as usize {
                        fates.push(WriteFate::Torn { sectors });
                    }
                }
                fates
            })
            .collect::<Vec<_>>();
        let count = choices.iter().try_fold(1usize, |n, c| {
            n.checked_mul(c.len())
                .ok_or_else(|| invalid("crash combinations overflow"))
        })?;
        if images.checked_add(count).is_none_or(|n| n > max_images) {
            return Err(invalid("crash exploration exceeds explicit bound"));
        }
        let frontier = script
            .acknowledgements
            .iter()
            .rev()
            .find(|(at, _, _)| *at <= cut_index + 1)
            .copied();
        let acknowledged = frontier.map_or(0, |(_, n, _)| n);
        if acknowledged > script.records.len() {
            return Err(invalid("acknowledgement exceeds scripted records"));
        }
        for mut combination in 0..count {
            let mut fates = Vec::with_capacity(choices.len());
            for available in &choices {
                let digit = combination % available.len();
                combination /= available.len();
                fates.push(
                    *available
                        .get(digit)
                        .ok_or_else(|| invalid("crash fate index outside combination"))?,
                );
            }
            let mut image = cut.fork()?;
            image.crash_with_fates(&fates)?;
            let scan = match WalScan::scan(&image, &script.wal_dir, script.uuid, true) {
                Ok(scan) => Some(scan),
                Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            let recovered = scan
                .as_ref()
                .map(|scan| scan.records().map(|(_, r)| r.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            if recovered.len() < acknowledged
                || !script.records.starts_with(&recovered)
                || !recovered.starts_with(
                    script
                        .records
                        .get(..acknowledged)
                        .ok_or_else(|| invalid("acknowledged prefix"))?,
                )
            {
                return Err(invalid(format!(
                    "crash at syscall cut {} lost an acknowledgement or recovered a non-prefix",
                    cut_index + 1
                )));
            }
            if let Some((dir, expected)) = &script.checkpoint {
                let checkpoints = FileCheckpoints::from_existing(Arc::new(image.clone()), dir);
                if let Some(id) = checkpoints.current()?
                    && checkpoints.read(id)? != *expected
                {
                    return Err(invalid(format!("checkpoint mismatch at syscall cut {}", cut_index + 1)));
                }
            }
            images += 1;
            if let Some((_, _, ack_lsn)) = frontier {
                let scan = scan.ok_or_else(|| invalid("acknowledged WAL directory vanished"))?;
                for segment in &scan.segments {
                    if segment.header.lsn_base >= ack_lsn {
                        continue;
                    }
                    let limit = (ack_lsn.0 - segment.header.lsn_base.0)
                        .min(image.open(&segment.path, OpenOpts::default())?.len()?)
                        as usize;
                    verify_file_faults(&image, &segment.path, limit, max_images, &mut images, |fault| {
                        WalScan::scan(fault, &script.wal_dir, script.uuid, false).is_err()
                    })?;
                    let receipt = script.wal_dir.join(format!("{:020}.ack", segment.header.segment_seq));
                    if let Ok(file) = image.open(&receipt, OpenOpts::default()) {
                        let size = usize::try_from(file.len()?).map_err(|_| invalid("receipt too large"))?;
                        verify_file_faults(&image, &receipt, size, max_images, &mut images, |fault| {
                            WalScan::scan(fault, &script.wal_dir, script.uuid, false).is_err()
                        })?;
                    } else if ack_lsn <= segment.end {
                        return Err(invalid("acknowledged segment has no durable receipt"));
                    }
                }
            }
            if let Some((dir, expected)) = &script.checkpoint {
                let checkpoints = FileCheckpoints::from_existing(Arc::new(image.clone()), dir);
                if let Some(id) = checkpoints.current()? {
                    let manifest = dir.join("ckpt").join(id.tick.to_string()).join("MANIFEST");
                    let manifest_len = usize::try_from(image.open(&manifest, OpenOpts::default())?.len()?)
                        .map_err(|_| invalid("checkpoint manifest too large"))?;
                    verify_file_faults(&image, &manifest, manifest_len, max_images, &mut images, |fault| {
                        FileCheckpoints::from_existing(Arc::new(fault.clone()), dir)
                            .read(id)
                            .is_err()
                    })?;
                    let current = dir.join("CURRENT");
                    let current_len = usize::try_from(image.open(&current, OpenOpts::default())?.len()?)
                        .map_err(|_| invalid("checkpoint pointer too large"))?;
                    verify_file_faults(&image, &current, current_len, max_images, &mut images, |fault| {
                        FileCheckpoints::from_existing(Arc::new(fault.clone()), dir)
                            .current()
                            .is_err()
                    })?;
                    for rel in expected.relations.keys() {
                        let path = dir
                            .join("ckpt")
                            .join(id.tick.to_string())
                            .join(format!("rel-{rel}.dat"));
                        let size = usize::try_from(image.open(&path, OpenOpts::default())?.len()?)
                            .map_err(|_| invalid("checkpoint file too large"))?;
                        verify_file_faults(&image, &path, size, max_images, &mut images, |fault| {
                            FileCheckpoints::from_existing(Arc::new(fault.clone()), dir)
                                .read(id)
                                .is_err()
                        })?;
                    }
                }
            }
        }
    }
    Ok(images)
}
fn verify_file_faults(
    image: &SimFs,
    path: &Path,
    limit: usize,
    max_images: usize,
    images: &mut usize,
    mut refuses: impl FnMut(&SimFs) -> bool,
) -> Result<(), StoreError> {
    for offset in 0..limit {
        *images = images
            .checked_add(1)
            .filter(|n| *n <= max_images)
            .ok_or_else(|| invalid("media-fault exploration exceeds explicit bound"))?;
        let fault = image.fork()?;
        fault.corrupt(path, offset)?;
        if !refuses(&fault) {
            return Err(invalid(format!(
                "media fault at {}:{offset} was accepted",
                path.display()
            )));
        }
    }
    Ok(())
}
