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

// ---- attributes and explicit ACLs (LANGUAGE §2.5, §18.3)

/// A two-role program with an external client role; `{REQ}` and `{PING}` are replaced by the attributes of the
/// channels `req` (Client -> A) and `ping` (A -> B), `{MORE}` by extra items.
const ROLES: &str = "program t version 1;
role Client: external;
role A;
role B;
static admins(p: Principal);
static pairs(p: Principal, q: Principal);
static nums(n: u64);
static peers(n: Node<B>);
{REQ} channel req(k: u64): Client -> A;
{PING} channel ping(k: u64): A -> B;
at A {
    input go(k: u64);
    output seen(k: u64);
    table allowed_a(p: Principal);
    s: on go(k), peers(n) { send ping(k) to n; }
    q: on req(k) { emit seen(k); }
}
at B {
    output got(k: u64);
    table allowed_b(p: Principal);
    r: on ping(k) { emit got(k); }
}
{MORE}
";

fn roles_src(req: &str, ping: &str, more: &str) -> &'static str {
    Box::leak(
        ROLES
            .replace("{REQ}", req)
            .replace("{PING}", ping)
            .replace("{MORE}", more)
            .into_boxed_str(),
    )
}

fn role_nodes() -> Vec<NodeSpec> {
    vec![
        NodeSpec {
            name: "a".to_owned(),
            role: Some("A".to_owned()),
        },
        NodeSpec {
            name: "b".to_owned(),
            role: Some("B".to_owned()),
        },
    ]
}

/// The code and message of every diagnostic compiling `src` for `nodes` reports.
fn diags_for(src: &'static str, nodes: &[NodeSpec]) -> Vec<(String, String)> {
    let mut sources = SourceDb::new();
    let d = match compile("test.bls", nodes, &mut One(src), &mut sources) {
        Ok((_, warnings)) => warnings,
        Err(BlsError::Rejected(d)) => d,
        Err(e) => panic!("{e}"),
    };
    d.iter()
        .map(|d| (d.code.as_str().to_owned(), d.message.clone()))
        .collect()
}

fn role_codes(req: &str, ping: &str, more: &str) -> Vec<String> {
    diags_for(roles_src(req, ping, more), &role_nodes())
        .into_iter()
        .map(|(c, _)| c)
        .collect()
}

#[test]
fn roles_template_compiles() {
    assert_eq!(role_codes("", "", ""), Vec::<String>::new());
}

/// The ACL of the channel named `name` in the lowered program.
fn acl_of(p: &blossom_ir::core::Program, name: &str) -> blossom_ir::core::AclSpec {
    let rel = p.rels.iter().find(|r| r.name.to_string() == name).unwrap();
    match &rel.class {
        blossom_ir::core::RelClass::Channel(ch) => ch.acl.clone(),
        other => panic!("{name} is not a channel: {other:?}"),
    }
}

fn rel_id(p: &blossom_ir::core::Program, name: &str) -> blossom_base::RelId {
    p.rels.iter().find(|r| r.name.to_string() == name).unwrap().id
}

#[test]
fn e01_del_lowers_its_explicit_acl() {
    use blossom_ir::core::{AclExplicit, AclSpec};
    let src = include_str!("../../../examples/e01_kvs.bls");
    let nodes = [NodeSpec {
        name: "s1".to_owned(),
        role: Some("Server".to_owned()),
    }];
    let mut sources = SourceDb::new();
    let (artifact, _) = match compile("e01.bls", &nodes, &mut One(src), &mut sources) {
        Ok(ok) => ok,
        Err(BlsError::Rejected(d)) => panic!("{:?}", d.iter().map(|d| d.message.clone()).collect::<Vec<_>>()),
        Err(e) => panic!("{e}"),
    };
    let p = artifact.program.get();
    assert_eq!(
        acl_of(p, "del"),
        AclSpec::Explicit(AclExplicit {
            roles: Vec::new(),
            external: true,
            principal_in: Some(rel_id(p, "admins")),
        })
    );
    assert_eq!(acl_of(p, "put"), AclSpec::Inferred);
}

#[test]
fn explicit_acl_with_roles_and_a_table() {
    use blossom_ir::core::{AclExplicit, AclSpec};
    let src = roles_src("", "#[accept(A, principal in allowed_b)]", "");
    let mut sources = SourceDb::new();
    let (artifact, _) = compile("test.bls", &role_nodes(), &mut One(src), &mut sources).unwrap();
    let p = artifact.program.get();
    let a = p.roles.iter().find(|r| r.name.to_string() == "A").unwrap().id;
    assert_eq!(
        acl_of(p, "ping"),
        AclSpec::Explicit(AclExplicit {
            roles: vec![a],
            external: false,
            principal_in: Some(rel_id(p, "allowed_b")),
        })
    );
}

