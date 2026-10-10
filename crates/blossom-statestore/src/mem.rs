//! A [`StateStore`] in memory: one process, shared by its clones. Faults can be injected per commit, for the
//! deterministic tests of hosts that race on one store (docs/design/STATELESS.md §10).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::tables::{KEYS_TABLE, Owner, RowChange, TableDef, TableStore, check_table};
use crate::{Commit, Snapshot, StateError, StateStore, Write, check_commit, check_name};

/// What an injected fault does to a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The commit runs as it would.
    Pass,
    /// The commit fails before it is applied ([`StateError::Unavailable`]).
    Fail,
    /// The commit is applied, but its caller hears that its outcome is unknown ([`StateError::Unknown`]).
    Lost,
}

/// Decides each commit's fault, given the object and the version the commit names.
type FaultFn = Box<dyn FnMut(&str, u64) -> Fault + Send>;

#[derive(Default)]
struct Inner {
    objects: BTreeMap<String, (u64, BTreeMap<String, Vec<u8>>)>,
    wakes: BTreeSet<(u64, String)>,
    side: BTreeMap<String, Vec<u8>>,
    /// The deployment whose tables these are, and the tables: their definitions and rows (owner, key, from) → until.
    deployment: Option<String>,
    defs: BTreeMap<String, TableDef>,
    rows: BTreeMap<String, Table>,
}

/// A history table's rows: (owner, key, from tick) → the tick it ends (`None`: open) and its typed values.
type Table = BTreeMap<(Owner, Vec<u8>, u64), (Option<u64>, Vec<crate::SqlValue>)>;

fn live(rows: &Table, owner: &Owner, key: &[u8], at: u64) -> bool {
    rows.range((owner.clone(), key.to_vec(), 0)..=(owner.clone(), key.to_vec(), at))
        .any(|(_, (to, _))| to.is_none_or(|t| t > at))
}

struct Shared {
    inner: Mutex<Inner>,
    /// Notified on every commit.
    committed: Condvar,
    faults: Mutex<Option<FaultFn>>,
}

/// A store in memory. Its clones share it, as handles of one database do.
#[derive(Clone)]
pub struct MemStore {
    shared: Arc<Shared>,
}

impl Default for MemStore {
    fn default() -> Self {
        MemStore {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner::default()),
                committed: Condvar::new(),
                faults: Mutex::new(None),
            }),
        }
    }
}

fn poisoned() -> StateError {
    StateError::Unavailable("the memory store's lock is poisoned".into())
}

impl MemStore {
    pub fn new() -> MemStore {
        MemStore::default()
    }

    /// Injects a fault into commits from now on: `f` decides each one's.
    pub fn set_faults(&self, f: impl FnMut(&str, u64) -> Fault + Send + 'static) -> Result<(), StateError> {
        *self.shared.faults.lock().map_err(|_| poisoned())? = Some(Box::new(f));
        Ok(())
    }

    /// Stops injecting faults.
    pub fn clear_faults(&self) -> Result<(), StateError> {
        *self.shared.faults.lock().map_err(|_| poisoned())? = None;
        Ok(())
    }

    fn inner(&self) -> Result<MutexGuard<'_, Inner>, StateError> {
        self.shared.inner.lock().map_err(|_| poisoned())
    }

    fn fault(&self, object: &str, expected: u64) -> Result<Fault, StateError> {
        let mut faults = self.shared.faults.lock().map_err(|_| poisoned())?;
        Ok(match faults.as_mut() {
            Some(f) => f(object, expected),
            None => Fault::Pass,
        })
    }
}

impl StateStore for MemStore {
    fn load(&self, object: &str) -> Result<Snapshot, StateError> {
        check_name("object name", object)?;
        let inner = self.inner()?;
        Ok(match inner.objects.get(object) {
            Some((version, entries)) => Snapshot {
                version: *version,
                entries: entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            },
            None => Snapshot::default(),
        })
    }

