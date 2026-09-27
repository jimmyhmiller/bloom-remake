#![deny(unsafe_op_in_unsafe_fn)]
//! SMT-LIB2 construction, robust response parsing and external solver drivers (M2.5).
mod asp;
pub mod conformance;
mod process;
mod sexp;
pub use asp::*;
pub use process::*;
pub use sexp::*;

/// Errors from external solvers and response decoding.
#[derive(Debug, thiserror::Error)]
pub enum SmtError {
    #[error("{solver} solver not found; searched {searched:?}")]
    SolverNotFound {
        solver: String,
        searched: Vec<std::path::PathBuf>,
    },
    #[error("solver error in `{command}`: {message}")]
    Solver { command: String, message: String },
    #[error("invalid SMT response at byte {offset}: {message}")]
    Parse { offset: usize, message: String },
    #[error("solver process ended: {0}")]
    Process(String),
    #[error("SMT operation requires {0}")]
    InvalidState(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Unimplemented(#[from] blossom_base::Unimplemented),
    #[error(transparent)]
    Internal(#[from] blossom_base::InternalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;
    #[test]
    fn sexp_parser_nested_and_comments() {
        let input = "; comment\n(model (define-fun |a b| () String \"hi \"\"there\"\"\"))";
        let parsed = Sexp::parse(input).unwrap();
        assert!(parsed.to_string().contains("|a b|"));
        assert_eq!(Sexp::parse(&parsed.to_string()).unwrap(), parsed);
    }
    #[test]
    fn sexp_parser_malformed() {
        for text in [")", "(a", "\"abc", "|x", "(((("] {
            assert!(Sexp::parse(text).is_err());
        }
        assert!(Sexp::parse_prefix("(a").unwrap().is_none());
        assert!(Sexp::parse_prefix("(a) (b)").unwrap().is_some());
    }
    proptest! {
        #[test]
        fn sexp_parser_fuzz_mirror(input in ".{0,1024}") {
            let result=Sexp::parse(&input);
            if let Ok(parsed)=result { prop_assert_eq!(Sexp::parse(&parsed.to_string()).unwrap(),parsed); }
        }
    }
    #[test]
    fn printer_roundtrip() {
        let x = Term::symbol("x");
        let term = Term::forall(
            [("x".into(), Sort::named("Int"))],
            Term::implies(Term::app(">", [x.clone(), Term::int(0)]), Term::eq(x, Term::int(1))),
        );
        assert_eq!(Sexp::parse(&term.to_string()).unwrap(), term.0);
        assert_eq!(
            Sort::array(Sort::named("Int"), Sort::bitvec(8)).to_string(),
            "(Array Int (_ BitVec 8))"
        );
        assert_eq!(Term::bitvec(3, 8).to_string(), "(_ bv3 8)");
    }
    #[test]
    fn timeout_is_unknown() {
        let mut solver = SmtProcess::spawn(&SmtConfig::default()).unwrap();
        solver.set_logic("QF_LIA").unwrap();
        assert_eq!(
            solver.check(&[], Duration::ZERO).unwrap(),
            SmtAnswer::Unknown("timeout".into())
        );
        assert_eq!(solver.check(&[], Duration::from_secs(5)).unwrap(), SmtAnswer::Sat);
    }
    #[test]
    fn solver_not_found_message() {
        let err = SmtError::SolverNotFound {
            solver: "z3".into(),
            searched: vec!["/missing/z3".into()],
        };
        assert!(err.to_string().contains("/missing/z3"));
        assert!(err.to_string().contains("z3"));
    }
    #[test]
    fn unsat_core_named() {
        let mut solver = SmtProcess::spawn(&SmtConfig::default()).unwrap();
        solver.set_logic("QF_LIA").unwrap();
        solver.assert(&Term::bool(true), Some("yes")).unwrap();
        solver.assert(&Term::bool(false), Some("no")).unwrap();
        assert_eq!(solver.check(&[], Duration::from_secs(5)).unwrap(), SmtAnswer::Unsat);
        let core = solver.unsat_core().unwrap();
        assert!(core.iter().any(|s| s.as_ref() == "no"));
    }
    #[test]
    fn model_parse_int_bool_bitvec_array_uninterpreted() {
        let raw=Sexp::parse("(model (define-fun i () Int (- 7)) (define-fun b () Bool true) (define-fun v () (_ BitVec 8) #x0f) (define-fun a () (Array Int Int) ((as const (Array Int Int)) 0)) (define-fun u () U U!val!0))").unwrap();
        let model = SmtModel::parse(raw).unwrap();
        assert_eq!(model.declarations.len(), 5);
        assert_eq!(model.declarations.get("i").unwrap().interpreted, ModelValue::Int(-7));
        assert_eq!(
            model.declarations.get("v").unwrap().interpreted,
            ModelValue::BitVec { value: 15, width: 8 }
        );
        assert_eq!(
            model.declarations.get("u").unwrap().interpreted,
            ModelValue::Uninterpreted("U!val!0".into())
        );
    }
}
