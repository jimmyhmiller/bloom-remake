//! The node's database in the runtime (docs/design/DATABASE.md): `blossom_node::database::Database`, opened by
//! recovery, with a thread of its own for the flushes the engine asks for, and a stopped store's read-only open for
//! tools (`blossom query --store`).

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use blossom_ir::ValidatedProgram;
pub use blossom_node::database::{Database, KEY_FORMAT};
use blossom_store::lsm::Flushed;
use blossom_store::{RealFs, StoreError, StoreLock, Vfs};

use crate::RuntimeError;

/// Starts the database thread: each request flushes the database (and compacts what is due), and `done` gets what
/// the tables then cover, or why the flush failed. The thread ends when the sender it returns is dropped.
pub(crate) fn start(
    db: Arc<Database>,
    done: Box<dyn Fn(Result<Flushed, String>) + Send>,
) -> Result<(Sender<()>, std::thread::JoinHandle<()>), RuntimeError> {
    let (tx, rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("database".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                done(db.flush().map_err(|e| e.to_string()));
            }
        })
        .map_err(RuntimeError::Io)?;
    Ok((tx, handle))
}

/// A stopped node's database, read without changing a file (`blossom query --store`): under the store's lock, its
/// tables, and the WAL records after them applied in memory (the records the node's next recovery keeps). The lock
/// comes back with it: the node cannot start while it is held.
pub fn open_offline(
    dir: &Path,
    program: &ValidatedProgram,
    names: Arc<[Arc<str>]>,
) -> Result<(Database, StoreLock), RuntimeError> {
    let fs: Arc<dyn Vfs> = Arc::new(RealFs);
    let lock = match StoreLock::acquire(&*fs, dir) {
        Ok(l) => l,
        Err(StoreError::Locked { pid, .. }) => {
            return Err(RuntimeError::Config(format!(
                "the node is running (process {}): query it through its admin listener",
                pid.trim()
            )));
        }
        Err(e) => return Err(RuntimeError::Store(e)),
    };
    let db = Database::open_read_only(fs, dir, program, names)?;
    Ok((db, lock))
}
