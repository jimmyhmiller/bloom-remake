//! Interned row patterns (S12).
//!
//! The encoder keys appearances and removals by a place and a row pattern, and asks about the same patterns at every
//! tick of a run, and in every run of a search. A pattern holds values (Kafka's rows carry byte strings), so hashing
//! and comparing it is the bulk of a lookup; interned once per search, a pattern is a number, and a place key is a
//! few words. The shared hazards ([`crate::shared`]) are keyed the same way, so ids are the search's, not a run's.

use std::sync::{Arc, RwLock};

use blossom_base::{DetMap, InternalError, internal_error};
use blossom_value::Value;

/// A row pattern: per column, a value it must hold, or any.
pub type Pattern = Vec<Option<Value>>;

/// A pattern as the table holds it.
pub type Shared = Arc<[Option<Value>]>;

/// An interned [`Pattern`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatId(u32);

#[derive(Default)]
struct Table {
    ids: DetMap<Shared, PatId>,
    patterns: Vec<Shared>,
}

/// The patterns of a search, shared by its runs and workers.
#[derive(Default)]
pub struct Patterns {
    table: RwLock<Table>,
}

impl Patterns {
    pub fn new() -> Patterns {
        Patterns::default()
    }

    /// The id of `pattern`, interned on first sight.
    pub fn intern(&self, pattern: &[Option<Value>]) -> Result<(PatId, Shared), InternalError> {
        if let Some(found) = self
            .table
            .read()
            .map_err(|_| internal_error!("the pattern table is poisoned"))?
            .ids
            .get_key_value(pattern)
        {
            return Ok((*found.1, Arc::clone(found.0)));
        }
        let mut table = self
            .table
            .write()
            .map_err(|_| internal_error!("the pattern table is poisoned"))?;
        if let Some((p, id)) = table.ids.get_key_value(pattern) {
            return Ok((*id, Arc::clone(p)));
        }
        let id = PatId(u32::try_from(table.patterns.len()).map_err(|_| internal_error!("too many patterns"))?);
        let shared: Shared = Arc::from(pattern);
        table.ids.insert(Arc::clone(&shared), id);
        table.patterns.push(Arc::clone(&shared));
        Ok((id, shared))
    }

    /// The pattern `id` names.
    pub fn get(&self, id: PatId) -> Result<Shared, InternalError> {
        self.table
            .read()
            .map_err(|_| internal_error!("the pattern table is poisoned"))?
            .patterns
            .get(id.0 as usize)
            .cloned()
            .ok_or_else(|| internal_error!("an unknown pattern {id:?}"))
    }
}

/// A run's view of the search's patterns: what it saw, without taking the table's lock again.
pub struct Local<'p> {
    patterns: &'p Patterns,
    ids: DetMap<Shared, PatId>,
    by_id: Vec<Option<Shared>>,
}

impl<'p> Local<'p> {
    pub fn new(patterns: &'p Patterns) -> Local<'p> {
        Local {
            patterns,
            ids: DetMap::default(),
            by_id: Vec::new(),
        }
    }

    fn remember(&mut self, id: PatId, p: &Shared) {
        let i = id.0 as usize;
        if self.by_id.len() <= i {
            self.by_id.resize(i + 1, None);
        }
        if let Some(slot) = self.by_id.get_mut(i) {
            *slot = Some(Arc::clone(p));
        }
    }

    /// The id of `pattern`.
    pub fn intern(&mut self, pattern: &[Option<Value>]) -> Result<PatId, InternalError> {
        if let Some(id) = self.ids.get(pattern) {
            return Ok(*id);
        }
        let (id, shared) = self.patterns.intern(pattern)?;
        self.remember(id, &shared);
        self.ids.insert(shared, id);
        Ok(id)
    }

    /// The pattern `id` names.
    pub fn get(&mut self, id: PatId) -> Result<Shared, InternalError> {
        if let Some(Some(p)) = self.by_id.get(id.0 as usize) {
            return Ok(Arc::clone(p));
        }
        let p = self.patterns.get(id)?;
        self.remember(id, &p);
        Ok(p)
    }
}
