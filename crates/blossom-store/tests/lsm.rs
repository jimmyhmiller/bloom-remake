//! The LSM tree (docs/design/DATABASE.md §2, §6) against a model: versioned puts and deletes, flushes and compactions
//! with a small memtable, blocks and history, and as-of reads of every version the history keeps; reopening; and a
//! crash at every filesystem operation of a flush and a compaction, after which the tree opens to the state its
//! manifest names and the versions after it apply again.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use blossom_store::lsm::{Lsm, LsmOptions, Op};
use blossom_store::{SimFs, Vfs, WriteFate};

#[cfg(test)]
/// A deterministic generator (SplitMix64).
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[cfg(test)]
/// Keys under four one-byte "relations", each with a variable-length body.
fn key(rng: &mut Rng) -> Vec<u8> {
    let mut k = vec![b'a' + rng.below(4) as u8];
    let n = 1 + rng.below(3);
    for _ in 0..n {
        k.push(rng.below(6) as u8);
    }
    k
}

#[cfg(test)]
/// One version's changes.
type Changes = Vec<(Vec<u8>, Op)>;

#[cfg(test)]
/// The changes of each version, and the model they build.
#[derive(Default, Clone)]
struct Model {
    log: Vec<(u64, Changes)>,
}

#[cfg(test)]
impl Model {
    /// The present keys with `prefix` as of `version`.
    fn at(&self, prefix: &[u8], version: u64) -> Vec<Vec<u8>> {
        let mut state: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
        for (v, changes) in &self.log {
            if *v > version {
                break;
            }
            for (k, op) in changes {
                state.insert(k.clone(), *op);
            }
        }
        state
            .into_iter()
            .filter(|(k, op)| *op == Op::Put && k.starts_with(prefix))
            .map(|(k, _)| k)
            .collect()
    }

    fn version(&mut self, rng: &mut Rng, v: u64) -> Vec<(Vec<u8>, Op)> {
        let n = 1 + rng.below(12);
        let mut changes: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
        for _ in 0..n {
            let op = if rng.below(3) == 0 { Op::Del } else { Op::Put };
            changes.insert(key(rng), op);
        }
        let changes: Vec<_> = changes.into_iter().collect();
        self.log.push((v, changes.clone()));
        changes
    }
}

#[cfg(test)]
fn opts() -> LsmOptions {
    LsmOptions {
        memtable_bytes: 600,
        block_bytes: 120,
        tier: 3,
        max_tables: 6,
        history: 25,
        cache_bytes: 2048,
    }
}

#[cfg(test)]
const PREFIXES: [&[u8]; 6] = [b"", b"a", b"b", b"c\x01", b"d\x02\x03", b"z"];

#[cfg(test)]
/// Every as-of read the tree allows agrees with the model.
fn agree(lsm: &Lsm, model: &Model) {
    let (floor, applied) = (lsm.floor().unwrap(), lsm.applied().unwrap());
    for v in floor..=applied {
        let present = model.at(b"", v);
        for p in PREFIXES {
            assert_eq!(lsm.scan(p, v).unwrap(), model.at(p, v), "as of {v}, prefix {p:?}");
        }
        // Point lookups of present keys and of keys the model never had or has deleted.
        for k in present
            .iter()
            .take(8)
            .chain([b"zz".to_vec(), b"a\x05\x05\x05".to_vec()].iter())
        {
            assert_eq!(lsm.get(k, v).unwrap(), present.contains(k), "get {k:?} as of {v}");
        }
        for (_, changes) in model.log.iter().filter(|(cv, _)| *cv <= v).rev().take(3) {
            for (k, _) in changes {
                assert_eq!(lsm.get(k, v).unwrap(), present.contains(k), "get {k:?} as of {v}");
            }
        }
    }
}

