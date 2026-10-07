//! The node's database (docs/design/DATABASE.md): its program's durable relations as of its released ticks, in a
//! versioned LSM tree under `<store>/db` (blossom-store's `lsm`), each row a key (`DurableCodec::row_key`) at the
//! tick that wrote it. It is the node's durable state: recovery starts from it ([`crate::recovery`]), the drivers
//! feed it each released tick's delta and flush it, and the WAL truncates behind its flushes
//! (`FileWal::truncation`). Its methods take `&self`, so a runtime may flush on a thread of its own while the engine
//! thread applies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use blossom_base::RelId;
use blossom_ir::ValidatedProgram;
use blossom_oracle::Row;
use blossom_store::lsm::{Flushed, Lsm, LsmOptions, Op};
use blossom_store::{StoreError, Vfs, WalScan};
use blossom_value::BlobRef;

use crate::NodeError;
use crate::durable::{Delta, DurableCodec, DurableImage, DurableSchema};

/// The format of the database's keys (`DurableCodec::row_key`): 1, the order-preserving encoding. A database written
/// in an older one is rebuilt from the recovered rows when its node opens it; a newer one is refused.
pub const KEY_FORMAT: u32 = 1;

/// How many keys a scan of the tree holds at once (`Lsm::scan_page`).
const PAGE_KEYS: usize = 512;

/// 8 bytes naming a keyspace: BLAKE3 over `parts`, each after its length.
fn label(parts: &[&[u8]]) -> [u8; 8] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_be_bytes());
        h.update(p);
    }
    let mut tag = [0u8; 8];
    for (t, b) in tag.iter_mut().zip(h.finalize().as_bytes()) {
        *t = *b;
    }
    tag
}

/// The keyspace of the definitions of the derived keyspaces (docs/design/DATABASE.md §7): one key each.
fn defs_tag() -> [u8; 8] {
    label(&[b"keyspace definitions"])
}

/// A definition key's kind: an index (then the relation's tag and the columns, `u32` big-endian each), or the blob
/// keyspaces of every relation.
const DEF_INDEX: u8 = 1;
const DEF_BLOBS: u8 = 2;
/// A durable view's keyspace (docs/design/DATABASE.md §8): then its definition's hash (32 bytes) and the generation
/// of views it belongs to (8, big-endian). Written with the view's complete rows.
const DEF_VIEW: u8 = 3;

/// The tags of a durable view's keyspaces: its rows, and the counts of its rows of more than one support.
fn view_tags(def: &[u8; 32], generation: u64) -> ([u8; 8], [u8; 8]) {
    let rows = label(&[b"view", def, &generation.to_be_bytes()]);
    (rows, label(&[b"view counts", &rows]))
}

fn cols_bytes(cols: &[usize]) -> Result<Vec<u8>, NodeError> {
    let mut out = Vec::with_capacity(cols.len() * 4);
    for c in cols {
        let c = u32::try_from(*c).map_err(|_| blossom_base::internal_error!("column {c} out of range"))?;
        out.extend_from_slice(&c.to_be_bytes());
    }
    Ok(out)
}

/// The tag of the index of the relation tagged `rel_tag` on `cols`, in that order.
fn index_tag(rel_tag: &[u8; 8], cols: &[usize]) -> Result<[u8; 8], NodeError> {
    Ok(label(&[b"index", rel_tag, &cols_bytes(cols)?]))
}

/// The tag of the blob keyspace of the relation tagged `rel_tag`: a key per blob a row holds, after the blob.
fn blob_tag(rel_tag: &[u8; 8]) -> [u8; 8] {
    label(&[b"blobs", rel_tag])
}

/// The derived keyspaces the tree keeps with every apply (docs/design/DATABASE.md §7).
#[derive(Default)]
struct Derived {
    /// Each relation's indexes: their columns, in key order.
    indexes: BTreeMap<RelId, BTreeSet<Vec<usize>>>,
    /// Whether the blob keyspaces are kept.
    blobs: bool,
    /// Definitions made while no version was applied: written with the next one.
    unwritten: Vec<Vec<u8>>,
    /// The durable views of this run: their keyspaces' tags (rows, counts).
    views: BTreeMap<RelId, ([u8; 8], [u8; 8])>,
    /// The views' definitions the database holds: each definition's newest generation.
    view_defs: BTreeMap<[u8; 32], u64>,
    /// Indexes of keyspaces no durable relation is known by (views', until they are opened).
    orphan_indexes: BTreeMap<[u8; 8], BTreeSet<Vec<usize>>>,
}