    fn version(&self, object: &str) -> Result<u64, StateError> {
        check_name("object name", object)?;
        Ok(self.inner()?.objects.get(object).map_or(0, |(v, _)| *v))
    }

    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StateError> {
        check_commit(object, writes)?;
        let fault = self.fault(object, expected)?;
        if fault == Fault::Fail {
            return Err(StateError::Unavailable(format!(
                "an injected failure of a commit to `{object}`"
            )));
        }
        let mut inner = self.inner()?;
        let current = inner.objects.get(object).map_or(0, |(v, _)| *v);
        if current != expected {
            return Ok(Commit::Conflict { current });
        }
        let (version, entries) = inner.objects.entry(object.to_owned()).or_default();
        for w in writes {
            match w {
                Write::Put(k, v) => {
                    entries.insert(k.clone(), v.clone());
                }
                Write::Delete(k) => {
                    entries.remove(k);
                }
            }
        }
        *version += 1;
        let version = *version;
        drop(inner);
        self.shared.committed.notify_all();
        match fault {
            Fault::Lost => Err(StateError::Unknown {
                object: object.to_owned(),
                reason: "an injected loss of the commit's answer".into(),
            }),
            Fault::Pass | Fault::Fail => Ok(Commit::Done { version }),
        }
    }

    fn wait(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError> {
        check_name("object name", object)?;
        let inner = self.inner()?;
        let (inner, _) = self
            .shared
            .committed
            .wait_timeout_while(inner, timeout, |i| {
                i.objects.get(object).map_or(0, |(v, _)| *v) <= since
            })
            .map_err(|_| poisoned())?;
        Ok(inner.objects.get(object).map_or(0, |(v, _)| *v))
    }

    fn schedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        self.inner()?.wakes.insert((at, object.to_owned()));
        Ok(())
    }

    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StateError> {
        Ok(self
            .inner()?
            .wakes
            .iter()
            .take_while(|(at, _)| *at <= now)
            .take(limit)
            .map(|(at, o)| (o.clone(), *at))
            .collect())
    }

    fn unschedule(&self, object: &str, at: u64) -> Result<(), StateError> {
        check_name("object name", object)?;
        self.inner()?.wakes.remove(&(at, object.to_owned()));
        Ok(())
    }

    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.inner()?.side.insert(key.to_owned(), value.to_vec());
        Ok(())
    }

    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError> {
        check_name("side key", key)?;
        Ok(self.inner()?.side.get(key).cloned())
    }

    fn delete_side(&self, key: &str) -> Result<(), StateError> {
        check_name("side key", key)?;
        self.inner()?.side.remove(key);
        Ok(())
    }

    fn tables(&self) -> Option<&dyn TableStore> {
        Some(self)
    }
}

impl TableStore for MemStore {
    fn ensure_tables(&self, deployment: &str, tables: &[TableDef]) -> Result<(), StateError> {
        for t in tables {
            check_table(t)?;
        }
        let mut inner = self.inner()?;
        match &inner.deployment {
            Some(d) if d != deployment => {
                return Err(StateError::Config(format!(
                    "the store holds the tables of deployment {d}, not {deployment}"
                )));
            }
            _ => inner.deployment = Some(deployment.to_owned()),
        }
        let keys = TableDef {
            name: KEYS_TABLE.into(),
            view: None,
            columns: Vec::new(),
        };
        for t in tables.iter().chain(std::iter::once(&keys)) {
            match inner.defs.get(&t.name) {
                Some(old) if old.columns != t.columns => {
                    return Err(StateError::Config(format!(
                        "table {} exists with other columns than the program's",
                        t.name
                    )));
                }
                Some(_) => {}
                None => {
                    inner.defs.insert(t.name.clone(), t.clone());
                    inner.rows.insert(t.name.clone(), Table::new());
                }
            }
        }
        Ok(())
    }

