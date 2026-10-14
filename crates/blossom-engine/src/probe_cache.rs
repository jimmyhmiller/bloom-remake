//! Synchronized acceleration caches. Row state stays immutable during worker evaluation;
//! only lazy indexes, hot answers and planner estimates are updated concurrently.
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Debug, Default)]
pub(crate) struct ProbeCache<T>(RwLock<T>);
impl<T> ProbeCache<T> {
    pub fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }
    pub fn borrow(&self) -> RwLockReadGuard<'_, T> {
        self.0.read()
    }
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write()
    }
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
}