impl Derived {
    /// The tag of `rel`'s rows: a durable view's keyspace, or the durable relation's.
    fn tag(&self, codec: &DurableCodec<'_>, rel: RelId) -> Result<[u8; 8], NodeError> {
        match self.views.get(&rel) {
            Some((rows, _)) => Ok(*rows),
            None => codec.rel_tag(rel),
        }
    }
}

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
///
/// It is the cold side of the engine's tiered tables (`blossom_engine::ColdTables`): their probes read it at its
/// newest applied version.
pub struct Database {
    lsm: Lsm,
    program: ValidatedProgram,
    schema: DurableSchema,
    names: Arc<[Arc<str>]>,
    /// The durable relations by their tags.
    tags: BTreeMap<[u8; 8], RelId>,
    /// The derived keyspaces, held while a version is applied or one is built.
    derived: Mutex<Derived>,
    read_only: bool,
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
        let db = Database::with(lsm, program, names, false)?;
        db.keep_blobs()?;
        Ok((db, fresh))
    }

    fn with(
        lsm: Lsm,
        program: &ValidatedProgram,
        names: Arc<[Arc<str>]>,
        read_only: bool,
    ) -> Result<Database, NodeError> {
        let mut db = Database {
            lsm,
            schema: DurableSchema::of(program.get()),
            program: program.clone(),
            names,
            tags: BTreeMap::new(),
            derived: Mutex::new(Derived::default()),
            read_only,
        };
        let codec = db.codec();
        let mut tags = BTreeMap::new();
        for (rel, _, _) in &db.schema.rels {
            tags.insert(codec.rel_tag(*rel)?, *rel);
        }
        drop(codec);
        db.tags = tags;
        db.load_definitions()?;
        Ok(db)
    }

    fn derived(&self) -> Result<std::sync::MutexGuard<'_, Derived>, NodeError> {
        self.derived
            .lock()
            .map_err(|_| blossom_base::internal_error!("the database's keyspace lock is poisoned").into())
    }

    /// Reads the derived keyspaces' definitions (those of relations this program has no more are left alone: no
    /// apply keeps them, nor their relation's rows).
    fn load_definitions(&self) -> Result<(), NodeError> {
        let Some(at) = self.lsm.applied()? else {
            return Ok(());
        };
        let tag = defs_tag();
        let mut d = self.derived()?;
        for key in self.lsm.scan(&tag, at)? {
            let body = key.get(tag.len()..).unwrap_or_default();
            match body.split_first() {
                Some((&DEF_BLOBS, [])) => d.blobs = true,
                Some((&DEF_INDEX, rest)) if rest.len() >= 8 && (rest.len() - 8) % 4 == 0 => {
                    let (rel_tag, cols) = rest.split_at(8);
                    let cols: Vec<usize> = cols
                        .chunks_exact(4)
                        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
                        .map(|c| u32::from_be_bytes(c) as usize)
                        .collect();
                    let Ok(rel_tag) = <[u8; 8]>::try_from(rel_tag) else {
                        continue;
                    };
                    match self.tags.get(&rel_tag) {
                        Some(rel) => {
                            d.indexes.entry(*rel).or_default().insert(cols);
                        }
                        None => {
                            d.orphan_indexes.entry(rel_tag).or_default().insert(cols);
                        }
                    }
                }
                Some((&DEF_VIEW, rest)) if rest.len() == 40 => {
                    let (def, generation) = rest.split_at(32);
                    if let (Ok(def), Ok(generation)) = (<[u8; 32]>::try_from(def), <[u8; 8]>::try_from(generation)) {
                        let generation = u64::from_be_bytes(generation);
                        let newest = d.view_defs.entry(def).or_insert(generation);
                        *newest = (*newest).max(generation);
                    }
                }
                _ => {
                    return Err(NodeError::Store(format!(
                        "the database holds a keyspace definition this build does not know ({body:?})"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Keeps the blob keyspaces from now on: built from the rows at the applied version when the tree has rows and
    /// none yet (a database from before them), in one amendment with their definition.
    fn keep_blobs(&self) -> Result<(), NodeError> {
        let mut d = self.derived()?;
        if d.blobs {
            return Ok(());
        }
        let mut def = defs_tag().to_vec();
        def.push(DEF_BLOBS);
        let Some(at) = self.lsm.applied()? else {
            d.blobs = true;
            d.unwritten.push(def);
            return Ok(());
        };
        let codec = self.codec();
        let mut changes = Vec::new();
        for (rel, _, _) in &self.schema.rels {
            let tag = codec.rel_tag(*rel)?;
            self.each_row(&codec, &tag, *rel, at, &mut |row| {
                for b in row_blobs(&row) {
                    changes.push((Self::blob_key(&codec, *rel, &b, &row)?, Op::Put));
                }
                Ok(())
            })?;
        }
        changes.push((def, Op::Put));
        self.lsm.amend(changes)?;
        d.blobs = true;
        Ok(())
    }

    /// The key of `row` of `rel` in the blob keyspace, under blob `b`.
    fn blob_key(codec: &DurableCodec<'_>, rel: RelId, b: &BlobRef, row: &Row) -> Result<Vec<u8>, NodeError> {
        let mut tag = blob_tag(&codec.rel_tag(rel)?).to_vec();
        tag.extend_from_slice(&b.hash);
        tag.extend_from_slice(&b.len.to_be_bytes());
        codec.tagged_key(&tag, rel, &[], row)
    }

    /// Calls `each` with every row of `rel` as of `at`, a page of keys at a time.
    fn each_row(
        &self,
        codec: &DurableCodec<'_>,
        tag: &[u8; 8],
        rel: RelId,
        at: u64,
        each: &mut dyn FnMut(Row) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let end = crate::keycode::successor(tag);
        self.each_key(tag, end.as_deref(), at, &mut |key| {
            each(codec.tagged_row(tag, rel, key)?)
        })
    }

    /// Calls `each` with every key from `start` to `end` as of `at`, a page at a time.
    fn each_key(
        &self,
        start: &[u8],
        end: Option<&[u8]>,
        at: u64,
        each: &mut dyn FnMut(&[u8]) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut from = start.to_vec();
        loop {
            let page = self.lsm.scan_page(&from, end, at, PAGE_KEYS)?;
            for key in &page.keys {
                each(key)?;
            }
            match page.next {
                Some(n) => from = n,
                None => return Ok(()),
            }
        }
    }

    /// Keeps an index of `rel` on `cols` from now on (a probe of a tiered table on columns its keys do not lead
    /// with): built from the rows at the applied version, in one amendment with its definition.
    pub fn keep_index(&self, rel: RelId, cols: &[usize]) -> Result<(), NodeError> {
        let mut d = self.derived()?;
        if d.indexes.get(&rel).is_some_and(|i| i.contains(cols)) {
            return Ok(());
        }
        if self.read_only {
            return Err(NodeError::Store("a database opened read-only builds no index".into()));
        }
        let codec = self.codec();
        let rel_tag = d.tag(&codec, rel)?;
        let mut def = defs_tag().to_vec();
        def.push(DEF_INDEX);
        def.extend_from_slice(&rel_tag);
        def.extend_from_slice(&cols_bytes(cols)?);
        match self.lsm.applied()? {
            None => d.unwritten.push(def),
            Some(at) => {
                let tag = index_tag(&rel_tag, cols)?;
                let mut changes = Vec::new();
                self.each_row(&codec, &rel_tag, rel, at, &mut |row| {
                    changes.push((codec.tagged_key(&tag, rel, cols, &row)?, Op::Put));
                    Ok(())
                })?;
                changes.push((def, Op::Put));
                self.lsm.amend(changes)?;
            }
        }
        d.indexes.entry(rel).or_default().insert(cols.to_vec());
        Ok(())
    }

    /// The keys a change of `row` of `rel` writes: its own, and one in each derived keyspace that holds it.
    fn expand(
        codec: &DurableCodec<'_>,
        d: &Derived,
        rel: RelId,
        row: &Row,
        op: Op,
        out: &mut Vec<(Vec<u8>, Op)>,
    ) -> Result<(), NodeError> {
        let rel_tag = d.tag(codec, rel)?;
        let all: Vec<usize> = (0..row.len()).collect();
        out.push((codec.tagged_key(&rel_tag, rel, &all, row)?, op));
        if let Some(indexes) = d.indexes.get(&rel) {
            for cols in indexes {
                out.push((codec.tagged_key(&index_tag(&rel_tag, cols)?, rel, cols, row)?, op));
            }
        }
        if d.blobs {
            for b in row_blobs(row) {
                out.push((Self::blob_key(codec, rel, &b, row)?, op));
            }
        }
        Ok(())
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
        let db = Database::with(lsm, program, names, true)?;
        let scan = WalScan::scan(&*fs, &crate::recovery::wal_dir(dir), uuid, false)?;
        let codec = db.codec();
        let after = db.flushed()?;
        for (lsn, rec) in scan.records() {
            if after.is_some_and(|a| rec.tick <= a) {
                continue;
            }
            let payload = crate::recovery::record_delta(rec, *lsn)?;
            db.apply_with(&codec, rec.tick, &codec.decode_delta(payload)?, &BTreeMap::new())?;
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
        let mut d = self.derived()?;
        let mut changes = Vec::new();
        for (rel, rows) in &image.rows {
            for row in rows {
                Self::expand(&codec, &d, *rel, row, Op::Put, &mut changes)?;
            }
        }
        changes.extend(d.unwritten.drain(..).map(|k| (k, Op::Put)));
        self.lsm.apply(tick, tick, changes)?;
        Ok(self.lsm.raise_floor(tick)?)
    }

    /// Applies a released tick's durable delta (none: the tick changed no durable row).
    pub fn apply(&self, tick: u64, delta: &Delta) -> Result<(), NodeError> {
        let codec = self.codec();
        self.apply_with(&codec, tick, delta, &BTreeMap::new())
    }

    /// Applies a released tick's durable delta and its changes to the durable views (each changed row's support
    /// before and after, DATABASE.md §8), as one version.
    pub fn apply_tick(
        &self,
        tick: u64,
        delta: &Delta,
        views: &BTreeMap<RelId, Vec<(Row, u64, u64)>>,
    ) -> Result<(), NodeError> {
        let codec = self.codec();
        self.apply_with(&codec, tick, delta, views)
    }

    fn apply_with(
        &self,
        codec: &DurableCodec<'_>,
        tick: u64,
        delta: &Delta,
        views: &BTreeMap<RelId, Vec<(Row, u64, u64)>>,
    ) -> Result<(), NodeError> {
        if delta.is_empty() && views.values().all(Vec::is_empty) {
            return Ok(());
        }
        let mut d = self.derived()?;
        let mut changes = Vec::new();
        for (rel, (inserted, deleted)) in &delta.changes {
            for row in deleted {
                Self::expand(codec, &d, *rel, row, Op::Del, &mut changes)?;
            }
            for row in inserted {
                Self::expand(codec, &d, *rel, row, Op::Put, &mut changes)?;
            }
        }
        for (rel, rows) in views {
            let counts =
                d.views.get(rel).map(|t| t.1).ok_or_else(|| {
                    blossom_base::internal_error!("changes to {rel:?}, which is no durable view here")
                })?;
            for (row, before, after) in rows {
                // The row's key (and its indexes') changes only when it comes or goes; a count key holds a support
                // of more than one.
                match (*before > 0, *after > 0) {
                    (false, true) => Self::expand(codec, &d, *rel, row, Op::Put, &mut changes)?,
                    (true, false) => Self::expand(codec, &d, *rel, row, Op::Del, &mut changes)?,
                    _ => {}
                }
                let all: Vec<usize> = (0..row.len()).collect();
                let count_key = |n: u64| -> Result<Vec<u8>, NodeError> {
                    let mut k = codec.tagged_key(&counts, *rel, &all, row)?;
                    k.extend_from_slice(&n.to_be_bytes());
                    Ok(k)
                };
                if *before > 1 {
                    changes.push((count_key(*before)?, Op::Del));
                }
                if *after > 1 {
                    changes.push((count_key(*after)?, Op::Put));
                }
            }
        }
        changes.extend(d.unwritten.drain(..).map(|k| (k, Op::Put)));
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

/// The blobs `row` holds.
fn row_blobs(row: &Row) -> BTreeSet<BlobRef> {
    let mut out = BTreeSet::new();
    for v in row.iter() {
        blossom_value::blobs_in(v, &mut out);
    }
    out
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("relations", &self.schema.rels.len())
            .field("read_only", &self.read_only)
            .finish()
    }
}

impl Database {
    /// How many rows of the durable relations hold each blob as of the newest applied version (a node booting on the
    /// database counts its carried rows' blobs from the blob keyspaces, a page at a time).
    pub fn blob_counts(&self) -> Result<BTreeMap<BlobRef, u64>, NodeError> {
        let mut out = BTreeMap::new();
        let Some(at) = self.lsm.applied()? else {
            return Ok(out);
        };
        if !self.derived()?.blobs {
            return Err(blossom_base::internal_error!("the database keeps no blob keyspace").into());
        }
        for tag in self.tags.keys() {
            let prefix = blob_tag(tag);
            let end = crate::keycode::successor(&prefix);
            self.each_key(&prefix, end.as_deref(), at, &mut |key| {
                let blob = key
                    .get(prefix.len()..prefix.len() + 40)
                    .and_then(|b| {
                        let (hash, len) = b.split_at(32);
                        Some(BlobRef {
                            hash: hash.try_into().ok()?,
                            len: u64::from_be_bytes(len.try_into().ok()?),
                        })
                    })
                    .ok_or_else(|| NodeError::Store("a blob key too short for its blob".into()))?;
                *out.entry(blob).or_insert(0) += 1;
                Ok(())
            })?;
        }
        Ok(out)
    }

    /// The tag of `rel`'s rows: a durable view's keyspace of this run, or the durable relation's.
    fn tag_of(&self, codec: &DurableCodec<'_>, rel: RelId) -> Result<[u8; 8], NodeError> {
        self.derived()?.tag(codec, rel)
    }

    /// `row`'s support in `rel` as of `at`: whether a table holds it; a durable view's count (its row key, and a count
    /// key for more than one support).
    fn view_support(&self, rel: RelId, row: &Row, at: u64) -> Result<u64, NodeError> {
        let codec = self.codec();
        let (tag, counts) = {
            let d = self.derived()?;
            match d.views.get(&rel) {
                Some(t) => (t.0, Some(t.1)),
                None => (d.tag(&codec, rel)?, None),
            }
        };
        let all: Vec<usize> = (0..row.len()).collect();
        if !self.lsm.get(&codec.tagged_key(&tag, rel, &all, row)?, at)? {
            return Ok(0);
        }
        let Some(counts) = counts else {
            return Ok(1);
        };
        let prefix = codec.tagged_key(&counts, rel, &all, row)?;
        let end = crate::keycode::successor(&prefix);
        let page = self.lsm.scan_page(&prefix, end.as_deref(), at, 1)?;
        match page.keys.first() {
            None => Ok(1),
            Some(key) => {
                let n = key
                    .get(prefix.len()..)
                    .and_then(|b| <[u8; 8]>::try_from(b).ok())
                    .map(u64::from_be_bytes)
                    .ok_or_else(|| NodeError::Store("a view's count key without its count".into()))?;
                Ok(n)
            }
        }
    }

    /// Opens the durable views' keyspaces for this run (`ColdTables::open_views`, DATABASE.md §8).
    fn open_view_keyspaces(&self, views: &[(RelId, [u8; 32])], resume: bool) -> Result<bool, NodeError> {
        let mut d = self.derived()?;
        let ready = resume && views.iter().all(|(_, def)| d.view_defs.contains_key(def));
        let fresh = d.view_defs.values().max().map_or(0, |g| g.saturating_add(1));
        d.views.clear();
        for (rel, def) in views {
            let generation = if ready {
                d.view_defs.get(def).copied().unwrap_or(fresh)
            } else {
                fresh
            };
            let tags = view_tags(def, generation);
            if let Some(cols) = d.orphan_indexes.remove(&tags.0) {
                d.indexes.entry(*rel).or_default().extend(cols);
            }
            d.views.insert(*rel, tags);
            if !ready {
                // Written with the next applied version, which holds the view's rows from its start.
                let mut key = defs_tag().to_vec();
                key.push(DEF_VIEW);
                key.extend_from_slice(def);
                key.extend_from_slice(&generation.to_be_bytes());
                d.unwritten.push(key);
                d.view_defs.insert(*def, generation);
            }
        }
        Ok(ready)
    }

    /// The rows a page-at-a-time scan of `start..end` finds, from keys under `tag`; `None` once there are more than
    /// `max` (the scan stops there).
    #[allow(clippy::too_many_arguments)]
    fn rows_between(
        &self,
        codec: &DurableCodec<'_>,
        tag: &[u8],
        rel: RelId,
        start: &[u8],
        end: Option<&[u8]>,
        at: u64,
        max: Option<usize>,
    ) -> Result<Option<Vec<Row>>, NodeError> {
        let mut out = Vec::new();
        let mut from = start.to_vec();
        loop {
            // With a bound, a page no larger than what proves it passed.
            let keys = max.map_or(PAGE_KEYS, |m| (m + 1).saturating_sub(out.len()).clamp(1, PAGE_KEYS));
            let page = self.lsm.scan_page(&from, end, at, keys)?;
            if max.is_some_and(|m| out.len() + page.keys.len() > m) {
                return Ok(None);
            }
            for key in &page.keys {
                out.push(codec.tagged_row(tag, rel, key)?);
            }
            match page.next {
                Some(n) => from = n,
                None => return Ok(Some(out)),
            }
        }
    }

    /// The first `n` rows in key order of `rel` as of `at` whose columns `cols` hold `values`, and whether there are
    /// more: a scan of one short page (only those rows are decoded).
    fn first_rows(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        at: u64,
        n: usize,
    ) -> Result<(Vec<Row>, bool), NodeError> {
        let codec = self.codec();
        let (tag, prefix) = self.probe_prefix(&codec, rel, cols, values, at)?;
        let end = crate::keycode::successor(&prefix);
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut from = prefix;
        let mut more = false;
        loop {
            let page = self
                .lsm
                .scan_page(&from, end.as_deref(), at, n.saturating_add(1).min(PAGE_KEYS))?;
            keys.extend(page.keys);
            if keys.len() > n {
                more = true;
                break;
            }
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        keys.truncate(n);
        let rows = keys
            .iter()
            .map(|k| codec.tagged_row(&tag, rel, k))
            .collect::<Result<_, _>>()?;
        Ok((rows, more))
    }

    /// The keyspace a probe on `cols` (no range) reads, and the prefix of its keys holding `values`: the relation's
    /// own keys for a leading run of its columns, else an index (kept from now on).
    fn probe_prefix(
        &self,
        codec: &DurableCodec<'_>,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        at: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), NodeError> {
        let rel_tag = self.tag_of(codec, rel)?;
        let tag: Vec<u8> = if cols.iter().enumerate().all(|(i, c)| i == *c) {
            rel_tag.to_vec()
        } else {
            if self.lsm.applied()? != Some(at) {
                return Err(
                    blossom_base::internal_error!("an index probe as of version {at}, not the newest applied").into(),
                );
            }
            self.keep_index(rel, cols)?;
            index_tag(&rel_tag, cols)?.to_vec()
        };
        let prefix = codec.tagged_prefix(&tag, rel, cols, values)?;
        Ok((tag, prefix))
    }

    /// The rows of `rel` as of `at` whose columns `cols` hold `values` (and, with `range`, whose column lies within
    /// it); `None` once there are more than `max`.
    fn probe_rows(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        range: Option<blossom_engine::ColRange<'_>>,
        at: u64,
        max: Option<usize>,
    ) -> Result<Option<Vec<Row>>, NodeError> {
        let codec = self.codec();
        let rel_tag = self.tag_of(&codec, rel)?;
        // The relation's own keys lead with its columns in declaration order: a probe on a leading run of them (and a
        // range on the next) reads them; any other reads an index.
        let leading = cols.iter().enumerate().all(|(i, c)| i == *c);
        let (tag, key_cols) = match range {
            Some((col, _, _)) if leading && col == cols.len() => (rel_tag, cols.to_vec()),
            None if leading => (rel_tag, cols.to_vec()),
            _ => {
                let mut key_cols = cols.to_vec();
                if let Some((col, _, _)) = range {
                    key_cols.push(col);
                }
                // An index answers only as of the version it was built at or after: probes read the newest.
                if self.lsm.applied()? != Some(at) {
                    return Err(blossom_base::internal_error!(
                        "an index probe as of version {at}, not the newest applied"
                    )
                    .into());
                }
                self.keep_index(rel, &key_cols)?;
                (index_tag(&rel_tag, &key_cols)?, key_cols)
            }
        };
        match range {
            Some((col, lo, hi)) => {
                let lead = key_cols.get(..cols.len()).unwrap_or_default();
                let (start, end) = codec.tagged_range(&tag, rel, lead, values, col, lo, hi)?;
                self.rows_between(&codec, &tag, rel, &start, end.as_deref(), at, max)
            }
            None => {
                let prefix = codec.tagged_prefix(&tag, rel, &key_cols, values)?;
                let end = crate::keycode::successor(&prefix);
                self.rows_between(&codec, &tag, rel, &prefix, end.as_deref(), at, max)
            }
        }
    }
}

/// A database error as the evaluator reports it.
fn storage(e: NodeError) -> blossom_ir::tick::EvalError {
    match e {
        NodeError::Internal(i) => blossom_ir::tick::EvalError::Internal(i),
        NodeError::Unimplemented(u) => blossom_ir::tick::EvalError::Unimplemented(u),
        other => blossom_ir::tick::EvalError::Storage(other.to_string()),
    }
}

impl blossom_engine::ColdTables for Database {
    fn version(&self) -> Result<Option<u64>, blossom_ir::tick::EvalError> {
        self.lsm.applied().map_err(|e| storage(e.into()))
    }

    fn tables(&self) -> Vec<RelId> {
        self.schema.rels.iter().map(|(r, _, _)| *r).collect()
    }

    fn contains(&self, rel: RelId, row: &Row, at: u64) -> Result<bool, blossom_ir::tick::EvalError> {
        let codec = self.codec();
        let tag = self.tag_of(&codec, rel).map_err(storage)?;
        let all: Vec<usize> = (0..row.len()).collect();
        let key = codec.tagged_key(&tag, rel, &all, row).map_err(storage)?;
        self.lsm.get(&key, at).map_err(|e| storage(e.into()))
    }

    fn support(&self, rel: RelId, row: &Row, at: u64) -> Result<u64, blossom_ir::tick::EvalError> {
        self.view_support(rel, row, at).map_err(storage)
    }

    fn open_views(&self, views: &[(RelId, [u8; 32])], resume: bool) -> Result<bool, blossom_ir::tick::EvalError> {
        self.open_view_keyspaces(views, resume).map_err(storage)
    }

    fn probe(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        range: Option<blossom_engine::ColRange<'_>>,
        at: u64,
    ) -> Result<Vec<Row>, blossom_ir::tick::EvalError> {
        self.probe_rows(rel, cols, values, range, at, None)
            .map_err(storage)?
            .ok_or_else(|| blossom_base::internal_error!("an unbounded probe stopped short").into())
    }

    fn probe_at_most(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        at: u64,
        max: usize,
    ) -> Result<Option<Vec<Row>>, blossom_ir::tick::EvalError> {
        self.probe_rows(rel, cols, values, None, at, Some(max)).map_err(storage)
    }

    fn probe_some(
        &self,
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        at: u64,
        n: usize,
    ) -> Result<(Vec<Row>, bool), blossom_ir::tick::EvalError> {
        self.first_rows(rel, cols, values, at, n).map_err(storage)
    }

    fn count(&self, rel: RelId, at: u64) -> Result<usize, blossom_ir::tick::EvalError> {
        let codec = self.codec();
        let tag = self.tag_of(&codec, rel).map_err(storage)?;
        let end = crate::keycode::successor(&tag);
        let mut n = 0usize;
        self.each_key(&tag, end.as_deref(), at, &mut |_| {
            n += 1;
            Ok(())
        })
        .map_err(storage)?;
        Ok(n)
    }
}
