//! Shared recursion bound for hostile `Value`, `LatValue`, and `GroupValue` decoding.
use std::cell::Cell;
thread_local! { static DEPTH: Cell<usize> = const { Cell::new(0) }; }
const MAX_DEPTH: usize = 128;
pub(crate) struct DepthGuard;
impl DepthGuard {
    pub(crate) fn enter<E>(on_limit: impl FnOnce() -> E) -> Result<Self, E> {
        DEPTH.with(|depth| {
            let next = depth.get() + 1;
            if next > MAX_DEPTH {
                Err(on_limit())
            } else {
                depth.set(next);
                Ok(Self)
            }
        })
    }
}
impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}
