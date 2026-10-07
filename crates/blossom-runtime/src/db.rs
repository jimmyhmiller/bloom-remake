//! The node's database (docs/design/DATABASE.md): its program's durable relations as of its released ticks, in a
//! versioned LSM tree under `<store>/db` (blossom-store's `lsm`), each row a key (`DurableCodec::row_key`) at the
//! tick that wrote it.
//!
//! The engine thread applies each released tick's durable delta; a database thread flushes the memtable when it
//! grows and compacts after. A recovery applies the WAL records after what the tables hold; the WAL keeps them,
//! because the committer truncates it only behind both the installed checkpoint and the database's flushed tick
//! ([`Database::flushed_tick`]). A database newer than its store (created for a store that had none) starts from the
//! recovered rows, at the last recovered tick.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_node::durable::{Delta, DurableCodec, DurableImage, DurableSchema};
use blossom_oracle::Row;
use blossom_store::lsm::{Lsm, LsmOptions, Op};
use blossom_store::{Lsn, Vfs};

use crate::RuntimeError;

/// The format of the database's keys (`DurableCodec::row_key`): 1, the order-preserving encoding. A database written
/// in an older one is rebuilt from the recovered rows when the node opens it; a newer one is refused.
pub const KEY_FORMAT: u32 = 1;

fn store_error(e: blossom_store::StoreError) -> RuntimeError {
    RuntimeError::Store(e)
}

