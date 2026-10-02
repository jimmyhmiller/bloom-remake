use crate::{
    OpenOpts, StoreError, Vfs, VfsFile, invalid, read_all,
    vfs::{atomic_write, read_path},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Absolute byte offset in the WAL stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Lsn(pub u64);
/// Opaque encoded whole-tick WAL record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalRecordBuf {
    pub batch: u64,
    pub tick: u64,
    pub now: i64,
    pub kind: u8,
    pub payload: Vec<u8>,
}
/// Successful sync watermark. Fields are private to prevent fabricating durability.
#[derive(Clone, Copy, Debug)]
pub struct SyncedUpTo {
    lsn: Lsn,
    tick: Option<u64>,
}
impl SyncedUpTo {
    /// Byte frontier covered by this sync.
    pub fn lsn(self) -> Lsn {
        self.lsn
    }
    /// Latest whole tick covered by this sync.
    pub fn synced_tick(self) -> Option<SyncedTick> {
        self.tick.map(|tick| SyncedTick { tick, lsn: self.lsn })
    }
}
/// A tick known to be durable, constructible only from a successful WAL sync.
#[derive(Clone, Copy, Debug)]
pub struct SyncedTick {
    pub(crate) tick: u64,
    pub(crate) lsn: Lsn,
}
impl SyncedTick {
    /// Covered logical tick.
    pub fn tick(self) -> u64 {
        self.tick
    }
    /// Covered WAL byte frontier.
    pub fn lsn(self) -> Lsn {
        self.lsn
    }
}
/// Proof that a checkpoint has been installed durably.
#[derive(Debug)]
pub struct TruncateToken {
    pub(crate) lsn: Lsn,
}
impl TruncateToken {
    /// The position the truncation reaches: the WAL wholly below it goes.
    pub fn lsn(&self) -> Lsn {
        self.lsn
    }
}
/// WAL seam. Invariant B: no writes of batch k+1 may be in flight before sync(k) returns.
/// A sync or append failure permanently poisons this incarnation; a failed sync must never be retried.
pub trait WalWriter: Send {
    fn append(&mut self, rec: &WalRecordBuf) -> Result<Lsn, StoreError>;
    fn sync(&mut self) -> Result<SyncedUpTo, StoreError>;
    fn truncate_through(&mut self, token: TruncateToken) -> Result<(), StoreError>;
}
/// Segment identity and opaque catalog bytes (catalog decoding belongs to M5.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentHeader {
    pub format: u16,
    pub store_uuid: [u8; 16],
    pub segment_seq: u64,
    pub restarts: u64,
    pub boot_nonce: u64,
    pub lsn_base: Lsn,
    pub catalog: Vec<u8>,
}
const MAX_RECORD: usize = 64 * 1024 * 1024;
const FIXED_RECORD: usize = 41;
const SYNC_MARKER: u8 = u8::MAX;
fn header_bytes(h: &SegmentHeader) -> Result<Vec<u8>, StoreError> {
    let mut b = b"BLSW".to_vec();
    b.extend(h.format.to_le_bytes());
    b.extend(h.store_uuid);
    b.extend(h.segment_seq.to_le_bytes());
    b.extend(h.restarts.to_le_bytes());
    b.extend(h.boot_nonce.to_le_bytes());
    b.extend(h.lsn_base.0.to_le_bytes());
    if b.len().checked_add(h.catalog.len()).is_none_or(|n| n > 1024 * 1024) {
        return Err(invalid("segment header exceeds one MiB"));
    }
    b.extend(
        u32::try_from(h.catalog.len())
            .map_err(|_| invalid("catalog too large"))?
            .to_le_bytes(),
    );
    b.extend(blake3::hash(&h.catalog).as_bytes());
    b.extend(&h.catalog);
    b.extend(crc32c::crc32c(&b).to_le_bytes());
    Ok(b)
}
fn corrupt(path: &Path, off: usize, reason: &str) -> StoreError {
    StoreError::Corruption {
        path: path.into(),
        offset: off as u64,
        reason: reason.into(),
    }
}
fn take<const N: usize>(b: &[u8], off: &mut usize) -> Result<[u8; N], StoreError> {
    let end = off.checked_add(N).ok_or_else(|| invalid("decode overflow"))?;
    let a = b
        .get(*off..end)
        .ok_or_else(|| invalid("truncated bytes"))?
        .try_into()
        .map_err(|_| invalid("decode length"))?;
    *off = end;
    Ok(a)
}
fn parse_header(b: &[u8]) -> Result<(SegmentHeader, usize), StoreError> {
    let mut off = 0;
    if take::<4>(b, &mut off)? != *b"BLSW" {
        return Err(invalid("segment magic"));
    }
    let format = u16::from_le_bytes(take(b, &mut off)?);
    let store_uuid = take(b, &mut off)?;
    let segment_seq = u64::from_le_bytes(take(b, &mut off)?);
    let restarts = u64::from_le_bytes(take(b, &mut off)?);
    let boot_nonce = u64::from_le_bytes(take(b, &mut off)?);
    let lsn_base = Lsn(u64::from_le_bytes(take(b, &mut off)?));
    let len = u32::from_le_bytes(take(b, &mut off)?) as usize;
    let digest = take::<32>(b, &mut off)?;
    let end = off.checked_add(len).ok_or_else(|| invalid("catalog overflow"))?;
    let catalog = b.get(off..end).ok_or_else(|| invalid("catalog truncated"))?.to_vec();
    off = end;
    let crc = u32::from_le_bytes(take(b, &mut off)?);
    if crc32c::crc32c(b.get(..off - 4).ok_or_else(|| invalid("header range"))?) != crc
        || blake3::hash(&catalog).as_bytes() != &digest
    {
        return Err(invalid("header checksum"));
    }
    Ok((
        SegmentHeader {
            format,
            store_uuid,
            segment_seq,
            restarts,
            boot_nonce,
            lsn_base,
            catalog,
        },
        off,
    ))
}
fn record_bytes(rec: &WalRecordBuf, lsn: Lsn) -> Result<Vec<u8>, StoreError> {
    let len = FIXED_RECORD
        .checked_add(rec.payload.len())
        .ok_or_else(|| invalid("record overflow"))?;
    if len > MAX_RECORD {
        return Err(invalid("record exceeds segment limit"));
    }
    let mut content = (len as u32).to_le_bytes().to_vec();
    content.extend(lsn.0.to_le_bytes());
    content.extend(rec.batch.to_le_bytes());
    content.extend(rec.tick.to_le_bytes());
    content.extend(rec.now.to_le_bytes());
    content.push(rec.kind);
    content.extend(&rec.payload);
    let mut b = content.get(..4).ok_or_else(|| invalid("record length"))?.to_vec();
    b.extend(crc32c::crc32c(&content).to_le_bytes());
    b.extend(content.get(4..).ok_or_else(|| invalid("record body"))?);
    Ok(b)
}
fn parse_record(b: &[u8], off: usize, base: u64) -> Result<(WalRecordBuf, usize), StoreError> {
    let mut at = off;
    let len = u32::from_le_bytes(take(b, &mut at)?) as usize;
    if !(FIXED_RECORD..=MAX_RECORD).contains(&len) {
        return Err(invalid("record length"));
    }
    let end = off.checked_add(len).ok_or_else(|| invalid("record overflow"))?;
    let raw = b.get(off..end).ok_or_else(|| invalid("record truncated"))?;
    // The LSN is the record's own position: checking it first rejects almost every misaligned candidate without
    // checksumming its (claimed) length, which keeps the torn-tail search linear.
    let claimed = raw
        .get(8..16)
        .and_then(|l| <[u8; 8]>::try_from(l).ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| invalid("record LSN"))?;
    if claimed != base.checked_add(off as u64).ok_or_else(|| invalid("LSN overflow"))? {
        return Err(invalid("record LSN"));
    }
    let crc = u32::from_le_bytes(take(b, &mut at)?);
    let mut content = raw.get(..4).ok_or_else(|| invalid("record length"))?.to_vec();
    content.extend(raw.get(8..).ok_or_else(|| invalid("record body"))?);
    if crc32c::crc32c(&content) != crc {
        return Err(invalid("record checksum"));
    }
    let lsn = u64::from_le_bytes(take(b, &mut at)?);
    if lsn != base.checked_add(off as u64).ok_or_else(|| invalid("LSN overflow"))? {
        return Err(invalid("record LSN"));
    }
    let batch = u64::from_le_bytes(take(b, &mut at)?);
    let tick = u64::from_le_bytes(take(b, &mut at)?);
    let now = i64::from_le_bytes(take(b, &mut at)?);
    let kind = take::<1>(b, &mut at)?.first().copied().ok_or_else(|| invalid("kind"))?;
    Ok((
        WalRecordBuf {
            batch,
            tick,
            now,
            kind,
            payload: b.get(at..end).ok_or_else(|| invalid("payload range"))?.to_vec(),
        },
        end,
    ))
}
/// File-backed append-only WAL; every new instance creates a new segment.
pub struct FileWal {
    fs: Arc<dyn Vfs>,
    dir: PathBuf,
    header: SegmentHeader,
    file: Box<dyn VfsFile>,
    base: u64,
    offset: u64,
    poisoned: bool,
    batch: Option<u64>,
    dirty: bool,
    last_tick: Option<u64>,
    certification: crate::Certification,
    /// Crc: the synced batch whose sync marker goes at the head of the next batch (so it is durable with that
    /// batch's one sync), as (the batch, its last tick).
    uncertified: Option<(u64, u64)>,
    /// Crc: whether this segment's receipt was written (at its first sync: it marks the segment as holding
    /// acknowledged data, so a damaged header is corruption, not an aborted creation).
    receipt_written: bool,
}
impl FileWal {
    /// This WAL's tail certification (strict by default; it must match how its store was created).
    pub fn certified(mut self, certification: crate::Certification) -> Self {
        self.certification = certification;
        self
    }

