//! Durable state (ARCHITECTURE §5.6): the rows of a program's `durable` relations, what a restart recovers.
//!
//! A tick whose durable rows change writes one WAL record: per changed relation, its name, schema hash, and the rows
//! inserted and deleted, in the tuple codec with `Node` values by name. The node's database (`crate::database`) holds
//! every durable relation's rows; recovery opens it and replays the WAL records after what its tables hold (a store
//! from before the database: its checkpoint chain, once). A record naming a relation this build does not have, or
//! with another schema hash, is a refusal (migrations are a later slice).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::core::Program;
use blossom_oracle::{Instance, Row};
use blossom_wire::codec::{Codec, NodeEncoding, WireError, WireLimits, get_varint, put_varint, take};

use crate::NodeError;

/// The durable relations of a program: id, name and schema hash.
#[derive(Clone, Debug)]
pub struct DurableSchema {
    pub rels: Vec<(RelId, Arc<str>, [u8; 16])>,
}

impl DurableSchema {
    pub fn of(p: &Program) -> DurableSchema {
        DurableSchema {
            rels: p
                .rels
                .iter_enumerated()
                .filter(|(_, r)| r.durable)
                .map(|(id, r)| {
                    (
                        id,
                        Arc::from(r.name.to_string()),
                        blossom_wire::catalog::schema_hash(p, id),
                    )
                })
                .collect(),
        }
    }

    fn by_name(&self, name: &str) -> Option<&(RelId, Arc<str>, [u8; 16])> {
        self.rels.iter().find(|(_, n, _)| &**n == name)
    }

    pub fn contains(&self, rel: RelId) -> bool {
        self.rels.iter().any(|(r, _, _)| *r == rel)
    }
}

/// The rows of every durable relation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DurableImage {
    pub rows: BTreeMap<RelId, BTreeSet<Row>>,
}

/// What a tick changed in the durable rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub changes: BTreeMap<RelId, (Vec<Row>, Vec<Row>)>,
    /// For the durable views' sources (docs/design/DATABASE.md §8): how the tick's rows differ from those carried
    /// into it (rows its rules wrote, and carried rows not present). Logged with the change (most are among its
    /// inserts and deletes, logged as bits over them): a restart's catch-up of the views starts from the rows of the
    /// tick the database's views were computed at.
    pub written: BTreeMap<RelId, (Vec<Row>, Vec<Row>)>,
}

impl Delta {
    /// Whether the tick changed no durable row.
    pub fn is_empty(&self) -> bool {
        self.changes.values().all(|(i, d)| i.is_empty() && d.is_empty())
    }

    /// Whether the tick has nothing to log: it changed no durable row, and its rows were the carried ones.
    pub fn is_quiet(&self) -> bool {
        self.is_empty() && self.written.values().all(|(i, d)| i.is_empty() && d.is_empty())
    }
}

impl DurableImage {
    /// The durable rows of an instance (a tick's carried state).
    pub fn of(instance: &Instance, schema: &DurableSchema) -> DurableImage {
        DurableImage {
            rows: schema
                .rels
                .iter()
                .map(|(r, _, _)| (*r, instance.rows(*r).cloned().collect()))
                .collect(),
        }
    }

    /// The change from `self` to `next`.
    pub fn delta(&self, next: &DurableImage) -> Delta {
        let empty = BTreeSet::new();
        let mut changes = BTreeMap::new();
        for (rel, rows) in &next.rows {
            let old = self.rows.get(rel).unwrap_or(&empty);
            let inserts: Vec<Row> = rows.difference(old).cloned().collect();
            let deletes: Vec<Row> = old.difference(rows).cloned().collect();
            if !inserts.is_empty() || !deletes.is_empty() {
                changes.insert(*rel, (inserts, deletes));
            }
        }
        for (rel, old) in &self.rows {
            if !next.rows.contains_key(rel) && !old.is_empty() {
                changes.insert(*rel, (Vec::new(), old.iter().cloned().collect()));
            }
        }
        Delta {
            changes,
            written: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, delta: &Delta) {
        for (rel, (inserts, deletes)) in &delta.changes {
            let rows = self.rows.entry(*rel).or_default();
            for r in deletes {
                rows.remove(r);
            }
            for r in inserts {
                rows.insert(r.clone());
            }
        }
    }

    /// The rows as a tick's carried instance.
    pub fn instance(&self) -> Instance {
        let mut out = Instance::default();
        for (rel, rows) in &self.rows {
            for r in rows {
                out.insert(*rel, r.clone());
            }
        }
        out
    }
}

/// Encodes and decodes durable rows: the tuple codec with `Node` values by name.
pub struct DurableCodec<'p> {
    program: &'p Program,
    schema: &'p DurableSchema,
    codec: Codec<'p>,
    names: Arc<[Arc<str>]>,
}

