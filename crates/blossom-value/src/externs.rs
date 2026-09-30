//! Host functions at the value level: [`ExternFn`], [`ExternTableFn`] and the [`ExternRegistry`]
//! (ARCHITECTURE §4.7; LANG-181, LANG-183).
//!
//! The registry holds implementations by path. The engine calls them through a word adapter and the oracle calls
//! them directly, so both share one implementation. A missing implementation is a load-time error that lists every
//! unbound path ([`ExternRegistry::unbound`]), never a runtime stub. Checking each registration's signature against
//! the program's declared types when a program is bound is implemented by WP M2.1.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use blossom_base::{TypeId, Unimplemented};

use crate::error::ValueError;
use crate::types::{IntTy, TypeDef, TypeTable};
use crate::value::Value;

/// A host function's failure. Inside a tick it aborts the tick with a located error (BLSR010).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExternError {
    /// The function refused its input.
    #[error("{0}")]
    Failed(Arc<str>),
    /// The arguments did not have the declared shape (a binding bug upstream).
    #[error("invalid arguments: {0}")]
    InvalidArguments(Arc<str>),
    /// The function relies on a feature that is not implemented yet.
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
}

/// A pure host function (`extern fn`, LANG-181): memoized per input per tick; purity is checked by double
/// evaluation under simulation.
pub trait ExternFn: Send + Sync {
    /// Calls the function.
    fn call(&self, args: &[Value]) -> Result<Value, ExternError>;
}

/// A host table function (`extern table fn`, LANG-183): produces rows for bound inputs. It may read the world; its
/// rows are recorded in the trace as inputs.
pub trait ExternTableFn: Send + Sync {
    /// Calls the function, returning its output rows.
    fn call(&self, args: &[Value]) -> Result<Vec<Vec<Value>>, ExternError>;
}

impl<F> ExternFn for F
where
    F: Fn(&[Value]) -> Result<Value, ExternError> + Send + Sync,
{
    fn call(&self, args: &[Value]) -> Result<Value, ExternError> {
        self(args)
    }
}

/// A closure used as an [`ExternTableFn`] (a newtype, because closures already implement [`ExternFn`] by shape).
pub struct TableFn<F>(pub F);

impl<F> ExternTableFn for TableFn<F>
where
    F: Fn(&[Value]) -> Result<Vec<Vec<Value>>, ExternError> + Send + Sync,
{
    fn call(&self, args: &[Value]) -> Result<Vec<Vec<Value>>, ExternError> {
        (self.0)(args)
    }
}

/// A host function's parameter or result type, independent of any program's type table (a `TypeId` names a type
/// only within one table). [`HostType::matches`] compares it with a program's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostType {
    Bool,
    Int(IntTy),
    Str,
    Bytes,
    Unit,
    Option(&'static HostType),
    Vec(&'static HostType),
    Tuple(&'static [HostType]),
}

impl HostType {
    /// Whether `ty` in `types` is this type.
    pub fn matches(&self, types: &TypeTable, ty: TypeId) -> bool {
        match (self, types.get(ty)) {
            (HostType::Bool, Some(TypeDef::Bool))
            | (HostType::Str, Some(TypeDef::Str))
            | (HostType::Bytes, Some(TypeDef::Bytes))
            | (HostType::Unit, Some(TypeDef::Unit)) => true,
            (HostType::Int(a), Some(TypeDef::Int(b))) => a == b,
            (HostType::Option(a), Some(TypeDef::Option(b))) | (HostType::Vec(a), Some(TypeDef::Vec(b))) => {
                a.matches(types, *b)
            }
            (HostType::Tuple(a), Some(TypeDef::Tuple(b))) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.matches(types, *y))
            }
            _ => false,
        }
    }
}

impl fmt::Display for HostType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostType::Bool => write!(f, "bool"),
            HostType::Int(t) => write!(f, "{}", t.name()),
            HostType::Str => write!(f, "String"),
            HostType::Bytes => write!(f, "Bytes"),
            HostType::Unit => write!(f, "()"),
            HostType::Option(t) => write!(f, "Option<{t}>"),
            HostType::Vec(t) => write!(f, "Vec<{t}>"),
            HostType::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
        }
    }
}

/// The signature of a host registration, compared with a program's `extern` declaration when the program is bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternSignature {
    pub params: Vec<HostType>,
    /// One scalar return for `extern fn`, or columns for `extern table fn`.
    pub outputs: Vec<HostType>,
    pub table: bool,
}

