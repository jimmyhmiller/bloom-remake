//! Opening a node's store and recovering its durable state (ARCHITECTURE §5.6).
//!
//! In order, and a crash anywhere in it leaves the old checkpoint and WAL intact:
//!
//! 1. take `LOCK`; read `META` and check the identity against the deployment;
//! 2. open the database (docs/design/DATABASE.md): the durable rows its tables hold, as of the tick they cover;
//! 3. replay the WAL records after that tick, into the recovered rows and the database (a torn tail is truncated:
//!    it was never synced, so never acknowledged). A store from before the database starts it from its checkpoint
//!    chain and the WAL after it, once: the database is flushed then, and the checkpoints go;
//! 4. reserve ticks: boot at `reserved + 1` (at tick 0 on the first boot), and make `boot + 65 536` the new bound;
//! 5. count the restart, pick the boot instant `max(wall, last_now + 1 ns)` (`META.last_now` bounds every instant a
//!    released tick had), reserve time up to one `TIME_STEP` past it, write `META`, and open a new WAL segment.
//!
//! The layout under the node's directory: `LOCK`, `META`, `db/` (the database), `wal/<seq>.seg`, `blobs/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_base::{RelId, internal_error};
use blossom_ir::ValidatedProgram;
use blossom_ir::tick::Row;
use blossom_store::{
    Certification, FileCheckpoints, FileWal, Lsn, MetaRecord, MetaStore, OpenMode, SegmentHeader, StoreError,
    StoreIdentity, StoreLock, Vfs, WalScan, durable_dir,
};
use blossom_value::time::{Instant, Tick};
use blossom_wire::codec::put_varint;

use crate::NodeError;
use crate::database::Database;
use crate::durable::{Delta, DurableCodec, DurableImage, DurableSchema};
use crate::node::{Boot, RESERVE_STEP, TIME_STEP};

/// The on-disk format of this build.
pub const FORMAT: u16 = 1;
/// The WAL record kind of a tick's durable delta.
pub const KIND_DELTA: u8 = 1;
/// The WAL record kind of a tick's durable delta led by the blobs it logs: a `u32` count, each blob as a `u32`
/// length and its bytes, then the delta. The record makes them durable with its sync (`BlobStore::write_logged`).
pub const KIND_DELTA_BLOBS: u8 = 2;
/// The largest blob a record logs; a larger one is made durable as a file before the record (`BlobStore::put_all`).
pub const INLINE_BLOB_MAX: usize = 1 << 20;
/// The most bytes one record holds in logged blobs and its delta together (records share a 64 MiB segment); the blobs
/// past it are made durable as files.
pub const INLINE_RECORD_MAX: usize = 8 << 20;

/// A tick's WAL record: its kind and payload, and the blobs it logs (`BlobStore::write_logged` once it is appended).
pub struct TickRecord {
    pub kind: u8,
    pub payload: Vec<u8>,
    pub logged: Vec<blossom_store::BlobBytes>,
}

/// The record of a tick with durable delta `delta` whose rows reference `blobs`: the blobs not yet durable or
/// logged are logged in it, up to the inline limits; the others are made durable as files first.
pub fn tick_record(
    store: &blossom_store::BlobStore,
    blobs: &[blossom_store::BlobBytes],
    delta: Vec<u8>,
) -> Result<TickRecord, NodeError> {
    let mut logged = Vec::new();
    let mut put = Vec::new();
    let mut inline = 0usize;
    let budget = INLINE_RECORD_MAX.saturating_sub(delta.len());
    for (b, bytes) in store.unwritten(blobs)? {
        if bytes.len() <= INLINE_BLOB_MAX && inline + bytes.len() <= budget {
            inline += bytes.len();
            logged.push((b, bytes));
        } else {
            put.push((b, bytes));
        }
    }
    store.put_all(&put)?;
    if logged.is_empty() {
        return Ok(TickRecord {
            kind: KIND_DELTA,
            payload: delta,
            logged,
        });
    }
    let mut payload = Vec::with_capacity(4 + inline + 4 * logged.len() + delta.len());
    let count = u32::try_from(logged.len()).map_err(|_| internal_error!("a record logs too many blobs"))?;
    payload.extend(count.to_le_bytes());
    for (_, bytes) in &logged {
        let len = u32::try_from(bytes.len()).map_err(|_| internal_error!("a logged blob exceeds 4 GiB"))?;
        payload.extend(len.to_le_bytes());
        payload.extend_from_slice(bytes);
    }
    payload.extend(delta);
    Ok(TickRecord {
        kind: KIND_DELTA_BLOBS,
        payload,
        logged,
    })
}