    fn scan_keys(
        &self,
        table: &str,
        owner: &Owner,
        lo: &[u8],
        hi: Option<&[u8]>,
        at: u64,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, StateError> {
        let inner = self.inner()?;
        let rows = inner
            .rows
            .get(table)
            .ok_or_else(|| StateError::Invalid(format!("no table {table}")))?;
        let mut out: Vec<Vec<u8>> = Vec::new();
        for ((o, key, from), (to, _)) in rows.range((owner.clone(), lo.to_vec(), 0)..) {
            if o != owner || hi.is_some_and(|h| key.as_slice() >= h) {
                break;
            }
            if *from <= at && to.is_none_or(|t| t > at) && out.last() != Some(key) {
                if out.len() == limit {
                    break;
                }
                out.push(key.clone());
            }
        }
        Ok(out)
    }

    fn has_key(&self, table: &str, owner: &Owner, key: &[u8], at: u64) -> Result<bool, StateError> {
        let inner = self.inner()?;
        let rows = inner
            .rows
            .get(table)
            .ok_or_else(|| StateError::Invalid(format!("no table {table}")))?;
        Ok(live(rows, owner, key, at))
    }

    fn commit_rows(
        &self,
        object: &str,
        expected: u64,
        writes: &[Write],
        owner: &Owner,
        rows: &[RowChange],
        prune_below: Option<u64>,
    ) -> Result<Commit, StateError> {
        check_commit(object, writes)?;
        let fault = self.fault(object, expected)?;
        if fault == Fault::Fail {
            return Err(StateError::Unavailable(format!(
                "an injected failure of a commit to `{object}`"
            )));
        }
        let mut inner = self.inner()?;
        let current = inner.objects.get(object).map_or(0, |(v, _)| *v);
        if current != expected {
            return Ok(Commit::Conflict { current });
        }
        // The rows first, on copies of the tables they touch: a change that cannot apply leaves everything as it was.
        let mut touched: BTreeMap<String, Table> = BTreeMap::new();
        for change in rows {
            let name = match change {
                RowChange::Open { table, .. } | RowChange::Close { table, .. } => table,
            };
            if !touched.contains_key(name) {
                let t = inner
                    .rows
                    .get(name)
                    .ok_or_else(|| StateError::Invalid(format!("no table {name}")))?
                    .clone();
                touched.insert(name.clone(), t);
            }
            let t = touched
                .get_mut(name)
                .ok_or_else(|| StateError::Invalid(format!("no table {name}")))?;
            match change {
                RowChange::Open { key, from, values, .. } => {
                    if t.range((owner.clone(), key.clone(), 0)..=(owner.clone(), key.clone(), u64::MAX))
                        .any(|(_, (to, _))| to.is_none())
                    {
                        return Err(StateError::Corrupt(format!("{name}: a key opened while open")));
                    }
                    t.insert((owner.clone(), key.clone(), *from), (None, values.clone()));
                }
                RowChange::Close { key, at, .. } => {
                    let open = t
                        .range_mut((owner.clone(), key.clone(), 0)..=(owner.clone(), key.clone(), u64::MAX))
                        .find(|(_, (to, _))| to.is_none())
                        .ok_or_else(|| StateError::Corrupt(format!("{name}: a key closed while not open")))?;
                    open.1.0 = Some(*at);
                }
            }
        }
        if let Some(floor) = prune_below {
            let names: Vec<String> = inner.rows.keys().cloned().collect();
            for name in names {
                let mut t = match touched.remove(&name) {
                    Some(t) => t,
                    None => inner.rows.get(&name).cloned().unwrap_or_default(),
                };
                t.retain(|(o, _, _), (to, _)| o != owner || to.is_none_or(|e| e > floor));
                touched.insert(name, t);
            }
        }
        for (name, t) in touched {
            inner.rows.insert(name, t);
        }
        let (version, entries) = inner.objects.entry(object.to_owned()).or_default();
        for w in writes {
            match w {
                Write::Put(k, v) => {
                    entries.insert(k.clone(), v.clone());
                }
                Write::Delete(k) => {
                    entries.remove(k);
                }
            }
        }
        *version += 1;
        let version = *version;
        drop(inner);
        self.shared.committed.notify_all();
        match fault {
            Fault::Lost => Err(StateError::Unknown {
                object: object.to_owned(),
                reason: "an injected loss of the commit's answer".into(),
            }),
            Fault::Pass | Fault::Fail => Ok(Commit::Done { version }),
        }
    }
}

/// A row still open: its key and typed values.
pub type OpenRow = (Vec<u8>, Vec<crate::SqlValue>);

impl MemStore {
    /// A table's open rows for `owner`, with their values (tests).
    pub fn open_rows(&self, table: &str, owner: &Owner) -> Result<Vec<OpenRow>, StateError> {
        let inner = self.inner()?;
        Ok(inner
            .rows
            .get(table)
            .map(|t| {
                t.iter()
                    .filter(|((o, _, _), (to, _))| o == owner && to.is_none())
                    .map(|((_, k, _), (_, v))| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The tables' definitions (tests).
    pub fn table_defs(&self) -> Result<Vec<TableDef>, StateError> {
        Ok(self.inner()?.defs.values().cloned().collect())
    }
}