/// Removes the tree under `db_dir` (its tables and manifest), so a database starts again there.
fn clear(fs: &dyn Vfs, db_dir: &Path) -> Result<(), RuntimeError> {
    let sst = db_dir.join("sst");
    match fs.list(&sst) {
        Err(blossom_store::StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(store_error(e)),
        Ok(files) => {
            for f in files {
                fs.remove(&f).map_err(store_error)?;
            }
            fs.sync_dir(&sst).map_err(store_error)?;
        }
    }
    for f in fs.list(db_dir).map_err(store_error)? {
        if f != sst {
            fs.remove(&f).map_err(store_error)?;
        }
    }
    fs.sync_dir(db_dir).map_err(store_error)
}

/// The first tick tables covering every tick up to `flushed` do not cover.
fn watermark(flushed: Option<u64>) -> u64 {
    flushed.map_or(0, |v| v.saturating_add(1))
}

/// Flushes `lsm`'s memtable, notes the first tick its tables do not cover in `mark`, and compacts what is due.
fn flush(lsm: &Lsm, mark: &AtomicU64) -> Result<(), RuntimeError> {
    let f = lsm.flush().map_err(store_error)?;
    mark.store(watermark(f.version), Ordering::SeqCst);
    while lsm.compact().map_err(store_error)? {}
    Ok(())
}

/// A node's database.
pub struct Database {
    lsm: Arc<Lsm>,
    artifact: Arc<BlsArtifact>,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    /// The first tick the tables do not cover (every tick below it is in them), for the committer's truncation.
    flushed: Arc<AtomicU64>,
    /// Asks the database thread to flush (closed when the database closes, which ends the thread).
    work: Arc<Mutex<Option<Sender<()>>>>,
}

impl Database {
    /// Opens (or creates) the database of the store `dir`. `fresh`: whether it had none before.
    pub fn open(
        fs: Arc<dyn Vfs>,
        dir: &Path,
        artifact: Arc<BlsArtifact>,
        names: Arc<[Arc<str>]>,
        opts: LsmOptions,
    ) -> Result<(Database, bool), RuntimeError> {
        let db_dir = dir.join("db");
        let fresh = match blossom_store::lsm::manifest_format(&*fs, &db_dir).map_err(store_error)? {
            None => true,
            Some(KEY_FORMAT) => false,
            // Keys of an older format: the tree goes, and the database starts again from the recovered rows.
            Some(f) if f < KEY_FORMAT => {
                clear(&*fs, &db_dir)?;
                true
            }
            // A newer build's database: rebuilding it would lose its history, so the node does not start.
            Some(f) => {
                return Err(RuntimeError::Config(format!(
                    "the store's database is in key format {f}, newer than this build's ({KEY_FORMAT})"
                )));
            }
        };
        let lsm = Lsm::open(
            fs,
            &db_dir,
            LsmOptions {
                format: KEY_FORMAT,
                ..opts
            },
        )
        .map_err(store_error)?;
        let flushed = Arc::new(AtomicU64::new(watermark(lsm.flushed().map_err(store_error)?.version)));
        let schema = DurableSchema::of(artifact.program.get());
        Ok((
            Database {
                lsm: Arc::new(lsm),
                artifact,
                schema,
                names,
                flushed,
                work: Arc::new(Mutex::new(None)),
            },
            fresh,
        ))
    }

    /// A stopped node's database, read without changing a file (`blossom query --store`): under the store's lock,
    /// its tables, and the WAL records after them applied in memory (the records the node's next recovery keeps).
    /// The lock comes back with it: the node cannot start while it is held.
    pub fn open_offline(
        dir: &Path,
        artifact: Arc<BlsArtifact>,
        names: Arc<[Arc<str>]>,
    ) -> Result<(Database, blossom_store::StoreLock), RuntimeError> {
        let fs: Arc<dyn Vfs> = Arc::new(blossom_store::RealFs);
        let lock = match blossom_store::StoreLock::acquire(&*fs, dir) {
            Ok(l) => l,
            Err(blossom_store::StoreError::Locked { pid, .. }) => {
                return Err(RuntimeError::Config(format!(
                    "the node is running (process {}): query it through its admin listener",
                    pid.trim()
                )));
            }
            Err(e) => return Err(store_error(e)),
        };
        let uuid = blossom_store::MetaStore::new(fs.clone(), dir)
            .read()
            .map_err(store_error)?
            .identity
            .store_uuid;
        let db_dir = dir.join("db");
        if blossom_store::lsm::manifest_format(&*fs, &db_dir).map_err(store_error)? != Some(KEY_FORMAT) {
            return Err(RuntimeError::Config(
                "the store has no database of this build's format: start the node once to (re)build it".into(),
            ));
        }
        let lsm = Lsm::open_read_only(
            fs.clone(),
            &db_dir,
            LsmOptions {
                format: KEY_FORMAT,
                ..LsmOptions::default()
            },
        )
        .map_err(store_error)?;
        let flushed = Arc::new(AtomicU64::new(watermark(lsm.flushed().map_err(store_error)?.version)));
        let db = Database {
            lsm: Arc::new(lsm),
            schema: DurableSchema::of(artifact.program.get()),
            artifact,
            names,
            flushed,
            work: Arc::new(Mutex::new(None)),
        };
        let scan = blossom_store::WalScan::scan(&*fs, &blossom_node::recovery::wal_dir(dir), uuid, false)
            .map_err(store_error)?;
        let codec = db.codec();
        let after = db.lsm.flushed().map_err(store_error)?.version;
        for (lsn, rec) in scan.records() {
            if after.is_some_and(|a| rec.tick <= a) {
                continue;
            }
            let payload = match rec.kind {
                blossom_node::recovery::KIND_DELTA => rec.payload.as_slice(),
                blossom_node::recovery::KIND_DELTA_BLOBS => blossom_node::recovery::logged_blobs(&rec.payload, *lsn)?.1,
                other => {
                    return Err(RuntimeError::Config(format!(
                        "the WAL record at LSN {} has kind {other}, which this build does not know",
                        lsn.0
                    )));
                }
            };
            db.apply_with(&codec, rec.tick, &codec.decode_delta(payload)?)?;
        }
        Ok((db, lock))
    }

    /// The codec of the program's durable rows.
    pub fn codec(&self) -> DurableCodec<'_> {
        DurableCodec::new(self.artifact.program.get(), &self.schema, self.names.clone())
    }

    /// The first tick the tables do not cover (shared with the committer).
    pub fn flushed_tick(&self) -> Arc<AtomicU64> {
        self.flushed.clone()
    }

    /// Asks the database thread for a flush (a no-op before it starts and after it closes): the committer asks when a
    /// truncation waits on the database.
    pub fn flush_request(&self) -> Box<dyn Fn() + Send> {
        let work = self.work.clone();
        Box::new(move || {
            if let Ok(w) = work.lock()
                && let Some(tx) = w.as_ref()
            {
                // The thread is gone only when the node stops.
                let _ = tx.send(());
            }
        })
    }

    /// Brings the database up to the recovered store: the WAL records after what its tables hold (`wal`, in order),
    /// or, for a database the store did not have (`fresh`), the recovered rows at the last recovered tick.
    pub fn recover(
        &self,
        fresh: bool,
        wal: &[(Lsn, u64, Delta)],
        image: &DurableImage,
        checkpoint_tick: Option<u64>,
    ) -> Result<(), RuntimeError> {
        let codec = self.codec();
        let last = wal.last().map(|(_, t, _)| *t).max(checkpoint_tick);
        if fresh {
            if let Some(last) = last {
                let mut changes = Vec::new();
                for (rel, rows) in &image.rows {
                    for row in rows {
                        changes.push((codec.row_key(*rel, row)?, Op::Put));
                    }
                }
                self.lsm.apply(last, last, changes).map_err(store_error)?;
                // The database knows nothing of the ticks before: an as-of read of one is refused, not answered
                // empty.
                self.lsm.raise_floor(last).map_err(store_error)?;
            }
            return Ok(());
        }
        let flushed = self.lsm.flushed().map_err(store_error)?.version;
        for (_, tick, delta) in wal.iter().filter(|(_, t, _)| flushed.is_none_or(|f| *t > f)) {
            self.apply_with(&codec, *tick, delta)?;
        }
        Ok(())
    }

    /// Starts the database thread: it flushes when asked, then compacts, then calls `flushed` (the committer may
    /// truncate further). A failed flush or compaction is `failed`'s, and ends the thread: the node faults, as for a
    /// failed WAL sync.
    /// It ends when the database goes (its request channel closes).
    pub fn start(
        &self,
        flushed: Box<dyn Fn() + Send>,
        failed: Box<dyn Fn(String) + Send>,
    ) -> Result<std::thread::JoinHandle<()>, RuntimeError> {
        let (tx, rx) = mpsc::channel::<()>();
        *self
            .work
            .lock()
            .map_err(|_| blossom_base::internal_error!("the database's work lock is poisoned"))? = Some(tx);
        let (lsm, mark) = (self.lsm.clone(), self.flushed.clone());
        std::thread::Builder::new()
            .name("database".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    while rx.try_recv().is_ok() {}
                    if let Err(e) = flush(&lsm, &mark) {
                        failed(format!("the database: {e}"));
                        return;
                    }
                    flushed();
                }
            })
            .map_err(RuntimeError::Io)
    }

    /// Closes the database thread's request channel: the thread ends after the flush it is doing.
    pub fn close(&self) {
        // A poisoned lock means the thread panicked holding it: it is gone either way.
        if let Ok(mut work) = self.work.lock() {
            *work = None;
        }
    }

    /// Flushes the memtable and compacts what is due, on the caller's thread.
    pub fn flush_now(&self) -> Result<(), RuntimeError> {
        flush(&self.lsm, &self.flushed)
    }

    /// Applies a released tick's durable delta.
    pub fn apply(&self, tick: u64, delta: &Delta) -> Result<(), RuntimeError> {
        let codec = self.codec();
        self.apply_with(&codec, tick, delta)?;
        if self.lsm.needs_flush().map_err(store_error)?
            && let Some(tx) = self
                .work
                .lock()
                .map_err(|_| blossom_base::internal_error!("the database's work lock is poisoned"))?
                .as_ref()
        {
            // The thread is gone only when the node stops.
            let _ = tx.send(());
        }
        Ok(())
    }

    fn apply_with(&self, codec: &DurableCodec<'_>, tick: u64, delta: &Delta) -> Result<(), RuntimeError> {
        if delta.is_empty() {
            return Ok(());
        }
        let mut changes = Vec::new();
        for (rel, (inserted, deleted)) in &delta.changes {
            for row in deleted {
                changes.push((codec.row_key(*rel, row)?, Op::Del));
            }
            for row in inserted {
                changes.push((codec.row_key(*rel, row)?, Op::Put));
            }
        }
        self.lsm.apply(tick, tick, changes).map_err(store_error)
    }

    /// The oldest tick an as-of read may ask for, and the newest applied (`None`: none yet).
    pub fn range(&self) -> Result<(u64, Option<u64>), RuntimeError> {
        Ok((
            self.lsm.floor().map_err(store_error)?,
            self.lsm.applied().map_err(store_error)?,
        ))
    }

    /// The rows of the durable relation `rel` whose leading columns are `leading`, as of `tick`.
    pub fn rows(&self, rel: RelId, leading: &[blossom_value::Value], tick: u64) -> Result<Vec<Row>, RuntimeError> {
        let codec = self.codec();
        let prefix = codec.key_prefix(rel, leading)?;
        let mut out = Vec::new();
        for key in self.lsm.scan(&prefix, tick).map_err(store_error)? {
            out.push(codec.key_row(rel, &key)?);
        }
        Ok(out)
    }

    /// The rows of `rel` whose leading columns are `leading` and whose next column lies within `lo` and `hi`, as of
    /// `tick`: a range scan, in key (value) order.
    pub fn rows_range(
        &self,
        rel: RelId,
        leading: &[blossom_value::Value],
        lo: std::ops::Bound<&blossom_value::Value>,
        hi: std::ops::Bound<&blossom_value::Value>,
        tick: u64,
    ) -> Result<Vec<Row>, RuntimeError> {
        let codec = self.codec();
        let (start, end) = codec.key_range(rel, leading, lo, hi)?;
        let mut out = Vec::new();
        for key in self.lsm.scan_range(&start, end.as_deref(), tick).map_err(store_error)? {
            out.push(codec.key_row(rel, &key)?);
        }
        Ok(out)
    }

    /// Every durable relation's rows as of `tick`.
    pub fn image(&self, tick: u64) -> Result<DurableImage, RuntimeError> {
        let mut image = DurableImage::default();
        for (rel, _, _) in &self.schema.rels {
            image
                .rows
                .insert(*rel, self.rows(*rel, &[], tick)?.into_iter().collect());
        }
        Ok(image)
    }

    /// The durable relations: id and name.
    pub fn relations(&self) -> Vec<(RelId, Arc<str>)> {
        self.schema.rels.iter().map(|(r, n, _)| (*r, n.clone())).collect()
    }
}