/// The blobs a `KIND_DELTA_BLOBS` record at `lsn` logs, and its delta.
pub fn logged_blobs(payload: &[u8], lsn: Lsn) -> Result<(Vec<blossom_store::BlobBytes>, &[u8]), NodeError> {
    let bad = || NodeError::Store(format!("the WAL record at LSN {} logs malformed blobs", lsn.0));
    let take = |at: usize, n: usize| payload.get(at..at.checked_add(n)?);
    let word = |at: usize| -> Result<usize, NodeError> {
        let b: [u8; 4] = take(at, 4).and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
        Ok(u32::from_le_bytes(b) as usize)
    };
    let count = word(0)?;
    let mut at = 4usize;
    let mut blobs = Vec::new();
    for _ in 0..count {
        let len = word(at)?;
        let bytes = take(at + 4, len).ok_or_else(bad)?;
        blobs.push((blossom_value::BlobRef::of(bytes), Arc::from(bytes)));
        at += 4 + len;
    }
    Ok((blobs, payload.get(at..).ok_or_else(bad)?))
}

/// Where and as whom a node's store is opened.
#[derive(Clone, Debug)]
pub struct StoreSpec {
    pub dir: PathBuf,
    /// How the database flushes, caches and keeps history.
    pub database: blossom_store::lsm::LsmOptions,
    /// The identity the deployment expects (`store_uuid` is ignored: it is the store's own).
    pub identity: StoreIdentity,
    pub mode: OpenMode,
    /// How the WAL certifies its tail. A new store records it; an existing one must have been created with it.
    pub certification: Certification,
}

/// An opened, recovered store.
pub struct Opened {
    pub boot: Boot,
    pub wal: FileWal,
    /// The node's database, holding every released tick of every incarnation (this boot's included once it runs).
    pub database: Arc<Database>,
    pub meta: MetaStore,
    /// The `META` record as written at this boot.
    pub record: MetaRecord,
    pub lock: StoreLock,
    /// The tick recovery started from (the database's tables, or a legacy checkpoint's), if any.
    pub base: Option<u64>,
    /// How many WAL records recovery replayed.
    pub replayed: usize,
    /// The node's durable blobs (FOREIGN-PROTOCOLS §5).
    pub blobs: Arc<blossom_store::BlobStore>,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("boot", &self.boot)
            .field("record", &self.record)
            .field("base", &self.base)
            .field("replayed", &self.replayed)
            .finish_non_exhaustive()
    }
}

fn not_found(e: &StoreError) -> bool {
    matches!(e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}

/// The WAL directory of a node directory.
pub fn wal_dir(dir: &Path) -> PathBuf {
    dir.join("wal")
}

/// The catalog a WAL segment header carries: every durable relation's name and schema hash.
fn catalog(schema: &DurableSchema) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, schema.rels.len() as u64);
    for (_, name, hash) in &schema.rels {
        put_varint(&mut out, name.len() as u64);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(hash);
    }
    out
}

