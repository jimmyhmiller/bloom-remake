//! Blossom frontend tests over in-memory programs: lattice typing, lifts and the lattice diagnostics.

use std::sync::Arc;

use blossom_base::SourceDb;

use crate::api::{BlsError, NodeSpec, compile};
use crate::ded::LoadedFile;
use crate::modules::Loader;

struct One(&'static str);

impl Loader for One {
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        match from {
            None => Ok(LoadedFile {
                key: Arc::from(path),
                text: self.0.to_owned(),
            }),
            Some(_) => Err(format!("no module `{path}` in this test")),
        }
    }
}

/// The codes of the diagnostics compiling `src` reports (empty when it compiles without warnings).
fn codes(src: &'static str) -> Vec<String> {
    let mut sources = SourceDb::new();
    let nodes = [NodeSpec {
        name: "n1".to_owned(),
        role: None,
    }];
    match compile("test.bls", &nodes, &mut One(src), &mut sources) {
        Ok((_, warnings)) => warnings.iter().map(|d| d.code.as_str().to_owned()).collect(),
        Err(BlsError::Rejected(d)) => d.iter().map(|d| d.code.as_str().to_owned()).collect(),
        Err(e) => panic!("{e}"),
    }
}

const HEAD: &str = "program t version 1;\ninput go(k: u64, v: u64);\n";

fn with_head(body: &str) -> &'static str {
    Box::leak(format!("{HEAD}{body}").into_boxed_str())
}

#[test]
fn lifts_and_thresholds_compile() {
    let src = with_head(
        "table m(k: u64, v: LMax<u64>);\n\
         table s(k: u64, v: LSet<u64>);\n\
         output big(k: u64);\n\
         a: on go(k, v) { emit m(k, v); emit s(k, set[v]); }\n\
         b: while m(k, x) where x >= 10 { emit big(k); }\n\
         c: while s(k, x) where x.size() > 2, 3 in x { emit big(k); }\n",
    );
    assert_eq!(codes(src), Vec::<String>::new());
}

#[test]
fn lattice_join_key_is_bls0304() {
    let src = with_head(
        "table m(k: u64, v: LMax<u64>);\n\
         table n(k: u64, v: LMax<u64>);\n\
         output same(k: u64);\n\
         a: on go(k, v) { emit m(k, v); emit n(k, v); }\n\
         b: while m(k, x), n(k, x) { emit same(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0304"]);
}

#[test]
fn lattice_equality_is_bls0305() {
    let src = with_head(
        "table m(k: u64, v: LMax<u64>);\n\
         output same(k: u64);\n\
         a: on go(k, v) { emit m(k, v); }\n\
         b: while m(k, x), m(j, y) where x == y { emit same(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0305"]);
}

#[test]
fn non_threshold_comparison_is_bls0306() {
    let src = with_head(
        "table m(k: u64, v: LMax<u64>);\n\
         output small(k: u64);\n\
         a: on go(k, v) { emit m(k, v); }\n\
         b: while m(k, x) where x <= 3 { emit small(k); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0306"]);
}

#[test]
fn deleting_a_lattice_is_bls0410() {
    let src = with_head(
        "table m(k: u64, v: LMax<u64>);\n\
         a: on go(k, v) { delete m(k, v); }\n",
    );
    assert_eq!(codes(src), vec!["BLS0410"]);
}

#[test]
fn bang_rule() {
    let missing = with_head(
        "table s(k: u64, v: LSet<u64>);\n\
         output empty(k: u64);\n\
         a: on go(k, v) { emit s(k, set[v]); }\n\
         b: while s(k, x) where x.is_empty() { emit empty(k); }\n",
    );
    assert_eq!(codes(missing), vec!["BLS0700"]);
    let superfluous = with_head(
        "table s(k: u64, v: LSet<u64>);\n\
         output some(k: u64);\n\
         a: on go(k, v) { emit s(k, set[v]); }\n\
         b: while s(k, x) where x.nonempty!() { emit some(k); }\n",
    );
    assert_eq!(codes(superfluous), vec!["BLS0701"]);
}
