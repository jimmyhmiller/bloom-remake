//! A [`StateStore`] in memory: one process, shared by its clones. Faults can be injected per commit, for the
//! deterministic tests of hosts that race on one store (docs/design/STATELESS.md §10).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

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
}