/// Creates a fresh store for `identity` at `dir`: the directory and a `META` that has never booted. Refuses when the
/// directory already holds a store.
pub fn init(
    fs: Arc<dyn Vfs>,
    dir: &Path,
    identity: &StoreIdentity,
    certification: Certification,
) -> Result<(), NodeError> {
    durable_dir(&*fs, dir)?;
    let _lock = StoreLock::acquire(&*fs, dir)?;
    let meta = MetaStore::new(fs.clone(), dir);
    match meta.read() {
        Ok(_) => Err(NodeError::Store(format!(
            "{} already holds a node store; refusing to initialize over it",
            dir.display()
        ))),
        Err(e) if not_found(&e) => write_fresh(&*fs, &meta, dir, identity, certification),
        Err(e) => Err(e.into()),
    }
}

/// Writes the `META` of a store that has never booted: restarts 0 means the first boot runs tick 0.
fn write_fresh(
    fs: &dyn Vfs,
    meta: &MetaStore,
    dir: &Path,
    identity: &StoreIdentity,
    certification: Certification,
) -> Result<(), NodeError> {
    durable_dir(fs, &wal_dir(dir))?;
    meta.write(&MetaRecord {
        identity: identity.clone(),
        node_id_map: Vec::new(),
        restarts: 0,
        reserved_tick: 0,
        last_now: i64::MIN,
        understood_version: 0,
        poison_deny_list: Vec::new(),
        clean_shutdown: false,
        certification,
    })?;
    Ok(())
}

