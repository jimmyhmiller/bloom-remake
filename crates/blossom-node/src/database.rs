//! The node's database (docs/design/DATABASE.md): its program's durable relations as of its released ticks, in a
//! versioned LSM tree under `<store>/db` (blossom-store's `lsm`), each row a key (`DurableCodec::row_key`) at the
//! tick that wrote it. It is the node's durable state: recovery starts from it ([`crate::recovery`]), the drivers
//! feed it each released tick's delta and flush it, and the WAL truncates behind its flushes
//! (`FileWal::truncation`). Its methods take `&self`, so a runtime may flush on a thread of its own while the engine
//! thread applies.

use std::path::Path;
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::ValidatedProgram;
use blossom_oracle::Row;
use blossom_store::lsm::{Flushed, Lsm, LsmOptions, Op};
use blossom_store::{StoreError, Vfs, WalScan};

use crate::NodeError;
use crate::durable::{Delta, DurableCodec, DurableImage, DurableSchema};

/// The format of the database's keys (`DurableCodec::row_key`): 1, the order-preserving encoding. A database written
/// in an older one is rebuilt from the recovered rows when its node opens it; a newer one is refused.
pub const KEY_FORMAT: u32 = 1;

/// Removes the tree under `db_dir` (its tables and manifest), so a database starts again there.
fn clear(fs: &dyn Vfs, db_dir: &Path) -> Result<(), NodeError> {
    let sst = db_dir.join("sst");
    match fs.list(&sst) {
        Err(StoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(files) => {
            for f in files {
                fs.remove(&f)?;
            }
            fs.sync_dir(&sst)?;
        }
    }
    for f in fs.list(db_dir)? {
        if f != sst {
            fs.remove(&f)?;
        }
    }
    Ok(fs.sync_dir(db_dir)?)
}

/// A node's database.
pub struct Database {
    lsm: Lsm,
    program: ValidatedProgram,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
}

impl Database {
    /// Opens (or creates) the database of the store `dir`, for `program` (whose durable relations it holds) and the
    /// deployment's node names by id. `fresh`: whether there was none (or one in an older key format, which goes).
    pub fn open(
        fs: Arc<dyn Vfs>,
        dir: &Path,
        program: &ValidatedProgram,
        names: Arc<[Arc<str>]>,
        opts: LsmOptions,
    ) -> Result<(Database, bool), NodeError> {
        let db_dir = dir.join("db");
        let fresh = match blossom_store::lsm::manifest_format(&*fs, &db_dir)? {
            None => true,
            Some(KEY_FORMAT) => false,
            // Keys of an older format: the tree goes, and the database starts again from the recovered rows.
            Some(f) if f < KEY_FORMAT => {
                clear(&*fs, &db_dir)?;
                true
            }
            // A newer build's database: rebuilding it would lose its history, so the node does not start.
            Some(f) => {
                return Err(NodeError::Store(format!(
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
        )?;
        Ok((Database::with(lsm, program, names), fresh))
    }

    fn with(lsm: Lsm, program: &ValidatedProgram, names: Arc<[Arc<str>]>) -> Database {
        Database {
            lsm,
            schema: DurableSchema::of(program.get()),
            program: program.clone(),
            names,
        }
    }

    /// A stopped node's database read without changing a file (a tool's: `blossom query --store`): its tables, and
    /// the WAL records after them applied in memory (the records the node's next recovery keeps). The caller holds the
    /// store's lock.
    pub fn open_read_only(
        fs: Arc<dyn Vfs>,
        dir: &Path,
        program: &ValidatedProgram,
        names: Arc<[Arc<str>]>,
    ) -> Result<Database, NodeError> {
        let uuid = blossom_store::MetaStore::new(fs.clone(), dir)
            .read()?
            .identity
            .store_uuid;
        let db_dir = dir.join("db");
        if blossom_store::lsm::manifest_format(&*fs, &db_dir)? != Some(KEY_FORMAT) {
            return Err(NodeError::Store(
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
        )?;
        let db = Database::with(lsm, program, names);
        let scan = WalScan::scan(&*fs, &crate::recovery::wal_dir(dir), uuid, false)?;
        let codec = db.codec();
        let after = db.flushed()?;
        for (lsn, rec) in scan.records() {
            if after.is_some_and(|a| rec.tick <= a) {
                continue;
            }
            let payload = crate::recovery::record_delta(rec, *lsn)?;
            db.apply_with(&codec, rec.tick, &codec.decode_delta(payload)?)?;
        }
        Ok(db)
    }

    /// The codec of the program's durable rows.
    pub fn codec(&self) -> DurableCodec<'_> {
        DurableCodec::new(self.program.get(), &self.schema, self.names.clone())
    }

    /// Starts the database from `image`, the store's rows as of `tick` (a store that had none: its checkpoint and
    /// WAL replayed, or nothing at all). The ticks before have no history here: an as-of read of one is refused.
    pub fn bootstrap(&self, tick: u64, image: &DurableImage) -> Result<(), NodeError> {
        let codec = self.codec();
        let mut changes = Vec::new();
        for (rel, rows) in &image.rows {
            for row in rows {
                changes.push((codec.row_key(*rel, row)?, Op::Put));
            }
        }
        self.lsm.apply(tick, tick, changes)?;
        Ok(self.lsm.raise_floor(tick)?)
    }

    /// Applies a released tick's durable delta (none: the tick changed no durable row).
    pub fn apply(&self, tick: u64, delta: &Delta) -> Result<(), NodeError> {
        let codec = self.codec();
        self.apply_with(&codec, tick, delta)
    }

    fn apply_with(&self, codec: &DurableCodec<'_>, tick: u64, delta: &Delta) -> Result<(), NodeError> {
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
        Ok(self.lsm.apply(tick, tick, changes)?)
    }

    /// Whether the memtable has grown past its size.
    pub fn needs_flush(&self) -> Result<bool, NodeError> {
        Ok(self.lsm.needs_flush()?)
    }

    /// Writes the memtable to the tables and compacts what is due: what the tables now cover (the WAL may truncate
    /// behind it).
    pub fn flush(&self) -> Result<Flushed, NodeError> {
        let flushed = self.lsm.flush()?;
        while self.lsm.compact()? {}
        Ok(flushed)
    }

    /// The tick every tick up to which is in the tables (`None`: none is).
    pub fn flushed(&self) -> Result<Option<u64>, NodeError> {
        Ok(self.lsm.flushed()?.version())
    }

    /// The oldest tick an as-of read may ask for, and the newest applied (`None`: none yet).
    pub fn range(&self) -> Result<(u64, Option<u64>), NodeError> {
        Ok((self.lsm.floor()?, self.lsm.applied()?))
    }

    /// The rows of the durable relation `rel` whose leading columns are `leading`, as of `tick`.
    pub fn rows(&self, rel: RelId, leading: &[blossom_value::Value], tick: u64) -> Result<Vec<Row>, NodeError> {
        let codec = self.codec();
        let prefix = codec.key_prefix(rel, leading)?;
        let mut out = Vec::new();
        for key in self.lsm.scan(&prefix, tick)? {
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
    ) -> Result<Vec<Row>, NodeError> {
        let codec = self.codec();
        let (start, end) = codec.key_range(rel, leading, lo, hi)?;
        let mut out = Vec::new();
        for key in self.lsm.scan_range(&start, end.as_deref(), tick)? {
            out.push(codec.key_row(rel, &key)?);
        }
        Ok(out)
    }

    /// Whether `rel` holds a row led by `principal` as of the newest tick applied (an ACL's `principal in REL`, read
    /// by prefix).
    pub fn committed(&self, rel: RelId, principal: &str) -> Result<bool, NodeError> {
        let Some(tick) = self.lsm.applied()? else {
            return Ok(false);
        };
        let lead = [blossom_value::Value::Principal(Arc::from(principal))];
        Ok(!self.rows(rel, &lead, tick)?.is_empty())
    }

    /// Every durable relation's rows as of the newest tick applied (none applied: none).
    pub fn latest_image(&self) -> Result<DurableImage, NodeError> {
        match self.lsm.applied()? {
            Some(t) => self.image(t),
            None => Ok(DurableImage::default()),
        }
    }

    /// Every durable relation's rows as of `tick`.
    pub fn image(&self, tick: u64) -> Result<DurableImage, NodeError> {
        let mut image = DurableImage::default();
        for (rel, _, _) in &self.schema.rels {
            let rows = self.rows(*rel, &[], tick)?;
            if !rows.is_empty() {
                image.rows.insert(*rel, rows.into_iter().collect());
            }
        }
        Ok(image)
    }

    /// What the tree holds (tables, versions, floor), for tools and tests.
    pub fn info(&self) -> Result<blossom_store::lsm::TreeInfo, NodeError> {
        Ok(self.lsm.info()?)
    }

    /// The durable relations: id and name.
    pub fn relations(&self) -> Vec<(RelId, Arc<str>)> {
        self.schema.rels.iter().map(|(r, n, _)| (*r, n.clone())).collect()
    }
}
