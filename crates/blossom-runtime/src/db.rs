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

fn store_error(e: blossom_store::StoreError) -> RuntimeError {
    RuntimeError::Store(e)
}

/// Flushes `lsm`'s memtable, notes the tick its tables now cover in `mark`, and compacts what is due.
fn flush(lsm: &Lsm, mark: &AtomicU64) -> Result<(), RuntimeError> {
    if let Some(f) = lsm.flush().map_err(store_error)? {
        mark.store(f.version, Ordering::SeqCst);
    }
    while lsm.compact().map_err(store_error)? {}
    Ok(())
}

/// A node's database.
pub struct Database {
    lsm: Arc<Lsm>,
    artifact: Arc<BlsArtifact>,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    /// The tick the tables cover, for the committer's truncation.
    flushed: Arc<AtomicU64>,
    /// Asks the database thread to flush (dropped when the database goes, which ends the thread).
    work: Mutex<Option<Sender<()>>>,
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
        let fresh = !fs.list(dir).map_err(store_error)?.iter().any(|p| p == &db_dir)
            || !fs
                .list(&db_dir)
                .map_err(store_error)?
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "MANIFEST"));
        let lsm = Lsm::open(fs, &db_dir, opts).map_err(store_error)?;
        let flushed = Arc::new(AtomicU64::new(lsm.flushed().map_err(store_error)?.version));
        let schema = DurableSchema::of(artifact.program.get());
        Ok((
            Database {
                lsm: Arc::new(lsm),
                artifact,
                schema,
                names,
                flushed,
                work: Mutex::new(None),
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
        let lsm = Lsm::open_read_only(fs.clone(), &dir.join("db"), LsmOptions::default()).map_err(store_error)?;
        let flushed = Arc::new(AtomicU64::new(lsm.flushed().map_err(store_error)?.version));
        let db = Database {
            lsm: Arc::new(lsm),
            schema: DurableSchema::of(artifact.program.get()),
            artifact,
            names,
            flushed,
            work: Mutex::new(None),
        };
        let scan = blossom_store::WalScan::scan(&*fs, &blossom_node::recovery::wal_dir(dir), uuid, false)
            .map_err(store_error)?;
        let codec = db.codec();
        let after = db.lsm.flushed().map_err(store_error)?.version;
        for (lsn, rec) in scan.records() {
            if rec.tick <= after {
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

    /// The tick the tables cover (shared with the committer).
    pub fn flushed_tick(&self) -> Arc<AtomicU64> {
        self.flushed.clone()
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
        if fresh && let Some(last) = last {
            let mut changes = Vec::new();
            for (rel, rows) in &image.rows {
                for row in rows {
                    changes.push((codec.row_key(*rel, row)?, Op::Put));
                }
            }
            self.lsm.apply(last, last, changes).map_err(store_error)?;
            return Ok(());
        }
        let flushed = self.lsm.flushed().map_err(store_error)?.version;
        for (_, tick, delta) in wal.iter().filter(|(_, t, _)| *t > flushed) {
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

    /// The newest tick applied, and the oldest an as-of read may ask for.
    pub fn range(&self) -> Result<(u64, u64), RuntimeError> {
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
            let row = codec.key_row(rel, &key)?;
            // The prefix may cover fewer leading columns than asked (field numbers reorder the encoding).
            if row.iter().zip(leading).all(|(a, b)| a == b) {
                out.push(row);
            }
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