impl<'p> DurableCodec<'p> {
    pub fn new(program: &'p Program, schema: &'p DurableSchema, names: Arc<[Arc<str>]>) -> DurableCodec<'p> {
        DurableCodec {
            program,
            schema,
            codec: Codec::new(program, NodeEncoding::ByName(names.clone()), WireLimits::default()),
            names,
        }
    }

    fn cols(&self, rel: RelId) -> Result<&'p [blossom_ir::core::Column], NodeError> {
        self.program
            .rels
            .get(rel)
            .map(|r| r.schema.cols.as_slice())
            .ok_or_else(|| blossom_base::internal_error!("durable relation {rel:?} is not declared").into())
    }

    fn name_hash(&self, rel: RelId) -> Result<(&'p str, [u8; 16]), NodeError> {
        self.schema
            .rels
            .iter()
            .find(|(r, _, _)| *r == rel)
            .map(|(_, n, h)| (&**n, *h))
            .ok_or_else(|| blossom_base::internal_error!("relation {rel:?} is not durable").into())
    }

    fn put_rows(&self, rel: RelId, rows: &[Row], out: &mut Vec<u8>) -> Result<(), NodeError> {
        let cols = self.cols(rel)?;
        put_varint(out, rows.len() as u64);
        for r in rows {
            self.codec.encode_row(cols, r, out)?;
        }
        Ok(())
    }

    fn get_rows(&self, rel: RelId, input: &mut &[u8]) -> Result<Vec<Row>, NodeError> {
        let cols = self.cols(rel)?;
        let n = get_varint(input)?;
        if n > input.len() as u64 {
            return Err(WireError::Limit("row count").into());
        }
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(Arc::from(self.codec.decode_row(cols, input)?));
        }
        Ok(out)
    }

    fn put_header(&self, rel: RelId, out: &mut Vec<u8>) -> Result<(), NodeError> {
        let (name, hash) = self.name_hash(rel)?;
        put_varint(out, name.len() as u64);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&hash);
        Ok(())
    }

    /// Reads a relation's name and schema hash, and finds it in this build.
    fn get_header(&self, input: &mut &[u8]) -> Result<RelId, NodeError> {
        let n = usize::try_from(get_varint(input)?).map_err(|_| WireError::Limit("name"))?;
        let name = std::str::from_utf8(take(input, n, "a relation name")?)
            .map_err(|_| WireError::Malformed("invalid UTF-8".into()))?
            .to_owned();
        let hash: [u8; 16] = take(input, 16, "a schema hash")?
            .try_into()
            .map_err(|_| WireError::Truncated("a schema hash"))?;
        match self.schema.by_name(&name) {
            Some((rel, _, h)) if *h == hash => Ok(*rel),
            Some(_) => Err(NodeError::Schema(format!(
                "the durable relation `{name}` was written with another schema (migrations arrive with a later slice)"
            ))),
            None => Err(NodeError::Schema(format!(
                "the durable state holds `{name}`, which this program does not declare durable"
            ))),
        }
    }

    /// The 8 bytes that begin every database key of the durable relation `rel` (docs/design/DATABASE.md §3): BLAKE3
    /// over its name and schema hash.
    pub fn rel_tag(&self, rel: RelId) -> Result<[u8; 8], NodeError> {
        let (name, hash) = self.name_hash(rel)?;
        let mut h = blake3::Hasher::new();
        h.update(name.as_bytes());
        h.update(&[0]);
        h.update(&hash);
        let mut tag = [0u8; 8];
        for (t, b) in tag.iter_mut().zip(h.finalize().as_bytes()) {
            *t = *b;
        }
        Ok(tag)
    }

    /// The key encoding of column `col`'s value `v` (`crate::keycode`).
    fn put_key_value(
        &self,
        col: &blossom_ir::core::Column,
        v: &blossom_value::Value,
        out: &mut Vec<u8>,
    ) -> Result<(), NodeError> {
        let fallback = |v: &blossom_value::Value| -> Result<Vec<u8>, String> {
            let mut bytes = Vec::new();
            self.codec
                .encode_row(std::slice::from_ref(col), std::slice::from_ref(v), &mut bytes)
                .map_err(|e| e.to_string())?;
            Ok(bytes)
        };
        crate::keycode::encode(v, &self.names, &fallback, out).map_err(|e| WireError::Malformed(e).into())
    }

    /// A row's database key (docs/design/DATABASE.md §3): its relation's tag; each column's value in the
    /// order-preserving key encoding, in declaration order (rows agreeing on their leading columns share a prefix,
    /// and keys order as rows do column by column); then the row in the durable codec and that encoding's length (a
    /// big-endian `u32`), which is what [`DurableCodec::key_row`] reads back.
    pub fn row_key(&self, rel: RelId, row: &Row) -> Result<Vec<u8>, NodeError> {
        let all: Vec<usize> = (0..row.len()).collect();
        self.tagged_key(&self.rel_tag(rel)?, rel, &all, row)
    }

    /// A key of a keyspace of `rel` (its rows, or a keyspace derived from them): `tag`; the values of columns `cols`
    /// of `row`, in that order, in the key encoding; the row in the durable codec; that encoding's length.
    pub fn tagged_key(&self, tag: &[u8], rel: RelId, cols: &[usize], row: &Row) -> Result<Vec<u8>, NodeError> {
        let decl = self.cols(rel)?;
        if decl.len() != row.len() {
            return Err(WireError::Malformed(format!("{} values for {} columns", row.len(), decl.len())).into());
        }
        let mut key = tag.to_vec();
        for c in cols {
            let (col, v) = decl
                .get(*c)
                .zip(row.get(*c))
                .ok_or_else(|| WireError::Malformed(format!("column {c} of a narrower relation")))?;
            self.put_key_value(col, v, &mut key)?;
        }
        let at = key.len();
        self.codec.encode_row(decl, row, &mut key)?;
        let len = u32::try_from(key.len() - at).map_err(|_| WireError::Limit("a row over 4 GiB"))?;
        key.extend_from_slice(&len.to_be_bytes());
        Ok(key)
    }

    /// The key prefix of the rows of `rel` whose leading columns (in declaration order) are `leading`.
    pub fn key_prefix(&self, rel: RelId, leading: &[blossom_value::Value]) -> Result<Vec<u8>, NodeError> {
        let cols: Vec<usize> = (0..leading.len()).collect();
        self.tagged_prefix(&self.rel_tag(rel)?, rel, &cols, leading)
    }

    /// The prefix of the keys under `tag` (keyed by columns `cols` of `rel`, [`DurableCodec::tagged_key`]) whose
    /// columns `cols` hold `values`.
    pub fn tagged_prefix(
        &self,
        tag: &[u8],
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
    ) -> Result<Vec<u8>, NodeError> {
        if cols.len() != values.len() {
            return Err(WireError::Malformed(format!("{} values for {} columns", values.len(), cols.len())).into());
        }
        let decl = self.cols(rel)?;
        let mut key = tag.to_vec();
        for (c, v) in cols.iter().zip(values) {
            let col = decl
                .get(*c)
                .ok_or_else(|| WireError::Malformed(format!("column {c} of a narrower relation")))?;
            self.put_key_value(col, v, &mut key)?;
        }
        Ok(key)
    }

    /// The keys of the rows of `rel` whose leading columns are `leading` and whose next column lies within `lo` and
    /// `hi` (in value order, which the key encoding keeps for the types a program compares): from the first key
    /// (inclusive) to the end (exclusive, `None`: no end).
    pub fn key_range(
        &self,
        rel: RelId,
        leading: &[blossom_value::Value],
        lo: std::ops::Bound<&blossom_value::Value>,
        hi: std::ops::Bound<&blossom_value::Value>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), NodeError> {
        let cols: Vec<usize> = (0..leading.len()).collect();
        self.tagged_range(&self.rel_tag(rel)?, rel, &cols, leading, leading.len(), lo, hi)
    }

    /// The keys under `tag` (keyed by columns `cols` then `col` of `rel`) whose columns `cols` hold `values` and
    /// whose column `col` lies within `lo` and `hi`: from the first key (inclusive) to the end (exclusive, `None`:
    /// no end).
    #[allow(clippy::too_many_arguments)]
    pub fn tagged_range(
        &self,
        tag: &[u8],
        rel: RelId,
        cols: &[usize],
        values: &[blossom_value::Value],
        col: usize,
        lo: std::ops::Bound<&blossom_value::Value>,
        hi: std::ops::Bound<&blossom_value::Value>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), NodeError> {
        use std::ops::Bound;
        let prefix = self.tagged_prefix(tag, rel, cols, values)?;
        let decl = self
            .cols(rel)?
            .get(col)
            .ok_or_else(|| WireError::Malformed("a range past the relation's last column".into()))?;
        let at = |v: &blossom_value::Value| -> Result<Vec<u8>, NodeError> {
            let mut k = prefix.clone();
            self.put_key_value(decl, v, &mut k)?;
            Ok(k)
        };
        let start = match lo {
            Bound::Unbounded => prefix.clone(),
            Bound::Included(v) => at(v)?,
            // Past every key with this value in the column.
            Bound::Excluded(v) => {
                crate::keycode::successor(&at(v)?).ok_or_else(|| WireError::Malformed("an empty range".into()))?
            }
        };
        let end = match hi {
            Bound::Unbounded => crate::keycode::successor(&prefix),
            Bound::Included(v) => crate::keycode::successor(&at(v)?),
            Bound::Excluded(v) => Some(at(v)?),
        };
        Ok((start, end))
    }

    /// The row a database key of `rel` holds.
    pub fn key_row(&self, rel: RelId, key: &[u8]) -> Result<Row, NodeError> {
        self.tagged_row(&self.rel_tag(rel)?, rel, key)
    }

    /// The row a key under `tag` of `rel` holds ([`DurableCodec::tagged_key`]): read from its end.
    pub fn tagged_row(&self, tag: &[u8], rel: RelId, key: &[u8]) -> Result<Row, NodeError> {
        if !key.starts_with(tag) {
            return Err(WireError::Malformed("a database key of another keyspace".into()).into());
        }
        let (rest, len) = key
            .split_at_checked(key.len().saturating_sub(4))
            .ok_or_else(|| WireError::Malformed("a database key without its row".into()))?;
        let len = u32::from_be_bytes(len.try_into().map_err(|_| WireError::Truncated("a database key"))?) as usize;
        let mut body = rest
            .get(rest.len().saturating_sub(len)..)
            .filter(|_| len <= rest.len().saturating_sub(tag.len()))
            .ok_or_else(|| WireError::Malformed("a database key shorter than its row".into()))?;
        let row = self.codec.decode_row(self.cols(rel)?, &mut body)?;
        if !body.is_empty() {
            return Err(WireError::Malformed("trailing bytes in a database key".into()).into());
        }
        Ok(Arc::from(row))
    }

    /// A WAL record payload: every changed relation's inserts and deletes.
    pub fn encode_delta(&self, delta: &Delta) -> Result<Vec<u8>, NodeError> {
        let mut out = Vec::new();
        let changed = || {
            delta
                .changes
                .iter()
                .filter(|(_, (i, d))| !i.is_empty() || !d.is_empty())
        };
        put_varint(&mut out, changed().count() as u64);
        for (rel, (inserts, deletes)) in changed() {
            self.put_header(*rel, &mut out)?;
            self.put_rows(*rel, inserts, &mut out)?;
            self.put_rows(*rel, deletes, &mut out)?;
        }
        // The rows the tick wrote beyond its carried ones (a record before them has none): each side as bits over the
        // change's rows of that side, and the rows not among them.
        let written: Vec<_> = delta
            .written
            .iter()
            .filter(|(_, (i, d))| !i.is_empty() || !d.is_empty())
            .collect();
        if written.is_empty() {
            return Ok(out);
        }
        put_varint(&mut out, written.len() as u64);
        let none = (Vec::new(), Vec::new());
        for (rel, (shown, hidden)) in written {
            self.put_header(*rel, &mut out)?;
            let (inserts, deletes) = delta.changes.get(rel).unwrap_or(&none);
            for (rows, among) in [(shown, inserts), (hidden, deletes)] {
                let at: BTreeMap<&Row, usize> = among.iter().enumerate().map(|(i, r)| (r, i)).collect();
                let mut bits = vec![0u8; among.len().div_ceil(8)];
                let mut others = Vec::new();
                for row in rows {
                    let Some(i) = at.get(row) else {
                        others.push(row.clone());
                        continue;
                    };
                    let byte = bits.get_mut(i / 8).ok_or_else(|| {
                        blossom_base::internal_error!("a written row's bit is past the change's rows")
                    })?;
                    *byte |= 1 << (i % 8);
                }
                out.extend_from_slice(&bits);
                self.put_rows(*rel, &others, &mut out)?;
            }
        }
        Ok(out)
    }

    pub fn decode_delta(&self, mut payload: &[u8]) -> Result<Delta, NodeError> {
        let input = &mut payload;
        let n = get_varint(input)?;
        let mut changes = BTreeMap::new();
        for _ in 0..n {
            let rel = self.get_header(input)?;
            let inserts = self.get_rows(rel, input)?;
            let deletes = self.get_rows(rel, input)?;
            changes.insert(rel, (inserts, deletes));
        }
        let mut written = BTreeMap::new();
        if !input.is_empty() {
            let n = get_varint(input)?;
            let none = (Vec::new(), Vec::new());
            for _ in 0..n {
                let rel = self.get_header(input)?;
                let (inserts, deletes) = changes.get(&rel).unwrap_or(&none);
                let mut sides = Vec::with_capacity(2);
                for among in [inserts, deletes] {
                    let bits = take(input, among.len().div_ceil(8), "a WAL record's written rows")?;
                    let mut rows: Vec<Row> = among
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| bits.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0))
                        .map(|(_, r)| r.clone())
                        .collect();
                    rows.extend(self.get_rows(rel, input)?);
                    sides.push(rows);
                }
                let hidden = sides.pop().unwrap_or_default();
                let shown = sides.pop().unwrap_or_default();
                written.insert(rel, (shown, hidden));
            }
        }
        if !input.is_empty() {
            return Err(WireError::Malformed("trailing bytes in a WAL record".into()).into());
        }
        Ok(Delta { changes, written })
    }

    /// A checkpoint of every durable relation: `relations[i]` holds relation `i` of the catalog.
    pub fn encode_image(&self, image: &DurableImage) -> Result<blossom_store::DurableSnapshot, NodeError> {
        let mut relations = BTreeMap::new();
        let mut catalog = Vec::new();
        put_varint(&mut catalog, self.schema.rels.len() as u64);
        for (i, (rel, _, _)) in self.schema.rels.iter().enumerate() {
            self.put_header(*rel, &mut catalog)?;
            let rows: Vec<Row> = image
                .rows
                .get(rel)
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default();
            let mut bytes = Vec::new();
            self.put_rows(*rel, &rows, &mut bytes)?;
            relations.insert(i as u32, bytes);
        }
        Ok(blossom_store::DurableSnapshot { relations, catalog })
    }

    pub fn decode_image(&self, snap: &blossom_store::DurableSnapshot) -> Result<DurableImage, NodeError> {
        let mut input = snap.catalog.as_slice();
        let n = get_varint(&mut input)?;
        let mut image = DurableImage::default();
        for i in 0..n {
            let rel = self.get_header(&mut input)?;
            let bytes = snap
                .relations
                .get(&(i as u32))
                .ok_or_else(|| NodeError::Schema(format!("the checkpoint lacks relation file {i}")))?;
            let mut rows_in = bytes.as_slice();
            let rows = self.get_rows(rel, &mut rows_in)?;
            if !rows_in.is_empty() {
                return Err(WireError::Malformed("trailing bytes in a checkpoint relation".into()).into());
            }
            image.rows.insert(rel, rows.into_iter().collect());
        }
        Ok(image)
    }
}