    /// Create a new incarnation segment. `base` is the absolute offset at the end of older segments.
    pub fn create(fs: Arc<dyn Vfs>, dir: &Path, header: SegmentHeader, base: Lsn) -> Result<Self, StoreError> {
        crate::vfs::durable_dir(&*fs, dir)?;
        if header.lsn_base != base {
            return Err(invalid("segment base LSN mismatch"));
        }
        let bytes = header_bytes(&header)?;
        let mut file = fs.open(
            &segment_path(dir, header.segment_seq),
            OpenOpts {
                create_new: true,
                ..OpenOpts::default()
            },
        )?;
        file.append(&bytes)?;
        file.sync_data()?;
        fs.sync_dir(dir)?;
        Ok(Self {
            fs,
            dir: dir.into(),
            header,
            file,
            base: base.0,
            offset: bytes.len() as u64,
            poisoned: false,
            batch: None,
            dirty: false,
            last_tick: None,
            certification: crate::Certification::Strict,
            uncertified: None,
            receipt_written: false,
        })
    }
    fn healthy(&self) -> Result<(), StoreError> {
        if self.poisoned {
            Err(StoreError::Poisoned)
        } else {
            Ok(())
        }
    }
    fn roll(&mut self) -> Result<(), StoreError> {
        self.file.sync_data()?;
        self.base = self
            .base
            .checked_add(self.offset)
            .ok_or_else(|| invalid("LSN overflow"))?;
        self.header.segment_seq = self
            .header
            .segment_seq
            .checked_add(1)
            .ok_or_else(|| invalid("segment sequence overflow"))?;
        self.header.lsn_base = Lsn(self.base);
        let bytes = header_bytes(&self.header)?;
        self.file = self.fs.open(
            &segment_path(&self.dir, self.header.segment_seq),
            OpenOpts {
                create_new: true,
                ..OpenOpts::default()
            },
        )?;
        self.file.append(&bytes)?;
        self.file.sync_data()?;
        self.fs.sync_dir(&self.dir)?;
        self.offset = bytes.len() as u64;
        self.receipt_written = false;
        Ok(())
    }

