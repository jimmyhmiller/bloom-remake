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
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.changes.values().all(|(i, d)| i.is_empty() && d.is_empty())
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
        Delta { changes }
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
        let cols = self.cols(rel)?;
        if cols.len() != row.len() {
            return Err(WireError::Malformed(format!("{} values for {} columns", row.len(), cols.len())).into());
        }
        let mut key = self.rel_tag(rel)?.to_vec();
        for (c, v) in cols.iter().zip(row.iter()) {
            self.put_key_value(c, v, &mut key)?;
        }
        let at = key.len();
        self.codec.encode_row(cols, row, &mut key)?;
        let len = u32::try_from(key.len() - at).map_err(|_| WireError::Limit("a row over 4 GiB"))?;
        key.extend_from_slice(&len.to_be_bytes());
        Ok(key)
    }

    /// The key prefix of the rows of `rel` whose leading columns (in declaration order) are `leading`.
    pub fn key_prefix(&self, rel: RelId, leading: &[blossom_value::Value]) -> Result<Vec<u8>, NodeError> {
        let cols = self.cols(rel)?;
        let cols = cols
            .get(..leading.len())
            .ok_or_else(|| WireError::Malformed(format!("{} leading columns of a narrower relation", leading.len())))?;
        let mut key = self.rel_tag(rel)?.to_vec();
        for (c, v) in cols.iter().zip(leading) {
            self.put_key_value(c, v, &mut key)?;
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
        use std::ops::Bound;
        let prefix = self.key_prefix(rel, leading)?;
        let col = self
            .cols(rel)?
            .get(leading.len())
            .ok_or_else(|| WireError::Malformed("a range past the relation's last column".into()))?;
        let at = |v: &blossom_value::Value| -> Result<Vec<u8>, NodeError> {
            let mut k = prefix.clone();
            self.put_key_value(col, v, &mut k)?;
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
        let tag = self.rel_tag(rel)?;
        if !key.starts_with(tag.as_slice()) {
            return Err(WireError::Malformed("a database key of another relation".into()).into());
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
        if !input.is_empty() {
            return Err(WireError::Malformed("trailing bytes in a WAL record".into()).into());
        }
        Ok(Delta { changes })
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
