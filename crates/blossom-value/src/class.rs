//! Operation classes and proof statuses (ARCHITECTURE §2.3, PLAN §4 D2).
//!
//! These enums live here, below both the IR and the lattice library, so `blossom-ir` (which re-exports them from
//! `core::lattice`) and `blossom-lattice` can be built independently.

use blossom_base::Symbol;
use serde::{Deserialize, Serialize};

/// The monotonicity class of an operation argument, relative to the natural order (LANG-125, LANGUAGE §11.4).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum MonoClass {
    /// Join-preserving: `f(a ⊔ b) = f(a) ⊔ f(b)`.
    Morphism,
    /// A morphism in each argument separately.
    Bimorphism,
    /// Order-preserving.
    Monotone,
    /// Order-reversing.
    Antitone,
    /// Neither.
    NonMonotone,
    /// A monotone map into `bool` (or into `Option<T>` whose `Some` never changes).
    Threshold,
    /// Does not depend on the argument.
    Constant,
}

impl MonoClass {
    /// Whether a call needs a bang (LANGUAGE §11.4): antitone and non-monotone operations do.
    pub const fn needs_bang(self) -> bool {
        matches!(self, MonoClass::Antitone | MonoClass::NonMonotone)
    }
}

/// The kind of a lattice operation in the operation catalogue (R04 §2.4).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum LatOpKind {
    /// A threshold.
    Threshold,
    /// A morphism.
    Morphism,
    /// A bimorphism.
    Bimorphism,
    /// Monotone.
    Monotone,
    /// Antitone.
    Antitone,
    /// Non-monotone.
    NonMonotone,
    /// `stable fn … after t`: never changes once the threshold method `after` holds (LANGUAGE §11.6).
    Stable {
        /// The guarding threshold method.
        after: Symbol,
    },
}

/// The height class of a lattice, for termination of lattice recursion (ENG-142).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum HeightClass {
    /// Ascending chain condition.
    Acc,
    /// p-stable.
    PStable,
    /// Not known.
    Unknown,
}

/// The status of a lattice's laws (ODD-09 (c), TEST-087).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum LawStatus {
    /// A built-in lattice.
    Builtin,
    /// Proved by the SMT backend.
    Proved,
    /// Property-tested by the law harness.
    Tested,
    /// Refuted (BLS0704).
    Refuted,
}

/// An algebraic property claim on a function (`#[injective]`, `#[commutative]`, …).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Claim {
    /// Not claimed.
    Absent,
    /// Claimed, with its checking status.
    Claimed(ProofStatus),
}

/// The status of a checked claim (TEST-087). Only [`ProofStatus::Proved`] enables the ANA-015 upgrades.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum ProofStatus {
    /// Not checked yet.
    Unchecked,
    /// Proved.
    Proved,
    /// Property-tested.
    Tested,
    /// Refuted.
    Refuted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_bang_rule() {
        let banged: Vec<MonoClass> = [
            MonoClass::Morphism,
            MonoClass::Bimorphism,
            MonoClass::Monotone,
            MonoClass::Antitone,
            MonoClass::NonMonotone,
            MonoClass::Threshold,
            MonoClass::Constant,
        ]
        .into_iter()
        .filter(|c| c.needs_bang())
        .collect();
        assert_eq!(banged, vec![MonoClass::Antitone, MonoClass::NonMonotone]);
    }

    #[test]
    fn class_serde_roundtrip() {
        let kinds = vec![
            LatOpKind::Threshold,
            LatOpKind::Stable {
                after: Symbol::intern("complete"),
            },
        ];
        let json = serde_json::to_string(&kinds).unwrap();
        assert_eq!(serde_json::from_str::<Vec<LatOpKind>>(&json).unwrap(), kinds);
        let claim = Claim::Claimed(ProofStatus::Tested);
        assert_eq!(
            serde_json::from_str::<Claim>(&serde_json::to_string(&claim).unwrap()).unwrap(),
            claim
        );
    }
}
