//! Durable state (ARCHITECTURE §5.6): the rows of a program's `durable` relations, what a restart recovers.
//!
//! A tick whose durable rows change writes one WAL record: per changed relation, its name, schema hash, and the rows
//! inserted and deleted, in the tuple codec with `Node` values by name. A checkpoint holds every durable relation's
//! rows. Recovery loads the checkpoint named by `CURRENT` and replays the WAL records after it; a record naming a
//! relation this build does not have, or with another schema hash, is a refusal (migrations are a later slice).

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

/// The most delta layers a checkpoint chain holds before the next checkpoint is a full image again.
pub const MAX_CHECKPOINT_LAYERS: usize = 16;

/// Whether the next checkpoint may be a delta layer on the installed chain `chain`, with the change since it known:
/// the chain has room, and its layers are still smaller than its image. Compacting when the layers outgrow the image
/// keeps the total work of checkpoints proportional to the change (as in a log-structured merge).
pub fn layer_fits(chain: Option<blossom_store::ChainInfo>) -> bool {
    chain.is_some_and(|c| c.layers < MAX_CHECKPOINT_LAYERS && c.layer_bytes < c.base_bytes.max(64 * 1024))
}

/// The net change of a run of consecutive deltas (the changes since a checkpoint): a row inserted then deleted
/// leaves no trace, and one deleted then inserted again none either. Its size follows the change, not the state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaAcc {
    changes: BTreeMap<RelId, (BTreeSet<Row>, BTreeSet<Row>)>,
}

impl DeltaAcc {
    /// Adds the next delta (which applies to the image the accumulated ones lead to).
    pub fn add(&mut self, d: &Delta) {
        for (rel, (inserts, deletes)) in &d.changes {
            let (ins, del) = self.changes.entry(*rel).or_default();
            for r in deletes {
                if !ins.remove(r) {
                    del.insert(r.clone());
                }
            }
            for r in inserts {
                if !del.remove(r) {
                    ins.insert(r.clone());
                }
            }
        }
    }

    /// The accumulated change as one delta, leaving the accumulator empty.
    pub fn take(&mut self) -> Delta {
        Delta {
            changes: std::mem::take(&mut self.changes)
                .into_iter()
                .filter(|(_, (i, d))| !i.is_empty() || !d.is_empty())
                .map(|(rel, (i, d))| (rel, (i.into_iter().collect(), d.into_iter().collect())))
                .collect(),
        }
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
}

impl<'p> DurableCodec<'p> {
    pub fn new(program: &'p Program, schema: &'p DurableSchema, names: Arc<[Arc<str>]>) -> DurableCodec<'p> {
        DurableCodec {
            program,
            schema,
            codec: Codec::new(program, NodeEncoding::ByName(names), WireLimits::default()),
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