#[test]
fn accept_argument_errors() {
    // An unknown role, an unknown relation.
    assert_eq!(role_codes("", "#[accept(C)]", ""), vec!["BLS0200"]);
    assert_eq!(
        role_codes("#[accept(external, principal in nobody)]", "", ""),
        vec!["BLS0200"]
    );
    // `principal in` a relation that is not unary, not of Principals, not static or table, or not at the receiver.
    assert_eq!(
        role_codes("#[accept(external, principal in pairs)]", "", ""),
        vec!["BLS0301"]
    );
    assert_eq!(
        role_codes("#[accept(external, principal in nums)]", "", ""),
        vec!["BLS0300"]
    );
    assert_eq!(
        role_codes("#[accept(external, principal in seen)]", "", ""),
        vec!["BLS0210"]
    );
    assert_eq!(
        role_codes("#[accept(external, principal in allowed_b)]", "", ""),
        vec!["BLS0404"]
    );
    // `external` on a channel whose source is not external; a role that is not the source.
    assert_eq!(role_codes("", "#[accept(external)]", ""), vec!["BLS0404"]);
    assert_eq!(role_codes("", "#[accept(B)]", ""), vec!["BLS0404"]);
    // An external role by name; no source at all; something that is not a source; two `#[accept]`s.
    assert_eq!(role_codes("#[accept(Client)]", "", ""), vec!["BLS0210"]);
    assert_eq!(role_codes("#[accept(principal in admins)]", "", ""), vec!["BLS0210"]);
    assert_eq!(role_codes("#[accept(3)]", "", ""), vec!["BLS0210"]);
    assert_eq!(
        role_codes("#[accept(external)] #[accept(external)]", "", ""),
        vec!["BLS0210"]
    );
    // `#[accept]` where it does not apply.
    assert_eq!(
        role_codes("", "", "#[accept(A)] static more(p: Principal);"),
        vec!["BLS0210"]
    );
}

#[test]
fn accept_on_a_role_free_channel() {
    let src = with_head("#[accept(external)] channel c(x: u64);\noutput o(x: u64);\na: on c(x) { emit o(x); }\n");
    assert_eq!(codes(src), vec!["BLS0404"]);
}

#[test]
fn unhandled_attributes_are_reported() {
    let has = |src: &'static str, code: &str, text: &str| {
        let d = diags_for(src, &role_nodes());
        assert!(
            d.iter().any(|(c, m)| c == code && m.contains(text)),
            "expected {code} mentioning `{text}`, got {d:?}"
        );
        assert_eq!(d.len(), 1, "{d:?}");
    };
    // Documented but not implemented: BLS0908 naming the feature.
    has(roles_src("", "#[fault(reliable)]", ""), "BLS0908", "LANG-155");
    has(roles_src("", "#[replicated]", ""), "BLS0908", "ANA-041");
    has(
        roles_src("", "", "static more(#[since(2)] p: Principal);"),
        "BLS0908",
        "LANG-261",
    );
    has(
        roles_src("", "", "static more(#[deprecated(since = 2)] p: Principal);"),
        "BLS0908",
        "LANG-265",
    );
    has(
        roles_src("", "", "#[finite(cap = 16)] module M { }"),
        "BLS0908",
        "ANA-122",
    );
    has(
        roles_src(
            "",
            "",
            "at B { output o2(k: u64); #[localize(chain)] h: on ping(k) { emit o2(k); } }",
        ),
        "BLS0908",
        "LANG-095",
    );
    has(
        roles_src(
            "",
            "",
            "at B { output o2(k: u64); h: on ping(k) { #[nondet(\"x\")] if k > 1 { emit o2(k); } } }",
        ),
        "BLS0908",
        "LANG-204",
    );
    has(
        roles_src(
            "",
            "",
            "at B { output o2(k: u64); h: on ping(k) { #[allow(unused)] emit o2(k); } }",
        ),
        "BLS0908",
        "TEST-091",
    );
    // Unknown, misplaced or malformed: BLS0210. `#[a, b]` is two attributes.
    has(
        roles_src("", "#[acept(A)]", ""),
        "BLS0210",
        "unknown attribute `#[acept]`",
    );
    has(
        roles_src("", "#[fault(lossy), acept(A)]", ""),
        "BLS0210",
        "unknown attribute `#[acept]`",
    );
    has(roles_src("", "#[fault(sometimes)]", ""), "BLS0210", "fault model");
    has(
        roles_src("", "", "#[readonly] static more(p: Principal);"),
        "BLS0210",
        "tables",
    );
    has(
        roles_src("", "", "enum E { #[unknown(1)] U, V }"),
        "BLS0210",
        "no arguments",
    );
    has(
        Box::leak(format!("#![allow(x)]\n{}", roles_src("", "", "")).into_boxed_str()),
        "BLS0210",
        "a file",
    );
    // Reserved for a later edition.
    has(roles_src("", "#[blazes(seal)]", ""), "BLS0907", "blazes");
}

#[test]
fn handled_attributes_pass() {
    assert_eq!(role_codes("", "#[fault(lossy)]", ""), Vec::<String>::new());
    assert_eq!(role_codes("", "", "enum E { #[unknown] U, V }"), Vec::<String>::new());
}
