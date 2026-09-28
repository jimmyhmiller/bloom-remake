//! The static reachability behind conservative negative support (TEST-025, CR-31): falsifying a fact of relation
//! `q` can make a fact of relation `p` appear only if `p` is reachable from `q` along rule edges. A negated read of
//! `p` at tick `t` is supported by the facts of every such `q` at ticks before `t`, and at `t` itself when some path
//! from `q` to `p` is purely deductive.

use std::collections::{BTreeMap, BTreeSet};

use blossom_artifact::ded::{DedArtifact, DedRelIdx, DedRelKind, EdgeTime};

/// For each relation `p`: the relations `q` it is reachable from, each with whether a purely deductive path exists.
#[derive(Clone, Debug, Default)]
pub struct Preds {
    preds: BTreeMap<u32, BTreeMap<u32, bool>>,
}

impl Preds {
    /// Reachability over a `.ded` program's rule graph. The crash oracle is not a source: faults cannot remove its
    /// facts.
    pub fn of(artifact: &DedArtifact) -> Preds {
        let mut edges: BTreeMap<u32, Vec<(u32, bool)>> = BTreeMap::new();
        for e in &artifact.edges {
            edges
                .entry(e.from.0)
                .or_default()
                .push((e.to.0, e.time == EdgeTime::Deductive));
        }
        let mut preds: BTreeMap<u32, BTreeMap<u32, bool>> = BTreeMap::new();
        for (src, rel) in artifact.rels.iter().enumerate() {
            if rel.kind == DedRelKind::Crash {
                continue;
            }
            let Ok(src) = u32::try_from(src) else { continue };
            // States: (relation, whether every edge so far was deductive).
            let mut seen: BTreeSet<(u32, bool)> = BTreeSet::new();
            let mut work = vec![(src, true)];
            while let Some((v, ded)) = work.pop() {
                for &(w, is_ded) in edges.get(&v).map_or(&[][..], Vec::as_slice) {
                    let d2 = ded && is_ded;
                    let slot = preds.entry(w).or_default().entry(src).or_insert(false);
                    *slot |= d2;
                    if seen.insert((w, d2)) {
                        work.push((w, d2));
                    }
                }
            }
        }
        Preds { preds }
    }

    /// The relations `p` is reachable from, with the deductive-path flag.
    pub fn of_rel(&self, p: u32) -> impl Iterator<Item = (u32, bool)> + '_ {
        self.preds.get(&p).into_iter().flatten().map(|(q, d)| (*q, *d))
    }
}

/// The index of a relation as a logical relation id.
pub fn logical(idx: DedRelIdx) -> u32 {
    idx.0
}
