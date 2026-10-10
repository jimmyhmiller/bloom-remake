//! External state stores for stateless hosting (docs/design/STATELESS.md §3).
//!
//! A [`StateStore`] keeps **objects**: each a set of entries (key → bytes) and a version that every commit raises by
//! one. A commit is atomic and conditional: it applies only at the version it names, so two hosts that run the same
//! object at once cannot both commit from the same state. Besides objects a store keeps **wake hints** (which objects
//! want running at which time, so any host can find them) and **side records** (unversioned, last write wins).
//!
//! The adapters are crates of their own (`blossom-statestore-sqlite`, `-postgres`, `-s3`); [`MemStore`] is the
//! in-memory one, with injected faults for deterministic tests. [`conformance`] is the suite every adapter runs.

pub mod conformance;
mod mem;
pub mod tables;

use std::time::Duration;

pub use mem::{Fault, MemStore};
pub use tables::{KEYS_TABLE, Owner, RowChange, SqlType, SqlValue, TableDef, TableStore};

/// An object's committed state: its version (0: never committed) and its entries, in key order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub version: u64,
    pub entries: Vec<(String, Vec<u8>)>,
}

/// One write of a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Put(String, Vec<u8>),
    Delete(String),
}

impl Write {
    pub fn key(&self) -> &str {
        match self {
            Write::Put(k, _) | Write::Delete(k) => k,
        }
    }
}

/// What a commit did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// Applied; the object is now at `version`.
    Done { version: u64 },
    /// Not applied: the object is at `current`, not the version the commit named.
    Conflict { current: u64 },
}

/// A state store's failure. A conflict is not one ([`Commit::Conflict`]).
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// The store could not be reached or refused the request; nothing was changed.
    #[error("the state store is unavailable: {0}")]
    Unavailable(String),
    /// A commit was sent but its outcome is not known (the connection failed after it). The caller must treat the
    /// object as unknown and load it again.
    #[error("a commit to `{object}` may or may not have applied: {reason}")]
    Unknown { object: String, reason: String },
    /// The store holds something this code cannot read.
    #[error("corrupt state in the store: {0}")]
    Corrupt(String),
    /// A request the store cannot take (a name it cannot hold, a value too large).
    #[error("invalid state store request: {0}")]
    Invalid(String),
    /// The store's configuration (its URL, credentials, schema) is wrong.
    #[error("state store configuration: {0}")]
    Config(String),
}

/// The storage a stateless host keeps everything in (docs/design/STATELESS.md §3). Object names, entry keys and side
/// keys are non-empty UTF-8 of at most [`MAX_NAME`] bytes with no NUL; [`check_name`] says which are.
pub trait StateStore: Send + Sync {
    /// The object's entries and version (version 0 and no entries: never committed).
    fn load(&self, object: &str) -> Result<Snapshot, StateError>;
    /// The object's version, without its entries.
    fn version(&self, object: &str) -> Result<u64, StateError>;
    /// Applies `writes` in order and raises the version to `expected + 1`, if the object is still at `expected`;
    /// else changes nothing and reports the version there. A write to a key the commit writes again is overwritten by
    /// the later one; deleting a key that is not there does nothing.
    fn commit(&self, object: &str, expected: u64, writes: &[Write]) -> Result<Commit, StateError>;
    /// Returns once the object's version is past `since`, or after `timeout`: the version then (which may still be
    /// `since`).
    fn wait(&self, object: &str, since: u64, timeout: Duration) -> Result<u64, StateError>;
    /// Adds a hint that the object wants waking at `at` (milliseconds since the epoch). Hints are a set: the same
    /// hint twice is one.
    fn schedule(&self, object: &str, at: u64) -> Result<(), StateError>;
    /// The hints due at `now` (at most `limit`), earliest first.
    fn due(&self, now: u64, limit: usize) -> Result<Vec<(String, u64)>, StateError>;
    /// Removes one hint (nothing when it is not there).
    fn unschedule(&self, object: &str, at: u64) -> Result<(), StateError>;
    /// Writes a side record: no version, last write wins.
    fn put_side(&self, key: &str, value: &[u8]) -> Result<(), StateError>;
    fn get_side(&self, key: &str) -> Result<Option<Vec<u8>>, StateError>;
    fn delete_side(&self, key: &str) -> Result<(), StateError>;
    /// Housekeeping a host runs now and then (its sweeper): for a store that leaves garbage behind (the S3 store's
    /// unreferenced blobs), collecting it. `now` is milliseconds since the epoch. Most stores have none.
    fn maintain(&self, _now: u64) -> Result<(), StateError> {
        Ok(())
    }
    /// The store's tables, when its objects' durable relations can be tables (docs/design/SQL-TABLES.md).
    fn tables(&self) -> Option<&dyn TableStore> {
        None
    }
}

/// The longest object name, entry key or side key, in bytes.
pub const MAX_NAME: usize = 1024;

/// Whether `name` can be an object name, entry key or side key.
pub fn check_name(what: &str, name: &str) -> Result<(), StateError> {
    if name.is_empty() {
        return Err(StateError::Invalid(format!("an empty {what}")));
    }
    if name.len() > MAX_NAME {
        return Err(StateError::Invalid(format!(
            "a {what} of {} bytes (at most {MAX_NAME})",
            name.len()
        )));
    }
    if name.contains('\0') {
        return Err(StateError::Invalid(format!("a {what} with a NUL byte")));
    }
    Ok(())
}

/// Checks a commit's names before it is sent.
pub fn check_commit(object: &str, writes: &[Write]) -> Result<(), StateError> {
    check_name("object name", object)?;
    for w in writes {
        check_name("entry key", w.key())?;
    }
    Ok(())
}