/// Opens and recovers the store of a node running `program`. `names` are the deployment's node names by id (durable
/// rows store nodes by name); `wall` is the wall clock now; `boot_nonce` is fresh entropy.
pub fn open(
    fs: Arc<dyn Vfs>,
    spec: &StoreSpec,
    program: &ValidatedProgram,
    names: Arc<[Arc<str>]>,
    wall: Instant,
    boot_nonce: u64,
) -> Result<Opened, NodeError> {
    let dir = spec.dir.as_path();
    // 1. Lock, then identity.
    if spec.mode == OpenMode::InitFresh {
        durable_dir(&*fs, dir)?;
    }
    let lock = match StoreLock::acquire(&*fs, dir) {
        Ok(l) => l,
        Err(e) if not_found(&e) => return Err(no_state(spec)),
        Err(e) => return Err(e.into()),
    };
    let meta = MetaStore::new(fs.clone(), dir);
    let mut record = match meta.read() {
        Ok(r) => r,
        Err(e) if not_found(&e) => match spec.mode {
            OpenMode::Existing => return Err(no_state(spec)),
            OpenMode::InitFresh => {
                let mut identity = spec.identity.clone();
                identity.store_uuid = fresh_uuid(&identity, boot_nonce, wall);
                write_fresh(&*fs, &meta, dir, &identity, spec.certification)?;
                meta.read()?
            }
        },
        Err(e) => return Err(e.into()),
    };
    check_identity(&record.identity, &spec.identity)?;
    if record.certification != spec.certification {
        return Err(NodeError::Store(format!(
            "the store at {} was created with {:?} tail certification; the deployment asks for {:?}",
            dir.display(),
            record.certification,
            spec.certification
        )));
    }
    let uuid = record.identity.store_uuid;
    let schema = DurableSchema::of(program.get());
    let codec = DurableCodec::new(program.get(), &schema, names.clone());
    // 2. The database: the rows its tables hold, as of the tick they cover. A store from before the database (none,
    //    or one of an older key format) starts from its checkpoint chain, once.
    let (database, fresh) = Database::open(fs.clone(), dir, program, names, spec.database)?;
    let database = Arc::new(database);
    let legacy = if fresh {
        FileCheckpoints::from_existing(fs.clone(), dir).current()?
    } else {
        None
    };
    //    The rows recovery replays onto: the database itself, or (a fresh one) an image it then starts from.
    let (mut image, from) = match (fresh, legacy) {
        (true, Some(id)) => {
            let checkpoints = FileCheckpoints::from_existing(fs.clone(), dir);
            let mut image = codec.decode_image(&checkpoints.read(id)?)?;
            // A checkpoint is its full image and the delta layers after it, applied in order.
            for layer in checkpoints.read_layers(id)? {
                image.apply(&codec.decode_delta(&layer)?);
            }
            (Some(image), Some(id.tick))
        }
        (true, None) => (Some(DurableImage::default()), None),
        (false, _) => (None, database.flushed()?),
    };
    // 3. The WAL after it.
    let wdir = wal_dir(dir);
    durable_dir(&*fs, &wdir)?;
    let scan = WalScan::scan(&*fs, &wdir, uuid, true)?;
    let blobs = Arc::new(blossom_store::BlobStore::open(fs.clone(), dir)?);
    let mut last_now = record.last_now;
    let mut last_tick: Option<u64> = from;
    let mut replayed = 0;
    // The ticks since the database's views (its flushed tick on, DATABASE.md §8): their deltas, in order, for the
    // engine's catch-up of the views.
    let mut since_views: Vec<(u64, Delta)> = Vec::new();
    for (lsn, rec) in scan.records() {
        if rec.kind == KIND_DELTA_BLOBS {
            // The blobs it logs are restored even when the base covers it: their files may not be durable yet (they
            // are synced before the WAL that logs them goes).
            let (logged, _) = logged_blobs(&rec.payload, *lsn)?;
            blobs.restore_logged(&logged, *lsn)?;
        }
        let delta = record_delta(rec, *lsn)?;
        if image.is_none() && from.is_none_or(|f| rec.tick >= f) {
            since_views.push((rec.tick, codec.decode_delta(delta)?));
        }
        if from.is_some_and(|b| rec.tick <= b) {
            continue;
        }
        if last_tick.is_some_and(|t| rec.tick <= t) {
            return Err(NodeError::Store(format!(
                "WAL record at LSN {} is for tick {}, not after tick {}",
                lsn.0,
                rec.tick,
                last_tick.unwrap_or_default()
            )));
        }
        let decoded = codec.decode_delta(delta)?;
        match &mut image {
            Some(image) => image.apply(&decoded),
            None => database.apply(rec.tick, &decoded)?,
        }
        last_tick = Some(rec.tick);
        last_now = last_now.max(rec.now);
        replayed += 1;
    }
    // A database the store did not have starts from the recovered rows, at the last recovered tick, and is flushed:
    // from here it is the base of every recovery, and the legacy checkpoints go.
    if let Some(image) = image {
        if let Some(t) = last_tick {
            database.bootstrap(t, &image)?;
        }
        database.flush()?;
        if legacy.is_some() {
            remove_checkpoints(&*fs, dir)?;
        }
    }
    // The blobs the recovered rows hold were made durable before their records synced (as files, or logged in a
    // record and restored above): check it, so a store that lost one refuses to start rather than failing a later
    // tick that reads it.
    let referenced = database.blob_counts()?;
    // The provisional files of blobs no surviving record logs go (their bytes may have gone with their records).
    blobs.drop_unrestored()?;
    // Every blob the store holds is durable, or pending (logged in a surviving record, synced before its WAL goes);
    // those no recovered row holds (a crash between a blob's write and its record's sync, or rows a replayed record
    // deleted) are the node's first candidates for collection.
    let stored: std::collections::BTreeSet<blossom_value::BlobRef> =
        blobs.list()?.into_iter().chain(blobs.pending_blobs()?).collect();
    if let Some(missing) = referenced.keys().find(|b| !stored.contains(b)) {
        return Err(NodeError::Store(format!(
            "a recovered row holds blob {}, which the blob store does not have",
            missing.hex()
        )));
    }
    // 4. Reserve ticks.
    let boot_tick = if record.restarts == 0 {
        0
    } else {
        record
            .reserved_tick
            .checked_add(1)
            .ok_or_else(|| internal_error!("the tick reservation overflows"))?
    };
    if last_tick.is_some_and(|t| t >= boot_tick) {
        return Err(NodeError::Store(format!(
            "the WAL holds tick {} but META reserved only up to tick {}",
            last_tick.unwrap_or_default(),
            record.reserved_tick
        )));
    }
    let reserved = boot_tick
        .checked_add(RESERVE_STEP)
        .ok_or_else(|| internal_error!("the tick reservation overflows"))?;
    // 5. The new incarnation.
    let now = Instant(wall.0.max(last_now.saturating_add(1)));
    record.restarts = record
        .restarts
        .checked_add(1)
        .ok_or_else(|| internal_error!("the restart counter overflows"))?;
    record.reserved_tick = reserved;
    let time_reserved = Instant(now.0.saturating_add(TIME_STEP));
    record.last_now = time_reserved.0;
    record.clean_shutdown = false;
    meta.write(&record)?;
    let seq = scan
        .segments
        .last()
        .map(|s| s.header.segment_seq.checked_add(1))
        .unwrap_or(Some(0))
        .ok_or_else(|| internal_error!("the WAL segment sequence overflows"))?;
    let base = scan_base(&scan)?;
    let wal = FileWal::create(
        fs.clone(),
        &wdir,
        SegmentHeader {
            format: FORMAT,
            store_uuid: uuid,
            segment_seq: seq,
            restarts: record.restarts,
            boot_nonce,
            lsn_base: base,
            catalog: catalog(&schema),
        },
        base,
    )?
    .certified(record.certification);
    // A database the store had: its views catch up from the ticks since (none for a database started here, whose views
    // are built at the first tick).
    let catch_up = if fresh { None } else { Some(catch_up_of(since_views)) };
    Ok(Opened {
        boot: Boot {
            database: database.clone(),
            tick: Tick(boot_tick),
            reserved: Tick(reserved),
            time_reserved,
            now,
            // Durable state was reloaded iff an earlier incarnation's first boot tick became durable (it always
            // leaves a WAL record, which the database's tables may since cover): a crash before that boots fresh
            // again.
            recovered: from.is_some() || replayed > 0,
            incarnation: record.restarts,
            blobs: blobs.clone(),
            stored,
            catch_up,
        },
        wal,
        database,
        meta,
        record,
        lock,
        base: from,
        replayed,
        blobs,
    })
}

