//! Slice 2: lattice stratification (SEM-102). An exact read of a lattice (`reveal!`, an antitone method, a negated
//! threshold) is a point of order, so a same-tick cycle through one is rejected with BLS0502; a cycle through
//! monotone reads only (a morphism like `d + w` on an `LMin`) is one recursive stratum.

use std::path::Path;

use blossom_driver::bls::compile_file;
use blossom_front::api::{BlsError, NodeSpec};

/// The codes of the errors compiling `rel` reports (empty when it compiles).
#[cfg(test)]
fn compile(rel: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    let (result, _) = compile_file(path.to_str().expect("a UTF-8 path"), &nodes);
    match result {
        Ok(_) => Vec::new(),
        Err(BlsError::Rejected(d)) => d.iter().map(|x| x.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{rel}: {e}"),
    }
}

#[test]
fn reveal_in_recursion_is_rejected() {
    let codes = compile("tests/corpus/lattices/BENCH-068a-reveal-in-recursion/program.bls");
    assert!(codes.iter().any(|c| c == "BLS0502"), "{codes:?}");
}

#[test]
fn antitone_method_in_recursion_is_rejected() {
    let codes = compile("tests/corpus/lattices/BENCH-068b-leq-in-recursion/program.bls");
    assert!(codes.iter().any(|c| c == "BLS0502"), "{codes:?}");
}

#[test]
fn monotone_lattice_recursion_stratifies() {
    for rel in [
        "tests/corpus/core/BENCH-031-lattice-sssp/program.bls",
        "tests/corpus/core/BENCH-032-lset-size-trap/program.bls",
        "tests/corpus/lattices/BENCH-052b-shortest-paths-cyclic/program.bls",
    ] {
        assert_eq!(compile(rel), Vec::<String>::new(), "{rel}");
    }
}
