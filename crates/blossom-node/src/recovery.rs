//! Opening a node's store and recovering its durable state (ARCHITECTURE §5.6).
//!
//! In order, and a crash anywhere in it leaves the old checkpoint and WAL intact:
//!
//! 1. take `LOCK`; read `META` and check the identity against the deployment;
//! 2. load the checkpoint named by `CURRENT`;
//! 3. replay the WAL records after the checkpoint (a torn tail is truncated: it was never synced, so never
//!    acknowledged);
//! 4. reserve ticks: boot at `reserved + 1` (at tick 0 on the first boot), and make `boot + 65 536` the new bound;
//! 5. count the restart, pick the boot instant `max(wall, last_now + 1 ns)` (`META.last_now` bounds every instant a
//!    released tick had), reserve time up to one `TIME_STEP` past it, write `META`, and open a new WAL segment.
//!
//! The layout under the node's directory: `LOCK`, `META`, `CURRENT`, `ckpt/<tick>/`, `wal/<seq>.seg`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_base::internal_error;
use blossom_ir::core::Program;
use blossom_store::{
    Certification, CheckpointId, FileCheckpoints, FileWal, Lsn, MetaRecord, MetaStore, OpenMode, SegmentHeader, StoreError,
    StoreIdentity, StoreLock, Vfs, WalScan, durable_dir,
};
use blossom_value::time::{Instant, Tick};
use blossom_wire::codec::put_varint;

use crate::NodeError;
use crate::durable::{DurableCodec, DurableImage, DurableSchema};
use crate::node::{Boot, RESERVE_STEP, TIME_STEP};

/// The on-disk format of this build.
pub const FORMAT: u16 = 1;
/// The WAL record kind of a tick's durable delta.
pub const KIND_DELTA: u8 = 1;

/// Where and as whom a node's store is opened.
#[derive(Clone, Debug)]
pub struct StoreSpec {
    pub dir: PathBuf,
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
    pub checkpoints: FileCheckpoints,
    pub meta: MetaStore,
    /// The `META` record as written at this boot.
    pub record: MetaRecord,
    pub lock: StoreLock,
    /// The checkpoint recovery started from.
    pub checkpoint: Option<CheckpointId>,
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
            .field("checkpoint", &self.checkpoint)
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
    program: &Program,
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
    let schema = DurableSchema::of(program);
    let codec = DurableCodec::new(program, &schema, names);
    // 2. The checkpoint.
    let checkpoints = FileCheckpoints::new(fs.clone(), dir)?;
    let checkpoint = checkpoints.current()?;
    let mut image = match checkpoint {
        Some(id) => {
            let mut image = codec.decode_image(&checkpoints.read(id)?)?;
            // A checkpoint is its full image and the delta layers after it, applied in order.
            for layer in checkpoints.read_layers(id)? {
                image.apply(&codec.decode_delta(&layer)?);
            }
            image
        }
        None => DurableImage::default(),
    };
    // 3. The WAL after it.
    let wdir = wal_dir(dir);
    durable_dir(&*fs, &wdir)?;
    let scan = WalScan::scan_certified(&*fs, &wdir, uuid, true, record.certification)?;
    let mut last_now = record.last_now;
    let mut last_tick: Option<u64> = checkpoint.map(|c| c.tick);
    let mut replayed = 0;
    for (lsn, rec) in scan.records() {
        if checkpoint.is_some_and(|c| *lsn < c.lsn) {
            continue;
        }
        if rec.kind != KIND_DELTA {
            return Err(NodeError::Store(format!(
                "WAL record at LSN {} has kind {}, which this build does not know",
                lsn.0, rec.kind
            )));
        }
        if last_tick.is_some_and(|t| rec.tick <= t) {
            return Err(NodeError::Store(format!(
                "WAL record at LSN {} is for tick {}, not after tick {}",
                lsn.0,
                rec.tick,
                last_tick.unwrap_or_default()
            )));
        }
        image.apply(&codec.decode_delta(&rec.payload)?);
        last_tick = Some(rec.tick);
        last_now = last_now.max(rec.now);
        replayed += 1;
    }
    // The blobs the recovered rows hold were made durable before their records synced: check it, so a store that
    // lost one refuses to start rather than failing a later tick that reads it.
    let blobs = Arc::new(blossom_store::BlobStore::open(fs.clone(), dir)?);
    let mut referenced = std::collections::BTreeSet::new();
    for rows in image.rows.values() {
        for r in rows {
            for v in r.iter() {
                blossom_value::blobs_in(v, &mut referenced);
            }
        }
    }
    // Every blob the store holds is durable; those no recovered row holds (a crash between a blob's write and its
    // record's sync, or rows a replayed record deleted) are the node's first candidates for collection.
    let stored: std::collections::BTreeSet<blossom_value::BlobRef> = blobs.list()?.into_iter().collect();
    if let Some(missing) = referenced.iter().find(|b| !stored.contains(b)) {
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
    Ok(Opened {
        boot: Boot {
            image,
            tick: Tick(boot_tick),
            reserved: Tick(reserved),
            time_reserved,
            now,
            // Durable state was reloaded iff an earlier incarnation's first boot tick became durable (it always
            // leaves a WAL record, which a checkpoint may since cover): a crash before that boots fresh again.
            recovered: checkpoint.is_some() || replayed > 0,
            incarnation: record.restarts,
            blobs: blobs.clone(),
            stored,
            at_checkpoint: checkpoint.is_some() && replayed == 0,
        },
        wal,
        checkpoints,
        meta,
        record,
        lock,
        checkpoint,
        replayed,
        blobs,
    })
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