impl ExternSignature {
    /// Whether a declaration with parameter types `params` and output types `outputs` (in `types`) has this
    /// signature.
    pub fn matches(&self, types: &TypeTable, params: &[TypeId], outputs: &[TypeId]) -> bool {
        self.params.len() == params.len()
            && self.outputs.len() == outputs.len()
            && self.params.iter().zip(params).all(|(h, t)| h.matches(types, *t))
            && self.outputs.iter().zip(outputs).all(|(h, t)| h.matches(types, *t))
    }
}

/// One host function of the standard library (FOREIGN-PROTOCOLS §4): its path and signature. `blossom-std-host`
/// implements exactly these; the compiler checks a program's `extern fn` declarations against them, so an extern
/// that names anything else is a compile error.
#[derive(Clone, Copy, Debug)]
pub struct StdExtern {
    pub path: &'static str,
    pub params: &'static [HostType],
    pub ret: HostType,
}

impl StdExtern {
    pub fn signature(&self) -> ExternSignature {
        ExternSignature {
            params: self.params.to_vec(),
            outputs: vec![self.ret],
            table: false,
        }
    }
}

const BYTES: HostType = HostType::Bytes;
const OPT_BYTES: HostType = HostType::Option(&HostType::Bytes);
const U8: HostType = HostType::Int(IntTy::U8);
const U32: HostType = HostType::Int(IntTy::U32);
const U64: HostType = HostType::Int(IntTy::U64);

/// The standard library's host functions. Decompression takes the largest output it may produce, and is `None`
/// past it or on malformed input.
pub const STD_EXTERNS: &[StdExtern] = &[
    StdExtern { path: "blossom_std::checksum::crc32c", params: &[BYTES], ret: U32 },
    StdExtern { path: "blossom_std::checksum::crc32", params: &[BYTES], ret: U32 },
    StdExtern { path: "blossom_std::compress::gzip_compress", params: &[BYTES, U8], ret: BYTES },
    StdExtern { path: "blossom_std::compress::gzip_decompress", params: &[BYTES, U64], ret: OPT_BYTES },
    StdExtern { path: "blossom_std::compress::snappy_compress", params: &[BYTES], ret: BYTES },
    StdExtern { path: "blossom_std::compress::snappy_decompress", params: &[BYTES, U64], ret: OPT_BYTES },
    StdExtern { path: "blossom_std::compress::lz4_compress", params: &[BYTES], ret: BYTES },
    StdExtern { path: "blossom_std::compress::lz4_decompress", params: &[BYTES, U64], ret: OPT_BYTES },
    StdExtern { path: "blossom_std::compress::zstd_compress", params: &[BYTES], ret: BYTES },
    StdExtern { path: "blossom_std::compress::zstd_decompress", params: &[BYTES, U64], ret: OPT_BYTES },
    StdExtern { path: "blossom_std::hash::sha256", params: &[BYTES], ret: BYTES },
    StdExtern { path: "blossom_std::hash::blake3", params: &[BYTES], ret: BYTES },
];

/// The standard host function at `path`.
pub fn std_extern(path: &str) -> Option<&'static StdExtern> {
    STD_EXTERNS.iter().find(|e| e.path == path)
}

/// Host function implementations by path (for example `blossom_std::hash::sha256`).
#[derive(Clone, Default)]
pub struct ExternRegistry {
    signatures: BTreeMap<Arc<str>, ExternSignature>,
    fns: BTreeMap<Arc<str>, Arc<dyn ExternFn>>,
    table_fns: BTreeMap<Arc<str>, Arc<dyn ExternTableFn>>,
}

impl ExternRegistry {
    /// An empty registry.
    pub fn new() -> ExternRegistry {
        ExternRegistry::default()
    }

    /// Registers an `extern fn`; a path may be registered once, as either kind.
    pub fn register_fn(&mut self, path: impl Into<Arc<str>>, f: impl ExternFn + 'static) -> Result<(), ValueError> {
        let path = path.into();
        self.check_free(&path)?;
        self.fns.insert(path, Arc::new(f));
        Ok(())
    }

    /// Registers an `extern table fn`; a path may be registered once, as either kind.
    pub fn register_table_fn(
        &mut self,
        path: impl Into<Arc<str>>,
        f: impl ExternTableFn + 'static,
    ) -> Result<(), ValueError> {
        let path = path.into();
        self.check_free(&path)?;
        self.table_fns.insert(path, Arc::new(f));
        Ok(())
    }

