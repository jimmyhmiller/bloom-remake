use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blossom_artifact::ded::{DedArtifact, DedRelKind, SpecFeed};
use blossom_base::{SourceDb, Symbol};
use blossom_ir::core::RelClass;

use super::*;

/// Files by name; `include` paths are looked up as written.
struct MemLoader(BTreeMap<&'static str, &'static str>);

impl DedLoader for MemLoader {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        self.0
            .get(path)
            .map(|t| LoadedFile {
                key: Arc::from(path),
                text: (*t).to_owned(),
            })
            .ok_or_else(|| "no such file".to_owned())
    }
}

/// Real files; `include` paths are relative to the including file.
struct FsLoader;

impl DedLoader for FsLoader {
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        let p = match from {
            Some(f) => Path::new(f).parent().unwrap_or(Path::new(".")).join(path),
            None => PathBuf::from(path),
        };
        let key = p.canonicalize().map_err(|e| e.to_string())?;
        let text = std::fs::read_to_string(&key).map_err(|e| e.to_string())?;
        Ok(LoadedFile {
            key: Arc::from(key.to_string_lossy().as_ref()),
            text,
        })
    }
}

fn compile_mem(files: &[(&'static str, &'static str)], nodes: &[&str]) -> Result<DedArtifact, DedError> {
    let mut loader = MemLoader(files.iter().copied().collect());
    let root = files.first().map(|(n, _)| *n).unwrap();
    compile(&[root], nodes, &mut loader, &mut SourceDb::new())
}

fn codes(files: &[(&'static str, &'static str)], nodes: &[&str]) -> Vec<String> {
    match compile_mem(files, nodes) {
        Ok(_) => Vec::new(),
        Err(DedError::Rejected(d)) => d.iter().map(|d| d.code.as_str().to_owned()).collect(),
        Err(e) => panic!("internal error: {e}"),
    }
}