/// The durable delta a WAL record carries (after the blobs a `KIND_DELTA_BLOBS` record logs).
pub fn record_delta(rec: &blossom_store::WalRecordBuf, lsn: Lsn) -> Result<&[u8], NodeError> {
    match rec.kind {
        KIND_DELTA => Ok(rec.payload.as_slice()),
        KIND_DELTA_BLOBS => Ok(logged_blobs(&rec.payload, lsn)?.1),
        other => Err(NodeError::Store(format!(
            "WAL record at LSN {} has kind {other}, which this build does not know",
            lsn.0
        ))),
    }
}

/// Removes a store's legacy checkpoint chain (`ckpt/` and `CURRENT`), once the database holds what it held.
fn remove_checkpoints(fs: &dyn Vfs, dir: &Path) -> Result<(), NodeError> {
    let ckpt = dir.join("ckpt");
    match fs.list(&ckpt) {
        Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(entries) => {
            for d in entries {
                for f in fs.list(&d)? {
                    fs.remove(&f)?;
                }
                fs.remove_dir(&d)?;
            }
            fs.sync_dir(&ckpt)?;
        }
    }
    let current = dir.join("CURRENT");
    if fs.list(dir)?.contains(&current) {
        fs.remove(&current)?;
        fs.sync_dir(dir)?;
    }
    Ok(())
}

/// The absolute offset after every existing segment: the stream position a new segment starts at.
fn scan_base(scan: &WalScan) -> Result<Lsn, NodeError> {
    // The scan truncated any torn tail, so the last segment ends where its last whole record ends.
    Ok(match scan.segments.last() {
        Some(s) => s.end,
        None => Lsn(0),
    })
}