    /// Registers a host scalar function with a concrete signature for load-time binding.
    pub fn register_typed_fn(
        &mut self,
        path: impl Into<Arc<str>>,
        params: Vec<HostType>,
        ret: HostType,
        f: impl ExternFn + 'static,
    ) -> Result<(), ValueError> {
        let path = path.into();
        self.register_fn(path.clone(), f)?;
        self.signatures.insert(
            path,
            ExternSignature {
                params,
                outputs: vec![ret],
                table: false,
            },
        );
        Ok(())
    }
    /// Registers a host table function with its output columns.
    pub fn register_typed_table_fn(
        &mut self,
        path: impl Into<Arc<str>>,
        params: Vec<HostType>,
        outputs: Vec<HostType>,
        f: impl ExternTableFn + 'static,
    ) -> Result<(), ValueError> {
        let path = path.into();
        self.register_table_fn(path.clone(), f)?;
        self.signatures.insert(
            path,
            ExternSignature {
                params,
                outputs,
                table: true,
            },
        );
        Ok(())
    }
    /// Checks a program's declaration of `path` (parameter and output types in `types`) against the registered
    /// implementation before the program is loaded. Untyped registrations are useful for direct tests but cannot be
    /// bound to source programs.
    pub fn bind(
        &self,
        path: &str,
        types: &TypeTable,
        params: &[TypeId],
        outputs: &[TypeId],
        table: bool,
    ) -> Result<(), ValueError> {
        if !self.contains(path) {
            return Err(ValueError::ExternSignature {
                path: path.into(),
                reason: "no host implementation registered".into(),
            });
        }
        let actual = self.signatures.get(path).ok_or_else(|| ValueError::ExternSignature {
            path: path.into(),
            reason: "host registration has no declared signature".into(),
        })?;
        if actual.table != table || !actual.matches(types, params, outputs) {
            return Err(ValueError::ExternSignature {
                path: path.into(),
                reason: format!("the program's declaration differs from the host's {actual:?}"),
            });
        }
        Ok(())
    }

    /// The signature registered at `path`, if it was registered with one.
    pub fn signature(&self, path: &str) -> Option<&ExternSignature> {
        self.signatures.get(path)
    }

    fn check_free(&self, path: &Arc<str>) -> Result<(), ValueError> {
        if self.contains(path) {
            Err(ValueError::DuplicateExtern(path.clone()))
        } else {
            Ok(())
        }
    }

    /// The `extern fn` registered at `path`.
    pub fn lookup_fn(&self, path: &str) -> Option<&Arc<dyn ExternFn>> {
        self.fns.get(path)
    }

    /// The `extern table fn` registered at `path`.
    pub fn lookup_table_fn(&self, path: &str) -> Option<&Arc<dyn ExternTableFn>> {
        self.table_fns.get(path)
    }

    /// Whether something is registered at `path`.
    pub fn contains(&self, path: &str) -> bool {
        self.fns.contains_key(path) || self.table_fns.contains_key(path)
    }

    /// Every registered path, sorted.
    pub fn paths(&self) -> Vec<&str> {
        let mut all: Vec<&str> = self.fns.keys().chain(self.table_fns.keys()).map(|p| &**p).collect();
        all.sort_unstable();
        all
    }

    /// The number of registered functions.
    pub fn len(&self) -> usize {
        self.fns.len() + self.table_fns.len()
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty() && self.table_fns.is_empty()
    }

    /// The paths among `paths` with no registered implementation, sorted and deduplicated: the load-time error
    /// that lists every unbound extern.
    pub fn unbound<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut missing: Vec<&'a str> = paths.into_iter().filter(|p| !self.contains(p)).collect();
        missing.sort_unstable();
        missing.dedup();
        missing
    }

    /// Moves every registration of `other` into this registry; fails on a path registered in both.
    pub fn merge(&mut self, other: ExternRegistry) -> Result<(), ValueError> {
        if let Some(dup) = other.paths().into_iter().find(|p| self.contains(p)) {
            return Err(ValueError::DuplicateExtern(dup.into()));
        }
        self.fns.extend(other.fns);
        self.table_fns.extend(other.table_fns);
        self.signatures.extend(other.signatures);
        Ok(())
    }
}