#[test]
fn reads_as_of_every_version_kept_agree_with_the_model() {
    for seed in 0..6 {
        let fs = SimFs::default();
        let fs: Arc<dyn Vfs> = Arc::new(fs);
        let dir = Path::new("/db");
        let lsm = Lsm::open(fs.clone(), dir, opts()).unwrap();
        let mut rng = Rng(seed);
        let mut model = Model::default();
        let mut compactions = 0;
        for v in 1..=160u64 {
            let changes = model.version(&mut rng, v);
            lsm.apply(v, v * 10, changes).unwrap();
            if lsm.needs_flush().unwrap() {
                lsm.flush().unwrap();
                while lsm.compact().unwrap() {
                    compactions += 1;
                }
            }
            if v % 20 == 0 {
                agree(&lsm, &model);
            }
        }
        assert!(compactions > 0, "seed {seed}: the workload never compacted");
        assert!(
            lsm.floor().unwrap() > 0,
            "seed {seed}: compaction merged no history away"
        );
        // A version merged away, and one not yet applied, are refused.
        assert!(lsm.scan(b"", lsm.floor().unwrap() - 1).is_err());
        assert!(lsm.scan(b"", 161).is_err());
        // Reopened: the tables give the state as of the flushed version; the versions after it apply again.
        lsm.flush().unwrap();
        let flushed = lsm.flushed().unwrap();
        assert_eq!((flushed.version, flushed.mark), (160, 1600));
        drop(lsm);
        let again = Lsm::open(fs.clone(), dir, opts()).unwrap();
        assert_eq!(again.applied().unwrap(), 160);
        agree(&again, &model);
    }
}

#[cfg(test)]
/// Runs the workload to `upto` with a flush when due, compactions after it; returns the model.
fn run(lsm: &Lsm, rng: &mut Rng, model: &mut Model, from: u64, upto: u64) {
    for v in from..=upto {
        let changes = model.version(rng, v);
        lsm.apply(v, v, changes).unwrap();
        if lsm.needs_flush().unwrap() {
            lsm.flush().unwrap();
            while lsm.compact().unwrap() {}
        }
    }
}

#[test]
fn a_crash_anywhere_in_a_flush_or_compaction_reopens_to_the_manifests_state() {
    let fs = SimFs::default();
    let dir = Path::new("/db");
    let mut rng = Rng(42);
    let mut model = Model::default();
    let dynfs: Arc<dyn Vfs> = Arc::new(fs.clone());
    let lsm = Lsm::open(dynfs.clone(), dir, opts()).unwrap();
    run(&lsm, &mut rng, &mut model, 1, 70);
    // The operations of the next flushes and compactions, recorded.
    fs.enable_crash_recording().unwrap();
    run(&lsm, &mut rng, &mut model, 71, 110);
    lsm.flush().unwrap();
    while lsm.compact().unwrap() {}
    let cuts = fs.recorded_cuts().unwrap();
    assert!(cuts.len() > 20, "only {} cuts", cuts.len());
    let fates = [WriteFate::Lost, WriteFate::Survive, WriteFate::Torn { sectors: 0 }];
    for (i, cut) in cuts.iter().enumerate() {
        for fate in fates {
            let mut crashed = cut.fork().unwrap();
            crashed.crash(&mut |_| fate).unwrap();
            let fs: Arc<dyn Vfs> = Arc::new(crashed);
            let reopened = Lsm::open(fs, dir, opts())
                .unwrap_or_else(|e| panic!("cut {i} ({fate:?}): the tree does not open: {e}"));
            let flushed = reopened.flushed().unwrap().version;
            assert!((60..=110).contains(&flushed), "cut {i}: flushed {flushed}");
            // The versions after the manifest's, from the WAL in a node: applied again, the tree is whole.
            for (v, changes) in model.log.iter().filter(|(v, _)| *v > flushed) {
                reopened.apply(*v, *v, changes.clone()).unwrap();
            }
            for p in PREFIXES {
                assert_eq!(
                    reopened.scan(p, 110).unwrap(),
                    model.at(p, 110),
                    "cut {i} ({fate:?}), prefix {p:?}"
                );
            }
        }
    }
}