    /// Appends `bytes` at the end of the current segment.
    fn append_bytes(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        if let Err(e) = self.file.append(bytes) {
            self.poisoned = true;
            return Err(e);
        }
        self.offset = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("offset overflow"))?;
        Ok(())
    }
}
fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{seq:020}.seg"))
}
fn receipt_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{seq:020}.ack"))
}
fn receipt_bytes(header: &SegmentHeader, end: Lsn) -> Vec<u8> {
    let mut bytes = b"BLSA".to_vec();
    bytes.extend(header.store_uuid);
    bytes.extend(header.segment_seq.to_le_bytes());
    bytes.extend(end.0.to_le_bytes());
    bytes.extend(blake3::hash(&bytes).as_bytes());
    bytes
}
fn read_receipt(fs: &dyn Vfs, dir: &Path, header: &SegmentHeader) -> Result<Option<Lsn>, StoreError> {
    let path = receipt_path(dir, header.segment_seq);
    let bytes = match read_path(fs, &path) {
        Ok(bytes) => bytes,
        Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if bytes.len() != 68
        || bytes.get(..4) != Some(b"BLSA")
        || bytes.get(4..20) != Some(header.store_uuid.as_slice())
        || bytes.get(20..28) != Some(header.segment_seq.to_le_bytes().as_slice())
        || bytes.get(36..68)
            != Some(
                blake3::hash(bytes.get(..36).ok_or_else(|| invalid("receipt length"))?)
                    .as_bytes()
                    .as_slice(),
            )
    {
        return Err(corrupt(&path, 0, "acknowledgement receipt checksum or identity"));
    }
    let end = bytes
        .get(28..36)
        .ok_or_else(|| corrupt(&path, 28, "receipt frontier"))?;
    let end = u64::from_le_bytes(end.try_into().map_err(|_| corrupt(&path, 28, "receipt frontier"))?);
    Ok(Some(Lsn(end)))
}
impl WalWriter for FileWal {
    fn append(&mut self, rec: &WalRecordBuf) -> Result<Lsn, StoreError> {
        self.healthy()?;
        if rec.kind == SYNC_MARKER || rec.batch == u64::MAX {
            return Err(invalid("reserved WAL marker kind or batch"));
        }
        if self
            .batch
            .is_some_and(|b| rec.batch < b || (rec.batch != b && self.dirty))
        {
            return Err(invalid("Invariant B: batch must sync before advancing"));
        }
        if self.last_tick.is_some_and(|t| rec.tick <= t) {
            return Err(invalid("WAL ticks must increase"));
        }
        // Crc recovery reads a batch's end from the next batch's number, so batches count up by one.
        if self.certification == crate::Certification::Crc
            && !self.dirty
            && self.batch.is_some_and(|b| b.checked_add(1) != Some(rec.batch))
        {
            return Err(invalid("with crc certification, each batch is the one after the last"));
        }
        let prospective = FIXED_RECORD
            .checked_add(rec.payload.len())
            .ok_or_else(|| invalid("record overflow"))?;
        if header_bytes(&self.header)?
            .len()
            .checked_add(prospective)
            .and_then(|n| n.checked_add(FIXED_RECORD))
            .is_none_or(|n| n > MAX_RECORD)
        {
            return Err(invalid("record plus sync marker exceeds segment limit"));
        }
        if self
            .offset
            .checked_add((prospective + FIXED_RECORD) as u64)
            .ok_or_else(|| invalid("segment overflow"))?
            > MAX_RECORD as u64
            && self.offset > header_bytes(&self.header)?.len() as u64
            && let Err(e) = self.roll()
        {
            self.poisoned = true;
            return Err(e);
        }
        // Crc: the last synced batch's marker leads this batch, and becomes durable with its sync.
        if let Some((_, tick)) = self.uncertified.take() {
            let marker = WalRecordBuf {
                batch: rec.batch,
                tick,
                now: 0,
                kind: SYNC_MARKER,
                payload: Vec::new(),
            };
            let lsn = Lsn(self
                .base
                .checked_add(self.offset)
                .ok_or_else(|| invalid("marker LSN overflow"))?);
            let bytes = record_bytes(&marker, lsn)?;
            self.append_bytes(&bytes)?;
        }
        let lsn = Lsn(self
            .base
            .checked_add(self.offset)
            .ok_or_else(|| invalid("LSN overflow"))?);
        let bytes = record_bytes(rec, lsn)?;
        self.append_bytes(&bytes)?;
        self.batch = Some(rec.batch);
        self.dirty = true;
        self.last_tick = Some(rec.tick);
        Ok(lsn)
    }
    fn sync(&mut self) -> Result<SyncedUpTo, StoreError> {
        self.healthy()?;
        if self.certification == crate::Certification::Crc {
            // One sync. This batch's marker is written at the head of the next one; the segment's receipt, once, at
            // its first sync.
            if let Err(e) = self.file.sync_data() {
                self.poisoned = true;
                return Err(e);
            }
            let end = Lsn(self
                .base
                .checked_add(self.offset)
                .ok_or_else(|| invalid("LSN overflow"))?);
            if self.dirty {
                if !self.receipt_written {
                    if let Err(e) = atomic_write(
                        &*self.fs,
                        &receipt_path(&self.dir, self.header.segment_seq),
                        &receipt_bytes(&self.header, end),
                    ) {
                        self.poisoned = true;
                        return Err(e);
                    }
                    self.receipt_written = true;
                }
                if let (Some(b), Some(t)) = (self.batch, self.last_tick) {
                    self.uncertified = Some((b, t));
                }
            }
            self.dirty = false;
            return Ok(SyncedUpTo {
                lsn: end,
                tick: self.last_tick,
            });
        }
        if self.dirty {
            // The marker must never survive a crash while the data it certifies is still volatile.
            // This first sync establishes that ordering; the second sync acknowledges the marker.
            if let Err(e) = self.file.sync_data() {
                self.poisoned = true;
                return Err(e);
            }
            let batch = self.batch.ok_or_else(|| invalid("dirty WAL without batch"))?;
            let marker = WalRecordBuf {
                batch: batch + 1,
                tick: self.last_tick.ok_or_else(|| invalid("dirty WAL without tick"))?,
                now: 0,
                kind: SYNC_MARKER,
                payload: Vec::new(),
            };
            let lsn = Lsn(self
                .base
                .checked_add(self.offset)
                .ok_or_else(|| invalid("marker LSN overflow"))?);
            let bytes = record_bytes(&marker, lsn)?;
            if let Err(e) = self.file.append(&bytes) {
                self.poisoned = true;
                return Err(e);
            }
            self.offset = self
                .offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| invalid("marker offset overflow"))?;
        }
        if let Err(e) = self.file.sync_data() {
            self.poisoned = true;
            return Err(e);
        }
        if self.dirty {
            let end = Lsn(self
                .base
                .checked_add(self.offset)
                .ok_or_else(|| invalid("receipt LSN overflow"))?);
            if let Err(e) = atomic_write(
                &*self.fs,
                &receipt_path(&self.dir, self.header.segment_seq),
                &receipt_bytes(&self.header, end),
            ) {
                self.poisoned = true;
                return Err(e);
            }
        }
        self.dirty = false;
        Ok(SyncedUpTo {
            lsn: Lsn(self
                .base
                .checked_add(self.offset)
                .ok_or_else(|| invalid("LSN overflow"))?),
            tick: self.last_tick,
        })
    }
    fn truncate_through(&mut self, token: TruncateToken) -> Result<(), StoreError> {
        self.healthy()?;
        if self.dirty {
            return Err(invalid("WAL truncation requires batch boundary"));
        }
        let scan = WalScan::scan(&*self.fs, &self.dir, self.header.store_uuid, false)?;
        for segment in scan.segments {
            if segment.header.segment_seq < self.header.segment_seq && segment.end <= token.lsn {
                self.fs.remove(&segment.path)?;
                match self.fs.remove(&receipt_path(&self.dir, segment.header.segment_seq)) {
                    Ok(()) => {}
                    Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        self.fs.sync_dir(&self.dir)?;
        Ok(())
    }
}
/// Whether damage at `off` can be the torn tail of the last, unsynced batch rather than corruption of synced data.
///
/// Batch `k + 1` is written only after batch `k`'s sync returned, and a sync ends with a marker whose batch is `k + 1`.
/// The records of the one unsynced batch can be lost or torn in any order. So the damage is a torn tail iff every
/// whole record after it belongs to a single data batch `B` that no marker certifies, and `B` is the batch the damage
/// is in: the batch of the record before the damage, or a later one when that record is a sync marker (the previous
/// batch ended there) or the damage is at the start of the segment.
///
/// Without markers (`Certification::Crc`) a batch's end is not recorded, so damage followed only by records of the
/// batch after the last whole record's is also a torn tail (a crash in the first record of the next batch).
fn torn_tail(bytes: &[u8], off: usize, base: u64, last: Option<(u64, u8)>, crc: bool) -> bool {
    let mut later: Option<u64> = None;
    for candidate in off + 1..bytes.len() {
        let Ok((r, _)) = parse_record(bytes, candidate, base) else {
            continue;
        };
        if r.kind == SYNC_MARKER || later.is_some_and(|b| b != r.batch) {
            return false;
        }
        later = Some(r.batch);
    }
    match (later, last) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(b), Some((lb, kind))) => {
            b == lb || (b > lb && kind == SYNC_MARKER) || (crc && Some(b) == lb.checked_add(1))
        }
    }
}

/// A segment file's sequence number, from its name.
fn segment_seq_of(path: &Path) -> Option<u64> {
    path.file_stem()?.to_str()?.parse().ok()
}

/// Recovered segment and its own opaque catalog.
#[derive(Debug)]
pub struct ScannedSegment {
    pub path: PathBuf,
    pub header: SegmentHeader,
    pub records: Vec<(Lsn, WalRecordBuf)>,
    pub end: Lsn,
}
/// Recovery scan with whole-record prefix semantics.
#[derive(Debug, Default)]
pub struct WalScan {
    pub segments: Vec<ScannedSegment>,
    pub end: Lsn,
}
impl WalScan {
    /// Scan canonical segment order; optionally repair a torn tail by truncating and syncing it.
    pub fn scan(fs: &dyn Vfs, dir: &Path, uuid: [u8; 16], repair: bool) -> Result<Self, StoreError> {
        Self::scan_certified(fs, dir, uuid, repair, crate::Certification::Strict)
    }

    /// [`WalScan::scan`] for a WAL written with `certification`.
    pub fn scan_certified(
        fs: &dyn Vfs,
        dir: &Path,
        uuid: [u8; 16],
        repair: bool,
        certification: crate::Certification,
    ) -> Result<Self, StoreError> {
        let crc = certification == crate::Certification::Crc;
        let paths = fs.list(dir)?;
        let mut result = Self::default();
        let mut base = 0u64;
        let mut previous_seq = None;
        let segments = paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "seg"))
            .collect::<Vec<_>>();
        let segment_count = segments.len();
        for (segment_index, path) in segments.into_iter().enumerate() {
            let mut file = fs.open(&path, OpenOpts::default())?;
            if file.len()? > MAX_RECORD as u64 {
                return Err(corrupt(&path, MAX_RECORD, "segment exceeds 64 MiB"));
            }
            let bytes = read_all(&*file)?;
            let (header, mut off) = match parse_header(&bytes) {
                Ok(h) => h,
                Err(e) => {
                    // A newest segment whose header never became durable (a crash inside `create` or `roll`, which
                    // can leave the directory entry without the data): no record was ever appended to it, because
                    // records follow the header's sync. With no receipt, nothing in it was acknowledged.
                    let newest = segment_index + 1 == segment_count;
                    let receipt =
                        segment_seq_of(&path).map(|seq| fs.open(&receipt_path(dir, seq), OpenOpts::default()));
                    let acknowledged = match receipt {
                        Some(Ok(_)) => true,
                        Some(Err(StoreError::Io(io))) if io.kind() == std::io::ErrorKind::NotFound => false,
                        Some(Err(other)) => return Err(other),
                        None => true,
                    };
                    if newest && !acknowledged {
                        if repair {
                            drop(file);
                            fs.remove(&path)?;
                            fs.sync_dir(dir)?;
                        }
                        break;
                    }
                    return Err(corrupt(&path, 0, &e.to_string()));
                }
            };
            let receipt = read_receipt(fs, dir, &header)?;
            if header.store_uuid != uuid
                || path != segment_path(dir, header.segment_seq)
                || previous_seq.is_some_and(|seq: u64| seq.checked_add(1) != Some(header.segment_seq))
            {
                return Err(corrupt(&path, 0, "segment identity"));
            }
            previous_seq = Some(header.segment_seq);
            if !result.segments.is_empty() && header.lsn_base.0 != base {
                return Err(corrupt(&path, 0, "segment stream base"));
            }
            base = header.lsn_base.0;
            if receipt.is_some_and(|end| end.0 < base + off as u64 || end.0 > base + bytes.len() as u64) {
                return Err(corrupt(&path, 0, "acknowledgement frontier outside segment"));
            }
            let mut records = Vec::new();
            // The batch and kind of the last whole record (none yet in this segment).
            let mut last: Option<(u64, u8)> = None;
            while off < bytes.len() {
                match parse_record(&bytes, off, base) {
                    Ok((rec, end)) => {
                        if last.is_some_and(|(b, _)| rec.batch < b) {
                            return Err(corrupt(&path, off, "batch decreases"));
                        }
                        last = Some((rec.batch, rec.kind));
                        if rec.kind != SYNC_MARKER {
                            records.push((Lsn(base + off as u64), rec));
                        }
                        off = end;
                    }
                    Err(_) => {
                        if segment_index + 1 < segment_count || receipt.is_some_and(|end| end.0 > base + off as u64) {
                            return Err(corrupt(
                                &path,
                                off,
                                "damage before a later segment or acknowledged data",
                            ));
                        }
                        if !torn_tail(&bytes, off, base, last, crc) {
                            return Err(corrupt(&path, off, "damage before a later synced batch"));
                        }
                        if repair {
                            file.truncate(off as u64)?;
                            file.sync_data()?;
                        }
                        break;
                    }
                }
            }
            // Absolute positions in later segments include original full segment lengths, including discarded tails.
            let end = Lsn(base.checked_add(off as u64).ok_or_else(|| invalid("scan overflow"))?);
            result.segments.push(ScannedSegment {
                path,
                header,
                records,
                end,
            });
            base = base
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| invalid("scan overflow"))?;
            result.end = end;
        }
        Ok(result)
    }
    /// Flatten recovered records in log order.
    pub fn records(&self) -> impl Iterator<Item = &(Lsn, WalRecordBuf)> {
        self.segments.iter().flat_map(|s| s.records.iter())
    }
}
/// In-memory durability for bounded model checking; preserves the same batch and poisoning contract.
#[derive(Default)]
pub struct MemDurability {
    records: Vec<(Lsn, WalRecordBuf)>,
    offset: u64,
    synced: usize,
    dirty: bool,
    batch: Option<u64>,
}
impl MemDurability {
    /// Records covered by the most recent sync.
    pub fn synced_records(&self) -> impl Iterator<Item = &(Lsn, WalRecordBuf)> {
        self.records.iter().take(self.synced)
    }
}
impl WalWriter for MemDurability {
    fn append(&mut self, rec: &WalRecordBuf) -> Result<Lsn, StoreError> {
        if self
            .batch
            .is_some_and(|b| rec.batch < b || (b != rec.batch && self.dirty))
        {
            return Err(invalid("Invariant B"));
        }
        if self.records.last().is_some_and(|(_, r)| rec.tick <= r.tick) {
            return Err(invalid("WAL ticks must increase"));
        }
        let lsn = Lsn(self.offset);
        let bytes = record_bytes(rec, lsn)?;
        self.offset = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("memory WAL overflow"))?;
        self.records.push((lsn, rec.clone()));
        self.batch = Some(rec.batch);
        self.dirty = true;
        Ok(lsn)
    }
    fn sync(&mut self) -> Result<SyncedUpTo, StoreError> {
        self.synced = self.records.len();
        self.dirty = false;
        Ok(SyncedUpTo {
            lsn: Lsn(self.offset),
            tick: self.records.last().map(|(_, r)| r.tick),
        })
    }
    fn truncate_through(&mut self, token: TruncateToken) -> Result<(), StoreError> {
        if self.dirty {
            return Err(invalid("truncation requires batch boundary"));
        }
        self.records.retain(|(lsn, _)| *lsn >= token.lsn);
        self.synced = self.records.len();
        Ok(())
    }
}
#[cfg(test)]
pub(crate) fn test_header_len(h: &SegmentHeader) -> usize {
    header_bytes(h).unwrap().len()
}