impl fmt::Debug for ExternRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternRegistry")
            .field("fns", &self.fns.keys().collect::<Vec<_>>())
            .field("table_fns", &self.table_fns.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(args: &[Value]) -> Result<Value, ExternError> {
        match args {
            [Value::Int(crate::IntValue::I64(n))] => n
                .checked_mul(2)
                .map(Value::i64)
                .ok_or_else(|| ExternError::Failed("overflow".into())),
            _ => Err(ExternError::InvalidArguments("expected one i64".into())),
        }
    }

    #[test]
    fn extern_registry_unbound_listed() {
        let mut reg = ExternRegistry::new();
        reg.register_fn("m::double", double).unwrap();
        reg.register_table_fn(
            "m::lines",
            TableFn(|_: &[Value]| Ok(vec![vec![Value::u64(1), Value::str("a")]])),
        )
        .unwrap();
        assert_eq!(
            reg.unbound(["m::zeta", "m::double", "m::alpha", "m::lines", "m::zeta"]),
            vec!["m::alpha", "m::zeta"]
        );
        assert!(reg.unbound(["m::double"]).is_empty());
    }

    #[test]
    fn extern_registry_register_and_call() {
        let mut reg = ExternRegistry::new();
        assert!(reg.is_empty());
        reg.register_fn("m::double", double).unwrap();
        assert!(matches!(
            reg.register_fn("m::double", double),
            Err(ValueError::DuplicateExtern(_))
        ));
        assert!(matches!(
            reg.register_table_fn("m::double", TableFn(|_: &[Value]| Ok(vec![]))),
            Err(ValueError::DuplicateExtern(_))
        ));
        let f = reg.lookup_fn("m::double").unwrap();
        assert_eq!(f.call(&[Value::i64(21)]), Ok(Value::i64(42)));
        assert!(matches!(f.call(&[Value::i64(i64::MAX)]), Err(ExternError::Failed(_))));
        assert!(reg.lookup_table_fn("m::double").is_none());
        let mut other = ExternRegistry::new();
        other
            .register_table_fn("m::rows", TableFn(|_: &[Value]| Ok(vec![vec![]])))
            .unwrap();
        reg.merge(other.clone()).unwrap();
        assert_eq!(reg.paths(), vec!["m::double", "m::rows"]);
        assert!(reg.merge(other).is_err());
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.lookup_table_fn("m::rows").unwrap().call(&[]).unwrap().len(), 1);
        assert_eq!(
            format!("{reg:?}"),
            r#"ExternRegistry { fns: ["m::double"], table_fns: ["m::rows"] }"#
        );
    }
}
#[cfg(test)]
mod m2_tests {
    use super::*;
    #[test]
    fn extern_signature_binding_is_exact() {
        let mut types = TypeTable::new();
        let a = types.insert(TypeDef::Int(IntTy::U64)).unwrap();
        let b = types.insert(TypeDef::Bytes).unwrap();
        let ob = types.insert(TypeDef::Option(b)).unwrap();
        let mut reg = ExternRegistry::new();
        reg.register_typed_fn(
            "math::f",
            vec![HostType::Int(IntTy::U64)],
            HostType::Option(&HostType::Bytes),
            |_: &[Value]| Ok(Value::Unit),
        )
        .unwrap();
        assert!(reg.bind("math::f", &types, &[a], &[ob], false).is_ok());
        for (params, outputs, table) in [(vec![b], vec![ob], false), (vec![a], vec![b], false), (vec![a], vec![ob], true)] {
            assert!(matches!(
                reg.bind("math::f", &types, &params, &outputs, table),
                Err(ValueError::ExternSignature { .. })
            ));
        }
        assert!(matches!(
            reg.bind("missing", &types, &[a], &[ob], false),
            Err(ValueError::ExternSignature { .. })
        ));
        reg.register_fn("untyped", |_: &[Value]| Ok(Value::Unit)).unwrap();
        assert!(matches!(
            reg.bind("untyped", &types, &[a], &[ob], false),
            Err(ValueError::ExternSignature { .. })
        ));
    }

    #[test]
    fn the_std_catalog_has_unique_paths_under_blossom_std() {
        let mut paths: Vec<&str> = STD_EXTERNS.iter().map(|e| e.path).collect();
        assert!(paths.iter().all(|p| p.starts_with("blossom_std::")));
        paths.sort_unstable();
        let n = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), n);
        assert_eq!(std_extern("blossom_std::checksum::crc32c").map(|e| e.ret), Some(HostType::Int(IntTy::U32)));
    }
}
