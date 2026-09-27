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

use blossom_base::Unimplemented;

use crate::error::ValueError;
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

/// Host function implementations by path (for example `blossom_std::hash::sha256`).
#[derive(Clone, Default)]
pub struct ExternRegistry {
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
