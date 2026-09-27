#![deny(unsafe_op_in_unsafe_fn)]
//! `blossom-value`: types and values (ARCHITECTURE §1.2, §2.1, §2.2, §4.1; LANGUAGE §5).
//!
//! - [`types`] — the type language and the hash-consed [`TypeTable`];
//! - [`value`] — the canonical [`Value`] model, covering every [`TypeDef`], with lattice and group values as data;
//! - [`order`] — the canonical total order of LANGUAGE §5.5 (`impl Ord for Value`, LANG-024);
//! - [`word`] — [`Word`], [`Lane`] and the order-preserving scalar encodings;
//! - [`fp`], [`prf`], [`digest`] — fingerprints (xxh3), the SipHash-1-3 PRF and seed streams, BLAKE3 and
//!   incremental set digests;
//! - [`store`], [`sink`] — [`ValueStore`], [`RecordBuilder`], [`RefValueStore`] and [`WordSink`];
//! - [`externs`] — [`ExternFn`], [`ExternTableFn`] and the [`ExternRegistry`];
//! - [`time`] — [`Tick`], [`NodeId`], [`Instant`], [`Duration`], [`Incarnation`];
//! - [`class`] — the operation-class enums (PLAN §4 D2), re-exported by `blossom-ir::core::lattice`;
//! - [`error`] — [`ValueError`].
//!
//! WP M1.1 published this type surface and implemented the type table and the canonical order. The encodings,
//! fingerprints, PRF, digests and the reference store are implemented by WP M2.1; until then those functions
//! return [`Unimplemented`](blossom_base::Unimplemented) naming their FEATURES id. The crate is frozen after
//! milestone M2 (ARCHITECTURE §1.6).

pub mod class;
pub mod digest;
pub mod error;
pub mod externs;
pub mod fp;
pub mod order;
pub mod prf;
pub mod sink;
pub mod store;
pub mod time;
pub mod types;
pub mod value;
pub mod word;

mod serde_util;
#[cfg(test)]
mod testgen;

pub use class::{Claim, HeightClass, LatOpKind, LawStatus, MonoClass, ProofStatus};
pub use digest::{Digest128, Digest256, SetElement};
pub use error::ValueError;
pub use externs::{ExternError, ExternFn, ExternRegistry, ExternTableFn, TableFn};
pub use fp::{ENCODING_VERSION, Fingerprint};
pub use prf::{PRF_VERSION, PrfStream, Seed, Seeds};
pub use sink::{IngestSlot, RowMeta, RowSender, WordSink};
pub use store::{RecordBuilder, RecordTarget, RefValueStore, ValueStore};
pub use time::{Duration, Incarnation, Instant, NodeId, Tick};
pub use types::{
    EnumDef, ExternCodecId, ExternTypeDef, FieldDef, FieldNo, IntTy, StructDef, TypeDef, TypeTable, VariantDef,
};
pub use value::{BlobRef, GroupValue, IntValue, LatValue, ModValue, SessionId, Value};
pub use word::{BytesWord, ColEncTag, Lane, NicheScalar, ScalarKind, StrWord, Word};

/// The version of this crate's public API (ARCHITECTURE §1.6).
pub const API_VERSION: u32 = 1;
