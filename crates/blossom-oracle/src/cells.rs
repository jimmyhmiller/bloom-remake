//! Lattice-valued relations (SEM-100, SEM-101, LANGUAGE §11.1): one row per key holding the join of every value
//! derived for it. A row whose lattice values are all ⊥ is absent.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::{InternalError, RelId, internal_error};
use blossom_ir::core::{LatticeCtor, Program};
use blossom_lattice::{Kind, LatticeError};
use blossom_value::Value;

use crate::Row;

/// The built-in lattice of every declared lattice, by id (`None` for lattices the oracle does not evaluate).
pub(crate) fn kinds(p: &Program) -> Vec<Option<Kind>> {
    fn kind(p: &Program, ctor: &LatticeCtor, depth: usize) -> Option<Kind> {
        if depth > p.lattices.len() {
            return None;
        }
        Some(match ctor {
            LatticeCtor::Bool => Kind::Bool,
            LatticeCtor::Max(_) => Kind::Max,
            LatticeCtor::Min(_) => Kind::Min,
            LatticeCtor::Set(_) => Kind::Set,
            LatticeCtor::PSet(_) => Kind::PSet,
            LatticeCtor::Point(_) => Kind::Point,
            LatticeCtor::Map(_, inner) => Kind::Map(Box::new(kind(p, &p.lattices.get(*inner)?.ctor, depth + 1)?)),
            _ => return None,
        })
    }
    p.lattices.iter().map(|l| kind(p, &l.ctor, 0)).collect()
}

/// How the rows of one lattice-valued relation merge.
#[derive(Clone, Debug)]
pub(crate) struct CellInfo {
    /// The columns that identify a cell: the key and the payload (which the key determines, SEM-050).
    pub ident: Vec<usize>,
    /// The key columns (what a lookup `r[k̄]` gives), in schema key order.
    pub key: Vec<usize>,
    /// The lattice columns and their lattices.
    pub lattice: Vec<(usize, Kind)>,
}

/// The merge rule of every lattice-valued relation.
pub(crate) fn cells(
    p: &Program,
    kinds: &[Option<Kind>],
) -> Result<BTreeMap<RelId, CellInfo>, blossom_base::Unimplemented> {
    let mut out = BTreeMap::new();
    for (id, r) in p.rels.iter_enumerated() {
        if r.schema.lattice.is_empty() {
            continue;
        }
        let mut lattice = Vec::new();
        for (col, l) in &r.schema.lattice {
            match kinds.get(l.index()).cloned().flatten() {
                Some(k) => lattice.push((col.index(), k)),
                None => {
                    return Err(blossom_base::unimplemented_error!(
                        "LANG-124",
                        "the lattice of `{}` in the oracle (slice 2 evaluates LBool, LMax, LMin, LSet, LPSet, LMap, LPoint)",
                        r.name
                    ));
                }
            }
        }
        let key: Vec<usize> = r.schema.key.iter().map(|c| c.index()).collect();
        let mut ident: Vec<usize> = key
            .iter()
            .copied()
            .chain(r.schema.payload.iter().map(|c| c.index()))
            .collect();
        ident.sort_unstable();
        out.insert(id, CellInfo { ident, key, lattice });
    }
    Ok(out)
}

impl CellInfo {
    /// Whether every lattice value of `row` is ⊥ (the row is then absent, SEM-101).
    pub fn is_bottom(&self, row: &[Value]) -> Result<bool, InternalError> {
        for (c, k) in &self.lattice {
            match row.get(*c) {
                Some(Value::Lattice(l)) => {
                    if !k.is_bottom(l) {
                        return Ok(false);
                    }
                }
                other => return Err(internal_error!("lattice column {c} holds {other:?}")),
            }
        }
        Ok(true)
    }

    /// The identity of `row`'s cell; `extra` trailing columns beyond the schema (a received tuple's sender) count.
    pub fn ident(&self, row: &[Value], extra: usize) -> Vec<Value> {
        let n = row.len();
        self.ident
            .iter()
            .copied()
            .chain(n.saturating_sub(extra)..n)
            .filter_map(|c| row.get(c).cloned())
            .collect()
    }

    /// The join of two rows of one cell.
    pub fn merge(&self, old: &[Value], new: &[Value]) -> Result<Row, MergeError> {
        let mut out = old.to_vec();
        for (c, k) in &self.lattice {
            match (old.get(*c), new.get(*c), out.get_mut(*c)) {
                (Some(Value::Lattice(a)), Some(Value::Lattice(b)), Some(slot)) => {
                    *slot = Value::Lattice(k.join(a, b).map_err(MergeError::Lattice)?);
                }
                (a, b, _) => {
                    return Err(MergeError::Internal(internal_error!(
                        "merging lattice column {c}: {a:?} and {b:?}"
                    )));
                }
            }
        }
        Ok(Arc::from(out))
    }
}

/// Why two rows of a cell did not merge.
#[derive(Debug)]
pub(crate) enum MergeError {
    /// An `LPoint` conflict (BLSR006) or another lattice failure.
    Lattice(LatticeError),
    Internal(InternalError),
}

/// Inserts `row` into `set`, merging it into the row of its cell when `rel` is lattice-valued. Returns whether `set`
/// changed. For output instances and outboxes, which have no index.
pub(crate) fn insert_merged(
    cells: &BTreeMap<RelId, CellInfo>,
    set: &mut BTreeSet<Row>,
    rel: RelId,
    row: Row,
) -> Result<bool, MergeError> {
    let Some(info) = cells.get(&rel) else {
        return Ok(set.insert(row));
    };
    if info.is_bottom(&row).map_err(MergeError::Internal)? {
        return Ok(false);
    }
    let id = info.ident(&row, 0);
    let old = set.iter().find(|r| info.ident(r, 0) == id).cloned();
    match old {
        None => Ok(set.insert(row)),
        Some(old) => {
            let merged = info.merge(&old, &row)?;
            if merged == old {
                return Ok(false);
            }
            set.remove(&old);
            set.insert(merged);
            Ok(true)
        }
    }
}