fn check_identity(found: &StoreIdentity, expected: &StoreIdentity) -> Result<(), NodeError> {
    let mismatch = |what: &str, a: &dyn std::fmt::Debug, b: &dyn std::fmt::Debug| {
        Err(NodeError::Store(format!(
            "the store belongs to another {what}: it holds {a:?}, the deployment expects {b:?}"
        )))
    };
    if found.deployment_id != expected.deployment_id {
        return mismatch("deployment", &found.deployment_id, &expected.deployment_id);
    }
    if found.program_id != expected.program_id {
        return mismatch("program", &found.program_id, &expected.program_id);
    }
    if found.node_name != expected.node_name {
        return mismatch("node", &found.node_name, &expected.node_name);
    }
    if found.principal != expected.principal {
        return mismatch("principal", &found.principal, &expected.principal);
    }
    if found.directory_digest != expected.directory_digest {
        return mismatch("directory", &found.directory_digest, &expected.directory_digest);
    }
    if found.format != FORMAT {
        return Err(NodeError::Store(format!(
            "the store has format {}, this build reads format {FORMAT}",
            found.format
        )));
    }
    Ok(())
}

/// A store uuid: unique per initialization (the boot nonce is fresh entropy).
pub fn fresh_uuid(identity: &StoreIdentity, nonce: u64, wall: Instant) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(b"blossom store uuid");
    h.update(&identity.deployment_id);
    h.update(identity.node_name.as_bytes());
    h.update(&nonce.to_le_bytes());
    h.update(&wall.0.to_le_bytes());
    let hash = h.finalize();
    let mut out = [0u8; 16];
    for (o, b) in out.iter_mut().zip(hash.as_bytes()) {
        *o = *b;
    }
    out
}

fn no_state(spec: &StoreSpec) -> NodeError {
    NodeError::Store(format!(
        "no durable state for node {} at {}; if this node is new or has been re-provisioned under a new identity, run \
         `blossom node init` or pass `--init-fresh`",
        spec.identity.node_name,
        spec.dir.display()
    ))
}

/// The catch-up of a database's views (DATABASE.md §8) from the deltas of the ticks since them, in order: the net
/// change of all but the last, and the last.
fn catch_up_of(mut ticks: Vec<(u64, Delta)>) -> blossom_ir::tick::CatchUp {
    let Some((last_tick, last)) = ticks.pop() else {
        return blossom_ir::tick::CatchUp::default();
    };
    // The net change: a row inserted then deleted (or deleted then inserted) is no change.
    let mut ins: BTreeMap<RelId, std::collections::BTreeSet<Row>> = BTreeMap::new();
    let mut del: BTreeMap<RelId, std::collections::BTreeSet<Row>> = BTreeMap::new();
    for (_, delta) in ticks {
        for (rel, (inserted, deleted)) in delta.changes {
            let (i, d) = (ins.entry(rel).or_default(), del.entry(rel).or_default());
            for row in deleted {
                if !i.remove(&row) {
                    d.insert(row);
                }
            }
            for row in inserted {
                if !d.remove(&row) {
                    i.insert(row);
                }
            }
        }
    }
    let changes = |m: BTreeMap<RelId, std::collections::BTreeSet<Row>>| -> BTreeMap<RelId, Vec<Row>> {
        m.into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(rel, rows)| (rel, rows.into_iter().collect()))
            .collect()
    };
    blossom_ir::tick::CatchUp {
        before: blossom_ir::tick::Changes {
            inserted: changes(ins),
            deleted: changes(del),
        },
        last: blossom_ir::tick::Changes {
            inserted: last
                .changes
                .iter()
                .map(|(r, (i, _))| (*r, i.clone()))
                .filter(|(_, i)| !i.is_empty())
                .collect(),
            deleted: last
                .changes
                .iter()
                .map(|(r, (_, d))| (*r, d.clone()))
                .filter(|(_, d)| !d.is_empty())
                .collect(),
        },
        last_tick,
    }
}