const SIMPLE: &str = "\
log(Node, Pload) :- bcast(Node, Pload);
node(Node, Neighbor)@next :- node(Node, Neighbor);
log(Node, Pload)@next :- log(Node, Pload);
log(Node2, Pload)@async :- bcast(Node1, Pload), node(Node1, Node2);
node(\"a\", \"b\")@1;
node(\"a\", \"c\")@1;
node(\"b\", \"a\")@1;
bcast(\"a\", \"data\")@1;
pre(N, P) :- log(M, P), node(M, N), notin bcast(M, P)@1, notin crash(M, M, _);
post(N, P) :- log(N, P);
";

fn rel<'a>(a: &'a DedArtifact, name: &str) -> &'a blossom_artifact::ded::DedRel {
    a.rels.iter().find(|r| r.name.as_str() == name).unwrap()
}

#[test]
fn simple_deliv_lowers_to_protocol_and_spec() {
    let a = compile_mem(&[("simple.ded", SIMPLE)], &["c", "a", "b"]).unwrap();
    assert_eq!(a.nodes, [Symbol::intern("a"), Symbol::intern("b"), Symbol::intern("c")]);
    let p = a.protocol.get();

    let log = rel(&a, "log");
    assert_eq!(log.kind, DedRelKind::Protocol);
    let log_rel = &p.rels.get(log.protocol.unwrap()).unwrap();
    assert!(matches!(log_rel.class, RelClass::Idb));
    assert_eq!(log_rel.schema.cols.len(), 1, "the location column is implicit");
    let chan = &p.rels.get(log.channel.unwrap()).unwrap();
    assert!(matches!(chan.class, RelClass::Channel(_)));
    assert_eq!(chan.schema.cols.len(), 2, "a channel keeps its destination");
    assert!(log.input.is_none());

    // `node` has facts and a rule: its facts arrive through a generated input.
    let node = rel(&a, "node");
    assert_ne!(node.input, node.protocol);
    assert!(matches!(
        p.rels.get(node.input.unwrap()).unwrap().class,
        RelClass::Event(_)
    ));
    // `bcast` has facts only: it is an input itself.
    let bcast = rel(&a, "bcast");
    assert_eq!(bcast.input, bcast.protocol);

    assert_eq!(a.inputs.len(), 4);
    assert!(a.inputs.iter().all(|f| f.tick.0 == 1));

    assert_eq!(rel(&a, "pre").kind, DedRelKind::Spec);
    assert_eq!(rel(&a, "crash").kind, DedRelKind::Crash);
    let spec = a.spec.as_ref().unwrap();
    let feeds = &spec.feeds;
    assert!(feeds.iter().any(|f| matches!(f, SpecFeed::Crash { .. })));
    assert!(
        feeds
            .iter()
            .any(|f| matches!(f, SpecFeed::AtTick { tick, .. } if tick.0 == 1))
    );
    assert_eq!(feeds.iter().filter(|f| matches!(f, SpecFeed::AtEot { .. })).count(), 2);
    // The spec keeps every column.
    assert_eq!(spec.program.get().rels.get(spec.pre).unwrap().schema.cols.len(), 2);
}

#[test]
fn program_without_pre_and_post_has_no_spec() {
    let a = compile_mem(&[("p.ded", "p(N, X)@next :- p(N, X);\np(\"a\", 1)@1;")], &["a"]).unwrap();
    assert!(a.spec.is_none());
}

#[test]
fn arithmetic_aggregates_and_constant_locations_lower() {
    let a = compile_mem(
        &[(
            "p.ded",
            "c(N, K + 1)@next :- c(N, K), K < 3;\n\
             c(\"a\", 0)@1;\n\
             total(N, sum<K>) :- c(N, K);\n\
             n(N, count<K>) :- c(N, K);\n\
             big(N) :- n(N, C), C > 1;\n\
             only_a(\"a\", K) :- c(\"a\", K);",
        )],
        &["a", "b"],
    )
    .unwrap();
    assert!(a.spec.is_none());
    assert_eq!(
        a.protocol.get().rules.len(),
        5 + 1,
        "five rules and the input bridge of `c`"
    );
}

#[test]
fn static_errors_have_their_codes() {
    assert_eq!(
        codes(&[("p.ded", "p(N, X) :- q(N, X);\nq(N) :- r(N);")], &["a"]),
        ["BLS0301"]
    );
    assert_eq!(codes(&[("p.ded", "p(N, X) :- q(N, X), r(M, X);")], &["a"]), ["BLS0508"]);
    assert_eq!(codes(&[("p.ded", "p(M, X) :- q(N, X), r(N, M);")], &["a"]), ["BLS0508"]);
    assert_eq!(
        codes(&[("p.ded", "p(N) :- q(N), notin crash(N, N, _);")], &["a"]),
        ["BLS0509"]
    );
    assert_eq!(codes(&[("p.ded", "p(N) :- q(N), r(N)@2;")], &["a"]), ["BLS0509"]);
    assert_eq!(codes(&[("p.ded", "q(\"z\")@1;")], &["a"]), ["BLS0200"]);
    assert_eq!(
        codes(&[("p.ded", "p(N, X) :- q(N, X), X > \"s\";")], &["a"]),
        ["BLS0300"]
    );
    assert_eq!(codes(&[("p.ded", "pre(N) :- q(N);")], &["a"]), ["BLS0900"]);
    assert_eq!(codes(&[("p.ded", "p(N, Y) :- q(N, X);")], &["a"]), ["BLS0500"]);
    assert_eq!(codes(&[("p.ded", "p(N) :- q(N), notin r(N, Y);")], &["a"]), ["BLS0500"]);
    assert_eq!(codes(&[("p.ded", "crash(\"a\", \"a\", 1)@1;")], &["a"]), ["BLS0405"]);
    assert_eq!(codes(&[("p.ded", "clock(N) :- q(N);")], &["a"]), ["BLS0201"]);
    assert_eq!(codes(&[("p.ded", "include \"missing.ded\";")], &["a"]), ["BLS0204"]);
    assert_eq!(
        codes(
            &[("p.ded", "include \"q.ded\";"), ("q.ded", "include \"p.ded\";")],
            &["a"]
        ),
        ["BLS0204"]
    );
    assert_eq!(codes(&[("p.ded", "p(N) :- q(N);")], &[]), ["BLS0200"]);
    assert_eq!(codes(&[("p.ded", "p(N) :- q(N);")], &["a", "a"]), ["BLS0200"]);
    assert_eq!(
        codes(
            &[(
                "p.ded",
                "p(N)@next :- q(N), notin pre(N);\npre(N) :- q(N);\npost(N) :- q(N);"
            )],
            &["a"]
        ),
        ["BLS0901"]
    );
    // A deductive rule that reads the spec is a spec helper.
    assert!(
        codes(
            &[(
                "p.ded",
                "p(N) :- q(N), notin pre(N);\npre(N) :- q(N);\npost(N) :- q(N);"
            )],
            &["a"]
        )
        .is_empty()
    );
    // With a spec, a relation that reads several nodes is a spec helper; without one it is an error.
    assert!(
        codes(
            &[(
                "p.ded",
                "d(X) :- q(N, X), q(M, X), N != M;\npre(X) :- d(X);\npost(X) :- d(X);"
            )],
            &["a"]
        )
        .is_empty()
    );
    assert_eq!(
        codes(&[("p.ded", "d(X) :- q(N, X), q(M, X), N != M;")], &["a"]),
        ["BLS0508", "BLS0508"]
    );
}

#[test]
fn a_file_included_twice_is_loaded_once() {
    let a = compile_mem(
        &[
            ("p.ded", "include \"q.ded\";\ninclude \"q.ded\";\np(N) :- q(N);"),
            ("q.ded", "q(\"a\")@1;"),
        ],
        &["a"],
    )
    .unwrap();
    assert_eq!(a.inputs.len(), 1);
}

/// The nodes a corpus case is compiled for: its failure spec's, or its deployment's.
fn case_nodes(manifest: &toml::Value) -> Vec<String> {
    let names = manifest
        .get("expect_ldfi")
        .and_then(|l| l.get("nodes"))
        .and_then(|n| n.as_array())
        .map(|a| a.iter().map(|n| n.as_str().unwrap().to_owned()).collect::<Vec<_>>());
    names.unwrap_or_else(|| {
        manifest["deploy"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap().to_owned())
            .collect()
    })
}

#[test]
fn every_molly_corpus_program_compiles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/ldfi/molly");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("manifest.toml").is_file())
        .collect();
    cases.sort();
    assert!(cases.len() >= 90);
    let mut failures = Vec::new();
    for case in cases {
        let manifest: toml::Value =
            toml::Value::Table(toml::from_str(&std::fs::read_to_string(case.join("manifest.toml")).unwrap()).unwrap());
        let program = case.join(manifest["program"].as_str().unwrap());
        let nodes = case_nodes(&manifest);
        let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let mut sources = SourceDb::new();
        match compile(&[program.to_str().unwrap()], &node_refs, &mut FsLoader, &mut sources) {
            Ok(a) => {
                if manifest.get("expect_ldfi").is_some() {
                    assert!(a.spec.is_some(), "{}: an LDFI case needs pre and post", case.display());
                }
            }
            Err(DedError::Rejected(d)) => failures.push(format!(
                "{}: {}",
                case.file_name().unwrap().to_string_lossy(),
                d.iter()
                    .map(|d| format!("{d} at {:?}", d.primary.map(|s| sources.line_col(s.file, s.lo))))
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            Err(e) => failures.push(format!("{}: {e}", case.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
