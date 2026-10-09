//! The memory store against the conformance suite, and its injected faults.

use blossom_statestore::conformance::Harness;
use blossom_statestore::{Commit, Fault, MemStore, StateError, StateStore, Write};

struct Mem(MemStore);

impl Harness for Mem {
    fn open(&self) -> Result<Box<dyn StateStore>, StateError> {
        Ok(Box::new(self.0.clone()))
    }
}

blossom_statestore::statestore_conformance!(|| Mem(MemStore::new()));

#[test]
fn a_failed_commit_applies_nothing_and_a_lost_one_applies() {
    let s = MemStore::new();
    s.set_faults(|_, expected| if expected == 0 { Fault::Fail } else { Fault::Lost })
        .unwrap();
    let w = [Write::Put("a".into(), b"1".to_vec())];
    assert!(matches!(s.commit("o", 0, &w), Err(StateError::Unavailable(_))));
    assert_eq!(s.version("o").unwrap(), 0);
    s.clear_faults().unwrap();
    assert_eq!(s.commit("o", 0, &w).unwrap(), Commit::Done { version: 1 });
    s.set_faults(|_, _| Fault::Lost).unwrap();
    assert!(matches!(s.commit("o", 1, &w), Err(StateError::Unknown { .. })));
    assert_eq!(
        s.version("o").unwrap(),
        2,
        "a lost commit's outcome is unknown to its caller, but it applied"
    );
}
